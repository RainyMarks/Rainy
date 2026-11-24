//! Bidirectional newline-delimited JSON-RPC 2.0 transport over child process stdio.
//!
//! Used by both `codex app-server --stdio` (which may omit `"jsonrpc": "2.0"` on the wire) and
//! ACP (`agy acp` and custom Agent Client Protocol servers).

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::process::{ProcessJob, console_text, kill_tree};

/// Maximum bytes kept from stderr for diagnostics when a subprocess fails.
const STDERR_TAIL_CHARS: usize = 1200;

/// Failure returned by a JSON-RPC call.
#[derive(Debug, Clone)]
pub enum JsonRpcError {
    Closed(String),
    Timeout,
    Cancelled,
    Rpc { code: i64, message: String },
    Other(String),
}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed(detail) if detail.is_empty() => write!(f, "External agent process closed"),
            Self::Closed(detail) => write!(f, "External agent process closed: {detail}"),
            Self::Timeout => write!(f, "External agent request timed out"),
            Self::Cancelled => write!(f, "Operation cancelled"),
            Self::Rpc { code, message } => write!(f, "RPC error {code}: {message}"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

/// A server-initiated JSON-RPC request (such as an approval callback) expecting a reply with the same `id`.
#[derive(Debug, Clone)]
pub struct ServerRequest {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

/// A one-way JSON-RPC notification from the external agent.
#[derive(Debug, Clone)]
pub struct ServerNotification {
    pub method: String,
    pub params: Value,
}

/// Event delivered to subscribers of a [`JsonRpcClient`].
#[derive(Debug, Clone)]
pub enum IncomingEvent {
    Notification(ServerNotification),
    Request(ServerRequest),
    Closed(String),
}

/// Subscription handle that unregisters itself automatically on drop.
pub struct EventStream {
    id: u64,
    subscribers: Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<IncomingEvent>>>>,
    rx: mpsc::UnboundedReceiver<IncomingEvent>,
}

impl EventStream {
    pub async fn recv(&mut self) -> Option<IncomingEvent> {
        self.rx.recv().await
    }

    /// An event already queued, without waiting.
    pub fn try_recv(&mut self) -> Option<IncomingEvent> {
        self.rx.try_recv().ok()
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        if let Ok(mut map) = self.subscribers.lock() {
            map.remove(&self.id);
        }
    }
}

struct Pending {
    answers: Mutex<HashMap<u64, oneshot::Sender<Result<Value, JsonRpcError>>>>,
    subscribers: Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<IncomingEvent>>>>,
    next_sub: AtomicU64,
}

impl Default for Pending {
    fn default() -> Self {
        Self {
            answers: Mutex::new(HashMap::new()),
            subscribers: Arc::new(Mutex::new(HashMap::new())),
            next_sub: AtomicU64::new(1),
        }
    }
}

impl Pending {
    fn broadcast(&self, event: IncomingEvent) {
        if let Ok(mut subs) = self.subscribers.lock() {
            subs.retain(|_, tx| tx.send(event.clone()).is_ok());
        }
    }

    fn incoming(&self, message: Value) {
        if let Value::Array(items) = message {
            for item in items {
                self.incoming(item);
            }
            return;
        }
        let method = message.get("method").and_then(Value::as_str).map(str::to_owned);
        let id = message.get("id").filter(|id| !id.is_null()).cloned();
        match (method, id) {
            // Response to one of our outgoing requests.
            (None, Some(id)) => {
                let numeric = id.as_u64().or_else(|| id.as_str().and_then(|s| s.parse::<u64>().ok()));
                let Some(tx) = numeric.and_then(|n| self.answers.lock().ok()?.remove(&n)) else {
                    return;
                };
                let outcome = match message.get("error").filter(|e| !e.is_null()) {
                    Some(err) => Err(JsonRpcError::Rpc {
                        code: err.get("code").and_then(Value::as_i64).unwrap_or(-32000),
                        message: err.get("message").and_then(Value::as_str).unwrap_or("unknown error").to_owned(),
                    }),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(outcome);
            }
            // Server -> Client reverse request (e.g. approval / permission prompt).
            (Some(method), Some(id)) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                self.broadcast(IncomingEvent::Request(ServerRequest { id, method, params }));
            }
            // Server -> Client notification (deltas, item updates, usage).
            (Some(method), None) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                self.broadcast(IncomingEvent::Notification(ServerNotification { method, params }));
            }
            (None, None) => {}
        }
    }

    fn fail_all(&self, detail: &str) {
        if let Ok(mut answers) = self.answers.lock() {
            for (_, tx) in answers.drain() {
                let _ = tx.send(Err(JsonRpcError::Closed(detail.to_owned())));
            }
        }
        self.broadcast(IncomingEvent::Closed(detail.to_owned()));
    }
}

/// A live external CLI process speaking newline-delimited JSON-RPC over stdio.
pub struct JsonRpcClient {
    name: String,
    include_version: bool,
    tx: mpsc::UnboundedSender<String>,
    pending: Arc<Pending>,
    next_id: AtomicU64,
    pub closed: CancellationToken,
    stderr: Arc<Mutex<String>>,
    child: AsyncMutex<Option<tokio::process::Child>>,
    job: Mutex<Option<ProcessJob>>,
}

impl JsonRpcClient {
    /// Spawn `program` with `args` in `cwd`, bound to the Host's job object and a dedicated per-process job on Windows.
    pub fn spawn(
        program: &Path,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<OsString, OsString>,
        name: &str,
        include_version: bool,
    ) -> std::io::Result<Arc<Self>> {
        let mut child = crate::process::spawn(program, args, cwd, env, true)?;
        let job = ProcessJob::new().inspect(|job| {
            let _ = job.assign_child(&child);
        });
        let missing = || std::io::Error::other("child stdio pipes are unavailable");
        let mut stdin = child.stdin.take().ok_or_else(missing)?;
        let stdout = child.stdout.take().ok_or_else(missing)?;
        let stderr = child.stderr.take().ok_or_else(missing)?;

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let client = Arc::new(Self {
            name: name.to_owned(),
            include_version,
            tx,
            pending: Arc::new(Pending::default()),
            next_id: AtomicU64::new(1),
            closed: CancellationToken::new(),
            stderr: Arc::new(Mutex::new(String::new())),
            child: AsyncMutex::new(Some(child)),
            job: Mutex::new(job),
        });

        // Writer task: serializes outgoing NDJSON lines cleanly.
        {
            let pending = client.pending.clone();
            let closed = client.closed.clone();
            let stderr_ref = client.stderr.clone();
            tokio::spawn(async move {
                let write = async {
                    while let Some(line) = rx.recv().await {
                        stdin.write_all(line.as_bytes()).await?;
                        stdin.flush().await?;
                    }
                    std::io::Result::Ok(())
                };
                tokio::select! {
                    outcome = write => {
                        if outcome.is_err() {
                            closed.cancel();
                            let tail = stderr_ref.lock().map(|s| s.trim().to_owned()).unwrap_or_default();
                            pending.fail_all(&tail);
                        }
                    }
                    () = closed.cancelled() => {}
                }
            });
        }

        // Stdout reader task: decodes one JSON object per line.
        {
            let pending = client.pending.clone();
            let closed = client.closed.clone();
            let stderr_ref = client.stderr.clone();
            let tag = client.name.clone();
            tokio::spawn(async move {
                let mut reader = tokio::io::BufReader::new(stdout);
                let mut buf = Vec::new();
                while reader.read_until(b'\n', &mut buf).await.is_ok_and(|n| n > 0) {
                    let line = String::from_utf8_lossy(&buf);
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        match serde_json::from_str::<Value>(trimmed) {
                            Ok(message) => pending.incoming(message),
                            Err(_) => log::debug!("[external:{tag}] non-json stdout: {}", &trimmed[..trimmed.len().min(200)]),
                        }
                    }
                    buf.clear();
                }
                closed.cancel();
                let tail = stderr_ref.lock().map(|s| s.trim().to_owned()).unwrap_or_default();
                pending.fail_all(&tail);
            });
        }

        // Stderr reader task: keeps a bounded diagnostic tail with console code-page decoding on Windows.
        {
            let stderr_ref = client.stderr.clone();
            let tag = client.name.clone();
            tokio::spawn(async move {
                let mut reader = tokio::io::BufReader::new(stderr);
                let mut buf = Vec::new();
                while reader.read_until(b'\n', &mut buf).await.is_ok_and(|n| n > 0) {
                    let text = console_text(&buf);
                    buf.clear();
                    let line = text.trim_end_matches(['\r', '\n']);
                    if !line.is_empty() {
                        log::info!("[external:{tag}] {line}");
                        if let Ok(mut kept) = stderr_ref.lock() {
                            kept.push_str(line);
                            kept.push('\n');
                            let count = kept.chars().count();
                            if count > STDERR_TAIL_CHARS {
                                *kept = kept.chars().skip(count - STDERR_TAIL_CHARS).collect();
                            }
                        }
                    }
                }
            });
        }

        Ok(client)
    }

    pub fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr.lock().map(|s| s.trim().to_owned()).unwrap_or_default()
    }

    /// Subscribe to server notifications and reverse requests without dropping messages.
    pub fn subscribe(&self) -> EventStream {
        let id = self.pending.next_sub.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut subs) = self.pending.subscribers.lock() {
            subs.insert(id, tx);
        }
        EventStream { id, subscribers: self.pending.subscribers.clone(), rx }
    }

    fn enqueue(&self, value: &Value) -> Result<(), JsonRpcError> {
        if self.is_closed() {
            return Err(JsonRpcError::Closed(self.stderr_tail()));
        }
        let mut line = serde_json::to_string(value).map_err(|e| JsonRpcError::Other(e.to_string()))?;
        line.push('\n');
        self.tx.send(line).map_err(|_| JsonRpcError::Closed(self.stderr_tail()))
    }

    /// Send a JSON-RPC request and wait for its result up to `timeout` or cancellation.
    pub async fn request(&self, method: &str, params: Value, timeout: Duration, cancel: &CancellationToken) -> Result<Value, JsonRpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut answers) = self.pending.answers.lock() {
            answers.insert(id, tx);
        }
        let mut payload = json!({ "id": id, "method": method, "params": params });
        if self.include_version {
            payload["jsonrpc"] = json!("2.0");
        }
        if let Err(err) = self.enqueue(&payload) {
            if let Ok(mut answers) = self.pending.answers.lock() {
                answers.remove(&id);
            }
            return Err(err);
        }
        let outcome = tokio::select! {
            ans = rx => ans.unwrap_or_else(|_| Err(JsonRpcError::Closed(self.stderr_tail()))),
            () = tokio::time::sleep(timeout) => Err(JsonRpcError::Timeout),
            () = cancel.cancelled() => Err(JsonRpcError::Cancelled),
            () = self.closed.cancelled() => Err(JsonRpcError::Closed(self.stderr_tail())),
        };
        if outcome.is_err()
            && let Ok(mut answers) = self.pending.answers.lock()
        {
            answers.remove(&id);
        }
        outcome
    }

    /// Send a one-way JSON-RPC notification.
    pub fn notify(&self, method: &str, params: Value) -> Result<(), JsonRpcError> {
        let mut payload = json!({ "method": method, "params": params });
        if self.include_version {
            payload["jsonrpc"] = json!("2.0");
        }
        self.enqueue(&payload)
    }

    /// Reply to a server-initiated JSON-RPC request with a success result.
    pub fn reply_result(&self, id: &Value, result: Value) -> Result<(), JsonRpcError> {
        let mut payload = json!({ "id": id, "result": result });
        if self.include_version {
            payload["jsonrpc"] = json!("2.0");
        }
        self.enqueue(&payload)
    }

    /// Reply to a server-initiated JSON-RPC request with a JSON-RPC error object.
    pub fn reply_error(&self, id: &Value, code: i64, message: &str) -> Result<(), JsonRpcError> {
        let mut payload = json!({ "id": id, "error": { "code": code, "message": message } });
        if self.include_version {
            payload["jsonrpc"] = json!("2.0");
        }
        self.enqueue(&payload)
    }

    /// Terminate the subprocess tree and release all waiting requests cleanly.
    pub async fn close(&self) {
        self.closed.cancel();
        let tail = self.stderr_tail();
        self.pending.fail_all(&tail);
        if let Some(mut child) = self.child.lock().await.take() {
            kill_tree(&mut child, Duration::from_millis(800)).await;
        }
        if let Ok(mut job) = self.job.lock()
            && let Some(job) = job.take()
        {
            job.terminate();
        }
    }
}

impl Drop for JsonRpcClient {
    fn drop(&mut self) {
        self.closed.cancel();
        if let Ok(mut job) = self.job.lock()
            && let Some(job) = job.take()
        {
            job.terminate();
        }
        if let Ok(mut guard) = self.child.try_lock()
            && let Some(mut child) = guard.take()
        {
            #[cfg(unix)]
            if let Some(pid) = child.id()
                && pid <= i32::MAX as u32
            {
                // SAFETY: child leads its own process group (`process_group(0)` in `crate::process::command`).
                unsafe {
                    let _ = libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = child.start_kill();
        }
    }
}

/// Environment for an external CLI subprocess: inherits user environment (preserving CLI auth env vars
/// such as `OPENAI_API_KEY`, `GEMINI_API_KEY`, `CODEX_HOME`, `HTTP(S)_PROXY`) while scrubbing Rainy's
/// own internal secrets (`RAINY_TOKEN`, `RAINY_*_KEY`, `RAINY_CREDENTIAL_*`).
pub fn external_env() -> BTreeMap<OsString, OsString> {
    let mut env: BTreeMap<OsString, OsString> = std::env::vars_os()
        .filter(|(name, _)| {
            let upper = name.to_string_lossy().to_uppercase();
            !(upper == "RAINY_TOKEN"
                || upper.starts_with("RAINY_CREDENTIAL")
                || (upper.starts_with("RAINY_") && (upper.ends_with("_KEY") || upper.ends_with("_SECRET") || upper.ends_with("_TOKEN"))))
        })
        .collect();
    if cfg!(windows) && !env.keys().any(|k| k.to_string_lossy().eq_ignore_ascii_case("PYTHONUTF8")) {
        env.insert(OsString::from("PYTHONUTF8"), OsString::from("1"));
    }
    env
}

/// Resolve an external CLI executable on the Host's `PATH` (or direct path), with common fallback locations
/// such as `~/.local/bin` on Unix and `%USERPROFILE%\.codex\bin` / `%LOCALAPPDATA%\Programs` on Windows.
pub fn resolve_cli(command: &str, env: &BTreeMap<OsString, OsString>) -> Option<std::path::PathBuf> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(found) = crate::process::resolve_executable(trimmed, Some(env)) {
        return Some(found);
    }
    // Check well-known user-level CLI install directories if not yet on the inherited PATH.
    let direct = Path::new(trimmed).is_absolute() || trimmed.contains('/') || (cfg!(windows) && trimmed.contains('\\'));
    if direct {
        return None;
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)?;
    let mut extra_dirs = vec![home.join(".local").join("bin"), home.join(".codex").join("bin")];
    if cfg!(windows)
        && let Some(local) = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
    {
        extra_dirs.push(local.join("Programs").join("Antigravity").join("bin"));
        extra_dirs.push(local.join("Antigravity").join("bin"));
    }
    let mut augmented = env.clone();
    let existing = augmented
        .iter()
        .find(|(k, _)| if cfg!(windows) { k.to_string_lossy().eq_ignore_ascii_case("PATH") } else { k.as_os_str() == "PATH" })
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let mut paths: Vec<std::path::PathBuf> = std::env::split_paths(&existing).collect();
    paths.extend(extra_dirs);
    if let Ok(joined) = std::env::join_paths(paths) {
        if cfg!(windows) {
            augmented.retain(|k, _| !k.to_string_lossy().eq_ignore_ascii_case("PATH"));
        }
        augmented.insert(OsString::from("PATH"), joined);
        return crate::process::resolve_executable(trimmed, Some(&augmented));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_env_scrubs_rainy_secrets_but_keeps_path() {
        let env = external_env();
        assert!(env.keys().any(|k| k.to_string_lossy().eq_ignore_ascii_case("PATH")));
        assert!(!env.keys().any(|k| k.to_string_lossy().eq_ignore_ascii_case("RAINY_TOKEN")));
    }

    #[test]
    fn incoming_routes_responses_requests_and_notifications() {
        let pending = Pending::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        pending.subscribers.lock().unwrap().insert(1, tx);
        let (ans_tx, mut ans_rx) = oneshot::channel();
        pending.answers.lock().unwrap().insert(7, ans_tx);

        // Codex-style response without "jsonrpc": "2.0"
        pending.incoming(json!({ "id": 7, "result": { "ok": true } }));
        assert_eq!(ans_rx.try_recv().unwrap().unwrap(), json!({ "ok": true }));

        // Server request
        pending.incoming(json!({ "id": "req-1", "method": "session/request_permission", "params": { "a": 1 } }));
        match rx.try_recv().unwrap() {
            IncomingEvent::Request(req) => {
                assert_eq!(req.id, json!("req-1"));
                assert_eq!(req.method, "session/request_permission");
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // Notification
        pending.incoming(json!({ "method": "item/agentMessage/delta", "params": { "delta": "hi" } }));
        match rx.try_recv().unwrap() {
            IncomingEvent::Notification(note) => {
                assert_eq!(note.method, "item/agentMessage/delta");
                assert_eq!(note.params["delta"], "hi");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
