//! Generic Agent Client Protocol (ACP) stdio JSON-RPC 2.0 driver.
//!
//! Powers Antigravity (`agy acp`) as well as user-configured custom ACP agents.
//! Compliance contract: ACP backends operate strictly as primary agents owning their own
//! prompt, history and tools — Rainy never injects custom system prompts or wraps `agy acp`
//! as a bare model API.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use rainy_protocol::chat::{ExternalModelInfo, PendingApproval, UsageSummary};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::jsonrpc::{IncomingEvent, JsonRpcClient, JsonRpcError, external_env};
use super::{ApprovalDecision, ExternalEvent, TurnRequest};
use crate::error::{HostError, HostResult};
use crate::locale::l;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const RPC_TIMEOUT: Duration = Duration::from_secs(45);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(3600);
const CANCEL_GRACE: Duration = Duration::from_secs(3);
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// Active turn control handle for an ACP session (`session/cancel`, `session/set_mode`).
pub struct AcpTurnControl {
    rpc: Arc<JsonRpcClient>,
    session_id: Mutex<Option<String>>,
    available_modes: Mutex<Vec<String>>,
}

impl AcpTurnControl {
    pub fn new(rpc: Arc<JsonRpcClient>) -> Self {
        Self { rpc, session_id: Mutex::new(None), available_modes: Mutex::new(Vec::new()) }
    }

    pub fn set_session(&self, session_id: String, modes: Vec<String>) {
        if let Ok(mut guard) = self.session_id.lock() {
            *guard = Some(session_id);
        }
        if let Ok(mut guard) = self.available_modes.lock() {
            *guard = modes;
        }
    }

    /// Notify the ACP agent to cancel the active `session/prompt`.
    pub fn cancel_prompt(&self) {
        if let Ok(guard) = self.session_id.lock()
            && let Some(session_id) = guard.as_ref()
        {
            let _ = self.rpc.notify("session/cancel", json!({ "sessionId": session_id }));
        }
    }

    /// Best-effort runtime permission mode sync via `session/set_mode`.
    pub async fn update_permission(&self, permission: &str) {
        let (session_id, modes) = {
            let sid = self.session_id.lock().ok().and_then(|g| g.clone());
            let modes = self.available_modes.lock().map(|g| g.clone()).unwrap_or_default();
            (sid, modes)
        };
        let Some(session_id) = session_id else { return };
        let Some(mode_id) = pick_acp_mode(permission, &modes) else { return };
        let cancel = CancellationToken::new();
        let _ = self.rpc.request("session/set_mode", json!({ "sessionId": session_id, "modeId": mode_id }), Duration::from_secs(10), &cancel).await;
    }
}

/// Metadata advertised by an ACP server during `initialize` / `session/new`.
#[derive(Debug, Clone, Default)]
pub struct AcpSessionMeta {
    pub session_id: String,
    pub models: Vec<ExternalModelInfo>,
    pub modes: Vec<String>,
}

/// A live ACP subprocess (`agy acp` or custom ACP server).
pub struct AcpProcess {
    pub kind: String,
    pub rpc: Arc<JsonRpcClient>,
    capabilities: Value,
    /// Maps Rainy chat `external_thread_id` -> cached advertised modes for that ACP session.
    known_modes: Mutex<HashMap<String, Vec<String>>>,
    last_used: Mutex<Instant>,
    active_turns: Mutex<usize>,
}

impl AcpProcess {
    /// Spawn an ACP server and complete the ACP `initialize` handshake.
    pub async fn spawn(kind: &str, binary: &Path, args: &[String], cwd: &Path) -> HostResult<Arc<Self>> {
        let env = external_env();
        let rpc = JsonRpcClient::spawn(binary, args, cwd, &env, kind, true).map_err(|error| {
            HostError::other(l(
                &format!("无法启动外部 ACP 代理 ({})：{error}", binary.display()),
                &format!("Could not start external ACP agent ({}): {error}", binary.display()),
            ))
        })?;
        let cancel = CancellationToken::new();
        // Keep `fs` and `terminal` false so the ACP agent acts as a self-contained primary agent.
        let init_params = json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": false, "writeTextFile": false },
                "terminal": false,
            },
            "clientInfo": {
                "name": "rainy-agent",
                "version": env!("CARGO_PKG_VERSION"),
            },
        });
        let init_res = match rpc.request("initialize", init_params, HANDSHAKE_TIMEOUT, &cancel).await {
            Ok(res) => res,
            Err(err) => {
                let tail = rpc.stderr_tail();
                rpc.close().await;
                return Err(acp_error(kind, "initialize", err, &tail));
            }
        };
        let capabilities = init_res.get("agentCapabilities").cloned().unwrap_or_else(|| json!({}));
        Ok(Arc::new(Self {
            kind: kind.to_owned(),
            rpc,
            capabilities,
            known_modes: Mutex::new(HashMap::new()),
            last_used: Mutex::new(Instant::now()),
            active_turns: Mutex::new(0),
        }))
    }

    pub fn is_alive(&self) -> bool {
        !self.rpc.is_closed()
    }

    pub fn idle_for(&self) -> Duration {
        let busy = self.active_turns.lock().map(|n| *n > 0).unwrap_or(false);
        if busy {
            return Duration::ZERO;
        }
        self.last_used.lock().map(|t| t.elapsed()).unwrap_or(Duration::ZERO)
    }

    fn touch(&self) {
        if let Ok(mut t) = self.last_used.lock() {
            *t = Instant::now();
        }
    }

    /// Create a fresh ACP session to inspect advertised models and modes (used by `external.probe`).
    pub async fn probe_session(&self, cwd: &Path, cancel: &CancellationToken) -> HostResult<AcpSessionMeta> {
        self.touch();
        let res = self
            .rpc
            .request("session/new", json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] }), RPC_TIMEOUT, cancel)
            .await
            .map_err(|err| acp_error(&self.kind, "session/new", err, &self.rpc.stderr_tail()))?;
        Ok(parse_acp_session_meta(&res))
    }

    /// Run one full conversational turn over ACP (`session/new|resume|load` -> optional mode/model -> `session/prompt`).
    pub async fn run_turn(
        self: &Arc<Self>,
        req: &TurnRequest,
        events: mpsc::UnboundedSender<ExternalEvent>,
        control: Arc<AcpTurnControl>,
        cancel: CancellationToken,
    ) -> HostResult<()> {
        if let Ok(mut n) = self.active_turns.lock() {
            *n += 1;
        }
        let _guard = ActiveGuard(self.clone());
        self.touch();

        let (meta, resumed_fallback) = self.ensure_session(req.thread_id.as_deref(), &req.cwd, &cancel).await?;
        let session_id = meta.session_id.clone();
        let modes = if !meta.modes.is_empty() {
            if let Ok(mut known) = self.known_modes.lock() {
                known.insert(session_id.clone(), meta.modes.clone());
            }
            meta.modes.clone()
        } else {
            self.known_modes.lock().ok().and_then(|m| m.get(&session_id).cloned()).unwrap_or_default()
        };
        control.set_session(session_id.clone(), modes.clone());
        let _ = events.send(ExternalEvent::ThreadReady { thread_id: session_id.clone() });
        if resumed_fallback {
            let _ = events.send(ExternalEvent::Notice {
                code: "external-thread-reset".into(),
                text: l("原外部会话未能续接，已开启新的会话。", "The previous external session could not be resumed, so a new session was started."),
            });
        }

        // Apply permission mode & model selection before prompting (best-effort; unsupported options do not fail the turn).
        if let Some(mode_id) = pick_acp_mode(&req.permission, &modes) {
            let _ = self.rpc.request("session/set_mode", json!({ "sessionId": session_id, "modeId": mode_id }), Duration::from_secs(10), &cancel).await;
        }
        let requested_model = req.model.as_deref().map(str::trim).filter(|m| !m.is_empty() && *m != "default");
        if let Some(model_id) = requested_model {
            let res = self.rpc.request("session/set_model", json!({ "sessionId": session_id, "modelId": model_id }), Duration::from_secs(10), &cancel).await;
            if res.is_err() {
                let _ = self
                    .rpc
                    .request(
                        "session/set_config_option",
                        json!({ "sessionId": session_id, "configId": "model", "value": model_id }),
                        Duration::from_secs(10),
                        &cancel,
                    )
                    .await;
            }
        }
        if let Some(thinking) = req.thinking.as_deref().map(str::trim).filter(|t| !t.is_empty() && *t != "off") {
            let _ = self
                .rpc
                .request(
                    "session/set_config_option",
                    json!({ "sessionId": session_id, "configId": "reasoning_effort", "value": thinking }),
                    Duration::from_secs(10),
                    &cancel,
                )
                .await;
        }

        // Subscribe BEFORE issuing `session/prompt` so early `session/update` chunks are never lost.
        let mut stream = self.rpc.subscribe();
        let prompt_blocks = build_acp_prompt_blocks(&req.prompt, &req.image_paths);
        let prompt_cancel = CancellationToken::new();
        let prompt_fut = self.rpc.request(
            "session/prompt",
            json!({ "sessionId": session_id, "prompt": prompt_blocks }),
            PROMPT_TIMEOUT,
            &prompt_cancel,
        );
        tokio::pin!(prompt_fut);

        let mut tool_state: HashMap<String, AcpToolState> = HashMap::new();
        let mut latest_usage: Option<UsageSummary> = None;

        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    control.cancel_prompt();
                    let deadline = tokio::time::Instant::now() + CANCEL_GRACE;
                    let _ = tokio::time::timeout_at(deadline, &mut prompt_fut).await;
                    prompt_cancel.cancel();
                    let _ = events.send(ExternalEvent::TurnFinished {
                        stop_reason: "aborted".into(),
                        error: None,
                        usage: latest_usage,
                        model: requested_model.map(str::to_owned),
                    });
                    return Ok(());
                }
                prompt_res = &mut prompt_fut => {
                    // Drain any trailing notifications already queued before `session/prompt` returned.
                    while let Ok(Some(IncomingEvent::Notification(note))) = tokio::time::timeout(Duration::from_millis(20), stream.recv()).await {
                        if matches_session(&note.params, &session_id) && note.method == "session/update" {
                            handle_acp_update(&note.params, &mut tool_state, &mut latest_usage, &events);
                        }
                    }
                    match prompt_res {
                        Ok(res) => {
                            let stop_reason = match res.get("stopReason").and_then(Value::as_str).unwrap_or("end_turn") {
                                "cancelled" => "aborted".to_owned(),
                                "refusal" | "error" => "error".to_owned(),
                                _ => "stop".to_owned(),
                            };
                            let error = (stop_reason == "error").then(|| {
                                res.get("error")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| l("外部代理拒绝或终止了请求。", "The external agent refused or stopped the request."))
                            });
                            let _ = events.send(ExternalEvent::TurnFinished {
                                stop_reason,
                                error,
                                usage: latest_usage,
                                model: requested_model.map(str::to_owned),
                            });
                        }
                        Err(err) => {
                            let detail = acp_error(&self.kind, "session/prompt", err, &self.rpc.stderr_tail()).message;
                            let _ = events.send(ExternalEvent::TurnFinished {
                                stop_reason: "error".into(),
                                error: Some(detail),
                                usage: latest_usage,
                                model: requested_model.map(str::to_owned),
                            });
                        }
                    }
                    return Ok(());
                }
                incoming = stream.recv() => {
                    let Some(incoming) = incoming else {
                        prompt_cancel.cancel();
                        let _ = events.send(ExternalEvent::TurnFinished {
                            stop_reason: "error".into(),
                            error: Some(l("外部 ACP 代理连接已断开。", "The external ACP agent connection closed.")),
                            usage: latest_usage,
                            model: requested_model.map(str::to_owned),
                        });
                        return Ok(());
                    };
                    match incoming {
                        IncomingEvent::Closed(tail) => {
                            prompt_cancel.cancel();
                            let detail = if tail.is_empty() {
                                l("外部 ACP 代理意外退出。", "The external ACP agent exited unexpectedly.")
                            } else {
                                l(&format!("外部 ACP 代理退出：{tail}"), &format!("External ACP agent exited: {tail}"))
                            };
                            let _ = events.send(ExternalEvent::TurnFinished {
                                stop_reason: "error".into(),
                                error: Some(detail),
                                usage: latest_usage,
                                model: requested_model.map(str::to_owned),
                            });
                            return Ok(());
                        }
                        IncomingEvent::Request(server_req) => {
                            if !matches_session(&server_req.params, &session_id) {
                                continue;
                            }
                            handle_acp_request(&self.rpc, server_req, &events, &cancel);
                        }
                        IncomingEvent::Notification(note) => {
                            if !matches_session(&note.params, &session_id) {
                                continue;
                            }
                            self.touch();
                            if note.method == "session/update" {
                                handle_acp_update(&note.params, &mut tool_state, &mut latest_usage, &events);
                            }
                        }
                    }
                }
            }
        }
    }

    async fn ensure_session(&self, existing: Option<&str>, cwd: &Path, cancel: &CancellationToken) -> HostResult<(AcpSessionMeta, bool)> {
        let cwd_str = cwd.to_string_lossy().into_owned();
        if let Some(session_id) = existing.filter(|id| !id.trim().is_empty()) {
            // If this process already holds this session in memory, reuse directly without reloading.
            if self.known_modes.lock().is_ok_and(|m| m.contains_key(session_id)) {
                return Ok((
                    AcpSessionMeta { session_id: session_id.to_owned(), models: Vec::new(), modes: Vec::new() },
                    false,
                ));
            }
            // Prefer `session/resume` (no replay), then fall back to `session/load` if supported.
            let resume_params = json!({ "sessionId": session_id, "cwd": cwd_str, "mcpServers": [] });
            match self.rpc.request("session/resume", resume_params.clone(), RPC_TIMEOUT, cancel).await {
                Ok(res) => {
                    let mut meta = parse_acp_session_meta(&res);
                    if meta.session_id.is_empty() {
                        meta.session_id = session_id.to_owned();
                    }
                    return Ok((meta, false));
                }
                Err(JsonRpcError::Cancelled) => return Err(HostError::new("aborted", l("已停止。", "Stopped."))),
                Err(_) => {
                    let load_supported = self.capabilities.get("loadSession").and_then(Value::as_bool).unwrap_or(true);
                    if load_supported {
                        match self.rpc.request("session/load", resume_params, RPC_TIMEOUT, cancel).await {
                            Ok(res) => {
                                let mut meta = parse_acp_session_meta(&res);
                                if meta.session_id.is_empty() {
                                    meta.session_id = session_id.to_owned();
                                }
                                return Ok((meta, false));
                            }
                            Err(JsonRpcError::Cancelled) => return Err(HostError::new("aborted", l("已停止。", "Stopped."))),
                            Err(err) => {
                                log::warn!("[external:{}] session/load {session_id} failed, creating new session: {err}", self.kind);
                            }
                        }
                    }
                }
            }
        }
        let res = self
            .rpc
            .request("session/new", json!({ "cwd": cwd_str, "mcpServers": [] }), RPC_TIMEOUT, cancel)
            .await
            .map_err(|err| acp_error(&self.kind, "session/new", err, &self.rpc.stderr_tail()))?;
        let meta = parse_acp_session_meta(&res);
        if meta.session_id.is_empty() {
            return Err(HostError::other(l("外部 ACP 代理未返回会话 ID。", "The external ACP agent did not return a session id.")));
        }
        Ok((meta, existing.is_some()))
    }

    pub async fn close(&self) {
        self.rpc.close().await;
    }
}

struct ActiveGuard(Arc<AcpProcess>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if let Ok(mut n) = self.0.active_turns.lock() {
            *n = n.saturating_sub(1);
        }
        self.0.touch();
    }
}

#[derive(Default)]
struct AcpToolState {
    name: String,
    kind: String,
    output: String,
    locations: Vec<String>,
    completed: bool,
}

fn matches_session(params: &Value, session_id: &str) -> bool {
    match params.get("sessionId").and_then(Value::as_str) {
        Some(id) => id == session_id,
        None => true,
    }
}

fn build_acp_prompt_blocks(prompt: &str, image_paths: &[PathBuf]) -> Vec<Value> {
    let mut blocks = vec![json!({ "type": "text", "text": prompt })];
    for path in image_paths {
        if let Ok(bytes) = std::fs::read(path)
            && bytes.len() <= MAX_IMAGE_BYTES
        {
            let mime = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
                Some("png") => "image/png",
                Some("gif") => "image/gif",
                Some("webp") => "image/webp",
                _ => "image/jpeg",
            };
            let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
            blocks.push(json!({ "type": "image", "data": data, "mimeType": mime }));
        }
    }
    blocks
}

/// Parse ACP `session/new` or `session/resume` response into session id, models, and available modes.
pub fn parse_acp_session_meta(res: &Value) -> AcpSessionMeta {
    let session_id = res.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_owned();
    let mut models = Vec::new();
    let current_model = res.pointer("/models/currentModelId").and_then(Value::as_str).map(str::to_owned);
    if let Some(arr) = res.pointer("/models/availableModels").and_then(Value::as_array) {
        for item in arr {
            if let Some(id) = item.get("modelId").or_else(|| item.get("id")).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                let display_name = item.get("name").or_else(|| item.get("displayName")).and_then(Value::as_str).map(str::to_owned);
                models.push(ExternalModelInfo {
                    id: id.to_owned(),
                    display_name,
                    reasoning_efforts: Vec::new(),
                    default_effort: None,
                    is_default: (current_model.as_deref() == Some(id)).then_some(true),
                });
            }
        }
    }
    let mut modes = Vec::new();
    if let Some(arr) = res.pointer("/modes/availableModes").and_then(Value::as_array) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                modes.push(id.to_owned());
            }
        }
    }
    // Also parse ACP v2 `configOptions` (category `model` / `mode` / `thought_level`).
    let mut thought_levels: Vec<String> = Vec::new();
    let mut default_thought: Option<String> = None;
    if let Some(options) = res.get("configOptions").and_then(Value::as_array) {
        for opt in options {
            let category = opt.get("category").and_then(Value::as_str).unwrap_or_default();
            let id = opt.get("id").and_then(Value::as_str).unwrap_or_default();
            let current = opt.get("currentValue").and_then(Value::as_str).map(str::to_owned);
            let values = collect_config_option_values(opt);
            if category == "model" || id == "model" {
                if models.is_empty() {
                    for (val, name) in values {
                        models.push(ExternalModelInfo {
                            is_default: (current.as_deref() == Some(val.as_str())).then_some(true),
                            id: val,
                            display_name: name,
                            reasoning_efforts: Vec::new(),
                            default_effort: None,
                        });
                    }
                }
            } else if category == "mode" || id == "mode" {
                if modes.is_empty() {
                    modes.extend(values.into_iter().map(|(val, _)| val));
                }
            } else if category == "thought_level" || id == "reasoning_effort" || id == "thinking" {
                thought_levels.extend(values.into_iter().map(|(val, _)| val));
                default_thought = current;
            }
        }
    }
    if !thought_levels.is_empty() {
        for model in &mut models {
            if model.reasoning_efforts.is_empty() {
                model.reasoning_efforts = thought_levels.clone();
            }
            if model.default_effort.is_none() {
                model.default_effort = default_thought.clone();
            }
        }
    }
    AcpSessionMeta { session_id, models, modes }
}

fn collect_config_option_values(opt: &Value) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let Some(items) = opt.get("options").and_then(Value::as_array) else {
        return out;
    };
    for item in items {
        if let Some(group_items) = item.get("options").and_then(Value::as_array) {
            for sub in group_items {
                if let Some(val) = sub.get("value").and_then(Value::as_str) {
                    out.push((val.to_owned(), sub.get("name").and_then(Value::as_str).map(str::to_owned)));
                }
            }
        } else if let Some(val) = item.get("value").and_then(Value::as_str) {
            out.push((val.to_owned(), item.get("name").and_then(Value::as_str).map(str::to_owned)));
        }
    }
    out
}

/// Choose the closest ACP mode id advertised by the server for Rainy's permission level.
pub fn pick_acp_mode(permission: &str, available: &[String]) -> Option<String> {
    if available.is_empty() {
        return None;
    }
    let preferred: &[&str] = match permission {
        "read-only" => &["plan", "read-only", "read_only", "readonly"],
        "auto" => &["accept-edits", "acceptEdits", "bypassPermissions", "auto", "yolo", "always-proceed"],
        _ => &["default", "ask", "code", "request-review"],
    };
    for candidate in preferred {
        if let Some(found) = available.iter().find(|mode| mode.eq_ignore_ascii_case(candidate)) {
            return Some(found.clone());
        }
    }
    None
}

fn handle_acp_update(
    params: &Value,
    tools: &mut HashMap<String, AcpToolState>,
    latest_usage: &mut Option<UsageSummary>,
    events: &mpsc::UnboundedSender<ExternalEvent>,
) {
    let update = params.get("update").unwrap_or(params);
    let kind = update.get("sessionUpdate").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "agent_message_chunk" => {
            if let Some(text) = extract_content_text(update.get("content"))
                && !text.is_empty()
            {
                let _ = events.send(ExternalEvent::TextDelta(text));
            }
        }
        "agent_thought_chunk" => {
            if let Some(text) = extract_content_text(update.get("content"))
                && !text.is_empty()
            {
                let _ = events.send(ExternalEvent::ThinkingDelta(text));
            }
        }
        "tool_call" => {
            let id = update.get("toolCallId").and_then(Value::as_str).unwrap_or_default();
            if id.is_empty() {
                return;
            }
            let tool_kind = update.get("kind").and_then(Value::as_str).unwrap_or("other");
            let title = update.get("title").and_then(Value::as_str).unwrap_or(tool_kind);
            let name = acp_tool_name(tool_kind, title);
            let mut args = update.get("rawInput").cloned().unwrap_or_else(|| json!({}));
            if !args.is_object() {
                args = json!({ "input": args });
            }
            if args.as_object().is_some_and(|m| m.is_empty()) && !title.is_empty() {
                args["title"] = json!(title);
            }
            let locations = extract_locations(update);
            tools.insert(
                id.to_owned(),
                AcpToolState { name: name.clone(), kind: tool_kind.to_owned(), output: String::new(), locations, completed: false },
            );
            let _ = events.send(ExternalEvent::ToolStarted { id: id.to_owned(), name, args });
        }
        "tool_call_update" => {
            let id = update.get("toolCallId").and_then(Value::as_str).unwrap_or_default();
            if id.is_empty() {
                return;
            }
            let state = tools.entry(id.to_owned()).or_insert_with(|| {
                let tool_kind = update.get("kind").and_then(Value::as_str).unwrap_or("other");
                let title = update.get("title").and_then(Value::as_str).unwrap_or(tool_kind);
                AcpToolState { name: acp_tool_name(tool_kind, title), kind: tool_kind.to_owned(), ..AcpToolState::default() }
            });
            let new_locations = extract_locations(update);
            for path in new_locations {
                if !state.locations.contains(&path) {
                    state.locations.push(path);
                }
            }
            if let Some(chunk) = extract_tool_update_text(update)
                && !chunk.is_empty()
            {
                if !state.output.is_empty() {
                    state.output.push('\n');
                }
                state.output.push_str(&chunk);
                let count = state.output.chars().count();
                if count > 16384 {
                    state.output = state.output.chars().skip(count - 16384).collect();
                }
            }
            let status = update.get("status").and_then(Value::as_str).unwrap_or_default();
            if matches!(status, "completed" | "failed" | "error" | "cancelled") && !state.completed {
                state.completed = true;
                let is_error = status != "completed";
                let details = build_acp_tool_details(id, state, update);
                let _ = events.send(ExternalEvent::ToolCompleted {
                    id: id.to_owned(),
                    name: state.name.clone(),
                    output: state.output.clone(),
                    is_error,
                    details,
                });
            } else if !state.output.is_empty() {
                let _ = events.send(ExternalEvent::ToolProgress {
                    id: id.to_owned(),
                    name: state.name.clone(),
                    partial: state.output.clone(),
                });
            }
        }
        "usage_update" => {
            let num = |k: &str| update.get(k).and_then(Value::as_f64).unwrap_or(0.0);
            let input = num("inputTokens").max(num("used"));
            let output = num("outputTokens");
            let usage = UsageSummary {
                input,
                output,
                cache_read: num("cachedReadTokens"),
                cache_write: num("cachedWriteTokens"),
                reasoning: update.get("thoughtTokens").and_then(Value::as_f64).filter(|v| *v > 0.0),
                cost: update.pointer("/cost/amount").and_then(Value::as_f64),
            };
            *latest_usage = Some(usage.clone());
            let _ = events.send(ExternalEvent::UsageUpdated(usage));
        }
        _ => {}
    }
}

fn handle_acp_request(
    rpc: &Arc<JsonRpcClient>,
    req: super::jsonrpc::ServerRequest,
    events: &mpsc::UnboundedSender<ExternalEvent>,
    cancel: &CancellationToken,
) {
    if req.method != "session/request_permission" {
        let _ = rpc.reply_error(&req.id, -32601, "Method not supported by Rainy");
        return;
    }
    let tool_call = req.params.get("toolCall").unwrap_or(&Value::Null);
    let call_id = tool_call
        .get("toolCallId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("acp-perm-{}", req.id));
    let kind = tool_call.get("kind").and_then(Value::as_str).unwrap_or("other");
    let title = tool_call.get("title").and_then(Value::as_str).unwrap_or(kind);
    let path = extract_locations(tool_call).into_iter().next().or_else(|| {
        tool_call.pointer("/rawInput/path").or_else(|| tool_call.pointer("/rawInput/file_path")).and_then(Value::as_str).map(str::to_owned)
    });
    let options: Vec<(String, String)> = req
        .params
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|opt| {
            let id = opt.get("optionId").and_then(Value::as_str)?.to_owned();
            let kind = opt.get("kind").and_then(Value::as_str).unwrap_or("allow_once").to_owned();
            Some((id, kind))
        })
        .collect();
    let has_always = options.iter().any(|(_, k)| k == "allow_always");
    let reason = match kind {
        "execute" => "command",
        "edit" | "delete" | "move" => "file-change",
        _ => "permission",
    };
    let approval = PendingApproval {
        tool_call_id: call_id,
        tool_name: acp_tool_name(kind, title),
        reason: reason.into(),
        rule: has_always.then(|| title.to_owned()),
        path,
        workdir: None,
    };
    let (tx, rx) = oneshot::channel::<ApprovalDecision>();
    if events
        .send(ExternalEvent::ApprovalRequested { approval, respond: Arc::new(Mutex::new(Some(tx))) })
        .is_err()
    {
        let _ = rpc.reply_result(&req.id, json!({ "outcome": { "outcome": "cancelled" } }));
        return;
    }
    let rpc = rpc.clone();
    let id = req.id;
    let cancel = cancel.clone();
    tokio::spawn(async move {
        let pick = |kinds: &[&str]| -> Option<String> {
            kinds.iter().find_map(|wanted| options.iter().find(|(_, k)| k == wanted).map(|(oid, _)| oid.clone()))
        };
        let outcome = tokio::select! {
            ans = rx => match ans {
                Ok(ApprovalDecision::Allow) => match pick(&["allow_once", "allow_always"]) {
                    Some(option_id) => json!({ "outcome": "selected", "optionId": option_id }),
                    None => json!({ "outcome": "cancelled" }),
                },
                Ok(ApprovalDecision::AllowSession) => match pick(&["allow_always", "allow_once"]) {
                    Some(option_id) => json!({ "outcome": "selected", "optionId": option_id }),
                    None => json!({ "outcome": "cancelled" }),
                },
                Ok(ApprovalDecision::Deny) | Err(_) => match pick(&["reject_once", "reject_always"]) {
                    Some(option_id) => json!({ "outcome": "selected", "optionId": option_id }),
                    None => json!({ "outcome": "cancelled" }),
                },
            },
            () = cancel.cancelled() => json!({ "outcome": "cancelled" }),
        };
        let _ = rpc.reply_result(&id, json!({ "outcome": outcome }));
    });
}

fn acp_tool_name(kind: &str, title: &str) -> String {
    match kind {
        "execute" => if cfg!(windows) { "pwsh" } else { "bash" }.to_owned(),
        "edit" | "delete" | "move" => "apply_patch".to_owned(),
        "read" => "read".to_owned(),
        "search" => "grep".to_owned(),
        "fetch" => "web_fetch".to_owned(),
        _ if !title.trim().is_empty() => title.chars().take(48).collect(),
        _ => "tool".to_owned(),
    }
}

fn extract_content_text(content: Option<&Value>) -> Option<String> {
    let value = content?;
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return Some(text.to_owned());
    }
    if let Some(arr) = value.as_array() {
        let joined: String = arr.iter().filter_map(|item| extract_content_text(Some(item))).collect::<Vec<_>>().join("\n");
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    if let Some(inner) = value.get("content") {
        return extract_content_text(Some(inner));
    }
    None
}

fn extract_tool_update_text(update: &Value) -> Option<String> {
    if let Some(text) = extract_content_text(update.get("content")) {
        return Some(text);
    }
    update.get("rawOutput").map(|raw| raw.as_str().map(str::to_owned).unwrap_or_else(|| raw.to_string()))
}

fn extract_locations(value: &Value) -> Vec<String> {
    value
        .get("locations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|loc| loc.get("path").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

fn build_acp_tool_details(id: &str, state: &AcpToolState, update: &Value) -> Option<Value> {
    let mut paths: Vec<String> = state.locations.clone();
    if let Some(content) = update.get("content").and_then(Value::as_array) {
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("diff")
                && let Some(path) = block.get("path").and_then(Value::as_str)
                && !paths.iter().any(|p| p == path)
            {
                paths.push(path.to_owned());
            }
        }
    }
    if matches!(state.kind.as_str(), "edit" | "delete" | "move") && !paths.is_empty() {
        let op = if state.kind == "delete" { "delete" } else { "update" };
        let files: Vec<Value> = paths
            .into_iter()
            .enumerate()
            .map(|(idx, path)| {
                json!({
                    "changeId": format!("{id}#{idx}"),
                    "path": path,
                    "operation": op,
                    "undoable": false,
                    "external": true,
                })
            })
            .collect();
        return Some(json!({ "files": files, "external": true }));
    }
    Some(json!({ "external": true }))
}

fn acp_error(kind: &str, method: &str, err: JsonRpcError, stderr: &str) -> HostError {
    match err {
        JsonRpcError::Cancelled => HostError::new("aborted", l("已停止。", "Stopped.")),
        JsonRpcError::Timeout => HostError::other(l(
            &format!("外部代理 {kind} 请求 {method} 超时。请检查登录与配置状态。"),
            &format!("External agent {kind} timed out on {method}. Check login and CLI setup."),
        )),
        other => {
            let suffix = if stderr.is_empty() { String::new() } else { format!(" ({stderr})") };
            HostError::other(l(
                &format!("外部代理 {kind} ({method}) 失败：{other}{suffix}"),
                &format!("External agent {kind} ({method}) failed: {other}{suffix}"),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_acp_models_modes_and_config_options() {
        let res = json!({
            "sessionId": "sess-42",
            "models": {
                "currentModelId": "gemini-3-pro",
                "availableModels": [
                    { "modelId": "gemini-3-pro", "name": "Gemini 3 Pro" },
                    { "modelId": "gemini-3-flash", "name": "Gemini 3 Flash" }
                ]
            },
            "modes": {
                "availableModes": [
                    { "id": "default", "name": "Default" },
                    { "id": "accept-edits", "name": "Accept Edits" },
                    { "id": "plan", "name": "Plan" }
                ]
            },
            "configOptions": [
                {
                    "id": "reasoning_effort",
                    "category": "thought_level",
                    "currentValue": "high",
                    "options": [
                        { "value": "low", "name": "Low" },
                        { "value": "high", "name": "High" },
                        { "value": "max", "name": "Max" }
                    ]
                }
            ]
        });
        let meta = parse_acp_session_meta(&res);
        assert_eq!(meta.session_id, "sess-42");
        assert_eq!(meta.models.len(), 2);
        assert_eq!(meta.models[0].is_default, Some(true));
        assert_eq!(meta.models[0].reasoning_efforts, vec!["low", "high", "max"]);
        assert_eq!(meta.models[0].default_effort.as_deref(), Some("high"));
        assert_eq!(pick_acp_mode("read-only", &meta.modes).as_deref(), Some("plan"));
        assert_eq!(pick_acp_mode("ask", &meta.modes).as_deref(), Some("default"));
        assert_eq!(pick_acp_mode("auto", &meta.modes).as_deref(), Some("accept-edits"));
    }
}
