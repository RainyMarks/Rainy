//! External CLI agent turns (`codex app-server --stdio`, `agy acp`, custom ACP) and Antigravity team tracking.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rainy_protocol::chat::{AssistantBlock, AssistantMessage, ExternalBackendRef, ExternalTeam, ImageContent, QueuedMessage, ResultContent, TeamActivity, TranscriptEntry, UsageSummary};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::super::changes;
use super::super::context::model_user_text;
use super::super::external::team::AntigravityTeam;
use super::super::external::{ApprovalDecision, ExternalEvent, ExternalTurnHandle, TurnRequest, external_provider_id};
use super::super::llm;
use super::super::store::MetaPatch;
use super::{Answer, Approval, RunInfo, SUPERSEDED, Session, StartGuard, TOOL_UPDATE_THROTTLE_MS, fallback_title, is_busy, refusal};
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::text::now_ms;

impl Session {
    /// Send a user message to an external CLI agent (`codex app-server --stdio` or ACP such as `agy acp`).
    pub(super) async fn send_external(self: &Arc<Self>, backend: ExternalBackendRef, text: &str, images: Vec<ImageContent>, steer: bool) -> HostResult<bool> {
        // Rainy's own /swarm is no command of an external agent. Antigravity and other ACP agents have commands of
        // their own (Antigravity's /swarm and /teamwork among them) and get the text as typed; Codex has none.
        if backend.kind == "codex" && super::super::swarm::swarm_command(text).is_some() {
            return Err(HostError::invalid(l(
                "Codex 没有 /swarm 命令。要它分头干活，直接用文字让它派子 Agent，例如“派两个子 Agent 分别检查前端和后端”。",
                "Codex has no /swarm command. To split the work, ask it in words to start sub-agents, e.g. \"start two sub-agents to check the frontend and the backend\".",
            )));
        }
        enum Action {
            Start,
            TrySteer(ExternalTurnHandle, Vec<Approval>),
            Queued(Vec<Approval>),
        }
        let mut images = Some(images);
        let action = {
            let mut inner = self.inner.lock().unwrap();
            if inner.closing {
                return Err(HostError::new("closed", l("对话正在关闭。", "This chat is closing.")));
            }
            if is_busy(&inner) {
                let waiting = std::mem::take(&mut inner.approvals);
                let steer = steer || !waiting.is_empty();
                let no_images = images.as_ref().is_some_and(Vec::is_empty);
                if steer && no_images && let Some(handle) = inner.external_turn.clone() {
                    Action::TrySteer(handle, waiting)
                } else {
                    let imgs = images.take().unwrap_or_default();
                    let queue_id = uuid::Uuid::new_v4().to_string();
                    let image_count = imgs.len() as u32;
                    inner.queue.push(QueuedMessage { id: queue_id.clone(), text: text.to_owned(), mode: "queue".into(), image_count });
                    inner.external_pending.insert(queue_id, (text.to_owned(), imgs));
                    Action::Queued(waiting)
                }
            } else {
                inner.starting = true;
                Action::Start
            }
        };
        match action {
            Action::Start => {
                let guard = StartGuard::new(self);
                self.start_external_run_claimed(backend, text.to_owned(), images.take().unwrap_or_default(), true, guard).await?;
                Ok(false)
            }
            Action::Queued(superseded) => {
                if !superseded.is_empty() {
                    for approval in superseded {
                        let _ = approval.settle.send(refusal(SUPERSEDED, false, Some("user")));
                    }
                    self.services.rpc.emit("sessions.changed", self.summary());
                }
                self.publish_state();
                Ok(true)
            }
            Action::TrySteer(handle, superseded) => {
                if !superseded.is_empty() {
                    for approval in superseded {
                        let _ = approval.settle.send(refusal(SUPERSEDED, false, Some("user")));
                    }
                    self.services.rpc.emit("sessions.changed", self.summary());
                }
                if matches!(handle.steer(text).await, Ok(true)) {
                    self.append(TranscriptEntry::User {
                        id: uuid::Uuid::new_v4().to_string(),
                        ts: now_ms() as f64,
                        text: text.to_owned(),
                        images: None,
                    })
                    .await;
                    return Ok(false);
                }
                // Live steer did not take; re-check atomically whether to queue or start a new turn.
                let start_now = {
                    let mut inner = self.inner.lock().unwrap();
                    if inner.closing {
                        return Err(HostError::new("closed", l("对话正在关闭。", "This chat is closing.")));
                    }
                    if is_busy(&inner) {
                        let imgs = images.take().unwrap_or_default();
                        let queue_id = uuid::Uuid::new_v4().to_string();
                        let image_count = imgs.len() as u32;
                        inner.queue.push(QueuedMessage { id: queue_id.clone(), text: text.to_owned(), mode: "queue".into(), image_count });
                        inner.external_pending.insert(queue_id, (text.to_owned(), imgs));
                        false
                    } else {
                        inner.starting = true;
                        true
                    }
                };
                if start_now {
                    let guard = StartGuard::new(self);
                    self.start_external_run_claimed(backend, text.to_owned(), images.take().unwrap_or_default(), true, guard).await?;
                    Ok(false)
                } else {
                    self.publish_state();
                    Ok(true)
                }
            }
        }
    }

    fn clear_external_start_failure(&self) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.run = None;
            inner.starting = false;
            inner.queue.clear();
            inner.external_pending.clear();
            if inner.status == "running" {
                inner.status = if inner.closing {
                    "idle"
                } else if inner.error.is_some() {
                    "error"
                } else {
                    "idle"
                };
            }
        }
        self.publish_state();
        self.services.rpc.emit("sessions.changed", self.summary());
    }

    pub(super) async fn start_external_run_claimed(
        self: &Arc<Self>,
        backend: ExternalBackendRef,
        text: String,
        images: Vec<ImageContent>,
        append_user: bool,
        guard: StartGuard<'_>,
    ) -> HostResult<()> {
        let prep: HostResult<()> = async {
            if self.inner.lock().unwrap().closing {
                return Err(HostError::new("closed", l("对话正在关闭。", "This chat is closing.")));
            }
            self.services.activity.assert_can_start()?;
            if self.chat().title.is_empty() {
                let chat = self.services.store.set_default_title(&self.id, &fallback_title(&text)).await?;
                self.inner.lock().unwrap().chat = chat;
            }
            if append_user && self.services.settings.prefs().get("autoKeepChanges").and_then(Value::as_bool) == Some(true) {
                let ids: Vec<String> = self.pending_changes().await.into_iter().flat_map(|group| group.calls.into_iter().map(|call| call.id)).collect();
                if let Err(error) = self.keep(&ids).await {
                    log::warn!("[review] {error}");
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = prep {
            guard.disarm();
            self.clear_external_start_failure();
            return Err(error);
        }
        let cancel = CancellationToken::new();
        let closed = {
            let mut inner = self.inner.lock().unwrap();
            if inner.closing {
                inner.run = None;
                inner.starting = false;
                inner.queue.clear();
                inner.external_pending.clear();
                inner.status = "idle";
                true
            } else {
                inner.run = Some(RunInfo {
                    started: now_ms(),
                    from: inner.entries.len(),
                    requests: 0,
                    usage: UsageSummary::default(),
                    aborted: false,
                    overflow_retried: false,
                    retries: 0,
                    cancel: cancel.clone(),
                });
                inner.error = None;
                inner.external_diff = None;
                inner.status = "running";
                // Spawn and record `self.task` while holding `inner` (`inner -> task`) so the task cannot complete
                // before `task` is installed and `starting` is cleared.
                let this = self.clone();
                let task = tokio::spawn(async move { this.drive_external(backend, text, images, append_user, cancel).await });
                *self.task.lock().unwrap() = Some(task);
                inner.starting = false;
                false
            }
        };
        guard.disarm();
        if closed {
            self.publish_state();
            self.services.rpc.emit("sessions.changed", self.summary());
            return Err(HostError::new("closed", l("对话正在关闭。", "This chat is closing.")));
        }
        self.publish_state();
        self.services.rpc.emit("sessions.changed", self.summary());
        self.services.memory.interrupt();
        Ok(())
    }

    async fn drive_external(self: Arc<Self>, backend: ExternalBackendRef, mut text: String, mut images: Vec<ImageContent>, mut append_user: bool, mut cancel: CancellationToken) {
        loop {
            if append_user {
                let stored_images = (!images.is_empty()).then(|| images.clone());
                self.append(TranscriptEntry::User {
                    id: uuid::Uuid::new_v4().to_string(),
                    ts: now_ms() as f64,
                    text: text.clone(),
                    images: stored_images,
                })
                .await;
            }
            let temp_images = self.write_external_images(&images);
            let chat = self.chat();
            let sel = self.selection();
            let req = TurnRequest {
                backend: backend.clone(),
                cwd: PathBuf::from(&chat.cwd),
                thread_id: chat.external_thread_id.clone(),
                prompt: model_user_text(&text),
                image_paths: temp_images.clone(),
                model: sel.as_ref().map(|s| s.model.clone()),
                thinking: sel.as_ref().and_then(|s| s.thinking.clone()),
                permission: self.permission(),
            };
            let provider = external_provider_id(&backend.kind);
            let model_name = req.model.clone().unwrap_or_else(|| "default".into());

            let started = self.services.external.start_turn(&self.id, req, cancel.clone()).await;
            let reply_model = match started {
                Err(err) => {
                    for path in &temp_images {
                        let _ = std::fs::remove_file(path);
                    }
                    let mut reply = external_reply(&provider, &model_name);
                    reply.stop_reason = if err.code == "aborted" { "aborted".into() } else { "error".into() };
                    if err.code != "aborted" {
                        reply.error_message = Some(err.message.clone());
                        self.inner.lock().unwrap().error = Some(err.message);
                    }
                    self.on_reply(&reply).await;
                    reply.model
                }
                Ok((handle, mut rx)) => {
                    self.inner.lock().unwrap().external_turn = Some(handle);
                    {
                        let mut inner = self.inner.lock().unwrap();
                        inner.stream_sent = None;
                        inner.message_started = now_ms();
                        inner.thinking_started.clear();
                        inner.thinking_ms.clear();
                    }
                    self.services.model_activity.store(now_ms(), Ordering::SeqCst);

                    let mut reply = external_reply(&provider, &model_name);
                    let mut active_tool_ids: HashSet<String> = HashSet::new();
                    let mut saw_turn_finished = false;

                    while let Some(event) = rx.recv().await {
                        match event {
                            ExternalEvent::ThreadReady { thread_id } => {
                                if self.chat().external_thread_id.as_deref() != Some(thread_id.as_str())
                                    && let Ok(updated) = self.services.store.set_meta(&self.id, MetaPatch { external_thread_id: Some(Some(thread_id)), ..MetaPatch::default() }).await
                                {
                                    self.inner.lock().unwrap().chat = updated;
                                    self.services.rpc.emit("sessions.changed", self.summary());
                                }
                                // An Antigravity swarm or teamwork the conversation starts is followed from here on.
                                let _ = self.watch_antigravity_team();
                            }
                            ExternalEvent::ModelsListed(models) => {
                                if self.services.external.remember_models(&backend.kind, models) {
                                    self.services.rpc.emit("external.changed", &self.services.external.status());
                                }
                            }
                            ExternalEvent::QuotaUpdated(quota) => {
                                if self.services.external.note_quota(&backend.kind, quota) {
                                    self.services.rpc.emit("external.changed", &self.services.external.status());
                                }
                            }
                            ExternalEvent::CommandsUpdated(commands) => {
                                self.services.external.remember_commands(&backend.kind, commands.clone());
                                self.services.rpc.emit("external.commands", json!({ "kind": backend.kind, "commands": commands }));
                            }
                            ExternalEvent::TeamUpdated(team) => {
                                let (revision, last_post) = (team.revision, team.last_post);
                                self.inner.lock().unwrap().external_team = Some(team);
                                self.services.rpc.emit("external.teamChanged", json!({ "sessionId": self.id, "revision": revision, "lastPost": last_post }));
                            }
                            ExternalEvent::ThinkingDelta(delta) => {
                                if delta.is_empty() {
                                    continue;
                                }
                                if let Some(AssistantBlock::Thinking { thinking, .. }) = reply.content.last_mut() {
                                    thinking.push_str(&delta);
                                    self.on_stream(llm::Event::Updated, &reply);
                                } else {
                                    let idx = reply.content.len();
                                    reply.content.push(AssistantBlock::Thinking { thinking: delta, thinking_signature: None, redacted: None, rest: serde_json::Map::new() });
                                    self.on_stream(llm::Event::BlockStarted(idx), &reply);
                                }
                            }
                            ExternalEvent::TextDelta(delta) => {
                                if delta.is_empty() {
                                    continue;
                                }
                                if let Some(AssistantBlock::Text { text, .. }) = reply.content.last_mut() {
                                    text.push_str(&delta);
                                    self.on_stream(llm::Event::Updated, &reply);
                                } else {
                                    if let Some(last_idx) = reply.content.len().checked_sub(1)
                                        && matches!(reply.content.get(last_idx), Some(AssistantBlock::Thinking { .. }))
                                    {
                                        self.on_stream(llm::Event::BlockEnded(last_idx), &reply);
                                    }
                                    let idx = reply.content.len();
                                    reply.content.push(AssistantBlock::Text { text: delta, text_signature: None, rest: serde_json::Map::new() });
                                    self.on_stream(llm::Event::BlockStarted(idx), &reply);
                                }
                            }
                            ExternalEvent::ToolStarted { id, name, mut args } => {
                                // A call of an Antigravity team member, not of the chat's own agent, says whose it is.
                                if backend.kind == "antigravity"
                                    && let Some(member) = self.antigravity_member(&id)
                                    && let Some(map) = args.as_object_mut()
                                {
                                    map.insert("teamMember".into(), json!(member));
                                }
                                if let Some(last_idx) = reply.content.len().checked_sub(1)
                                    && matches!(reply.content.get(last_idx), Some(AssistantBlock::Thinking { .. }))
                                {
                                    self.on_stream(llm::Event::BlockEnded(last_idx), &reply);
                                }
                                let arguments = match args.clone() {
                                    Value::Object(map) => map,
                                    other => {
                                        let mut map = serde_json::Map::new();
                                        if !other.is_null() {
                                            map.insert("input".into(), other);
                                        }
                                        map
                                    }
                                };
                                let idx = reply.content.len();
                                reply.content.push(AssistantBlock::ToolCall { id: id.clone(), name: name.clone(), arguments, rest: serde_json::Map::new() });
                                active_tool_ids.insert(id.clone());
                                self.on_stream(llm::Event::BlockStarted(idx), &reply);
                                self.inner.lock().unwrap().running_tools.insert(id.clone(), now_ms());
                                self.services.rpc.emit("session.tool", json!({ "sessionId": self.id, "toolCallId": id, "toolName": name, "phase": "start", "args": args }));
                            }
                            ExternalEvent::ToolProgress { id, name, partial } => {
                                let now = now_ms();
                                let should_emit = {
                                    let mut inner = self.inner.lock().unwrap();
                                    if now.saturating_sub(inner.tool_updates.get(&id).copied().unwrap_or(0)) >= TOOL_UPDATE_THROTTLE_MS {
                                        inner.tool_updates.insert(id.clone(), now);
                                        true
                                    } else {
                                        false
                                    }
                                };
                                if should_emit {
                                    self.services.rpc.emit("session.tool", json!({ "sessionId": self.id, "toolCallId": id, "toolName": name, "phase": "update", "partial": partial }));
                                }
                            }
                            ExternalEvent::ToolCompleted { id, name, output, is_error, details } => {
                                // A call that ended while its approval waited (Antigravity gives a member's 60 s, then fails
                                // the call without withdrawing the request): the approval bar goes.
                                if self.inner.lock().unwrap().approvals.iter().any(|item| item.approval.tool_call_id == id) {
                                    self.settle(&id, refusal(l("外部 Agent 已不再等待这次批准。", "The external agent stopped waiting for this approval."), false, None));
                                }
                                if active_tool_ids.contains(&id) && !reply.content.is_empty() {
                                    reply.stop_reason = "toolUse".into();
                                    self.on_reply(&reply).await;
                                    active_tool_ids.clear();
                                    let current_model = reply.model.clone();
                                    reply = external_reply(&provider, &current_model);
                                    let mut inner = self.inner.lock().unwrap();
                                    inner.stream_sent = None;
                                    inner.message_started = now_ms();
                                    inner.thinking_started.clear();
                                    inner.thinking_ms.clear();
                                }
                                let started = {
                                    let mut inner = self.inner.lock().unwrap();
                                    inner.tool_updates.remove(&id);
                                    inner.running_tools.remove(&id)
                                };
                                self.services.rpc.emit("session.tool", json!({ "sessionId": self.id, "toolCallId": id, "toolName": name, "phase": "end" }));
                                let has_files = details.as_ref().and_then(|d| d.get("files")).and_then(Value::as_array).is_some_and(|a| !a.is_empty());
                                self.append(TranscriptEntry::ToolResult {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    ts: now_ms() as f64,
                                    tool_call_id: id,
                                    tool_name: name.clone(),
                                    content: vec![ResultContent::Text { text: output }],
                                    is_error,
                                    details,
                                    duration_ms: started.map(|s| now_ms().saturating_sub(s) as f64),
                                })
                                .await;
                                if (has_files || changes::CHANGE_TOOLS.contains(&name.as_str())) && !is_error {
                                    self.services.rpc.emit("session.changes", json!({ "sessionId": self.id }));
                                }
                            }
                            ExternalEvent::ApprovalRequested { mut approval, respond } => {
                                if approval.agent.is_none() && backend.kind == "antigravity" {
                                    approval.agent = self.antigravity_member(&approval.tool_call_id);
                                }
                                let session = self.clone();
                                let cancel_child = cancel.clone();
                                tokio::spawn(async move {
                                    let ans = session.wait_for_approval(approval, &cancel_child).await;
                                    let decision = match ans {
                                        Answer::Allow => ApprovalDecision::Allow,
                                        Answer::AllowSession => ApprovalDecision::AllowSession,
                                        Answer::Refuse { .. } => ApprovalDecision::Deny,
                                    };
                                    if let Ok(mut slot) = respond.lock()
                                        && let Some(tx) = slot.take()
                                    {
                                        let _ = tx.send(decision);
                                    }
                                });
                            }
                            ExternalEvent::DiffUpdated { diff } => {
                                self.inner.lock().unwrap().external_diff = Some(diff);
                                self.services.rpc.emit("session.changes", json!({ "sessionId": self.id }));
                            }
                            ExternalEvent::UsageUpdated(usage) => {
                                reply.usage.input = usage.input;
                                reply.usage.output = usage.output;
                                reply.usage.cache_read = usage.cache_read;
                                reply.usage.cache_write = usage.cache_write;
                                reply.usage.reasoning = usage.reasoning;
                            }
                            ExternalEvent::Notice { code, text } => {
                                self.notice(&code, text).await;
                            }
                            ExternalEvent::TurnFinished { stop_reason, error, usage, model } => {
                                saw_turn_finished = true;
                                if let Some(m) = model.filter(|m| !m.is_empty()) {
                                    reply.model = m;
                                }
                                if let Some(u) = usage {
                                    reply.usage.input = u.input;
                                    reply.usage.output = u.output;
                                    reply.usage.cache_read = u.cache_read;
                                    reply.usage.cache_write = u.cache_write;
                                    reply.usage.reasoning = u.reasoning;
                                }
                                if !reply.content.is_empty() || stop_reason == "error" {
                                    reply.stop_reason = stop_reason.clone();
                                    reply.error_message = error.clone();
                                    if stop_reason == "error"
                                        && let Some(err) = error
                                    {
                                        self.inner.lock().unwrap().error = Some(err);
                                    }
                                    self.on_reply(&reply).await;
                                } else if stop_reason == "aborted" {
                                    if let Some(run) = &mut self.inner.lock().unwrap().run {
                                        run.aborted = true;
                                    }
                                }
                                break;
                            }
                        }
                    }

                    // Always cancel & clear any pending approval waits left behind by the turn (especially on abrupt disconnect).
                    cancel.cancel();
                    let dangling_approvals: Vec<Approval> = std::mem::take(&mut self.inner.lock().unwrap().approvals);
                    if !dangling_approvals.is_empty() {
                        for approval in dangling_approvals {
                            let _ = approval.settle.send(refusal("Operation aborted", true, None));
                        }
                        self.publish_state();
                        self.services.rpc.emit("sessions.changed", self.summary());
                    }

                    if !saw_turn_finished {
                        let aborted = self.inner.lock().unwrap().run.as_ref().is_some_and(|run| run.aborted);
                        if !aborted {
                            let message = l("外部 Agent 连接意外断开。", "The external agent disconnected unexpectedly.");
                            reply.stop_reason = "error".into();
                            reply.error_message = Some(message.clone());
                            self.inner.lock().unwrap().error = Some(message);
                            self.on_reply(&reply).await;
                        } else if !reply.content.is_empty() {
                            reply.stop_reason = "aborted".into();
                            self.on_reply(&reply).await;
                        }
                    }

                    for path in &temp_images {
                        let _ = std::fs::remove_file(path);
                    }
                    self.inner.lock().unwrap().external_turn = None;
                    self.services.rpc.emit("session.changes", json!({ "sessionId": self.id }));
                    // The turn used some of the account's limits: read them again in the background (Codex usually sent them
                    // during the turn already, and a reading less than a minute old is kept).
                    {
                        let (external, rpc, kind, cwd) = (self.services.external.clone(), self.services.rpc.clone(), backend.kind.clone(), PathBuf::from(&self.chat().cwd));
                        tokio::spawn(async move {
                            if external.refresh_quota(&kind, false, &cwd).await {
                                rpc.emit("external.changed", &external.status());
                            }
                        });
                    }
                    reply.model
                }
            };

            match self.finish_external_run(&provider, &reply_model).await {
                Some((next_text, next_images)) => {
                    let next_cancel = CancellationToken::new();
                    let closing = {
                        let mut inner = self.inner.lock().unwrap();
                        if inner.closing {
                            inner.starting = false;
                            inner.queue.clear();
                            inner.external_pending.clear();
                            inner.status = "idle";
                            true
                        } else {
                            inner.run = Some(RunInfo {
                                started: now_ms(),
                                from: inner.entries.len(),
                                requests: 0,
                                usage: UsageSummary::default(),
                                aborted: false,
                                overflow_retried: false,
                                retries: 0,
                                cancel: next_cancel.clone(),
                            });
                            inner.starting = false;
                            inner.error = None;
                            false
                        }
                    };
                    if closing {
                        self.publish_state();
                        self.services.rpc.emit("sessions.changed", self.summary());
                        break;
                    }
                    text = next_text;
                    images = next_images;
                    append_user = true;
                    cancel = next_cancel;
                }
                None => break,
            }
        }
    }

    /// Follow the chat's Antigravity team (a swarm, teamwork or other subagents): read Antigravity's folder of the
    /// conversation again every 1.5 s while anyone works, every 5 s otherwise, and tell the interface when it changed.
    /// A new conversation's folder appears only once Antigravity works in it, so while the chat runs it is looked for
    /// every 1.5 s; an idle chat without one is looked at again when it runs or is shown.
    fn watch_antigravity_team(self: &Arc<Self>) -> Option<Arc<Mutex<AntigravityTeam>>> {
        let chat = self.chat();
        if chat.external_backend.as_ref()?.kind != "antigravity" {
            return None;
        }
        let conversation = chat.external_thread_id.clone()?;
        {
            let inner = self.inner.lock().unwrap();
            if let Some(team) = &inner.agy_team {
                return Some(team.clone());
            }
            if inner.closing || inner.agy_watch.as_ref().is_some_and(|task| !task.is_finished()) {
                return None;
            }
        }
        // Two folder checks: found at once when it is there.
        let found = AntigravityTeam::locate(&conversation).map(|mut team| {
            team.refresh();
            Arc::new(Mutex::new(team))
        });
        let weak = Arc::downgrade(self);
        let mut follow = found.clone();
        let task = tokio::spawn(async move {
            loop {
                let fast = {
                    let Some(session) = weak.upgrade() else { break };
                    if session.inner.lock().unwrap().closing {
                        break;
                    }
                    let running = session.running();
                    match &follow {
                        None if !running => break,
                        None => true,
                        Some(team) => running || team.lock().unwrap().busy(),
                    }
                };
                tokio::time::sleep(Duration::from_millis(if fast { 1500 } else { 5000 })).await;
                let changed = match follow.clone() {
                    None => {
                        let conversation = conversation.clone();
                        let located = tokio::task::spawn_blocking(move || {
                            AntigravityTeam::locate(&conversation).map(|mut team| {
                                team.refresh();
                                team
                            })
                        })
                        .await
                        .ok()
                        .flatten();
                        let Some(team) = located else { continue };
                        let marks = team.has_members().then(|| team.marks());
                        let team = Arc::new(Mutex::new(team));
                        let Some(session) = weak.upgrade() else { break };
                        session.inner.lock().unwrap().agy_team = Some(team.clone());
                        follow = Some(team);
                        marks
                    }
                    Some(team) => tokio::task::spawn_blocking(move || {
                        let mut team = team.lock().unwrap();
                        (team.refresh() && team.has_members()).then(|| team.marks())
                    })
                    .await
                    .ok()
                    .flatten(),
                };
                let Some(session) = weak.upgrade() else { break };
                if let Some((revision, last_post)) = changed {
                    session.services.rpc.emit("external.teamChanged", json!({ "sessionId": session.id, "revision": revision, "lastPost": last_post }));
                }
            }
        });
        let registered = {
            let mut inner = self.inner.lock().unwrap();
            if inner.closing || inner.agy_watch.as_ref().is_some_and(|existing| !existing.is_finished()) {
                task.abort();
                return inner.agy_team.clone();
            }
            if let Some(team) = &found {
                inner.agy_team = Some(team.clone());
            }
            inner.agy_watch = Some(task);
            found.clone()
        };
        // A team found at once is news to the interface too (it opens the swarm pane while the chat runs).
        if let Some((revision, last_post)) = registered.as_ref().and_then(|team| {
            let team = team.lock().unwrap();
            team.has_members().then(|| team.marks())
        }) {
            self.services.rpc.emit("external.teamChanged", json!({ "sessionId": self.id, "revision": revision, "lastPost": last_post }));
        }
        registered
    }

    /// The name of the Antigravity team member a tool call belongs to (`None` for the chat's own agent's calls).
    fn antigravity_member(self: &Arc<Self>, call_id: &str) -> Option<String> {
        let conversation = super::super::external::team::member_of_call(call_id)?;
        if let Some(team) = self.watch_antigravity_team()
            && let Some(name) = team.lock().unwrap().member_name(conversation)
        {
            return Some(name);
        }
        let thread = self.chat().external_thread_id?;
        AntigravityTeam::locate(&thread)?.member_name(conversation)
    }

    /// The team the chat's external agent runs, with the board posts newer than `after`; `None` without one.
    pub fn external_team(self: &Arc<Self>, after: Option<u64>) -> Option<ExternalTeam> {
        let mut team = match self.watch_antigravity_team() {
            Some(agy) => {
                let team = agy.lock().unwrap();
                if !team.has_members() {
                    return None;
                }
                team.snapshot(after)
            }
            None => {
                let mut team = self.inner.lock().unwrap().external_team.clone()?;
                if let Some(after) = after {
                    team.posts.retain(|post| post.seq > after);
                }
                team
            }
        };
        team.session_id = self.id.clone();
        // The chat's own agent works while the chat runs.
        let running = self.running();
        if let Some(lead) = team.members.first_mut()
            && (running || team.source == "codex")
        {
            lead.status = if running { "running" } else { "idle" }.into();
        }
        Some(team)
    }

    /// The latest steps of a member of the chat's external team.
    pub async fn team_activity(self: &Arc<Self>, member: &str) -> HostResult<Vec<TeamActivity>> {
        let not_found = || HostError::new("not-found", l("这个成员不在当前对话的团队里。", "This member is not in this chat's team."));
        if let Some(agy) = self.watch_antigravity_team() {
            let member = member.to_owned();
            return tokio::task::spawn_blocking(move || agy.lock().unwrap().activity(&member, 120))
                .await
                .ok()
                .flatten()
                .ok_or_else(not_found);
        }
        let main = self.chat().external_thread_id.ok_or_else(not_found)?;
        self.services.external.codex_activity(&main, member, 120).await.ok_or_else(not_found)
    }

    async fn finish_external_run(&self, provider: &str, model: &str) -> Option<(String, Vec<ImageContent>)> {
        let run = {
            let mut inner = self.inner.lock().unwrap();
            inner.streaming = None;
            inner.stream_sent = None;
            inner.running_tools.clear();
            inner.stop_after_step = false;
            inner.refused.clear();
            inner.external_turn = None;
            inner.run.take()
        };
        if let Some(run) = run {
            if run.aborted {
                self.notice("stopped", l("已停止。", "Stopped.")).await;
            } else if run.requests > 0 {
                self.append(TranscriptEntry::Turn {
                    id: uuid::Uuid::new_v4().to_string(),
                    ts: now_ms() as f64,
                    duration_ms: now_ms().saturating_sub(run.started) as f64,
                    usage: run.usage,
                    provider: provider.to_owned(),
                    model: model.to_owned(),
                    requests: f64::from(run.requests),
                })
                .await;
            }
        }
        self.services.memory.resume();
        self.services.model_activity.store(now_ms(), Ordering::SeqCst);

        let (next, transitioned) = {
            let mut inner = self.inner.lock().unwrap();
            let next = if inner.closing {
                None
            } else {
                let next_id = inner.queue.iter().find(|item| inner.external_pending.contains_key(&item.id)).map(|item| item.id.clone());
                next_id.and_then(|id| {
                    inner.queue.retain(|item| item.id != id);
                    inner.external_pending.remove(&id)
                })
            };
            if next.is_some() {
                inner.starting = true;
                (next, false)
            } else {
                let target = if inner.error.is_some() { "error" } else { "idle" };
                let changed = inner.status != target;
                inner.status = target;
                (None, changed)
            }
        };
        if next.is_some() {
            self.publish_state();
            return next;
        }
        if transitioned {
            self.publish_state();
            self.services.rpc.emit("sessions.changed", self.summary());
        }
        None
    }

    fn write_external_images(&self, images: &[ImageContent]) -> Vec<PathBuf> {
        use base64::Engine as _;
        if images.is_empty() {
            return Vec::new();
        }
        let dir = self.services.env.home.join("spill").join(&self.id);
        if crate::store::create_private_dir(&dir).is_err() {
            return Vec::new();
        }
        let mut paths = Vec::new();
        for img in images {
            let ext = match img.mime_type.as_str() {
                "image/png" => "png",
                "image/gif" => "gif",
                "image/webp" => "webp",
                _ => "jpg",
            };
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&img.data) {
                let path = dir.join(format!("ext-img-{}.{ext}", uuid::Uuid::new_v4()));
                if std::fs::write(&path, bytes).is_ok() {
                    paths.push(path);
                }
            }
        }
        paths
    }
}

fn external_reply(provider: &str, model: &str) -> AssistantMessage {
    let mut rest = serde_json::Map::new();
    rest.insert("role".into(), json!("assistant"));
    AssistantMessage {
        content: Vec::new(),
        api: provider.to_owned(),
        provider: provider.to_owned(),
        model: model.to_owned(),
        usage: Default::default(),
        stop_reason: "pending".into(),
        error_message: None,
        timestamp: now_ms() as f64,
        rest,
    }
}
