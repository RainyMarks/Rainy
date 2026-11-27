//! OpenAI Codex CLI driver speaking the official `codex app-server --stdio` JSON-RPC protocol.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rainy_protocol::chat::{ExternalModelInfo, PendingApproval, UsageSummary};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::common::{CancelOnDrop, TurnTracker, format_rpc_error, push_bounded_tail, spawn_approval_bridge};
use super::jsonrpc::{IncomingEvent, JsonRpcClient, JsonRpcError, external_env};
use super::team::CodexTeam;
use super::{ApprovalDecision, ExternalEvent, TurnRequest};
use crate::error::{HostError, HostResult};
use crate::locale::l;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const INTERRUPT_GRACE: Duration = Duration::from_secs(3);
const MAX_CODEX_TEAMS: usize = 64;

/// Active turn control handle for a running Codex turn (supports live `turn/steer` and `turn/interrupt`).
pub struct CodexTurnControl {
    rpc: Arc<JsonRpcClient>,
    thread_id: Mutex<Option<String>>,
    turn_id: Mutex<Option<String>>,
}

impl CodexTurnControl {
    pub fn new(rpc: Arc<JsonRpcClient>) -> Self {
        Self { rpc, thread_id: Mutex::new(None), turn_id: Mutex::new(None) }
    }

    pub fn set_thread(&self, thread_id: String) {
        if let Ok(mut guard) = self.thread_id.lock() {
            *guard = Some(thread_id);
        }
    }

    pub fn set_turn(&self, turn_id: String) {
        if let Ok(mut guard) = self.turn_id.lock() {
            *guard = Some(turn_id);
        }
    }

    /// Steer an in-flight turn via `turn/steer`. Returns `Ok(true)` if delivered, `Ok(false)` if not yet started.
    pub async fn steer(&self, text: &str) -> HostResult<bool> {
        let (thread_id, turn_id) = {
            let t = self.thread_id.lock().ok().and_then(|g| g.clone());
            let u = self.turn_id.lock().ok().and_then(|g| g.clone());
            (t, u)
        };
        let (Some(thread_id), Some(turn_id)) = (thread_id, turn_id) else {
            return Ok(false);
        };
        let params = json!({
            "threadId": thread_id,
            "expectedTurnId": turn_id,
            "input": [{ "type": "text", "text": text }],
        });
        let cancel = CancellationToken::new();
        self.rpc
            .request("turn/steer", params, RPC_TIMEOUT, &cancel)
            .await
            .map_err(|e| HostError::other(l(&format!("Codex 插话失败：{e}"), &format!("Codex steer failed: {e}"))))?;
        Ok(true)
    }

    /// Ask Codex to interrupt the current turn cleanly via `turn/interrupt`.
    pub async fn interrupt(&self) {
        let (thread_id, turn_id) = {
            let t = self.thread_id.lock().ok().and_then(|g| g.clone());
            let u = self.turn_id.lock().ok().and_then(|g| g.clone());
            (t, u)
        };
        if let (Some(thread_id), Some(turn_id)) = (thread_id, turn_id) {
            let cancel = CancellationToken::new();
            let _ = self.rpc.request("turn/interrupt", json!({ "threadId": thread_id, "turnId": turn_id }), INTERRUPT_GRACE, &cancel).await;
        }
    }
}

/// A live `codex app-server --stdio` subprocess capable of hosting multiple threads.
pub struct CodexProcess {
    pub rpc: Arc<JsonRpcClient>,
    tracker: Arc<TurnTracker>,
    /// The sub-agents of each main thread, kept across its turns.
    teams: Mutex<HashMap<String, CodexTeam>>,
    /// Maps Rainy chat `session_id` -> Codex main `thread_id` so closing a chat frees its team cache.
    session_threads: Mutex<HashMap<String, String>>,
}

/// What a turn keeps about the team: the sub-agent cards it opened and the team revision it last sent.
#[derive(Default)]
struct TurnTeam {
    cards: HashSet<String>,
    sent: u64,
}

/// The transcript id of a sub-agent's card.
fn card_id(thread: &str) -> String {
    format!("agent:{thread}")
}

impl CodexProcess {
    /// Spawn `codex app-server --stdio` and complete the `initialize` handshake.
    pub async fn spawn(binary: &Path, extra_args: &[String], cwd: &Path) -> HostResult<Arc<Self>> {
        let mut args: Vec<String> = if extra_args.iter().any(|arg| arg == "app-server") {
            extra_args.to_vec()
        } else {
            let mut base = vec!["app-server".to_owned(), "--stdio".to_owned()];
            base.extend(extra_args.iter().cloned());
            base
        };
        if !args.iter().any(|a| a == "--stdio" || a.starts_with("--listen")) {
            args.push("--stdio".to_owned());
        }
        let env = external_env();
        let rpc = JsonRpcClient::spawn(binary, &args, cwd, &env, "codex", false).map_err(|error| {
            HostError::other(l(
                &format!("无法启动 Codex ({})：{error}", binary.display()),
                &format!("Could not start Codex ({}): {error}", binary.display()),
            ))
        })?;
        let cancel = CancellationToken::new();
        let init_params = json!({
            "clientInfo": {
                "name": "rainy-agent",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "experimentalApi": true,
            },
        });
        if let Err(err) = rpc.request("initialize", init_params, HANDSHAKE_TIMEOUT, &cancel).await {
            let tail = rpc.stderr_tail();
            rpc.close().await;
            return Err(codex_error("initialize", err, &tail));
        }
        let _ = rpc.notify("initialized", json!({}));
        Ok(Arc::new(Self {
            rpc,
            tracker: TurnTracker::new(),
            teams: Mutex::new(HashMap::new()),
            session_threads: Mutex::new(HashMap::new()),
        }))
    }

    pub fn is_alive(&self) -> bool {
        !self.rpc.is_closed()
    }

    pub fn idle_for(&self) -> Duration {
        self.tracker.idle_for()
    }

    fn touch(&self) {
        self.tracker.touch();
    }

    /// Release the cached Codex team associated with a closed Rainy chat session.
    pub fn remove_session_team(&self, session_id: &str) {
        let thread_id = self.session_threads.lock().ok().and_then(|mut map| map.remove(session_id));
        if let Some(thread_id) = thread_id
            && let Ok(mut teams) = self.teams.lock()
        {
            teams.remove(&thread_id);
        }
    }

    /// Query models advertised by Codex via `model/list`.
    pub async fn list_models(&self, cancel: &CancellationToken) -> HostResult<Vec<ExternalModelInfo>> {
        self.touch();
        let value = self
            .rpc
            .request("model/list", json!({}), RPC_TIMEOUT, cancel)
            .await
            .map_err(|err| codex_error("model/list", err, &self.rpc.stderr_tail()))?;
        Ok(parse_codex_models(&value))
    }

    /// The account's usage limits via `account/rateLimits/read`: no thread, no turn, nothing used up.
    pub async fn read_rate_limits(&self, cancel: &CancellationToken) -> HostResult<Value> {
        self.touch();
        self.rpc
            .request("account/rateLimits/read", Value::Null, RPC_TIMEOUT, cancel)
            .await
            .map_err(|err| codex_error("account/rateLimits/read", err, &self.rpc.stderr_tail()))
    }

    /// Run one full conversational turn on Codex, emitting normalized [`ExternalEvent`] updates until `turn/completed`.
    pub async fn run_turn(
        self: &Arc<Self>,
        session_id: &str,
        req: &TurnRequest,
        events: mpsc::UnboundedSender<ExternalEvent>,
        control: Arc<CodexTurnControl>,
        cancel: CancellationToken,
    ) -> HostResult<()> {
        let _guard = self.tracker.begin_turn();
        let turn_done = CancelOnDrop(CancellationToken::new());

        let (sandbox_mode, approval_policy, sandbox_policy) = map_permission(&req.permission, &req.cwd);
        let model = normalize_model(req.model.as_deref());
        let effort = normalize_effort(req.thinking.as_deref());

        let (thread_id, resumed_fallback) = self
            .ensure_thread(req.thread_id.as_deref(), &req.cwd, model.as_deref(), sandbox_mode, approval_policy, &cancel)
            .await?;
        if let Ok(mut map) = self.session_threads.lock() {
            map.insert(session_id.to_owned(), thread_id.clone());
        }
        control.set_thread(thread_id.clone());
        let _ = events.send(ExternalEvent::ThreadReady { thread_id: thread_id.clone() });
        if resumed_fallback {
            let _ = events.send(ExternalEvent::Notice {
                code: "external-thread-reset".into(),
                text: l("原 Codex 对话未能续接，已开启新的 Codex 线程。", "The previous Codex thread could not be resumed, so a new Codex thread was started."),
            });
        }

        // Subscribe BEFORE issuing `turn/start` so early deltas or fast completions are never missed.
        let mut stream = self.rpc.subscribe();

        let mut input: Vec<Value> = vec![json!({ "type": "text", "text": req.prompt })];
        for path in &req.image_paths {
            input.push(json!({ "type": "localImage", "path": path.to_string_lossy() }));
        }
        let mut start_params = json!({
            "threadId": thread_id,
            "input": input,
            "cwd": req.cwd.to_string_lossy(),
            "approvalPolicy": approval_policy,
            "sandboxPolicy": sandbox_policy,
        });
        if let Some(m) = &model {
            start_params["model"] = json!(m);
        }
        if let Some(e) = &effort {
            start_params["effort"] = json!(e);
        }

        let start_res = self
            .rpc
            .request("turn/start", start_params, RPC_TIMEOUT, &cancel)
            .await
            .map_err(|err| codex_error("turn/start", err, &self.rpc.stderr_tail()))?;
        let turn_id = start_res
            .pointer("/turn/id")
            .or_else(|| start_res.get("turnId"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(tid) = &turn_id {
            control.set_turn(tid.clone());
        }

        let mut tool_outputs: HashMap<String, String> = HashMap::new();
        let mut tool_names: HashMap<String, String> = HashMap::new();
        let mut text_delta_items: HashSet<String> = HashSet::new();
        let mut saw_anonymous_delta = false;
        let mut latest_usage: Option<UsageSummary> = None;
        let mut turn_team = TurnTeam::default();

        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    control.interrupt().await;
                    // Give Codex a brief grace window to flush `turn/completed`, otherwise exit cleanly.
                    let deadline = tokio::time::Instant::now() + INTERRUPT_GRACE;
                    while tokio::time::Instant::now() < deadline {
                        let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
                        match tokio::time::timeout(remain, stream.recv()).await {
                            Ok(Some(IncomingEvent::Notification(note))) if note.method == "turn/completed" => {
                                if matches_thread(&note.params, &thread_id, false) {
                                    break;
                                }
                            }
                            Ok(Some(IncomingEvent::Closed(_))) | Ok(None) | Err(_) => break,
                            _ => {}
                        }
                    }
                    let _ = events.send(ExternalEvent::TurnFinished {
                        stop_reason: "aborted".into(),
                        error: None,
                        usage: latest_usage,
                        model: model.clone(),
                    });
                    return Ok(());
                }
                incoming = stream.recv() => {
                    let Some(incoming) = incoming else {
                        let _ = events.send(ExternalEvent::TurnFinished {
                            stop_reason: "error".into(),
                            error: Some(l("Codex 进程连接已断开。", "The Codex process connection closed.")),
                            usage: latest_usage,
                            model: model.clone(),
                        });
                        return Ok(());
                    };
                    match incoming {
                        IncomingEvent::Closed(tail) => {
                            let detail = if tail.is_empty() {
                                l("Codex 进程意外退出。", "The Codex process exited unexpectedly.")
                            } else {
                                l(&format!("Codex 进程退出：{tail}"), &format!("Codex exited: {tail}"))
                            };
                            let _ = events.send(ExternalEvent::TurnFinished {
                                stop_reason: "error".into(),
                                error: Some(detail),
                                usage: latest_usage,
                                model: model.clone(),
                            });
                            return Ok(());
                        }
                        IncomingEvent::Request(server_req) => {
                            // Never broadcast a request missing `threadId` across concurrent chats.
                            let solo = self.tracker.active_count() <= 1;
                            let agent = if matches_thread(&server_req.params, &thread_id, solo) {
                                None
                            } else {
                                let child = server_req.params.get("threadId").and_then(Value::as_str).unwrap_or_default();
                                match self.agent_name(&thread_id, child) {
                                    Some(name) => Some(format!("@{name}")),
                                    None => continue,
                                }
                            };
                            handle_codex_request(&self.rpc, server_req, &events, &cancel, &turn_done.0, agent);
                        }
                        IncomingEvent::Notification(note) => {
                            // Global account notification carries no `threadId`.
                            if note.method == "account/rateLimits/updated" {
                                if let Some(quota) = super::quota::codex(&note.params) {
                                    let _ = events.send(ExternalEvent::QuotaUpdated(quota));
                                }
                                continue;
                            }
                            let solo = self.tracker.active_count() <= 1;
                            if !matches_thread(&note.params, &thread_id, solo && note.method != "error") {
                                // A sub-agent's own thread: its card and the team, never the chat's transcript.
                                if let Some(child) = note.params.get("threadId").and_then(Value::as_str) {
                                    self.on_agent_note(&thread_id, child, &note.method, &note.params, &mut turn_team, &events);
                                }
                                continue;
                            }
                            // Ignore late notifications belonging to a previous cancelled turn on the same thread.
                            if let (Some(expected), Some(got)) = (turn_id.as_deref(), notification_turn_id(&note.params))
                                && expected != got
                            {
                                continue;
                            }
                            self.touch();
                            match note.method.as_str() {
                                "item/agentMessage/delta" => {
                                    if let Some(delta) = note.params.get("delta").and_then(Value::as_str)
                                        && !delta.is_empty()
                                    {
                                        match note.params.get("itemId").and_then(Value::as_str) {
                                            Some(item_id) => {
                                                text_delta_items.insert(item_id.to_owned());
                                            }
                                            None => saw_anonymous_delta = true,
                                        }
                                        let _ = events.send(ExternalEvent::TextDelta(delta.to_owned()));
                                    }
                                }
                                "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
                                    if let Some(delta) = note.params.get("delta").and_then(Value::as_str)
                                        && !delta.is_empty()
                                    {
                                        let _ = events.send(ExternalEvent::ThinkingDelta(delta.to_owned()));
                                    }
                                }
                                "item/started" => {
                                    if let Some(item) = note.params.get("item") {
                                        self.on_team_item(&thread_id, &thread_id, item, false, &mut turn_team, &events);
                                        handle_item_started(item, &mut tool_names, &events);
                                    }
                                }
                                "item/commandExecution/outputDelta" | "item/fileChange/outputDelta" => {
                                    if let (Some(item_id), Some(delta)) = (
                                        note.params.get("itemId").and_then(Value::as_str),
                                        note.params.get("delta").and_then(Value::as_str),
                                    ) {
                                        let entry = tool_outputs.entry(item_id.to_owned()).or_default();
                                        push_bounded_tail(entry, delta, 16384);
                                        let name = tool_names.get(item_id).cloned().unwrap_or_else(|| {
                                            if note.method.contains("fileChange") { "apply_patch".into() } else { "bash".into() }
                                        });
                                        let _ = events.send(ExternalEvent::ToolProgress {
                                            id: item_id.to_owned(),
                                            name,
                                            partial: entry.clone(),
                                        });
                                    }
                                }
                                "item/completed" => {
                                    if let Some(item) = note.params.get("item") {
                                        self.on_team_item(&thread_id, &thread_id, item, true, &mut turn_team, &events);
                                        handle_item_completed(item, &mut tool_names, &mut tool_outputs, &text_delta_items, saw_anonymous_delta, &events);
                                    }
                                }
                                "turn/diff/updated" => {
                                    if let Some(diff) = note.params.get("diff").and_then(Value::as_str) {
                                        let _ = events.send(ExternalEvent::DiffUpdated { diff: diff.to_owned() });
                                    }
                                }
                                "thread/tokenUsage/updated" => {
                                    if let Some(usage) = parse_codex_usage(&note.params) {
                                        latest_usage = Some(usage.clone());
                                        let _ = events.send(ExternalEvent::UsageUpdated(usage));
                                    }
                                }
                                "turn/completed" => {
                                    let status = note.params.pointer("/turn/status").and_then(Value::as_str).unwrap_or("completed");
                                    let (stop_reason, error) = match status {
                                        "interrupted" => ("aborted".to_owned(), None),
                                        "failed" => {
                                            let msg = note
                                                .params
                                                .pointer("/turn/error/message")
                                                .or_else(|| note.params.pointer("/turn/error"))
                                                .and_then(Value::as_str)
                                                .map(str::to_owned)
                                                .unwrap_or_else(|| l("Codex 执行失败。", "Codex turn failed."));
                                            ("error".to_owned(), Some(msg))
                                        }
                                        _ => ("stop".to_owned(), None),
                                    };
                                    let _ = events.send(ExternalEvent::TurnFinished {
                                        stop_reason,
                                        error,
                                        usage: latest_usage,
                                        model: model.clone(),
                                    });
                                    return Ok(());
                                }
                                "error" => {
                                    let will_retry = note.params.get("willRetry").and_then(Value::as_bool).unwrap_or(false);
                                    let msg = note
                                        .params
                                        .pointer("/error/message")
                                        .or_else(|| note.params.get("message"))
                                        .and_then(Value::as_str)
                                        .unwrap_or("Codex error");
                                    if will_retry {
                                        let _ = events.send(ExternalEvent::Notice {
                                            code: "external-retry".into(),
                                            text: l(&format!("Codex 正在重试：{msg}"), &format!("Codex retrying: {msg}")),
                                        });
                                    } else {
                                        let _ = events.send(ExternalEvent::TurnFinished {
                                            stop_reason: "error".into(),
                                            error: Some(msg.to_owned()),
                                            usage: latest_usage,
                                            model: model.clone(),
                                        });
                                        return Ok(());
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    async fn ensure_thread(
        &self,
        existing: Option<&str>,
        cwd: &Path,
        model: Option<&str>,
        sandbox: &str,
        approval_policy: &str,
        cancel: &CancellationToken,
    ) -> HostResult<(String, bool)> {
        if let Some(thread_id) = existing.filter(|id| !id.trim().is_empty()) {
            let mut params = json!({
                "threadId": thread_id,
                "cwd": cwd.to_string_lossy(),
                "sandbox": sandbox,
                "approvalPolicy": approval_policy,
            });
            if let Some(m) = model {
                params["model"] = json!(m);
            }
            match self.rpc.request("thread/resume", params, RPC_TIMEOUT, cancel).await {
                Ok(res) => {
                    let id = extract_thread_id(&res).unwrap_or_else(|| thread_id.to_owned());
                    return Ok((id, false));
                }
                Err(JsonRpcError::Cancelled) => return Err(HostError::new("aborted", l("已停止。", "Stopped."))),
                Err(err) => {
                    log::warn!("[external:codex] thread/resume {thread_id} failed, starting fresh thread: {err}");
                }
            }
        }
        let mut params = json!({
            "cwd": cwd.to_string_lossy(),
            "sandbox": sandbox,
            "approvalPolicy": approval_policy,
        });
        if let Some(m) = model {
            params["model"] = json!(m);
        }
        let res = self
            .rpc
            .request("thread/start", params, RPC_TIMEOUT, cancel)
            .await
            .map_err(|err| codex_error("thread/start", err, &self.rpc.stderr_tail()))?;
        let id = extract_thread_id(&res).ok_or_else(|| {
            HostError::other(l("Codex 未返回有效的线程 ID。", "Codex did not return a valid thread id."))
        })?;
        Ok((id, existing.is_some()))
    }

    pub async fn close(&self) {
        self.rpc.close().await;
    }

    /// The name of a sub-agent of `main`'s team.
    fn agent_name(&self, main: &str, thread: &str) -> Option<String> {
        self.teams.lock().ok()?.get(main)?.name_of(thread)
    }

    /// Send the team when it changed since the turn last did.
    fn send_team(&self, main: &str, turn: &mut TurnTeam, events: &mpsc::UnboundedSender<ExternalEvent>) {
        let Ok(teams) = self.teams.lock() else { return };
        let Some(team) = teams.get(main) else { return };
        if team.is_empty() || team.revision() == turn.sent {
            return;
        }
        turn.sent = team.revision();
        let _ = events.send(ExternalEvent::TeamUpdated(team.snapshot("", "running")));
    }

    /// A team item (`subAgentActivity`, `collabAgentToolCall`) on `sender`'s thread: the main thread's or a
    /// sub-agent's. Sub-agents the main thread starts get a card in the chat; its waits show as a step.
    fn on_team_item(&self, main: &str, sender: &str, item: &Value, completed: bool, turn: &mut TurnTeam, events: &mpsc::UnboundedSender<ExternalEvent>) {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            // Each activity comes as a started and a completed item; the completed one carries the same facts.
            "subAgentActivity" if completed => {
                let activity = item.get("kind").and_then(Value::as_str).unwrap_or_default();
                let child = item.get("agentThreadId").and_then(Value::as_str).unwrap_or_default().to_owned();
                let (started, name, reply) = {
                    let Ok(mut teams) = self.teams.lock() else { return };
                    if teams.len() >= MAX_CODEX_TEAMS
                        && !teams.contains_key(main)
                        && let Some(evict) = teams.keys().find(|k| k.as_str() != main).cloned()
                    {
                        teams.remove(&evict);
                    }
                    let team = teams.entry(main.to_owned()).or_insert_with(|| CodexTeam::new(main));
                    let started = team.sub_agent_activity(sender, item);
                    (started, team.name_of(&child).unwrap_or_default(), team.reply_of(&child))
                };
                if started.is_some() && sender == main {
                    turn.cards.insert(child.clone());
                    let _ = events.send(ExternalEvent::ToolStarted { id: card_id(&child), name: "task".into(), args: json!({ "description": format!("@{name}"), "external": true }) });
                }
                if matches!(activity, "completed" | "interrupted") && turn.cards.remove(&child) {
                    let output = reply.unwrap_or_else(|| l("（没有回复）", "(no reply)"));
                    let _ = events.send(ExternalEvent::ToolCompleted {
                        id: card_id(&child),
                        name: "task".into(),
                        output,
                        is_error: activity == "interrupted",
                        details: Some(json!({ "external": true, "agent": name })),
                    });
                }
                self.send_team(main, turn, events);
            }
            "collabAgentToolCall" => {
                let tool = item.get("tool").and_then(Value::as_str).unwrap_or_default();
                let mut started = Vec::new();
                let mut finished = Vec::new();
                if let Ok(mut teams) = self.teams.lock() {
                    let team = teams.entry(main.to_owned()).or_insert_with(|| CodexTeam::new(main));
                    // Multi-agent v2 starts agents with `spawnAgent` instead of a sub-agent activity.
                    if tool == "spawnAgent" && completed {
                        for id in item.get("receiverThreadIds").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                            if !team.knows(id)
                                && let Some(id) = team.sub_agent_activity(sender, &json!({ "agentThreadId": id, "kind": "started", "agentPath": "", "model": item.get("model") }))
                            {
                                started.push((id.clone(), team.name_of(&id).unwrap_or_default()));
                            }
                        }
                    }
                    team.collab_call(sender, item);
                    // A wait tells when agents are through; v2 agents report it nowhere else.
                    for id in &turn.cards {
                        if let Some(failed) = team.finished(id)
                            && !started.iter().any(|(new, _)| new == id)
                        {
                            finished.push((id.clone(), team.name_of(id).unwrap_or_default(), team.reply_of(id), failed));
                        }
                    }
                }
                if sender == main {
                    for (child, name) in started {
                        turn.cards.insert(child.clone());
                        let _ = events.send(ExternalEvent::ToolStarted { id: card_id(&child), name: "task".into(), args: json!({ "description": format!("@{name}"), "external": true }) });
                    }
                }
                for (child, name, reply, failed) in finished {
                    turn.cards.remove(&child);
                    let _ = events.send(ExternalEvent::ToolCompleted {
                        id: card_id(&child),
                        name: "task".into(),
                        output: reply.unwrap_or_else(|| l("（没有回复）", "(no reply)")),
                        is_error: failed,
                        details: Some(json!({ "external": true, "agent": name })),
                    });
                }
                if sender == main && tool == "wait"
                    && let Some(id) = item.get("id").and_then(Value::as_str)
                {
                    if completed {
                        let _ = events.send(ExternalEvent::ToolCompleted {
                            id: id.to_owned(),
                            name: "agent_wait".into(),
                            output: String::new(),
                            is_error: item.get("status").and_then(Value::as_str) == Some("failed"),
                            details: Some(json!({ "external": true })),
                        });
                    } else {
                        let _ = events.send(ExternalEvent::ToolStarted { id: id.to_owned(), name: "agent_wait".into(), args: json!({ "external": true }) });
                    }
                }
                self.send_team(main, turn, events);
            }
            _ => {}
        }
    }

    /// A notification of a sub-agent's own thread.
    fn on_agent_note(&self, main: &str, child: &str, method: &str, params: &Value, turn: &mut TurnTeam, events: &mpsc::UnboundedSender<ExternalEvent>) {
        if !self.teams.lock().ok().is_some_and(|teams| teams.get(main).is_some_and(|team| team.knows(child))) {
            return;
        }
        self.touch();
        // Agents a sub-agent starts in turn.
        if matches!(method, "item/started" | "item/completed")
            && let Some(item) = params.get("item")
            && matches!(item.get("type").and_then(Value::as_str), Some("subAgentActivity" | "collabAgentToolCall"))
        {
            self.on_team_item(main, child, item, method == "item/completed", turn, events);
        }
        let line = self.teams.lock().ok().and_then(|mut teams| teams.get_mut(main)?.agent_event(child, method, params));
        if let Some(line) = line
            && turn.cards.contains(child)
        {
            let _ = events.send(ExternalEvent::ToolProgress { id: card_id(child), name: "task".into(), partial: line });
        }
        self.send_team(main, turn, events);
    }

    /// The latest steps of one sub-agent of `main`'s team.
    pub fn agent_activity(&self, main: &str, thread: &str, limit: usize) -> Option<Vec<rainy_protocol::chat::TeamActivity>> {
        self.teams.lock().ok()?.get(main)?.activity(thread, limit)
    }
}

fn matches_thread(params: &Value, thread_id: &str, allow_missing: bool) -> bool {
    match params.get("threadId").and_then(Value::as_str) {
        Some(id) => id == thread_id,
        None => allow_missing,
    }
}

fn notification_turn_id(params: &Value) -> Option<&str> {
    params
        .get("turnId")
        .or_else(|| params.pointer("/turn/id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn extract_thread_id(res: &Value) -> Option<String> {
    res.pointer("/thread/id")
        .or_else(|| res.get("threadId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn normalize_model(model: Option<&str>) -> Option<String> {
    model.map(str::trim).filter(|m| !m.is_empty() && *m != "default").map(str::to_owned)
}

fn normalize_effort(thinking: Option<&str>) -> Option<String> {
    thinking.map(str::trim).filter(|e| !e.is_empty() && *e != "off").map(str::to_owned)
}

/// Map Rainy's chat permission mode (`read-only` / `ask` / `auto`) to Codex's sandbox & approval policy.
pub fn map_permission(mode: &str, cwd: &Path) -> (&'static str, &'static str, Value) {
    match mode {
        "read-only" => ("read-only", "never", json!({ "type": "readOnly" })),
        "auto" => (
            "workspace-write",
            "never",
            json!({ "type": "workspaceWrite", "writableRoots": [cwd.to_string_lossy()] }),
        ),
        _ => (
            "workspace-write",
            "on-request",
            json!({ "type": "workspaceWrite", "writableRoots": [cwd.to_string_lossy()] }),
        ),
    }
}

/// Parse `model/list` response into protocol [`ExternalModelInfo`] rows.
pub fn parse_codex_models(value: &Value) -> Vec<ExternalModelInfo> {
    value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("hidden").and_then(Value::as_bool) != Some(true))
        .filter_map(|item| {
            let id = item.get("model").or_else(|| item.get("id")).and_then(Value::as_str)?.trim();
            if id.is_empty() {
                return None;
            }
            let display_name = item
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let reasoning_efforts: Vec<String> = item
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|opt| {
                    opt.get("reasoningEffort")
                        .or_else(|| opt.as_str().map(|_| opt))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect();
            let default_effort = item.get("defaultReasoningEffort").and_then(Value::as_str).map(str::to_owned);
            let is_default = item.get("isDefault").and_then(Value::as_bool).filter(|&v| v);
            Some(ExternalModelInfo { id: id.to_owned(), display_name, reasoning_efforts, default_effort, is_default })
        })
        .collect()
}

fn parse_codex_usage(params: &Value) -> Option<UsageSummary> {
    let breakdown = params.pointer("/tokenUsage/last").or_else(|| params.pointer("/tokenUsage/total"))?;
    let num = |k: &str| breakdown.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let reasoning = breakdown.get("reasoningOutputTokens").and_then(Value::as_f64).filter(|v| *v > 0.0);
    Some(UsageSummary {
        input: num("inputTokens"),
        output: num("outputTokens"),
        cache_read: num("cachedInputTokens"),
        cache_write: num("cacheWriteInputTokens"),
        reasoning,
        cost: None,
    })
}

fn handle_item_started(item: &Value, tool_names: &mut HashMap<String, String>, events: &mpsc::UnboundedSender<ExternalEvent>) {
    let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
    let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
    if id.is_empty() {
        return;
    }
    match kind {
        "commandExecution" => {
            let name = if cfg!(windows) { "pwsh" } else { "bash" }.to_owned();
            tool_names.insert(id.to_owned(), name.clone());
            let mut args = json!({});
            if let Some(cmd) = item.get("command").and_then(Value::as_str) {
                args["command"] = json!(cmd);
            }
            if let Some(cwd) = item.get("cwd").and_then(Value::as_str) {
                args["workdir"] = json!(cwd);
            }
            let _ = events.send(ExternalEvent::ToolStarted { id: id.to_owned(), name, args });
        }
        "fileChange" => {
            let name = "apply_patch".to_owned();
            tool_names.insert(id.to_owned(), name.clone());
            let args = json!({ "changes": item.get("changes").cloned().unwrap_or_else(|| json!([])) });
            let _ = events.send(ExternalEvent::ToolStarted { id: id.to_owned(), name, args });
        }
        "mcpToolCall" => {
            let server = item.get("server").and_then(Value::as_str).unwrap_or("mcp");
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("call");
            let name = format!("mcp__{server}__{tool}");
            tool_names.insert(id.to_owned(), name.clone());
            let args = item.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let _ = events.send(ExternalEvent::ToolStarted { id: id.to_owned(), name, args });
        }
        "webSearch" => {
            let name = "web_search".to_owned();
            tool_names.insert(id.to_owned(), name.clone());
            let query = item.get("query").and_then(Value::as_str).unwrap_or_default();
            let _ = events.send(ExternalEvent::ToolStarted { id: id.to_owned(), name, args: json!({ "query": query }) });
        }
        _ => {}
    }
}

fn handle_item_completed(
    item: &Value,
    tool_names: &mut HashMap<String, String>,
    tool_outputs: &mut HashMap<String, String>,
    text_delta_items: &HashSet<String>,
    saw_anonymous_delta: bool,
    events: &mpsc::UnboundedSender<ExternalEvent>,
) {
    let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
    let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "agentMessage" => {
            let already_streamed = (!id.is_empty() && text_delta_items.contains(id)) || (id.is_empty() && saw_anonymous_delta);
            if !already_streamed {
                let text = item
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        item.get("content").and_then(Value::as_array).map(|arr| {
                            arr.iter()
                                .filter_map(|p| p.get("text").and_then(Value::as_str))
                                .collect::<Vec<_>>()
                                .join("")
                        })
                    })
                    .unwrap_or_default();
                if !text.is_empty() {
                    let _ = events.send(ExternalEvent::TextDelta(text));
                }
            }
        }
        "commandExecution" => {
            let name = tool_names.remove(id).unwrap_or_else(|| if cfg!(windows) { "pwsh".into() } else { "bash".into() });
            let buffered = tool_outputs.remove(id).unwrap_or_default();
            let output = item
                .get("aggregatedOutput")
                .or_else(|| item.get("output"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or(buffered);
            let exit_code = item.get("exitCode").and_then(Value::as_i64);
            let status = item.get("status").and_then(Value::as_str).unwrap_or("completed");
            let is_error = status == "failed" || status == "declined" || exit_code.is_some_and(|code| code != 0);
            let details = exit_code.map(|code| json!({ "exitCode": code, "external": true }));
            let _ = events.send(ExternalEvent::ToolCompleted { id: id.to_owned(), name, output, is_error, details });
        }
        "fileChange" => {
            let name = tool_names.remove(id).unwrap_or_else(|| "apply_patch".into());
            let buffered = tool_outputs.remove(id).unwrap_or_default();
            let status = item.get("status").and_then(Value::as_str).unwrap_or("completed");
            let is_error = status == "failed" || status == "declined";
            let mut files = Vec::new();
            let mut summary_lines = Vec::new();
            if let Some(changes) = item.get("changes").and_then(Value::as_array) {
                for (idx, change) in changes.iter().enumerate() {
                    if let Some(path) = change.get("path").and_then(Value::as_str) {
                        let raw_kind = change
                            .get("kind")
                            .and_then(|k| k.as_str().or_else(|| k.get("type").and_then(Value::as_str)))
                            .unwrap_or("update");
                        let operation = match raw_kind {
                            "add" | "create" => "create",
                            "delete" | "remove" => "delete",
                            _ => "update",
                        };
                        files.push(json!({
                            "changeId": format!("{id}#{idx}"),
                            "path": path,
                            "operation": operation,
                            "undoable": false,
                            "external": true,
                        }));
                        summary_lines.push(format!("{operation}: {path}"));
                    }
                }
            }
            let output = if !summary_lines.is_empty() { summary_lines.join("\n") } else { buffered };
            let details = (!files.is_empty()).then(|| json!({ "files": files, "external": true }));
            let _ = events.send(ExternalEvent::ToolCompleted { id: id.to_owned(), name, output, is_error, details });
        }
        "mcpToolCall" => {
            let name = tool_names.remove(id).unwrap_or_else(|| {
                let server = item.get("server").and_then(Value::as_str).unwrap_or("mcp");
                let tool = item.get("tool").and_then(Value::as_str).unwrap_or("call");
                format!("mcp__{server}__{tool}")
            });
            let status = item.get("status").and_then(Value::as_str).unwrap_or("completed");
            let is_error = status == "failed" || item.get("error").is_some_and(|e| !e.is_null());
            let output = item
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    item.get("result")
                        .map(|r| r.as_str().map(str::to_owned).unwrap_or_else(|| r.to_string()))
                })
                .unwrap_or_default();
            let _ = events.send(ExternalEvent::ToolCompleted { id: id.to_owned(), name, output, is_error, details: Some(json!({ "external": true })) });
        }
        "webSearch" => {
            let name = tool_names.remove(id).unwrap_or_else(|| "web_search".into());
            let query = item.get("query").and_then(Value::as_str).unwrap_or_default();
            let _ = events.send(ExternalEvent::ToolCompleted {
                id: id.to_owned(),
                name,
                output: query.to_owned(),
                is_error: false,
                details: Some(json!({ "external": true })),
            });
        }
        _ => {}
    }
}

fn handle_codex_request(
    rpc: &Arc<JsonRpcClient>,
    req: super::jsonrpc::ServerRequest,
    events: &mpsc::UnboundedSender<ExternalEvent>,
    cancel: &CancellationToken,
    turn_done: &CancellationToken,
    agent: Option<String>,
) {
    let decision_reply = |decision: Option<ApprovalDecision>| {
        let value = match decision {
            Some(ApprovalDecision::Allow) => "accept",
            Some(ApprovalDecision::AllowSession) => "acceptForSession",
            Some(ApprovalDecision::Deny) => "decline",
            None => "cancel",
        };
        json!({ "decision": value })
    };
    match req.method.as_str() {
        "item/commandExecution/requestApproval" | "execCommandApproval" => {
            let item_id = req
                .params
                .get("approvalId")
                .or_else(|| req.params.get("itemId"))
                .or_else(|| req.params.get("callId"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("codex-cmd-{}", req.id));
            let command = req.params.get("command").and_then(Value::as_str).map(str::to_owned);
            let workdir = req.params.get("cwd").and_then(Value::as_str).map(str::to_owned);
            let rule = command.clone().or_else(|| req.params.get("reason").and_then(Value::as_str).map(str::to_owned));
            let approval = PendingApproval {
                tool_call_id: item_id,
                tool_name: if cfg!(windows) { "pwsh".into() } else { "bash".into() },
                reason: "command".into(),
                rule,
                path: None,
                workdir,
                agent,
            };
            spawn_approval_bridge(rpc, req.id, approval, events, cancel, turn_done, json!({ "decision": "decline" }), decision_reply);
        }
        "item/fileChange/requestApproval" | "applyPatchApproval" => {
            let item_id = req
                .params
                .get("itemId")
                .or_else(|| req.params.get("callId"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("codex-file-{}", req.id));
            let path = req.params.get("grantRoot").and_then(Value::as_str).map(str::to_owned);
            let rule = req.params.get("reason").and_then(Value::as_str).map(str::to_owned).or_else(|| path.clone());
            let approval = PendingApproval {
                tool_call_id: item_id,
                tool_name: "apply_patch".into(),
                reason: "file-change".into(),
                rule,
                path,
                workdir: None,
                agent,
            };
            spawn_approval_bridge(rpc, req.id, approval, events, cancel, turn_done, json!({ "decision": "decline" }), decision_reply);
        }
        "item/permissions/requestApproval" => {
            let item_id = req
                .params
                .get("itemId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("codex-perm-{}", req.id));
            let reason = req.params.get("reason").and_then(Value::as_str).map(str::to_owned);
            let requested = req.params.get("permissions").cloned().unwrap_or_else(|| json!({}));
            let approval = PendingApproval {
                tool_call_id: item_id,
                tool_name: "permissions".into(),
                reason: "permission".into(),
                rule: reason,
                path: None,
                workdir: None,
                agent,
            };
            spawn_approval_bridge(rpc, req.id, approval, events, cancel, turn_done, json!({ "permissions": {} }), move |decision| match decision {
                Some(ApprovalDecision::Allow) => json!({ "permissions": requested, "scope": "turn" }),
                Some(ApprovalDecision::AllowSession) => json!({ "permissions": requested, "scope": "session" }),
                Some(ApprovalDecision::Deny) | None => json!({ "permissions": {} }),
            });
        }
        _ => {
            let _ = rpc.reply_error(&req.id, -32601, "Method not supported by Rainy");
        }
    }
}

fn codex_error(method: &str, err: JsonRpcError, stderr: &str) -> HostError {
    format_rpc_error(
        "Codex",
        "Codex",
        method,
        err,
        stderr,
        format!("Codex 请求 {method} 超时。请检查本机 Codex 登录状态（运行 `codex login`）。"),
        format!("Codex request {method} timed out. Check your Codex login (`codex login`)."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_rainy_permissions_to_codex_policies() {
        let cwd = Path::new("/repo");
        assert_eq!(map_permission("read-only", cwd), ("read-only", "never", json!({ "type": "readOnly" })));
        assert_eq!(
            map_permission("ask", cwd),
            ("workspace-write", "on-request", json!({ "type": "workspaceWrite", "writableRoots": ["/repo"] }))
        );
        assert_eq!(
            map_permission("auto", cwd),
            ("workspace-write", "never", json!({ "type": "workspaceWrite", "writableRoots": ["/repo"] }))
        );
    }

    #[test]
    fn parses_codex_model_list_and_skips_hidden() {
        let payload = json!({
            "data": [
                {
                    "id": "gpt-5.3-codex",
                    "model": "gpt-5.3-codex",
                    "displayName": "GPT-5.3 Codex",
                    "hidden": false,
                    "isDefault": true,
                    "defaultReasoningEffort": "medium",
                    "supportedReasoningEfforts": [
                        { "reasoningEffort": "low", "description": "Fast" },
                        { "reasoningEffort": "medium", "description": "Balanced" },
                        { "reasoningEffort": "high", "description": "Deep" }
                    ]
                },
                { "id": "hidden-model", "model": "hidden-model", "hidden": true }
            ]
        });
        let models = parse_codex_models(&payload);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5.3-codex");
        assert_eq!(models[0].display_name.as_deref(), Some("GPT-5.3 Codex"));
        assert_eq!(models[0].reasoning_efforts, vec!["low", "medium", "high"]);
        assert_eq!(models[0].default_effort.as_deref(), Some("medium"));
        assert_eq!(models[0].is_default, Some(true));
    }
}
