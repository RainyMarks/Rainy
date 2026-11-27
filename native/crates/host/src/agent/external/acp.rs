//! Generic Agent Client Protocol (ACP) stdio JSON-RPC 2.0 driver.
//!
//! Powers Antigravity (`agy acp`) as well as user-configured custom ACP agents.
//! Compliance contract: ACP backends operate strictly as primary agents owning their own
//! prompt, history and tools — Rainy never injects custom system prompts or wraps `agy acp`
//! as a bare model API.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use rainy_protocol::chat::{ExternalCommand, ExternalModelInfo, PendingApproval, UsageSummary};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::common::{CancelOnDrop, TurnTracker, format_rpc_error, push_bounded_tail, spawn_approval_bridge};
use super::jsonrpc::{IncomingEvent, JsonRpcClient, JsonRpcError, external_env};
use super::{ApprovalDecision, ExternalEvent, TurnRequest};
use crate::error::{HostError, HostResult};
use crate::locale::l;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const RPC_TIMEOUT: Duration = Duration::from_secs(45);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(3600);
const CANCEL_GRACE: Duration = Duration::from_secs(3);
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// The most text of a reported edit kept to show its diff.
const MAX_DIFF_TEXT: usize = 256 * 1024;
const MAX_KNOWN_SESSIONS: usize = 256;

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
    /// The id of the config option that sets the thinking level (`thought_level` for Antigravity).
    pub thought_option: Option<String>,
}

/// A live ACP subprocess (`agy acp` or custom ACP server).
pub struct AcpProcess {
    pub kind: String,
    pub rpc: Arc<JsonRpcClient>,
    capabilities: Value,
    /// Session IDs already live in this ACP process (even when the server advertises no modes).
    known_sessions: Mutex<HashSet<String>>,
    /// Maps ACP `sessionId` -> cached advertised modes for that ACP session.
    known_modes: Mutex<HashMap<String, Vec<String>>>,
    /// The thinking-level option each ACP session advertised.
    thought_options: Mutex<HashMap<String, String>>,
    tracker: Arc<TurnTracker>,
}

impl AcpProcess {
    /// Spawn an ACP server and complete the ACP `initialize` handshake.
    pub async fn spawn(kind: &str, binary: &Path, args: &[String], cwd: &Path) -> HostResult<Arc<Self>> {
        let env = external_env();
        let rpc = JsonRpcClient::spawn(binary, args, cwd, &env, kind, true).map_err(|error| {
            HostError::other(l(
                &format!("无法启动外部 ACP Agent ({})：{error}", binary.display()),
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
            known_sessions: Mutex::new(HashSet::new()),
            known_modes: Mutex::new(HashMap::new()),
            thought_options: Mutex::new(HashMap::new()),
            tracker: TurnTracker::new(),
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

    fn remember_session(&self, session_id: &str) {
        if let Ok(mut known) = self.known_sessions.lock() {
            if known.len() >= MAX_KNOWN_SESSIONS && !known.contains(session_id) {
                known.clear();
            }
            known.insert(session_id.to_owned());
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
        let _guard = self.tracker.begin_turn();
        let turn_done = CancelOnDrop(CancellationToken::new());
        let read_only = req.permission == "read-only";

        // Subscribed before the session is made: an agent lists its commands right after `session/new`.
        let mut stream = self.rpc.subscribe();
        let (meta, resumed_fallback) = self.ensure_session(req.thread_id.as_deref(), &req.cwd, &cancel).await?;
        let session_id = meta.session_id.clone();
        self.remember_session(&session_id);
        if !meta.models.is_empty() {
            let _ = events.send(ExternalEvent::ModelsListed(meta.models.clone()));
        }
        // What came while the session was made is no part of this turn (`session/load` replays the whole history):
        // only the commands it listed are kept.
        while let Some(early) = stream.try_recv() {
            match early {
                IncomingEvent::Notification(note) if note.method == "session/update" && matches_session(&note.params, &session_id) => {
                    if let Some(commands) = acp_commands(&note.params) {
                        let _ = events.send(ExternalEvent::CommandsUpdated(commands));
                    }
                }
                IncomingEvent::Request(server_req) => {
                    if matches_session(&server_req.params, &session_id) {
                        handle_acp_request(&self.rpc, server_req, &events, &cancel, &turn_done.0, read_only);
                    } else {
                        let _ = self.rpc.reply_error(&server_req.id, -32602, "Unknown or inactive sessionId");
                    }
                }
                _ => {}
            }
        }
        let modes = if !meta.modes.is_empty() {
            if let Ok(mut known) = self.known_modes.lock() {
                known.insert(session_id.clone(), meta.modes.clone());
            }
            meta.modes.clone()
        } else {
            self.known_modes.lock().ok().and_then(|m| m.get(&session_id).cloned()).unwrap_or_default()
        };
        let thought_option = match &meta.thought_option {
            Some(option) => {
                if let Ok(mut known) = self.thought_options.lock() {
                    known.insert(session_id.clone(), option.clone());
                }
                Some(option.clone())
            }
            None => self.thought_options.lock().ok().and_then(|known| known.get(&session_id).cloned()),
        };
        control.set_session(session_id.clone(), modes.clone());
        let _ = events.send(ExternalEvent::ThreadReady { thread_id: session_id.clone() });
        if resumed_fallback {
            let _ = events.send(ExternalEvent::Notice {
                code: "external-thread-reset".into(),
                text: l("原外部对话未能续接，已开启新的对话。", "The previous external session could not be resumed, so a new session was started."),
            });
        }

        let requested_model = self.configure_session_options(&session_id, req, &modes, thought_option.as_deref(), &events, &cancel).await;
        self.drive_prompt_loop(&session_id, req, requested_model, read_only, &mut stream, &events, &control, &cancel, &turn_done.0)
            .await
    }

    async fn configure_session_options<'a>(
        &self,
        session_id: &str,
        req: &'a TurnRequest,
        modes: &[String],
        thought_option: Option<&str>,
        events: &mpsc::UnboundedSender<ExternalEvent>,
        cancel: &CancellationToken,
    ) -> Option<&'a str> {
        if let Some(mode_id) = pick_acp_mode(&req.permission, modes) {
            let res = self.rpc.request("session/set_mode", json!({ "sessionId": session_id, "modeId": mode_id }), Duration::from_secs(10), cancel).await;
            if let Err(err) = res
                && req.permission == "read-only"
            {
                let _ = events.send(ExternalEvent::Notice {
                    code: "external-readonly-failed".into(),
                    text: l(
                        &format!("外部 Agent 切换只读模式失败（{err}），已启用写入与命令拦截。"),
                        &format!("The external agent failed to enter read-only mode ({err}); edit and command permissions will be denied."),
                    ),
                });
            }
        } else if req.permission == "read-only" {
            let _ = events.send(ExternalEvent::Notice {
                code: "external-readonly-unsupported".into(),
                text: l(
                    "当前外部 Agent 未提供只读模式，写入与命令权限请求将自动拒绝。",
                    "This external agent does not advertise a read-only mode; edit and command permission prompts will be denied automatically.",
                ),
            });
        }
        let requested_model = req.model.as_deref().map(str::trim).filter(|m| !m.is_empty() && *m != "default");
        if let Some(model_id) = requested_model {
            let res = self.rpc.request("session/set_model", json!({ "sessionId": session_id, "modelId": model_id }), Duration::from_secs(10), cancel).await;
            if res.is_err() {
                let _ = self
                    .rpc
                    .request(
                        "session/set_config_option",
                        json!({ "sessionId": session_id, "configId": "model", "value": model_id }),
                        Duration::from_secs(10),
                        cancel,
                    )
                    .await;
            }
        }
        // After the model: choosing a model resets its thinking level. The option the session advertised, else the
        // ids agents use (Antigravity's is `thought_level` and refuses any other).
        if let Some(thinking) = req.thinking.as_deref().map(str::trim).filter(|t| !t.is_empty() && *t != "off") {
            let options = match thought_option {
                Some(option) => vec![option.to_owned()],
                None => vec!["thought_level".to_owned(), "reasoning_effort".to_owned()],
            };
            for option in options {
                let res = self
                    .rpc
                    .request(
                        "session/set_config_option",
                        json!({ "sessionId": session_id, "configId": option, "value": thinking }),
                        Duration::from_secs(10),
                        cancel,
                    )
                    .await;
                if res.is_ok() {
                    break;
                }
            }
        }
        requested_model
    }

    #[allow(clippy::too_many_arguments)]
    async fn drive_prompt_loop(
        &self,
        session_id: &str,
        req: &TurnRequest,
        requested_model: Option<&str>,
        read_only: bool,
        stream: &mut super::jsonrpc::EventStream,
        events: &mpsc::UnboundedSender<ExternalEvent>,
        control: &Arc<AcpTurnControl>,
        cancel: &CancellationToken,
        turn_done: &CancellationToken,
    ) -> HostResult<()> {
        let prompt_blocks = build_acp_prompt_blocks(&req.prompt, &req.image_paths).await;
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
                    let settled = tokio::time::timeout_at(deadline, &mut prompt_fut).await.is_ok();
                    prompt_cancel.cancel();
                    if !settled {
                        // The subprocess ignored `session/cancel`; terminate it so stale updates cannot leak into the next turn.
                        self.rpc.close().await;
                    }
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
                    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(20), stream.recv()).await {
                        match ev {
                            IncomingEvent::Notification(note) if matches_session(&note.params, session_id) && note.method == "session/update" => {
                                handle_acp_update(&note.params, &req.cwd, &mut tool_state, &mut latest_usage, events);
                            }
                            IncomingEvent::Request(server_req) => {
                                let _ = self.rpc.reply_error(&server_req.id, -32602, "Turn already finished");
                            }
                            IncomingEvent::Closed(_) => break,
                            _ => {}
                        }
                    }
                    match prompt_res {
                        Ok(res) => {
                            // The turn's tokens; `usage_update` only tells how full the context window is.
                            if let Some(usage) = res.get("usage").and_then(acp_turn_usage) {
                                latest_usage = Some(usage);
                            }
                            let stop_reason = match res.get("stopReason").and_then(Value::as_str).unwrap_or("end_turn") {
                                "cancelled" => "aborted".to_owned(),
                                "refusal" | "error" => "error".to_owned(),
                                _ => "stop".to_owned(),
                            };
                            let error = (stop_reason == "error").then(|| {
                                res.get("error")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| l("外部 Agent 拒绝或终止了请求。", "The external agent refused or stopped the request."))
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
                            error: Some(l("外部 ACP Agent 连接已断开。", "The external ACP agent connection closed.")),
                            usage: latest_usage,
                            model: requested_model.map(str::to_owned),
                        });
                        return Ok(());
                    };
                    match incoming {
                        IncomingEvent::Closed(tail) => {
                            prompt_cancel.cancel();
                            let detail = if tail.is_empty() {
                                l("外部 ACP Agent 意外退出。", "The external ACP agent exited unexpectedly.")
                            } else {
                                l(&format!("外部 ACP Agent 退出：{tail}"), &format!("External ACP agent exited: {tail}"))
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
                            if !matches_session(&server_req.params, session_id) {
                                let _ = self.rpc.reply_error(&server_req.id, -32602, "Unknown or inactive sessionId");
                                continue;
                            }
                            handle_acp_request(&self.rpc, server_req, events, cancel, turn_done, read_only);
                        }
                        IncomingEvent::Notification(note) => {
                            if !matches_session(&note.params, session_id) {
                                continue;
                            }
                            self.touch();
                            if note.method == "session/update" {
                                handle_acp_update(&note.params, &req.cwd, &mut tool_state, &mut latest_usage, events);
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
            if self.known_sessions.lock().is_ok_and(|s| s.contains(session_id)) {
                return Ok((AcpSessionMeta { session_id: session_id.to_owned(), ..AcpSessionMeta::default() }, false));
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
            return Err(HostError::other(l("外部 ACP Agent 未返回对话 ID。", "The external ACP agent did not return a session id.")));
        }
        Ok((meta, existing.is_some()))
    }

    pub async fn close(&self) {
        self.rpc.close().await;
    }
}

#[derive(Default)]
struct AcpToolState {
    name: String,
    kind: String,
    output: String,
    locations: Vec<String>,
    /// Diff snapshots collected during `in_progress` updates so they survive if `completed` omits them.
    diffs: HashMap<String, (String, String)>,
    completed: bool,
}

fn matches_session(params: &Value, session_id: &str) -> bool {
    match params.get("sessionId").and_then(Value::as_str) {
        Some(id) => id == session_id,
        None => true,
    }
}

async fn build_acp_prompt_blocks(prompt: &str, image_paths: &[PathBuf]) -> Vec<Value> {
    let mut blocks = vec![json!({ "type": "text", "text": prompt })];
    for path in image_paths {
        if let Ok(bytes) = tokio::fs::read(path).await
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

/// Thinking levels by name, weakest first.
const LEVEL_NAMES: [&str; 8] = ["none", "off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Parse ACP `session/new` or `session/resume` response into session id, models, and available modes.
pub fn parse_acp_session_meta(res: &Value) -> AcpSessionMeta {
    let session_id = res.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_owned();
    // ACP's older `models` list.
    let current_model = res.pointer("/models/currentModelId").and_then(Value::as_str).map(str::to_owned);
    let listed: Vec<(String, Option<String>)> = res
        .pointer("/models/availableModels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item.get("modelId").or_else(|| item.get("id")).and_then(Value::as_str).filter(|s| !s.is_empty())?;
            Some((id.to_owned(), item.get("name").or_else(|| item.get("displayName")).and_then(Value::as_str).map(str::to_owned)))
        })
        .collect();
    let mut modes = Vec::new();
    if let Some(arr) = res.pointer("/modes/availableModes").and_then(Value::as_array) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                modes.push(id.to_owned());
            }
        }
    }
    // ACP v2 `configOptions` (category `model` / `mode` / `thought_level`).
    let mut config_models: Vec<(String, Option<String>)> = Vec::new();
    let mut config_current: Option<String> = None;
    let mut thought_option: Option<String> = None;
    let mut thought_levels: Vec<String> = Vec::new();
    let mut default_thought: Option<String> = None;
    if let Some(options) = res.get("configOptions").and_then(Value::as_array) {
        for opt in options {
            let category = opt.get("category").and_then(Value::as_str).unwrap_or_default();
            let id = opt.get("id").and_then(Value::as_str).unwrap_or_default();
            let current = opt.get("currentValue").and_then(Value::as_str).map(str::to_owned);
            let values = collect_config_option_values(opt);
            if category == "model" || id == "model" {
                config_models = values;
                config_current = current;
            } else if category == "mode" || id == "mode" {
                if modes.is_empty() {
                    modes.extend(values.into_iter().map(|(val, _)| val));
                }
            } else if category == "thought_level" || matches!(id, "thought_level" | "reasoning_effort" | "thinking") {
                thought_option = (!id.is_empty()).then(|| id.to_owned());
                thought_levels = values.into_iter().map(|(val, _)| val).collect();
                default_thought = current;
            }
        }
    }
    // Antigravity lists each model once per thinking level in `models` (`gemini-4-neon-high`, ...) and the models
    // themselves in `configOptions`, whose thinking levels are the current model's: each model offers the levels its
    // entries end in (Claude has no `xhigh`, GPT-OSS no level at all).
    let is_level = |text: &str| LEVEL_NAMES.contains(&text) || thought_levels.iter().any(|level| level == text);
    let mut offered: HashMap<&str, Vec<String>> = HashMap::new();
    for (id, _) in &listed {
        if config_models.iter().any(|(model, _)| model == id) {
            continue;
        }
        let base = config_models
            .iter()
            .filter_map(|(model, _)| {
                let level = id.strip_prefix(model.as_str())?.strip_prefix('-')?;
                is_level(level).then_some((model.as_str(), level))
            })
            .max_by_key(|(model, _)| model.len());
        if let Some((model, level)) = base {
            offered.entry(model).or_default().push(level.to_owned());
        }
    }
    let rank = |level: &String| LEVEL_NAMES.iter().position(|name| name == level).unwrap_or(LEVEL_NAMES.len());
    let models = if !offered.is_empty() {
        config_models
            .iter()
            .map(|(id, name)| {
                let current = config_current.as_deref() == Some(id.as_str());
                let mut efforts = if current && !thought_levels.is_empty() { thought_levels.clone() } else { offered.get(id.as_str()).cloned().unwrap_or_default() };
                efforts.sort_by_key(rank);
                efforts.dedup();
                ExternalModelInfo {
                    id: id.clone(),
                    display_name: name.clone(),
                    default_effort: default_thought.clone().filter(|level| efforts.contains(level)),
                    reasoning_efforts: efforts,
                    is_default: current.then_some(true),
                }
            })
            .collect()
    } else {
        // Every model with the advertised levels, from `models` when it lists any.
        let (source, current) = if listed.is_empty() { (&config_models, config_current.as_deref()) } else { (&listed, current_model.as_deref()) };
        source
            .iter()
            .map(|(id, name)| ExternalModelInfo {
                id: id.clone(),
                display_name: name.clone(),
                reasoning_efforts: thought_levels.clone(),
                default_effort: if thought_levels.is_empty() { None } else { default_thought.clone() },
                is_default: (current == Some(id.as_str())).then_some(true),
            })
            .collect()
    };
    AcpSessionMeta { session_id, models, modes, thought_option }
}

/// The tokens a `session/prompt` result reports for its turn (`inputTokens` leaves out the cached part).
fn acp_turn_usage(usage: &Value) -> Option<UsageSummary> {
    let num = |key: &str| usage.get(key).and_then(Value::as_f64);
    let (input, output) = (num("inputTokens"), num("outputTokens"));
    if input.is_none() && output.is_none() {
        return None;
    }
    Some(UsageSummary {
        input: input.unwrap_or(0.0),
        output: output.unwrap_or(0.0),
        cache_read: num("cachedReadTokens").unwrap_or(0.0),
        cache_write: num("cachedWriteTokens").unwrap_or(0.0),
        reasoning: num("thoughtTokens").filter(|v| *v > 0.0),
        cost: None,
    })
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

/// The commands of an `available_commands_update`: `{ name, description, input: { hint } | null }` each.
fn acp_commands(params: &Value) -> Option<Vec<ExternalCommand>> {
    let update = params.get("update").unwrap_or(params);
    if update.get("sessionUpdate").and_then(Value::as_str) != Some("available_commands_update") {
        return None;
    }
    let list = update.get("availableCommands").and_then(Value::as_array)?;
    Some(
        list.iter()
            .filter_map(|command| {
                let name = command.get("name").and_then(Value::as_str)?.trim().trim_start_matches('/').to_owned();
                let valid = !name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_whitespace);
                valid.then(|| ExternalCommand {
                    name,
                    description: command.get("description").and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(|text| text.chars().take(300).collect()),
                    hint: command.pointer("/input/hint").and_then(Value::as_str).map(|hint| hint.chars().take(120).collect()),
                })
            })
            .take(100)
            .collect(),
    )
}

fn handle_acp_update(
    params: &Value,
    cwd: &Path,
    tools: &mut HashMap<String, AcpToolState>,
    latest_usage: &mut Option<UsageSummary>,
    events: &mpsc::UnboundedSender<ExternalEvent>,
) {
    let update = params.get("update").unwrap_or(params);
    let kind = update.get("sessionUpdate").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "available_commands_update" => {
            if let Some(commands) = acp_commands(params) {
                let _ = events.send(ExternalEvent::CommandsUpdated(commands));
            }
        }
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
            add_rainy_args(tool_kind, &locations, cwd, &mut args);
            tools.insert(
                id.to_owned(),
                AcpToolState { name: name.clone(), kind: tool_kind.to_owned(), output: String::new(), locations, diffs: HashMap::new(), completed: false },
            );
            let _ = events.send(ExternalEvent::ToolStarted { id: id.to_owned(), name, args });
        }
        "tool_call_update" => {
            let id = update.get("toolCallId").and_then(Value::as_str).unwrap_or_default();
            if id.is_empty() {
                return;
            }
            let newly_seen = !tools.contains_key(id);
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
            if newly_seen {
                let title = update.get("title").and_then(Value::as_str).unwrap_or(&state.kind);
                let mut args = update.get("rawInput").cloned().unwrap_or_else(|| json!({}));
                if !args.is_object() {
                    args = json!({ "input": args });
                }
                if args.as_object().is_some_and(|m| m.is_empty()) && !title.is_empty() {
                    args["title"] = json!(title);
                }
                add_rainy_args(&state.kind, &state.locations, cwd, &mut args);
                let _ = events.send(ExternalEvent::ToolStarted { id: id.to_owned(), name: state.name.clone(), args });
            }
            collect_acp_diffs(update, &mut state.locations, &mut state.diffs);
            if let Some(chunk) = extract_tool_update_text(update)
                && !chunk.is_empty()
            {
                if !state.output.is_empty() {
                    push_bounded_tail(&mut state.output, "\n", 16384);
                }
                push_bounded_tail(&mut state.output, &chunk, 16384);
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
            // ACP's `usage_update` tells how full the context window is (`used` of `size`), which is no count of the
            // tokens a request took; the turn's own come with `session/prompt`'s result. Only token counts are kept.
            if update.get("inputTokens").is_none() && update.get("outputTokens").is_none() {
                return;
            }
            let num = |k: &str| update.get(k).and_then(Value::as_f64).unwrap_or(0.0);
            let input = num("inputTokens");
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

fn pick_acp_option(options: &[(String, String)], wanted: &[&str]) -> Option<String> {
    wanted.iter().find_map(|target| options.iter().find(|(_, k)| k == target).map(|(oid, _)| oid.clone()))
}

fn handle_acp_request(
    rpc: &Arc<JsonRpcClient>,
    req: super::jsonrpc::ServerRequest,
    events: &mpsc::UnboundedSender<ExternalEvent>,
    cancel: &CancellationToken,
    turn_done: &CancellationToken,
    read_only: bool,
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
        tool_call.pointer("/rawInput/path").or_else(|| tool_call.pointer("/rawInput/file_path")).and_then(Value::as_str).map(acp_path)
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
    if read_only && matches!(kind, "execute" | "edit" | "delete" | "move") {
        let outcome = match pick_acp_option(&options, &["reject_once", "reject_always"]) {
            Some(option_id) => json!({ "outcome": "selected", "optionId": option_id }),
            None => json!({ "outcome": "cancelled" }),
        };
        let _ = rpc.reply_result(&req.id, json!({ "outcome": outcome }));
        return;
    }
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
        agent: None,
    };
    spawn_approval_bridge(
        rpc,
        req.id,
        approval,
        events,
        cancel,
        turn_done,
        json!({ "outcome": { "outcome": "cancelled" } }),
        move |decision| {
            let outcome = match decision {
                Some(ApprovalDecision::Allow) => match pick_acp_option(&options, &["allow_once", "allow_always"]) {
                    Some(option_id) => json!({ "outcome": "selected", "optionId": option_id }),
                    None => json!({ "outcome": "cancelled" }),
                },
                Some(ApprovalDecision::AllowSession) => match pick_acp_option(&options, &["allow_always", "allow_once"]) {
                    Some(option_id) => json!({ "outcome": "selected", "optionId": option_id }),
                    None => json!({ "outcome": "cancelled" }),
                },
                Some(ApprovalDecision::Deny) => match pick_acp_option(&options, &["reject_once", "reject_always"]) {
                    Some(option_id) => json!({ "outcome": "selected", "optionId": option_id }),
                    None => json!({ "outcome": "cancelled" }),
                },
                None => json!({ "outcome": "cancelled" }),
            };
            json!({ "outcome": outcome })
        },
    );
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
    update
        .get("rawOutput")
        .filter(|raw| !raw.is_null() && raw.as_object().is_none_or(|map| !map.is_empty()))
        .map(|raw| raw.as_str().map(str::to_owned).unwrap_or_else(|| raw.to_string()))
}

/// Rainy's names for what an ACP agent's call works on, beside the agent's own (Antigravity's `CommandLine` and
/// `Path`), so a command row shows the command and a file row the file, relative to the project when inside it.
fn add_rainy_args(kind: &str, locations: &[String], cwd: &Path, args: &mut Value) {
    let Some(map) = args.as_object_mut() else { return };
    let text = |map: &serde_json::Map<String, Value>, keys: &[&str]| keys.iter().find_map(|key| map.get(*key).and_then(Value::as_str).map(str::to_owned));
    match kind {
        "execute" if !map.contains_key("command") => {
            if let Some(command) = text(map, &["CommandLine", "commandLine", "cmd"]) {
                map.insert("command".into(), json!(command));
            }
        }
        "read" | "edit" | "delete" | "move" if !map.contains_key("file_path") => {
            let path = locations.first().cloned().or_else(|| text(map, &["Path", "path", "AbsolutePath", "TargetFile"]).map(|path| acp_path(&path)));
            if let Some(path) = path {
                map.insert("file_path".into(), json!(crate::agent::changes::path_for_model(&cwd.to_string_lossy(), &path)));
            }
        }
        _ => {}
    }
}

fn extract_locations(value: &Value) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for path in value.get("locations").and_then(Value::as_array).into_iter().flatten().filter_map(|loc| loc.get("path").and_then(Value::as_str)) {
        let path = acp_path(path);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// One spelling of a path the agent reports: Antigravity names one Windows file `C:\a\b.txt`, `C:/a/b.txt` and
/// `/C:/a/b.txt` within a single call.
fn acp_path(raw: &str) -> String {
    let is_drive = |text: &str| matches!(text.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic());
    let path = raw.strip_prefix('/').filter(|rest| is_drive(rest)).unwrap_or(raw);
    if is_drive(path) { path.replace('/', "\\") } else { path.to_owned() }
}

fn collect_acp_diffs(update: &Value, paths: &mut Vec<String>, texts: &mut HashMap<String, (String, String)>) {
    for block in update.get("content").and_then(Value::as_array).into_iter().flatten() {
        if block.get("type").and_then(Value::as_str) != Some("diff") {
            continue;
        }
        let Some(path) = block.get("path").and_then(Value::as_str).map(acp_path) else { continue };
        if !paths.contains(&path) {
            paths.push(path.clone());
        }
        let before = block.get("oldText").and_then(Value::as_str).unwrap_or_default();
        if let Some(after) = block.get("newText").and_then(Value::as_str)
            && before.len() + after.len() <= MAX_DIFF_TEXT
        {
            texts.insert(path, (before.to_owned(), after.to_owned()));
        }
    }
}

fn build_acp_tool_details(id: &str, state: &AcpToolState, update: &Value) -> Option<Value> {
    if state.kind == "execute" {
        let mut details = json!({ "external": true });
        let raw = update.get("rawOutput");
        if let Some(code) = raw.and_then(|raw| raw.get("exit_code").or_else(|| raw.get("exitCode"))).and_then(Value::as_i64) {
            details["exitCode"] = json!(code);
        }
        return Some(details);
    }
    // The files it names, and for those an ACP `diff` shows (including `in_progress` updates), before and after text.
    let mut paths: Vec<String> = state.locations.clone();
    let mut texts: HashMap<String, (String, String)> = state.diffs.clone();
    collect_acp_diffs(update, &mut paths, &mut texts);
    if matches!(state.kind.as_str(), "edit" | "delete" | "move") && !paths.is_empty() {
        let op = if state.kind == "delete" { "delete" } else { "update" };
        let files: Vec<Value> = paths
            .into_iter()
            .enumerate()
            .map(|(idx, path)| {
                let mut file = json!({
                    "changeId": format!("{id}#{idx}"),
                    "operation": op,
                    "undoable": false,
                    "external": true,
                });
                if let Some((before, after)) = texts.remove(&path) {
                    file["before"] = json!(before);
                    file["after"] = json!(after);
                }
                file["path"] = json!(path);
                file
            })
            .collect();
        return Some(json!({ "files": files, "external": true }));
    }
    Some(json!({ "external": true }))
}

fn acp_error(kind: &str, method: &str, err: JsonRpcError, stderr: &str) -> HostError {
    format_rpc_error(
        &format!("外部 Agent {kind}"),
        &format!("External agent {kind}"),
        method,
        err,
        stderr,
        format!("外部 Agent {kind} 请求 {method} 超时。请检查登录与配置状态。"),
        format!("External agent {kind} timed out on {method}. Check login and CLI setup."),
    )
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
        assert_eq!(meta.thought_option.as_deref(), Some("reasoning_effort"));
    }

    /// What `agy acp` 1.5.16 answers to `session/new` (the model ids as it sent them, the option lists cut down).
    #[test]
    fn reads_antigravity_models_with_the_levels_each_offers() {
        let ids = [
            "gemini-4-neon-high", "gemini-4-neon-xhigh", "gemini-4-neon-medium", "gemini-4-neon-low",
            "gemini-pro-orchestrator-neon", "gemini-pro-orchestrator", "gemini-pro-orchestrator-argon",
            "gemini-3.8-flash-high", "gemini-3.8-flash-medium", "gemini-3.8-flash-low",
            "claude-opus-5-5-low", "claude-opus-5-5-medium", "claude-opus-5-5-high", "gpt-oss-120b-medium",
        ];
        let option = |value: &str, name: &str| json!({ "value": value, "name": name });
        let res = json!({
            "sessionId": "f0c9fb3e-0429-4a6c-b0b8-a15de20df612",
            "models": {
                "currentModelId": "gemini-4-neon-high",
                "availableModels": ids.iter().map(|id| json!({ "modelId": id, "name": id })).collect::<Vec<_>>(),
            },
            "modes": { "currentModeId": "default", "availableModes": [{ "id": "default" }, { "id": "accept-edits" }, { "id": "auto" }, { "id": "plan" }] },
            "configOptions": [
                { "id": "model", "category": "model", "type": "select", "currentValue": "gemini-4-neon", "options": [
                    option("gemini-4-neon", "Gemini 4 Neon"), option("gemini-pro-orchestrator-neon", "Orchestrator (Neon)"),
                    option("gemini-pro-orchestrator", "Orchestrator (Carbon)"), option("gemini-pro-orchestrator-argon", "Orchestrator (Argon)"),
                    option("gemini-3.8-flash", "Gemini 3.8 Flash"), option("claude-opus-5-5", "Claude Opus 5.5"),
                    option("gpt-oss-120b-medium", "GPT-OSS 120B (Medium)"),
                ] },
                { "id": "thought_level", "category": "thought_level", "type": "select", "currentValue": "high", "options": [
                    option("low", "Low"), option("medium", "Medium"), option("high", "High"), option("xhigh", "Extra"),
                ] },
                { "id": "mode", "category": "mode", "type": "select", "currentValue": "default", "options": [option("default", "Default")] },
            ],
        });
        let meta = parse_acp_session_meta(&res);
        assert_eq!(meta.thought_option.as_deref(), Some("thought_level"));
        assert_eq!(meta.modes, vec!["default", "accept-edits", "auto", "plan"]);
        let ids: Vec<&str> = meta.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["gemini-4-neon", "gemini-pro-orchestrator-neon", "gemini-pro-orchestrator", "gemini-pro-orchestrator-argon", "gemini-3.8-flash", "claude-opus-5-5", "gpt-oss-120b-medium"]);
        let model = |id: &str| meta.models.iter().find(|m| m.id == id).unwrap();
        assert_eq!(model("gemini-4-neon").is_default, Some(true));
        assert_eq!(model("gemini-4-neon").display_name.as_deref(), Some("Gemini 4 Neon"));
        assert_eq!(model("gemini-4-neon").reasoning_efforts, ["low", "medium", "high", "xhigh"]);
        assert_eq!(model("gemini-3.8-flash").reasoning_efforts, ["low", "medium", "high"]);
        assert_eq!(model("claude-opus-5-5").reasoning_efforts, ["low", "medium", "high"]);
        assert_eq!(model("claude-opus-5-5").default_effort.as_deref(), Some("high"));
        assert_eq!(model("claude-opus-5-5").is_default, None);
        // Models of their own, not levels of `gemini-pro-orchestrator`; GPT-OSS's `-medium` is part of its name.
        for id in ["gemini-pro-orchestrator", "gemini-pro-orchestrator-neon", "gpt-oss-120b-medium"] {
            assert!(model(id).reasoning_efforts.is_empty(), "{id}");
            assert_eq!(model(id).default_effort, None, "{id}");
        }
    }

    #[test]
    fn one_spelling_per_reported_path_and_the_turns_tokens() {
        assert_eq!(acp_path("/C:/Users/a/hello.txt"), r"C:\Users\a\hello.txt");
        assert_eq!(acp_path("C:/Users/a/hello.txt"), r"C:\Users\a\hello.txt");
        assert_eq!(acp_path(r"C:\Users\a\hello.txt"), r"C:\Users\a\hello.txt");
        assert_eq!(acp_path("/home/a/hello.txt"), "/home/a/hello.txt");
        let call = json!({ "locations": [{ "path": r"C:\w\hello.txt" }, { "path": "C:/w/hello.txt" }, { "path": "/C:/w/hello.txt" }] });
        assert_eq!(extract_locations(&call), [r"C:\w\hello.txt"]);
        let state = AcpToolState { name: "apply_patch".into(), kind: "edit".into(), locations: extract_locations(&call), ..AcpToolState::default() };
        let done = json!({ "content": [{ "type": "diff", "path": "/C:/w/hello.txt", "newText": "hi" }] });
        let details = build_acp_tool_details("call_1", &state, &done).unwrap();
        assert_eq!(details["files"].as_array().unwrap().len(), 1, "{details}");

        let usage = json!({ "inputTokens": 15167, "outputTokens": 51, "cachedReadTokens": 10945, "cachedWriteTokens": 0, "thoughtTokens": 0, "totalTokens": 26163 });
        let usage = acp_turn_usage(&usage).unwrap();
        assert_eq!((usage.input, usage.output, usage.cache_read, usage.reasoning), (15167.0, 51.0, 10945.0, None));
        assert!(acp_turn_usage(&json!({ "used": 13120, "size": 1048576 })).is_none());
    }

    /// A command and an edit of an existing file as `agy acp` 1.5.16 reports them, and its context-window report.
    #[test]
    fn names_antigravity_calls_the_way_rainy_shows_them() {
        let cwd = Path::new(r"C:\w");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tools = HashMap::new();
        let mut usage = None;
        let mut feed = |update: Value| handle_acp_update(&json!({ "sessionId": "s", "update": update }), cwd, &mut tools, &mut usage, &tx);
        let command = r#"python -c "print(6*7)""#;
        feed(json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "name": "run_command", "kind": "execute", "title": command, "status": "in_progress",
            "rawInput": { "CommandLine": command, "WaitMsBeforeAsync": 5000, "toolSummary": "Run python math print" } }));
        feed(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "kind": "execute", "title": command, "status": "completed",
            "rawOutput": { "output": "Command exited with code 0.\n\nCommand output:\n42\r\n", "exit_code": 0 },
            "content": [{ "type": "content", "content": { "type": "text", "text": "Command exited with code 0.\n\nCommand output:\n42\r\n" } }] }));
        feed(json!({ "sessionUpdate": "tool_call", "toolCallId": "e1", "name": "edit_file", "kind": "edit", "title": "File hello.txt", "status": "in_progress",
            "locations": [{ "path": r"C:\w\hello.txt" }], "rawInput": { "Path": r"C:\w\hello.txt", "Replacement": "hello", "Target": "hi" } }));
        feed(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "e1", "kind": "edit", "title": "File /C:/w/hello.txt", "status": "completed",
            "locations": [{ "path": "/C:/w/hello.txt" }], "rawOutput": {}, "content": [{ "type": "diff", "path": "/C:/w/hello.txt", "newText": "hello", "oldText": "hi" }] }));
        feed(json!({ "sessionUpdate": "usage_update", "used": 13120, "size": 1048576 }));
        let events: Vec<ExternalEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(events.len(), 4, "{events:?}");
        let ExternalEvent::ToolStarted { args, .. } = &events[0] else { panic!("{events:?}") };
        assert_eq!(args["command"], command);
        assert_eq!(args["CommandLine"], command, "the agent's own names stay");
        let ExternalEvent::ToolCompleted { details: Some(details), output, .. } = &events[1] else { panic!("{events:?}") };
        assert_eq!(details["exitCode"], 0);
        assert!(output.contains("42"));
        let ExternalEvent::ToolStarted { name, args, .. } = &events[2] else { panic!("{events:?}") };
        assert_eq!(name, "apply_patch");
        assert_eq!(args["file_path"], if cfg!(windows) { "hello.txt" } else { r"C:\w\hello.txt" });
        let ExternalEvent::ToolCompleted { details: Some(details), output, .. } = &events[3] else { panic!("{events:?}") };
        assert!(output.is_empty(), "an empty raw output is no output: {output:?}");
        let files = details["files"].as_array().unwrap();
        assert_eq!(files.len(), 1, "{details}");
        assert_eq!((files[0]["path"].as_str(), files[0]["before"].as_str(), files[0]["after"].as_str()), (Some(r"C:\w\hello.txt"), Some("hi"), Some("hello")));
        assert!(usage.is_none(), "a context-window report is no token count");
    }
}
