//! The agent side of the Host over RPC (3.x `agent/index.ts`): chats, models, MCP, skills, project memory, Git
//! messages, map explanations, editor AI, proxy and permission settings.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rainy_protocol::chat::{ExternalAgentConfig, ExternalBackendRef, ImageContent, SessionSummary};
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::complete;
use super::external::{ExternalManager, check_backend_ref, external_provider_id};
use super::inline::{EditRequest, InlineAssist, SuggestRequest};
use super::llm::{Level, Llm, reply_text};
use super::mcp::McpManager;
use super::memory::{self, ProjectMemory};
use super::models::{Completion, Models, deepseek_flash};
use super::session::{Services, Session, summary_of};
use super::skills;
use super::snapshots::Snapshots;
use super::store::{ChatStore, MetaPatch, NewChat, Parent};
use super::tools::browser::Preview;
use crate::Host;
use crate::error::{HostError, HostResult};
use crate::git::{self, Git};
use crate::locale::l;
use crate::map::ProjectMaps;
use crate::network::{PASSWORD_MASK, proxy_url};
use crate::rpc;
use crate::settings::{McpServerConfig, ModelSelection, ProxySettings, check_server};

/// Spill files and undo snapshots older than this are deleted at startup.
const RETENTION: Duration = Duration::from_secs(30 * 24 * 3600);
const GIT_MESSAGE_TIMEOUT: Duration = Duration::from_secs(90);

fn not_found_chat() -> HostError {
    HostError::new("not-found", l("对话不存在。", "This chat does not exist."))
}

fn no_worktree() -> HostError {
    HostError::new("no-worktree", l("这个对话没有独立工作区。", "This chat has no worktree."))
}

fn no_model() -> HostError {
    HostError::new("no-model", l("还没有可用的模型。请先在“设置 → 模型”中添加一个模型。", "No model is set up yet. Add one in Settings → Models."))
}

fn permission_mode(mode: &str) -> HostResult<String> {
    if matches!(mode, "read-only" | "ask" | "auto") { Ok(mode.to_owned()) } else { Err(HostError::invalid("Unknown permission mode.")) }
}

/// The agent side of one Host.
pub struct Agent {
    services: Arc<Services>,
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    /// One chat opens at a time, so two requests for the same chat share it.
    opening: tokio::sync::Mutex<()>,
    inline: InlineAssist,
    git: Git,
    maps: Arc<ProjectMaps>,
    /// The folder Settings → Skills last showed, for the status sent when an MCP server changes.
    extensions_cwd: Mutex<Option<String>>,
    /// What swarms act on chats through; the hub holds it weakly.
    swarm_chats: Mutex<Option<Arc<dyn super::swarm::Chats>>>,
}

impl Agent {
    /// Build the agent side of `host` and register its RPC methods.
    pub async fn start(host: &Arc<Host>, preview: Option<Preview>) -> HostResult<Arc<Self>> {
        let env = host.env.clone();
        let store = ChatStore::load(env.home.join("chats"))?;
        let llm = Arc::new(Llm::new(host.network.clone(), env.version));
        let models = Arc::new(Models::new(host.settings.clone(), host.network.clone(), llm));
        if env.configure_deepseek {
            models.configure(&serde_json::to_value(deepseek_flash())?)?;
        }
        let mcp = McpManager::new(host.network.clone());
        {
            // Web MCP servers that failed may be reachable through the new proxy settings.
            let mcp = mcp.clone();
            host.network.on_change(move || mcp.retry_web_servers());
        }
        let model_activity = Arc::new(AtomicU64::new(0));
        let memory = {
            let (activity, last, rpc) = (host.activity.clone(), model_activity.clone(), host.rpc.clone());
            ProjectMemory::new(memory::Deps {
                root: env.carrier_state_root.join("project-memory"),
                target: env.target_id.clone(),
                models: models.clone(),
                chats: store.clone(),
                projects: host.projects.clone(),
                registry: host.registry.clone(),
                busy: Box::new(move || activity.active()),
                last_model_activity: Box::new(move || last.load(Ordering::SeqCst)),
                changed: Box::new(move |workspace_id| rpc.emit("memory.changed", json!({ "workspaceId": workspace_id }))),
            })
        };
        let snapshots = Arc::new(Snapshots::new(env.home.join("snapshots")));
        let swarm = super::swarm::Hub::new(env.home.join("swarms"), store.clone(), host.rpc.clone());
        let external = ExternalManager::new(host.settings.clone(), env.windows);
        let services = Arc::new(Services {
            env: env.clone(),
            settings: host.settings.clone(),
            models: models.clone(),
            store,
            mcp: mcp.clone(),
            memory,
            projects: host.projects.clone(),
            runtime: host.runtime.clone(),
            activity: host.activity.clone(),
            rpc: host.rpc.clone(),
            snapshots: snapshots.clone(),
            network: host.network.clone(),
            preview,
            model_activity,
            swarm,
            languages: host.ide.get().map(|ide| ide.languages.clone()),
            ide_tools: host.ide.get().map(|ide| ide.tools.clone()),
            maps: host.maps.clone(),
            external,
        });
        let rpc = host.rpc.clone();
        let agent = Arc::new(Agent {
            inline: InlineAssist::new(models.clone(), host.settings.clone(), move |request_id, text| rpc.emit("inline.progress", json!({ "requestId": request_id, "text": text }))),
            git: Git::new(host.projects.clone(), host.runtime.clone()),
            maps: host.maps.clone(),
            services,
            sessions: Arc::default(),
            opening: tokio::sync::Mutex::new(()),
            extensions_cwd: Mutex::new(None),
            swarm_chats: Mutex::new(None),
        });
        {
            let sessions = agent.sessions.clone();
            host.activity.add(move || sessions.lock().unwrap().values().any(|session| session.running()));
        }
        mcp.sync(&host.settings.get().mcp_servers);
        {
            let agent = Arc::downgrade(&agent);
            mcp.on_change(move || {
                if let Some(agent) = agent.upgrade() {
                    let cwd = agent.extensions_cwd.lock().unwrap().clone();
                    agent.services.rpc.emit("extensions.changed", agent.extensions_status(cwd.as_deref()));
                }
            });
        }
        {
            let agent = Arc::downgrade(&agent);
            models.on_change(move |status| {
                let Some(agent) = agent.upgrade() else { return };
                agent.services.rpc.emit("models.changed", status);
                // Open chats show the default model and its context window; both may have changed.
                for session in agent.live_sessions() {
                    session.refresh();
                }
            });
        }
        let home = env.home.clone();
        tokio::task::spawn_blocking(move || {
            sweep_spill(&home.join("spill"));
            snapshots.sweep(RETENTION);
        });
        let chats: Arc<dyn super::swarm::Chats> = Arc::new(SwarmChats(Arc::downgrade(&agent)));
        agent.services.swarm.set_chats(Arc::downgrade(&chats));
        *agent.swarm_chats.lock().unwrap() = Some(chats);
        agent.register();
        Ok(agent)
    }

    /// Stop chats, memory, MCP servers and editor requests, and save the chat index.
    pub async fn close(&self) {
        self.inline.close();
        let sessions = self.live_sessions();
        futures::future::join_all(sessions.iter().map(|session| session.close())).await;
        self.services.external.close().await;
        self.services.memory.close().await;
        self.services.mcp.close().await;
        if let Err(error) = self.services.store.flush() {
            log::error!("[chats] saving the index failed: {error}");
        }
    }

    fn live_sessions(&self) -> Vec<Arc<Session>> {
        self.sessions.lock().unwrap().values().cloned().collect()
    }

    fn live(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    /// A chat, loaded from disk the first time it is asked for.
    async fn session(&self, id: &str) -> HostResult<Arc<Session>> {
        if let Some(session) = self.live(id) {
            return Ok(session);
        }
        let _opening = self.opening.lock().await;
        if let Some(session) = self.live(id) {
            return Ok(session);
        }
        if self.services.store.get(id).is_none() {
            return Err(not_found_chat());
        }
        let session = Session::open(self.services.clone(), id).await?;
        self.sessions.lock().unwrap().insert(id.to_owned(), session.clone());
        Ok(session)
    }

    fn summary(&self, id: &str) -> HostResult<SessionSummary> {
        if let Some(session) = self.live(id) {
            return Ok(session.summary());
        }
        self.services.store.get(id).map(|chat| summary_of(&chat, "idle", false)).ok_or_else(not_found_chat)
    }

    fn cwd_for(&self, workspace_id: Option<&str>) -> Option<String> {
        workspace_id.and_then(|workspace| self.services.projects.get(workspace)).map(|project| project.path)
    }

    fn extensions_status(&self, cwd: Option<&str>) -> Value {
        let trusted = cwd.is_some_and(|dir| self.services.settings.is_workspace_trusted(None, Some(Path::new(dir))));
        json!({
            "skills": skills::discover(cwd.map(Path::new)),
            "servers": self.services.settings.get().mcp_servers,
            "status": self.services.mcp.status(),
            "trusted": trusted,
        })
    }

    fn security_status(&self, workspace_id: Option<&str>) -> Value {
        let rules = workspace_id.map(|workspace| self.services.settings.permission_rules(workspace)).unwrap_or_default();
        let workspace_path = self.cwd_for(workspace_id);
        let trusted = self.services.settings.is_workspace_trusted(workspace_id, workspace_path.as_deref().map(Path::new));
        let mut status = json!({
            "defaultMode": self.services.settings.default_permission(),
            "rules": rules,
            "trusted": trusted,
            "trustedWorkspaces": self.services.settings.trusted_workspaces(),
        });
        if let Some(path) = workspace_path {
            status["workspacePath"] = json!(path);
        }
        status
    }

    fn save_servers(&self, change: impl FnOnce(&mut Vec<McpServerConfig>) -> HostResult<()>) -> HostResult<Value> {
        let data = self.services.settings.update(|data| change(&mut data.mcp_servers))?;
        self.services.mcp.sync(&data.mcp_servers);
        let cwd = self.extensions_cwd.lock().unwrap().clone();
        let status = self.extensions_status(cwd.as_deref());
        self.services.rpc.emit("extensions.changed", &status);
        Ok(status)
    }

    /// The proxy choice from Settings, where a masked password stands for the stored one.
    fn proxy_choice(&self, params: ProxyParams) -> HostResult<ProxySettings> {
        if !matches!(params.mode.as_str(), "system" | "off" | "manual") {
            return Err(HostError::invalid("Unknown proxy mode."));
        }
        let manual = params.mode == "manual";
        let typed = ProxySettings { url: manual.then(|| params.url.unwrap_or_default().trim().to_owned()), mode: params.mode };
        let choice = self.services.network.with_stored_password(typed);
        if !manual {
            return Ok(choice);
        }
        let url = proxy_url(choice.url.as_deref())
            .ok_or_else(|| HostError::invalid(l("代理地址无效，应为 host:port 或 http://host:port。", "The proxy address is invalid. Use host:port or http://host:port.")))?;
        let password = url::Url::parse(&url).ok().and_then(|url| url.password().map(|password| percent_encoding::percent_decode_str(password).decode_utf8_lossy().into_owned()));
        // The mask stands for the saved password only at the saved address.
        if password.as_deref() == Some(PASSWORD_MASK) {
            return Err(HostError::invalid(l("代理地址已更改，请重新输入代理密码。", "The proxy address changed. Enter the proxy password again.")));
        }
        Ok(choice)
    }

    async fn network_status(&self) -> Value {
        let target = self.services.models.resolve(None).map(|resolved| resolved.setup.base_url);
        json!(self.services.network.status(target.as_deref()).await)
    }

    // ── registration ──

    /// Register one method whose parameters deserialize into `P`.
    fn on<P, T, F, Fut>(self: &Arc<Self>, method: &str, handler: F)
    where
        P: DeserializeOwned + Send + 'static,
        T: Serialize + 'static,
        F: Fn(Arc<Agent>, P) -> Fut + Copy + Send + Sync + 'static,
        Fut: Future<Output = HostResult<T>> + Send + 'static,
    {
        let agent = self.clone();
        self.services.rpc.register(method, move |params| {
            let agent = agent.clone();
            async move { handler(agent, rpc::params(params)?).await }
        });
    }

    fn register(self: &Arc<Self>) {
        self.register_models();
        self.register_external();
        self.register_sessions();
        self.register_chat();
        self.register_tools();
        self.register_settings();
    }

    fn register_external(self: &Arc<Self>) {
        self.on("external.status", |agent, _: Value| async move { Ok(agent.services.external.status()) });
        // Read an agent's usage limits again (Codex and Antigravity), unless read within the last minute and not
        // `force`d; the answer and `external.changed` carry them.
        self.on("external.quota", |agent, params: Value| async move {
            let kind = params.get("kind").and_then(Value::as_str).unwrap_or_default().to_owned();
            let force = params.get("force").and_then(Value::as_bool).unwrap_or(false);
            let cwd = params.get("cwd").and_then(Value::as_str).map(PathBuf::from).filter(|path| path.is_dir()).unwrap_or_else(|| agent.services.env.home.clone());
            if agent.services.external.refresh_quota(&kind, force, &cwd).await {
                agent.services.rpc.emit("external.changed", &agent.services.external.status());
            }
            Ok(agent.services.external.status())
        });
        self.on("external.probe", |agent, params: ExternalProbeParams| async move {
            let cwd = match params.cwd.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
                Some(raw) => {
                    let path = PathBuf::from(raw);
                    if !path.is_dir() {
                        return Err(HostError::invalid(l("探测目录不存在或不是有效文件夹。", "The probe directory does not exist or is not a valid folder.")));
                    }
                    path
                }
                None => agent.services.env.home.clone(),
            };
            agent
                .services
                .external
                .probe(params.kind.as_deref(), params.binary.as_deref(), params.args.as_deref(), &cwd)
                .await
        });
        self.on("external.configure", |agent, params: ExternalConfigureParams| async move {
            let status = agent.services.external.configure(params.config, params.remove).await?;
            agent.services.rpc.emit("external.changed", &status);
            Ok(status)
        });
    }

    fn register_models(self: &Arc<Self>) {
        self.on("models.status", |agent, _: Value| async move { Ok(agent.services.models.status()) });
        self.on("models.configure", |agent, input: Value| async move { agent.services.models.configure(&input) });
        self.on("models.remove", |agent, params: ProviderParams| async move { agent.services.models.remove(&params.provider) });
        self.on("models.select", |agent, selection: ModelSelection| async move { agent.services.models.select(&selection) });
        self.on("models.discover", |agent, input: Value| async move { agent.services.models.discover(&input).await });
        self.on("models.probe", |agent, input: Value| async move { agent.services.models.probe(&input).await });
        self.on("models.connectStrata", |agent, input: Value| async move { agent.services.models.connect_strata(&input).await });
        self.on("prompt.global", |agent, params: TextParams| async move { agent.services.models.set_global_prompt(&params.text) });
    }

    fn register_sessions(self: &Arc<Self>) {
        self.on("sessions.list", |agent, params: ListParams| async move {
            let chats = agent.services.store.list();
            chats
                .into_iter()
                .filter(|chat| params.workspace_id.is_none() || chat.workspace_id == params.workspace_id)
                .filter(|chat| params.archived.is_none_or(|archived| chat.archived == archived))
                .map(|chat| agent.summary(&chat.id))
                .collect::<HostResult<Vec<_>>>()
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
            let created = agent.summary(&chat.id)?;
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
        self.on("sessions.branch", |agent, params: BranchParams| async move {
            let chat = agent.services.store.get(&params.session_id).ok_or_else(not_found_chat)?;
            let mut worktree = chat.worktree.ok_or_else(no_worktree)?;
            agent.git.worktree_branch(&worktree, &params.name).await?;
            worktree.branch = Some(params.name.trim().to_owned());
            agent.set_meta(&params.session_id, MetaPatch { worktree: Some(Some(worktree)), ..MetaPatch::default() }).await
        });
        self.on("sessions.handOff", |agent, params: SessionRef| async move {
            let chat = agent.services.store.get(&params.session_id).ok_or_else(not_found_chat)?;
            let worktree = chat.worktree.ok_or_else(no_worktree)?;
            if agent.live(&params.session_id).is_some_and(|session| session.running()) {
                return Err(HostError::new("busy", l("对话还在运行，请等它停下再移交。", "The chat is still running. Hand it off once it stops.")));
            }
            let files = agent.git.worktree_hand_off(&worktree, &agent.services.env.home.join("worktrees")).await?;
            // The chat opens again in the project's checkout.
            let live = agent.sessions.lock().unwrap().remove(&params.session_id);
            if let Some(session) = live {
                session.close().await;
            }
            let summary = agent.set_meta(&params.session_id, MetaPatch { cwd: Some(worktree.repository.clone()), worktree: Some(None), ..MetaPatch::default() }).await?;
            Ok(json!({ "session": summary, "files": files }))
        });
        self.on("sessions.delete", |agent, params: SessionRef| async move {
            let stored = agent.services.store.get(&params.session_id);
            let is_coordinator = stored.as_ref().and_then(|chat| chat.swarm.as_ref()).is_some_and(|link| link.coordinator == params.session_id);
            if is_coordinator {
                let workers = agent.services.swarm.worker_ids(&params.session_id);
                agent.services.swarm.cleanup_coordinator(&params.session_id);
                for worker_id in workers {
                    let worker = agent.sessions.lock().unwrap().remove(&worker_id);
                    if let Some(worker) = worker {
                        worker.close().await;
                    }
                }
            }
            let live = agent.sessions.lock().unwrap().remove(&params.session_id);
            if let Some(session) = live {
                session.close().await;
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
            let created = agent.summary(&chat.id)?;
            agent.services.rpc.emit("sessions.changed", &created);
            Ok(created)
        });
    }

    async fn set_meta(&self, id: &str, patch: MetaPatch) -> HostResult<SessionSummary> {
        self.services.store.set_meta(id, patch).await?;
        let changed = self.summary(id)?;
        self.services.rpc.emit("sessions.changed", &changed);
        Ok(changed)
    }

    fn register_chat(self: &Arc<Self>) {
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
            let (base, current) = agent.session(&params.session_id).await?.change_base(&params.path)?;
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

    fn register_tools(self: &Arc<Self>) {
        self.on("map.explain", |agent, params: WorkspaceRef| async move {
            agent.maps.explain(&params.workspace_id, &agent.services.models, &agent.services.settings.locale()).await
        });
        self.on("map.footprint", |agent, params: SessionRef| async move {
            let snapshot = agent.session(&params.session_id).await?.snapshot(None);
            let turns = crate::map::footprint::footprint(&snapshot.entries, snapshot.streaming.as_ref(), &snapshot.summary.cwd);
            Ok(json!({ "turns": turns }))
        });
        for method in ["git.status", "git.stage", "git.unstage", "git.discard", "git.commit", "git.init", "git.message"] {
            let agent = self.clone();
            self.services.rpc.register(method, move |params| {
                let agent = agent.clone();
                async move {
                    let request: GitRequest = rpc::params(params)?;
                    let (workspace, root, git) = (request.workspace_id.as_str(), request.root_id.as_str(), &agent.git);
                    match method {
                        "git.status" => git.status(workspace).await,
                        "git.stage" => git.stage(workspace, root, &request.paths).await.map(|()| Value::Null),
                        "git.unstage" => git.unstage(workspace, root, &request.paths).await.map(|()| Value::Null),
                        "git.discard" => git.discard(workspace, root, &request.paths).await.map(|()| Value::Null),
                        "git.commit" => git.commit(workspace, root, &request.message, request.stage_all).await,
                        "git.init" => git.init(workspace, root).await.map(|()| Value::Null),
                        _ => agent.commit_message(workspace, root).await,
                    }
                }
            });
        }
        self.on("inline.complete", |agent, request: SuggestRequest| async move { Ok(agent.inline.complete(request).await) });
        self.on("inline.edit", |agent, request: EditRequest| async move { agent.inline.edit(request).await });
        self.on("inline.cancel", |agent, params: RequestRef| async move {
            agent.inline.cancel(&params.request_id);
            Ok(Value::Null)
        });
    }

    /// A commit message for the changes of one folder, written by the default model.
    async fn commit_message(&self, workspace_id: &str, root_id: &str) -> HostResult<Value> {
        let resolved = self.services.models.resolve(None).ok_or_else(no_model)?;
        let prompt = self.git.message_prompt(workspace_id, root_id).await?;
        let options = Completion { system: Some(git::MESSAGE_SYSTEM.into()), max_tokens: Some(600), thinking: Some(Level::Off), cancel: None };
        let reply = self.services.models.complete(&resolved, &prompt, &[], GIT_MESSAGE_TIMEOUT, options).await;
        if reply.stop_reason == "error" || reply.stop_reason == "aborted" {
            return Err(HostError::new("message-failed", reply.error_message.unwrap_or_else(|| l("生成提交说明失败。", "The commit message could not be written."))));
        }
        let message = git::clean_message(&reply_text(&reply));
        if message.is_empty() {
            return Err(HostError::new("message-failed", l("模型没有给出提交说明，请重试。", "The model gave no commit message. Try again.")));
        }
        Ok(json!({ "message": message }))
    }

    fn register_settings(self: &Arc<Self>) {
        self.on("network.status", |agent, _: Value| async move { Ok(agent.network_status().await) });
        self.on("network.set", |agent, params: ProxyParams| async move {
            let choice = agent.proxy_choice(params)?;
            let data = agent.services.settings.update(|data| {
                data.proxy = Some(choice);
                Ok(())
            })?;
            agent.services.network.configure(data.proxy);
            Ok(agent.network_status().await)
        });
        self.on("network.test", |agent, params: ProxyParams| async move {
            let proxy = agent.proxy_choice(params)?;
            let resolved = agent.services.models.resolve(None).ok_or_else(|| HostError::new("no-model", l("请先配置模型，再测试连接。", "Configure a model before testing the connection.")))?;
            let url = resolved.setup.base_url;
            let target = url::Url::parse(&url).map_err(|error| HostError::invalid(error.to_string()))?;
            let started = crate::text::now_ms();
            let client = agent.services.network.client_with(&proxy, &target).await;
            // Any HTTP answer, even 401 or 404, shows the service is reachable through these settings.
            let answer = client.get(target).timeout(Duration::from_secs(15)).send().await;
            let ms = crate::text::now_ms().saturating_sub(started);
            Ok(match answer {
                Ok(response) => json!({ "ok": true, "url": url, "status": response.status().as_u16(), "ms": ms }),
                Err(error) => {
                    let cause = std::error::Error::source(&error).map(|cause| format!(": {cause}")).unwrap_or_default();
                    json!({ "ok": false, "url": url, "ms": ms, "error": format!("{error}{cause}") })
                }
            })
        });
        self.on("security.status", |agent, params: OptionalWorkspace| async move { Ok(agent.security_status(params.workspace_id.as_deref())) });
        self.on("security.setDefault", |agent, params: ModeParams| async move {
            let next = permission_mode(&params.mode)?;
            let previous = agent.services.settings.default_permission();
            if next != previous {
                // Chats from before 2.1.1 have no mode of their own; they keep the one they have been running with.
                for chat in agent.services.store.list().into_iter().filter(|chat| chat.permission.is_none()) {
                    match agent.live(&chat.id) {
                        Some(session) => {
                            session.set_permission(&previous).await?;
                        }
                        None => {
                            agent.services.store.set_meta(&chat.id, MetaPatch { permission: Some(previous.clone()), ..MetaPatch::default() }).await?;
                        }
                    }
                }
            }
            agent.services.settings.set_default_permission(&next)?;
            for session in agent.live_sessions() {
                session.refresh();
            }
            let status = agent.security_status(None);
            agent.services.rpc.emit("security.changed", json!({ "defaultMode": status["defaultMode"] }));
            Ok(status)
        });
        self.on("security.addRule", |agent, params: RuleParams| async move {
            agent.services.settings.set_permission_rule_kind(&params.workspace_id, &params.kind, &params.rule, true)?;
            let status = agent.security_status(Some(&params.workspace_id));
            agent.services.rpc.emit("security.changed", &status);
            Ok(status)
        });
        self.on("security.removeRule", |agent, params: RuleParams| async move {
            agent.services.settings.set_permission_rule_kind(&params.workspace_id, &params.kind, &params.rule, false)?;
            let status = agent.security_status(Some(&params.workspace_id));
            agent.services.rpc.emit("security.changed", &status);
            Ok(status)
        });
        for method in ["settings.trustWorkspace", "security.setTrust"] {
            self.on(method, |agent, params: TrustParams| async move {
                let target = params
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned)
                    .or_else(|| agent.cwd_for(params.workspace_id.as_deref()))
                    .or_else(|| params.workspace_id.as_deref().map(str::trim).filter(|id| !id.is_empty()).map(str::to_owned))
                    .ok_or_else(|| HostError::invalid(l("需要提供工作区 ID 或路径。", "A workspaceId or path is required.")))?;
                agent.services.settings.set_workspace_trusted(&target, params.trusted)?;
                let status = agent.security_status(params.workspace_id.as_deref());
                agent.services.rpc.emit("security.changed", &status);
                let cwd = agent.extensions_cwd.lock().unwrap().clone();
                agent.services.rpc.emit("extensions.changed", agent.extensions_status(cwd.as_deref()));
                Ok(status)
            });
        }
        self.on("hooks.list", |agent, params: OptionalWorkspace| async move {
            let global = agent.services.settings.hooks();
            let cwd = agent.cwd_for(params.workspace_id.as_deref());
            let trusted = agent.services.settings.is_workspace_trusted(params.workspace_id.as_deref(), cwd.as_deref().map(Path::new));
            let has_project = cwd.as_deref().is_some_and(|dir| super::hooks::has_project_hooks(Path::new(dir)));
            let project = if trusted {
                cwd.as_deref().map(|dir| super::hooks::load_project_hooks(Path::new(dir))).unwrap_or_default()
            } else {
                Vec::new()
            };
            Ok(json!({
                "global": global,
                "project": project,
                "trusted": trusted,
                "hasProjectHooks": has_project,
                "projectHooksSkipped": !trusted && has_project,
            }))
        });
        self.on("hooks.save", |agent, params: HooksParams| async move {
            let saved = agent.services.settings.set_hooks(params.hooks)?;
            Ok(json!({ "global": saved }))
        });
        self.on("extensions.status", |agent, params: ExtensionsParams| async move {
            *agent.extensions_cwd.lock().unwrap() = params.cwd.clone();
            Ok(agent.extensions_status(params.cwd.as_deref()))
        });
        self.on("extensions.saveServer", |agent, params: SaveServerParams| async move {
            check_server(&params.server)?;
            agent.save_servers(|servers| {
                let previous = params.previous_name.as_deref().unwrap_or(&params.server.name);
                if params.server.name != previous && servers.iter().any(|item| item.name == params.server.name) {
                    let name = &params.server.name;
                    return Err(HostError::new("duplicate", l(&format!("已存在名为 {name} 的 MCP 服务器。"), &format!("An MCP server named {name} already exists."))));
                }
                match servers.iter().position(|item| item.name == previous) {
                    Some(at) => servers[at] = params.server,
                    None => servers.push(params.server),
                }
                Ok(())
            })
        });
        self.on("extensions.removeServer", |agent, params: NameParams| async move {
            agent.save_servers(|servers| {
                servers.retain(|item| item.name != params.name);
                Ok(())
            })
        });
        self.on("memory.status", |agent, params: WorkspaceRef| async move { agent.services.memory.status(&params.workspace_id).await });
        self.on("memory.setEnabled", |agent, params: MemorySwitches| async move {
            agent.services.memory.set_enabled(&params.workspace_id, params.enabled, params.generation_enabled).await
        });
        self.on("memory.edit", |agent, params: MemoryEdit| async move {
            agent.services.memory.edit(&params.workspace_id, &params.id, &params.text, params.expected_revision).await
        });
        self.on("memory.delete", |agent, params: MemoryNote| async move { agent.services.memory.remove(&params.workspace_id, Some(&params.id)).await });
        self.on("memory.clear", |agent, params: WorkspaceRef| async move { agent.services.memory.remove(&params.workspace_id, None).await });
    }
}

/// Delete spill files older than [`RETENTION`] and prune emptied session spill folders.
fn sweep_spill(root: &Path) {
    let Ok(sessions) = std::fs::read_dir(root) else { return };
    let cutoff = std::time::SystemTime::now() - RETENTION;
    for directory in sessions.flatten() {
        let dir_path = directory.path();
        let Ok(files) = std::fs::read_dir(&dir_path) else { continue };
        let mut remaining = 0usize;
        for file in files.flatten() {
            let path: PathBuf = file.path();
            if file.metadata().and_then(|info| info.modified()).is_ok_and(|modified| modified < cutoff) {
                if std::fs::remove_file(path).is_err() {
                    remaining += 1;
                }
            } else {
                remaining += 1;
            }
        }
        if remaining == 0 {
            let _ = std::fs::remove_dir(&dir_path);
        }
    }
}

// ── parameters ──

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionRef {
    session_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TeamParams {
    session_id: String,
    /// Only board posts newer than this.
    #[serde(default)]
    after: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TeamMemberParams {
    session_id: String,
    member_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemParams {
    session_id: String,
    id: String,
}

impl ItemParams {
    fn command_id(&self) -> HostResult<&str> {
        if self.id.trim().is_empty() {
            return Err(HostError::invalid(l("命令编号无效。", "The command id is invalid.")));
        }
        Ok(&self.id)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListParams {
    workspace_id: Option<String>,
    archived: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateParams {
    workspace_id: Option<String>,
    cwd: Option<String>,
    permission: Option<String>,
    /// Work in a new Git worktree of the project instead of its checkout.
    #[serde(default)]
    worktree: bool,
    #[serde(default)]
    external_backend: Option<ExternalBackendRef>,
    #[serde(default)]
    model: Option<ModelSelection>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExternalProbeParams {
    kind: Option<String>,
    binary: Option<String>,
    args: Option<Vec<String>>,
    cwd: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExternalConfigureParams {
    config: ExternalAgentConfig,
    #[serde(default)]
    remove: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchParams {
    query: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchParams {
    session_id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetParams {
    session_id: String,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameParams {
    session_id: String,
    title: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveParams {
    session_id: String,
    archived: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PinParams {
    session_id: String,
    pinned: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForkParams {
    session_id: String,
    entry_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendParams {
    session_id: String,
    text: String,
    images: Option<Vec<ImageContent>>,
    mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrchestrateParams {
    session_id: String,
    /// `null` turns orchestration off.
    executor: Option<ModelSelection>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetModelParams {
    session_id: String,
    #[serde(flatten)]
    selection: ModelSelection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModeSessionParams {
    session_id: String,
    mode: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApproveParams {
    session_id: String,
    tool_call_id: String,
    decision: String,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PathParams {
    session_id: String,
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallsParams {
    session_id: String,
    tool_call_ids: Vec<String>,
}

impl CallsParams {
    fn checked(&self) -> HostResult<&[String]> {
        if self.tool_call_ids.is_empty() || self.tool_call_ids.len() > 1000 || self.tool_call_ids.iter().any(|id| id.is_empty() || id.len() > 512) {
            return Err(HostError::invalid("Invalid tool call ids."));
        }
        Ok(&self.tool_call_ids)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompleteParams {
    session_id: Option<String>,
    workspace_id: Option<String>,
    query: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceRef {
    workspace_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OptionalWorkspace {
    workspace_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitRequest {
    workspace_id: String,
    #[serde(default)]
    root_id: String,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    stage_all: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestRef {
    request_id: String,
}

#[derive(Deserialize)]
struct ProxyParams {
    mode: String,
    url: Option<String>,
}

#[derive(Deserialize)]
struct ModeParams {
    mode: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuleParams {
    workspace_id: String,
    kind: String,
    rule: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrustParams {
    workspace_id: Option<String>,
    path: Option<String>,
    trusted: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HooksParams {
    hooks: Vec<super::hooks::HookRule>,
}

#[derive(Deserialize)]
struct ExtensionsParams {
    cwd: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveServerParams {
    server: McpServerConfig,
    previous_name: Option<String>,
}

#[derive(Deserialize)]
struct NameParams {
    name: String,
}

#[derive(Deserialize)]
struct ProviderParams {
    provider: String,
}

#[derive(Deserialize)]
struct TextParams {
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemorySwitches {
    workspace_id: String,
    enabled: Option<bool>,
    generation_enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemoryEdit {
    workspace_id: String,
    id: String,
    text: String,
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemoryNote {
    workspace_id: String,
    id: String,
}

/// The chats a swarm acts on: the agent's own operations on them.
struct SwarmChats(std::sync::Weak<Agent>);

impl SwarmChats {
    fn agent(&self) -> HostResult<Arc<Agent>> {
        self.0.upgrade().ok_or_else(|| HostError::new("closed", l("Host 正在关闭。", "The Host is closing.")))
    }
}

impl super::swarm::Chats for SwarmChats {
    fn create_worker(&self, coordinator: String, name: String, first: String, model: Option<ModelSelection>, isolated: bool) -> futures::future::BoxFuture<'static, HostResult<String>> {
        let agent = self.agent();
        Box::pin(async move {
            let agent = agent?;
            let lead = agent.services.store.get(&coordinator).ok_or_else(not_found_chat)?;
            // Workers start from the project's checkout, also when the coordinator runs in a worktree of its own.
            let base = lead.worktree.as_ref().map_or_else(|| lead.cwd.clone(), |worktree| worktree.repository.clone());
            let (cwd, worktree) = if isolated {
                let (worktree, inside) = agent.git.worktree_add(Path::new(&base), &agent.services.env.home.join("worktrees")).await?;
                (inside, Some(worktree))
            } else {
                (base, None)
            };
            let lead_model = agent.live(&coordinator).and_then(|session| session.selection()).or(lead.model.clone());
            let chat = agent.services.store.create(NewChat {
                workspace_id: lead.workspace_id.clone(),
                cwd,
                parent: None,
                entries: Vec::new(),
                title: format!("@{name}"),
                model: model.or(lead_model),
                permission: Some(lead.permission.clone().unwrap_or_else(|| agent.services.settings.default_permission())),
                worktree,
                swarm: Some(rainy_protocol::chat::SwarmLink { coordinator: coordinator.clone(), name }),
                external_backend: None,
            })?;
            agent.services.rpc.emit("sessions.changed", agent.summary(&chat.id)?);
            agent.session(&chat.id).await?.send(&first, Vec::new(), false).await?;
            Ok(chat.id)
        })
    }

    fn deliver(&self, id: String, text: String) -> futures::future::BoxFuture<'static, HostResult<()>> {
        let agent = self.agent();
        Box::pin(async move {
            agent?.session(&id).await?.send_swarm(&text).await?;
            Ok(())
        })
    }

    fn resolve_model(&self, hint: &str) -> HostResult<ModelSelection> {
        self.agent()?.services.models.resolve_by_hint(hint)
    }

    fn land(&self, id: String) -> futures::future::BoxFuture<'static, HostResult<usize>> {
        let agent = self.agent();
        Box::pin(async move {
            let agent = agent?;
            let chat = agent.services.store.get(&id).ok_or_else(not_found_chat)?;
            let worktree = chat.worktree.ok_or_else(no_worktree)?;
            let files = agent.git.worktree_hand_off(&worktree, &agent.services.env.home.join("worktrees")).await?;
            let live = agent.sessions.lock().unwrap().remove(&id);
            if let Some(session) = live {
                session.close().await;
            }
            agent.set_meta(&id, MetaPatch { cwd: Some(worktree.repository.clone()), worktree: Some(None), ..MetaPatch::default() }).await?;
            Ok(files)
        })
    }

    fn retire(&self, id: String, discard: bool) -> futures::future::BoxFuture<'static, HostResult<()>> {
        let agent = self.agent();
        Box::pin(async move {
            let agent = agent?;
            if let Some(session) = agent.live(&id) {
                session.abort();
            }
            let chat = agent.services.store.get(&id).ok_or_else(not_found_chat)?;
            if let (Some(worktree), true) = (&chat.worktree, discard) {
                agent.git.worktree_remove(worktree).await;
                agent.set_meta(&id, MetaPatch { cwd: Some(worktree.repository.clone()), worktree: Some(None), ..MetaPatch::default() }).await?;
            }
            agent.set_meta(&id, MetaPatch { archived: Some(true), ..MetaPatch::default() }).await?;
            Ok(())
        })
    }

    fn abort(&self, id: &str) {
        if let Some(session) = self.0.upgrade().and_then(|agent| agent.live(id)) {
            session.abort();
        }
    }

    fn state(&self, id: &str) -> super::swarm::ChatState {
        match self.0.upgrade().and_then(|agent| agent.live(id)) {
            Some(session) => session.swarm_state(),
            None => super::swarm::ChatState { status: "idle".into(), awaiting_approval: false, user_waiting: false, has_pending: false },
        }
    }

    fn last_reply(&self, id: &str) -> Option<String> {
        self.0.upgrade().and_then(|agent| agent.live(id)).and_then(|session| session.last_reply())
    }
}
