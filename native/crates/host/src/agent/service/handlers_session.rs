//! Session lifecycle (`sessions.*`, `usage.stats`) and chat interaction (`chat.*`, `swarm.*`, `external.team*`) RPC handlers.

use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};

use super::params::{
    ApproveParams, ArchiveParams, BranchParams, CallsParams, CompleteParams, CreateParams, ForkParams, GetParams, ItemParams, ListParams, ModeSessionParams, OrchestrateParams, PathParams,
    PinParams, RenameParams, SearchParams, SendParams, SessionRef, SetModelParams, TeamMemberParams, TeamParams,
};
use super::{Agent, no_worktree, not_found_chat, permission_mode};
use crate::agent::complete;
use crate::agent::external::{check_backend_ref, external_provider_id};
use crate::agent::session::summary_of;
use crate::agent::store::{MetaPatch, NewChat, Parent};
use crate::error::HostError;
use crate::locale::l;
use crate::settings::ModelSelection;

impl Agent {
    pub(super) fn register_sessions(self: &Arc<Self>) {
        self.on("sessions.list", |agent, params: ListParams| async move {
            let chats = agent.services.store.list();
            let live_summaries: std::collections::HashMap<String, rainy_protocol::chat::SessionSummary> = {
                let sessions = agent.sessions.lock().unwrap();
                sessions.iter().map(|(id, entry)| (id.clone(), entry.session.summary())).collect()
            };
            let list = chats
                .into_iter()
                .filter(|chat| params.workspace_id.is_none() || chat.workspace_id == params.workspace_id)
                .filter(|chat| params.archived.is_none_or(|archived| chat.archived == archived))
                .map(|chat| live_summaries.get(&chat.id).cloned().unwrap_or_else(|| summary_of(&chat, "idle", false)))
                .collect::<Vec<_>>();
            Ok(list)
        });
        self.on("sessions.create", |agent, params: CreateParams| async move {
            let cwd = agent.cwd_for(params.workspace_id.as_deref()).or(params.cwd).unwrap_or_else(|| agent.services.env.home.to_string_lossy().into_owned());
            // Every chat records its mode, so a later change of the default in Settings does not reach it.
            let permission = match params.permission {
                Some(mode) => permission_mode(&mode)?,
                None => agent.services.settings.default_permission(),
            };
            let (cwd, worktree) = if params.worktree {
                let (worktree, inside) = agent.git.worktree_add(Path::new(&cwd), &agent.services.env.home.join("worktrees")).await?;
                (inside, Some(worktree))
            } else {
                (cwd, None)
            };
            let external_backend = match params.external_backend {
                Some(backend) => Some(check_backend_ref(&backend)?),
                None => None,
            };
            let model = match (&external_backend, params.model) {
                (Some(backend), Some(selection)) => Some(ModelSelection {
                    provider: external_provider_id(&backend.kind),
                    model: if selection.model.trim().is_empty() { "default".into() } else { selection.model.trim().to_owned() },
                    thinking: selection.thinking.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned),
                }),
                (Some(_), None) => None,
                (None, sel) => sel,
            };
            let chat = agent.services.store.create(NewChat { workspace_id: params.workspace_id, cwd, parent: None, entries: Vec::new(), title: String::new(), model, permission: Some(permission), worktree, swarm: None, external_backend })?;
            let created = summary_of(&chat, "idle", false);
            agent.services.rpc.emit("sessions.changed", &created);
            Ok(created)
        });
        self.on("sessions.get", |agent, params: GetParams| async move { Ok(agent.session(&params.session_id).await?.page_snapshot(params.limit)) });
        self.on("sessions.rename", |agent, params: RenameParams| async move {
            let title = params.title.trim();
            if title.is_empty() || title.len() > 80 {
                return Err(HostError::invalid(l("标题需为 1–80 字节。", "A title must be 1 to 80 bytes long.")));
            }
            agent.set_meta(&params.session_id, MetaPatch { title: Some(title.to_owned()), ..MetaPatch::default() }).await
        });
        self.on("sessions.archive", |agent, params: ArchiveParams| async move {
            if params.archived
                && let Some(session) = agent.live(&params.session_id)
            {
                session.abort();
            }
            agent.set_meta(&params.session_id, MetaPatch { archived: Some(params.archived), ..MetaPatch::default() }).await
        });
        self.on("sessions.pin", |agent, params: PinParams| async move {
            agent.set_meta(&params.session_id, MetaPatch { pinned: Some(params.pinned), ..MetaPatch::default() }).await
        });
        self.on("sessions.read", |agent, params: SessionRef| async move {
            let chat = agent.services.store.get(&params.session_id).ok_or_else(not_found_chat)?;
            if chat.read_at.is_some_and(|read| read >= chat.updated_at) {
                return agent.summary(&params.session_id);
            }
            let now = (crate::text::now_ms() as f64).max(chat.updated_at);
            agent.set_meta(&params.session_id, MetaPatch { read_at: Some(now), ..MetaPatch::default() }).await
        });
        self.on("sessions.search", |agent, params: SearchParams| async move {
            let words: Vec<String> = params.query.split_whitespace().map(str::to_lowercase).take(8).collect();
            let store = agent.services.store.clone();
            let found = tokio::task::spawn_blocking(move || store.search(&words, 40)).await.map_err(|error| HostError::other(error.to_string()))?;
            Ok(found.into_iter().map(|(id, snippet)| json!({ "sessionId": id, "snippet": snippet })).collect::<Vec<_>>())
        });
        self.on("usage.stats", |agent, params: rainy_protocol::UsageStatsParams| async move {
            if params.workspace_id.as_ref().is_some_and(|id| id.len() > 128) || params.days > 3660 {
                return Err(HostError::new("invalid", l("用量统计的参数无效。", "The usage request is not valid.")));
            }
            let store = agent.services.store.clone();
            let now = crate::text::now_ms() as f64;
            tokio::task::spawn_blocking(move || store.usage_stats(&params, now)).await.map_err(|error| HostError::other(error.to_string()))
        });
        self.on("sessions.branch", |agent, params: BranchParams| async move {
            let chat = agent.services.store.get(&params.session_id).ok_or_else(not_found_chat)?;
            let mut worktree = chat.worktree.ok_or_else(no_worktree)?;
            agent.git.worktree_branch(&worktree, &params.name).await?;
            worktree.branch = Some(params.name.trim().to_owned());
            agent.set_meta(&params.session_id, MetaPatch { worktree: Some(Some(worktree)), ..MetaPatch::default() }).await
        });
        self.on("sessions.handOff", |agent, params: SessionRef| async move {
            // Serialize under `opening` so no concurrent `session()` / `send()` can reopen or run in the worktree while handing off.
            let _opening = agent.opening.lock().await;
            let chat = agent.services.store.get(&params.session_id).ok_or_else(not_found_chat)?;
            let worktree = chat.worktree.ok_or_else(no_worktree)?;
            if agent.live(&params.session_id).is_some_and(|session| session.running()) {
                return Err(HostError::new("busy", l("对话还在运行，请等它停下再移交。", "The chat is still running. Hand it off once it stops.")));
            }
            let live = agent.sessions.lock().unwrap().remove(&params.session_id);
            if let Some(entry) = live {
                entry.session.close().await;
            }
            let files = agent.git.worktree_hand_off(&worktree, &agent.services.env.home.join("worktrees")).await?;
            let summary = agent.set_meta(&params.session_id, MetaPatch { cwd: Some(worktree.repository.clone()), worktree: Some(None), ..MetaPatch::default() }).await?;
            Ok(json!({ "session": summary, "files": files }))
        });
        self.on("sessions.delete", |agent, params: SessionRef| async move {
            // Serialize under `opening` so `Agent::session` cannot reopen the chat while it is closing and being deleted.
            let _opening = agent.opening.lock().await;
            let stored = agent.services.store.get(&params.session_id);
            let is_coordinator = stored.as_ref().and_then(|chat| chat.swarm.as_ref()).is_some_and(|link| link.coordinator == params.session_id);
            if is_coordinator {
                let workers = agent.services.swarm.worker_ids(&params.session_id);
                agent.services.swarm.cleanup_coordinator(&params.session_id);
                for worker_id in workers {
                    let worker = agent.sessions.lock().unwrap().remove(&worker_id);
                    if let Some(worker) = worker {
                        worker.session.close().await;
                    }
                }
            }
            let live = agent.sessions.lock().unwrap().remove(&params.session_id);
            if let Some(entry) = live {
                entry.session.close().await;
            }
            let worktree = stored.and_then(|chat| chat.worktree);
            agent.services.store.delete(&params.session_id).await?;
            if let Some(worktree) = worktree {
                agent.git.worktree_remove(&worktree).await;
            }
            agent.services.snapshots.remove(&params.session_id);
            let _ = std::fs::remove_dir_all(agent.services.env.home.join("spill").join(&params.session_id));
            agent.services.rpc.emit("sessions.removed", json!({ "sessionId": params.session_id }));
            Ok(Value::Null)
        });
        self.on("sessions.fork", |agent, params: ForkParams| async move {
            let file = agent.services.store.read(&params.session_id).await?;
            let index = file.entries.iter().position(|entry| entry.id() == params.entry_id).ok_or_else(|| HostError::new("not-found", l("消息不存在。", "This message does not exist.")))?;
            // Keep the tool results that answer calls in the cut-off reply.
            let end = index + file.entries[index + 1..].iter().take_while(|entry| matches!(entry, rainy_protocol::chat::TranscriptEntry::ToolResult { .. })).count();
            let title = if file.chat.title.is_empty() { l("新对话", "New chat") } else { file.chat.title.clone() };
            let chat = agent.services.store.create(NewChat {
                workspace_id: file.chat.workspace_id.clone(),
                // A branch of a chat in a worktree works in the project's checkout: the worktree stays the first chat's.
                cwd: file.chat.worktree.as_ref().map_or_else(|| file.chat.cwd.clone(), |worktree| worktree.repository.clone()),
                parent: Some(Parent { session_id: params.session_id, entry_id: params.entry_id }),
                entries: file.entries[..=end].to_vec(),
                title: l(&format!("{title}（分支）"), &format!("{title} (branch)")),
                model: file.chat.model.clone(),
                permission: Some(file.chat.permission.clone().unwrap_or_else(|| agent.services.settings.default_permission())),
                worktree: None,
                swarm: None,
                external_backend: file.chat.external_backend.clone(),
            })?;
            let created = summary_of(&chat, "idle", false);
            agent.services.rpc.emit("sessions.changed", &created);
            Ok(created)
        });
    }

    pub(super) fn register_chat(self: &Arc<Self>) {
        self.on("chat.send", |agent, params: SendParams| async move {
            let steer = match params.mode.as_deref() {
                None | Some("queue") => false,
                Some("steer") => true,
                Some(_) => return Err(HostError::invalid("Unknown send mode.")),
            };
            let queued = agent.session(&params.session_id).await?.send(&params.text, params.images.unwrap_or_default(), steer).await?;
            Ok(json!({ "queued": queued }))
        });
        self.on("chat.abort", |agent, params: SessionRef| async move {
            if let Some(session) = agent.live(&params.session_id) {
                session.abort();
            }
            Ok(Value::Null)
        });
        self.on("chat.unqueue", |agent, params: ItemParams| async move { Ok(agent.session(&params.session_id).await?.unqueue(&params.id)) });
        self.on("chat.setModel", |agent, params: SetModelParams| async move { agent.session(&params.session_id).await?.set_model(&params.selection).await });
        self.on("swarm.get", |agent, params: SessionRef| async move {
            let hub = &agent.services.swarm;
            let Some(coordinator) = hub.coordinator_of(&params.session_id) else { return Ok(Value::Null) };
            let title = agent.services.store.get(&coordinator).map(|chat| chat.title).unwrap_or_default();
            Ok(json!({ "coordinatorId": coordinator, "title": title, "posts": hub.posts(&coordinator), "members": hub.members(&coordinator) }))
        });
        self.on("swarm.stop", |agent, params: SessionRef| async move {
            if let Some(coordinator) = agent.services.swarm.coordinator_of(&params.session_id) {
                agent.services.swarm.stop(&coordinator);
            }
            Ok(Value::Null)
        });
        self.on("chat.setOrchestrate", |agent, params: OrchestrateParams| async move {
            agent.session(&params.session_id).await?.set_orchestrate(params.executor.as_ref()).await
        });
        self.on("chat.compact", |agent, params: SessionRef| async move { Ok(json!({ "message": agent.session(&params.session_id).await?.compact_now().await? })) });
        self.on("session.contextBreakdown", |agent, params: SessionRef| async move { agent.session(&params.session_id).await?.context_breakdown() });
        // The team an external agent runs for a chat (an Antigravity swarm or teamwork, Codex sub-agents).
        self.on("external.team", |agent, params: TeamParams| async move { Ok(agent.session(&params.session_id).await?.external_team(params.after)) });
        self.on("external.teamMember", |agent, params: TeamMemberParams| async move { agent.session(&params.session_id).await?.team_activity(&params.member_id).await });
        self.on("chat.contextBreakdown", |agent, params: SessionRef| async move { agent.session(&params.session_id).await?.context_breakdown() });
        self.on("chat.retry", |agent, params: SessionRef| async move { agent.session(&params.session_id).await?.retry().await.map(|()| Value::Null) });
        self.on("chat.stopCommand", |agent, params: ItemParams| async move { agent.session(&params.session_id).await?.stop_command(params.command_id()?).await });
        self.on("chat.dismissCommand", |agent, params: ItemParams| async move { agent.session(&params.session_id).await?.dismiss_command(params.command_id()?) });
        self.on("chat.setPermission", |agent, params: ModeSessionParams| async move { agent.session(&params.session_id).await?.set_permission(&permission_mode(&params.mode)?).await });
        self.on("chat.approve", |agent, params: ApproveParams| async move {
            if !matches!(params.decision.as_str(), "allow" | "always" | "deny") || params.reason.as_ref().is_some_and(|reason| reason.chars().count() > 4000) {
                return Err(HostError::invalid("Invalid approval."));
            }
            let session = agent.live(&params.session_id).ok_or_else(|| HostError::new("not-found", l("这个操作已不在等待批准。", "This action is no longer waiting for approval.")))?;
            session.approve(&params.tool_call_id, &params.decision, params.reason.as_deref()).map(|()| Value::Null)
        });
        self.on("chat.changes", |agent, params: SessionRef| async move { Ok(agent.session(&params.session_id).await?.changes().await) });
        self.on("chat.changeBase", |agent, params: PathParams| async move {
            let (base, current) = agent.session(&params.session_id).await?.change_base(&params.path).await?;
            Ok(json!({ "base": base, "current": current }))
        });
        self.on("chat.keep", |agent, params: CallsParams| async move {
            let ids = params.checked()?;
            agent.session(&params.session_id).await?.keep(ids).await.map(|()| Value::Null)
        });
        self.on("chat.revert", |agent, params: CallsParams| async move {
            let ids = params.checked()?;
            agent.session(&params.session_id).await?.revert(ids).await
        });
        self.on("chat.complete", |agent, params: CompleteParams| async move {
            let chat = params.session_id.as_deref().and_then(|id| agent.services.store.get(id));
            let cwd = chat.map(|chat| chat.cwd).or_else(|| agent.cwd_for(params.workspace_id.as_deref()));
            let query = params.query.clone();
            let mut items = match cwd {
                Some(cwd) => tokio::task::spawn_blocking(move || complete::files(Path::new(&cwd), &query)).await.map_err(|error| HostError::other(error.to_string()))?,
                None => Vec::new(),
            };
            items.extend(complete::chats(&agent.services.store.list(), &params.query, params.session_id.as_deref()));
            Ok(items)
        });
    }
}
