//! One MCP connection (JSON-RPC 2.0): over a local program's standard streams, over Streamable HTTP, or over the older
//! HTTP+SSE transport that many `/sse` addresses still speak.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent::llm::sse::Events;
use crate::network::Network;

/// The protocol version Rainy asks for; servers may answer with an older one they support.
pub const PROTOCOL_VERSION: &str = "2025-11-25";
const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05", "2024-10-07"];
/// Wait for a notification or an answer to the server's request to be taken.
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a request failed.
#[derive(Debug)]
pub enum Failure {
    /// The HTTP server refused the request with this status.
    Http(u16, String),
    /// The connection is gone.
    Closed,
    Timeout,
    Cancelled,
    /// The server answered with a JSON-RPC error, or something else went wrong.
    Other(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Http(status, text) => write!(formatter, "Streamable HTTP error: Error POSTing to endpoint (HTTP {status}): {text}"),
            Failure::Closed => write!(formatter, "Connection closed"),
            Failure::Timeout => write!(formatter, "Request timed out"),
            Failure::Cancelled => write!(formatter, "Request cancelled"),
            Failure::Other(message) => write!(formatter, "{message}"),
        }
    }
}

/// Requests waiting for their answer, and the progress listeners of tool calls.
#[derive(Default)]
struct Waiting {
    answers: Mutex<HashMap<u64, oneshot::Sender<Result<Value, Failure>>>>,
    progress: Mutex<HashMap<String, mpsc::UnboundedSender<()>>>,
}

impl Waiting {
    /// Route one message from the server: answers to our requests, its requests to us (answered on `reply`), and
    /// progress notifications.
    fn incoming(&self, message: Value, reply: &mpsc::UnboundedSender<Value>) {
        if let Value::Array(items) = message {
            for item in items {
                self.incoming(item, reply);
            }
            return;
        }
        let method = message["method"].as_str();
        match (method, message.get("id").filter(|id| !id.is_null())) {
            (None, Some(id)) => {
                let Some(waiting) = id.as_u64().and_then(|id| self.answers.lock().unwrap().remove(&id)) else { return };
                let answer = match message.get("error") {
                    Some(error) => Err(Failure::Other(format!("MCP error {}: {}", error["code"], error["message"].as_str().unwrap_or("unknown error")))),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = waiting.send(answer);
            }
            (Some(method), Some(id)) => {
                let answer = match method {
                    "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                    "roots/list" => json!({ "jsonrpc": "2.0", "id": id, "result": { "roots": [] } }),
                    _ => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "Method not found" } }),
                };
                let _ = reply.send(answer);
            }
            (Some("notifications/progress"), None) => {
                let token = match &message["params"]["progressToken"] {
                    Value::String(token) => token.clone(),
                    other => other.to_string(),
                };
                if let Some(sender) = self.progress.lock().unwrap().get(&token) {
                    let _ = sender.send(());
                }
            }
            _ => {}
        }
    }

    fn fail_all(&self) {
        for (_, waiting) in self.answers.lock().unwrap().drain() {
            let _ = waiting.send(Err(Failure::Closed));
        }
    }
}

/// How messages leave.
enum Outgoing {
    /// Lines for the program's standard input, written in order by a task of their own, so a request given up while
    /// the program does not read never leaves half a message behind.
    Stdio(mpsc::UnboundedSender<String>),
    Http { client: reqwest::Client, url: String, headers: Vec<(String, String)>, session: Mutex<Option<String>>, version: Mutex<Option<String>> },
    Sse { client: reqwest::Client, endpoint: String, headers: Vec<(String, String)> },
}

/// What a local server wrote on its error stream last, for the message shown when it does not start.
pub type Stderr = Arc<Mutex<String>>;

/// A connected client.
pub struct Client {
    outgoing: Outgoing,
    waiting: Arc<Waiting>,
    /// Answers to the server's requests, sent in order by a task of their own.
    replies: mpsc::UnboundedSender<Value>,
    next: AtomicU64,
    /// Cancelled when the connection closes, from either side.
    pub closed: CancellationToken,
    child: AsyncMutex<Option<tokio::process::Child>>,
    instructions: Mutex<String>,
}

/// Read a stream of server-sent events into `waiting` until it ends or the connection closes.
fn pump_events(mut body: impl futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin + Send + 'static, waiting: Arc<Waiting>, reply: mpsc::UnboundedSender<Value>, closed: CancellationToken, end_closes: bool) {
    tokio::spawn(async move {
        let mut events = Events::default();
        loop {
            tokio::select! {
                chunk = body.next() => {
                    let Some(Ok(chunk)) = chunk else { break };
                    for (name, data) in events.push(&chunk) {
                        if (name.is_empty() || name == "message")
                            && let Ok(message) = serde_json::from_str::<Value>(&data)
                        {
                            waiting.incoming(message, &reply);
                        }
                    }
                }
                () = closed.cancelled() => break,
            }
        }
        if end_closes {
            closed.cancel();
            waiting.fail_all();
        }
    });
}

/// Route every message a local program writes, one per line, until its output ends. Lines are read as bytes, so one
/// that is not UTF-8 is dropped like any other that is no message, and the pipe is drained to its end.
async fn read_messages(stdout: impl tokio::io::AsyncRead + Unpin, waiting: &Waiting, reply: &mpsc::UnboundedSender<Value>) {
    let mut stdout = tokio::io::BufReader::new(stdout);
    let mut line = Vec::new();
    while stdout.read_until(b'\n', &mut line).await.is_ok_and(|read| read > 0) {
        if let Ok(message) = serde_json::from_str::<Value>(String::from_utf8_lossy(&line).trim()) {
            waiting.incoming(message, reply);
        }
        line.clear();
    }
}

/// Log what a local program writes on its error stream and keep the end of it, until the stream ends. Python servers
/// on a Chinese Windows write there in the console's code page.
async fn keep_errors(stderr: impl tokio::io::AsyncRead + Unpin, kept: Stderr, server: String) {
    let mut stderr = tokio::io::BufReader::new(stderr);
    let mut bytes = Vec::new();
    while stderr.read_until(b'\n', &mut bytes).await.is_ok_and(|read| read > 0) {
        let line = crate::process::console_text(&bytes);
        bytes.clear();
        let line = line.trim_end_matches(['\r', '\n']);
        log::info!("[mcp:{server}] {}", line.trim_end());
        let mut text = kept.lock().unwrap();
        text.push_str(line);
        text.push('\n');
        let excess = text.chars().count().saturating_sub(600);
        if excess > 0 {
            *text = text.chars().skip(excess).collect();
        }
    }
}

impl Client {
    fn new(outgoing: Outgoing, child: Option<tokio::process::Child>) -> Arc<Self> {
        let (replies, mut queue) = mpsc::unbounded_channel::<Value>();
        let client = Arc::new(Self {
            outgoing,
            waiting: Arc::default(),
            replies,
            next: AtomicU64::new(1),
            closed: CancellationToken::new(),
            child: AsyncMutex::new(child),
            instructions: Mutex::new(String::new()),
        });
        let weak = Arc::downgrade(&client);
        tokio::spawn(async move {
            while let Some(answer) = queue.recv().await {
                let Some(client) = weak.upgrade() else { break };
                let _ = client.send_within(&answer, NOTIFY_TIMEOUT).await;
            }
        });
        client
    }

    /// A local program speaking MCP on its standard streams.
    pub fn stdio(program: &std::path::Path, args: &[String], env: &std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>, cwd: &std::path::Path, name: &str) -> std::io::Result<(Arc<Self>, Stderr)> {
        let mut child = crate::process::spawn(program, args, cwd, env, true)?;
        let missing = || std::io::Error::other("the program's streams are not available");
        let (mut stdin, stdout, stderr) = (child.stdin.take().ok_or_else(missing)?, child.stdout.take().ok_or_else(missing)?, child.stderr.take().ok_or_else(missing)?);
        let (lines, mut queue) = mpsc::unbounded_channel::<String>();
        let client = Self::new(Outgoing::Stdio(lines), Some(child));
        let (waiting, closed) = (client.waiting.clone(), client.closed.clone());
        tokio::spawn(async move {
            let write = async {
                while let Some(line) = queue.recv().await {
                    stdin.write_all(line.as_bytes()).await?;
                    stdin.flush().await?;
                }
                std::io::Result::Ok(())
            };
            tokio::select! {
                written = write => if written.is_err() {
                    closed.cancel();
                    waiting.fail_all();
                },
                () = closed.cancelled() => {}
            }
        });
        let (waiting, reply, closed) = (client.waiting.clone(), client.replies.clone(), client.closed.clone());
        tokio::spawn(async move {
            read_messages(stdout, &waiting, &reply).await;
            closed.cancel();
            waiting.fail_all();
        });
        let errors: Stderr = Arc::default();
        tokio::spawn(keep_errors(stderr, errors.clone(), name.to_owned()));
        Ok((client, errors))
    }

    /// A Streamable HTTP server.
    pub async fn http(network: &Network, url: &url::Url, headers: Vec<(String, String)>) -> Arc<Self> {
        let client = network.client(url).await;
        Self::new(Outgoing::Http { client, url: url.to_string(), headers, session: Mutex::new(None), version: Mutex::new(None) }, None)
    }

    /// An HTTP+SSE server: the event stream opens first and names the address messages are posted to.
    pub async fn sse(network: &Network, url: &url::Url, headers: Vec<(String, String)>, timeout: Duration) -> Result<Arc<Self>, Failure> {
        let http = network.client(url).await;
        let mut request = http.get(url.clone()).header("accept", "text/event-stream");
        for (name, value) in &headers {
            request = request.header(name, value);
        }
        let response = tokio::time::timeout(timeout, request.send()).await.map_err(|_| Failure::Timeout)?.map_err(|error| Failure::Other(error.to_string()))?;
        let status = response.status().as_u16();
        let content_type = response.headers().get("content-type").and_then(|value| value.to_str().ok()).unwrap_or_default().to_owned();
        if !(200..300).contains(&status) {
            return Err(Failure::Http(status, response.text().await.unwrap_or_default()));
        }
        if !content_type.to_lowercase().starts_with("text/event-stream") {
            return Err(Failure::Other(format!("Unexpected content type: {content_type}")));
        }
        let mut body = response.bytes_stream();
        let mut events = Events::default();
        let deadline = Instant::now() + timeout;
        let mut early: Vec<String> = Vec::new();
        let endpoint = 'found: loop {
            let chunk = tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), body.next())
                .await
                .map_err(|_| Failure::Other(crate::locale::l("HTTP+SSE 连接超时：服务器没有发来会话地址。", "The HTTP+SSE connection timed out: the server sent no session address.")))?;
            let Some(Ok(chunk)) = chunk else { return Err(Failure::Closed) };
            for (name, data) in events.push(&chunk) {
                if name == "endpoint" {
                    let endpoint = url.join(data.trim()).map_err(|error| Failure::Other(error.to_string()))?;
                    if endpoint.origin() != url.origin() {
                        return Err(Failure::Other(format!("Endpoint origin does not match connection origin: {}", endpoint.origin().ascii_serialization())));
                    }
                    break 'found endpoint.to_string();
                }
                early.push(data);
            }
        };
        let client = Self::new(Outgoing::Sse { client: http, endpoint, headers }, None);
        for data in early {
            if let Ok(message) = serde_json::from_str::<Value>(&data) {
                client.waiting.incoming(message, &client.replies);
            }
        }
        pump_events(body, client.waiting.clone(), client.replies.clone(), client.closed.clone(), true);
        Ok(client)
    }

    /// Send one message; an HTTP answer is routed like any other message from the server.
    async fn send(&self, message: &Value) -> Result<(), Failure> {
        match &self.outgoing {
            Outgoing::Stdio(lines) => lines.send(format!("{message}\n")).map_err(|_| Failure::Closed),
            Outgoing::Sse { client, endpoint, headers } => {
                let mut request = client.post(endpoint).header("content-type", "application/json").body(message.to_string());
                for (name, value) in headers {
                    request = request.header(name, value);
                }
                let response = request.send().await.map_err(|error| Failure::Other(format!("fetch failed: {}", crate::agent::llm::transport_error(&error))))?;
                let status = response.status().as_u16();
                if !(200..300).contains(&status) {
                    return Err(Failure::Http(status, response.text().await.unwrap_or_default()));
                }
                Ok(())
            }
            Outgoing::Http { client, url, headers, session, version } => {
                let mut request = client.post(url).header("content-type", "application/json").header("accept", "application/json, text/event-stream").body(message.to_string());
                for (name, value) in headers {
                    request = request.header(name, value);
                }
                if let Some(session) = session.lock().unwrap().clone() {
                    request = request.header("mcp-session-id", session);
                }
                if let Some(version) = version.lock().unwrap().clone() {
                    request = request.header("mcp-protocol-version", version);
                }
                let response = request.send().await.map_err(|error| Failure::Other(format!("fetch failed: {}", crate::agent::llm::transport_error(&error))))?;
                if let Some(id) = response.headers().get("mcp-session-id").and_then(|value| value.to_str().ok()) {
                    *session.lock().unwrap() = Some(id.to_owned());
                }
                let status = response.status().as_u16();
                if !(200..300).contains(&status) {
                    return Err(Failure::Http(status, response.text().await.unwrap_or_default()));
                }
                if status == 202 {
                    return Ok(());
                }
                let content_type = response.headers().get("content-type").and_then(|value| value.to_str().ok()).unwrap_or_default().to_lowercase();
                if content_type.starts_with("application/json") {
                    let text = response.text().await.map_err(|error| Failure::Other(error.to_string()))?;
                    if let Ok(message) = serde_json::from_str::<Value>(&text) {
                        self.waiting.incoming(message, &self.replies);
                    }
                } else if content_type.starts_with("text/event-stream") {
                    pump_events(response.bytes_stream(), self.waiting.clone(), self.replies.clone(), self.closed.clone(), false);
                } else if message.get("id").is_some() && message.get("method").is_some() {
                    return Err(Failure::Other(format!("Unexpected content type: {content_type}")));
                }
                Ok(())
            }
        }
    }

    /// Send one message, giving up when the connection closes or after `limit`: an HTTP server may hold the request
    /// open without ever answering.
    async fn send_within(&self, message: &Value, limit: Duration) -> Result<(), Failure> {
        tokio::select! {
            sent = tokio::time::timeout(limit, self.send(message)) => sent.unwrap_or(Err(Failure::Timeout)),
            () = self.closed.cancelled() => Err(Failure::Closed),
        }
    }

    /// Send a notification.
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), Failure> {
        self.send_within(&json!({ "jsonrpc": "2.0", "method": method, "params": params }), NOTIFY_TIMEOUT).await
    }

    /// One request. Progress the server reports restarts the timeout, up to `total`; a cancelled or timed-out request
    /// is cancelled at the server too. Sending counts towards the timeout: over HTTP the answer may be the body of the
    /// request itself.
    pub async fn request(self: &Arc<Self>, method: &str, mut params: Value, timeout: Duration, total: Duration, cancel: &CancellationToken) -> Result<Value, Failure> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (answer, waiting) = oneshot::channel();
        self.waiting.answers.lock().unwrap().insert(id, answer);
        let (progressed, mut progress) = mpsc::unbounded_channel();
        let token = id.to_string();
        if method == "tools/call" && params.is_object() {
            params["_meta"] = json!({ "progressToken": id });
            self.waiting.progress.lock().unwrap().insert(token.clone(), progressed);
        }
        let cleanup = || {
            self.waiting.answers.lock().unwrap().remove(&id);
            self.waiting.progress.lock().unwrap().remove(&token);
        };
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let sending = self.send(&message);
        let started = Instant::now();
        let mut deadline = started + timeout;
        let mut sent = false;
        tokio::pin!(sending, waiting);
        let result = loop {
            let wait = deadline.min(started + total).saturating_duration_since(Instant::now());
            tokio::select! {
                outcome = &mut sending, if !sent => match outcome {
                    Ok(()) => sent = true,
                    Err(failure) => break Err(failure),
                },
                answer = &mut waiting => break answer.unwrap_or(Err(Failure::Closed)),
                Some(()) = progress.recv() => deadline = Instant::now() + timeout,
                () = tokio::time::sleep(wait) => break Err(Failure::Timeout),
                () = cancel.cancelled() => break Err(Failure::Cancelled),
                () = self.closed.cancelled() => break Err(Failure::Closed),
            }
        };
        cleanup();
        if let Err(failure @ (Failure::Timeout | Failure::Cancelled)) = &result {
            // As in the TypeScript SDK, the server hears of it without the caller waiting.
            let (client, reason) = (self.clone(), failure.to_string());
            tokio::spawn(async move {
                let _ = client.notify("notifications/cancelled", json!({ "requestId": id, "reason": reason })).await;
            });
        }
        result
    }

    /// The MCP handshake; the server's instructions are kept.
    pub async fn initialize(self: &Arc<Self>, timeout: Duration) -> Result<(), Failure> {
        let params = json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": { "name": "Rainy", "version": env!("CARGO_PKG_VERSION") } });
        let result = self.request("initialize", params, timeout, timeout, &CancellationToken::new()).await?;
        let version = result["protocolVersion"].as_str().unwrap_or_default();
        if !SUPPORTED_VERSIONS.contains(&version) {
            return Err(Failure::Other(format!("Server's protocol version is not supported: {version}")));
        }
        if let Outgoing::Http { version: held, .. } = &self.outgoing {
            *held.lock().unwrap() = Some(version.to_owned());
        }
        *self.instructions.lock().unwrap() = result["instructions"].as_str().unwrap_or_default().to_owned();
        self.send_within(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }), timeout).await
    }

    pub fn instructions(&self) -> String {
        self.instructions.lock().unwrap().clone()
    }

    /// Every tool the server lists, page by page, all pages within `timeout`.
    pub async fn list_tools(self: &Arc<Self>, timeout: Duration) -> Result<Vec<Value>, Failure> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let deadline = Instant::now() + timeout;
        for _ in 0..100 {
            let params = cursor.as_ref().map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            let left = deadline.saturating_duration_since(Instant::now());
            let result = self.request("tools/list", params, left, left, &CancellationToken::new()).await?;
            tools.extend(result["tools"].as_array().cloned().unwrap_or_default());
            cursor = result["nextCursor"].as_str().filter(|next| !next.is_empty()).map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    /// Close the connection: an HTTP session is ended, a local program stopped.
    pub async fn close(&self) {
        self.closed.cancel();
        if let Outgoing::Http { client, url, headers, session, .. } = &self.outgoing {
            let held = session.lock().unwrap().clone();
            if let Some(session) = held {
                let mut request = client.delete(url).header("mcp-session-id", session).timeout(Duration::from_secs(3));
                for (name, value) in headers {
                    request = request.header(name, value);
                }
                let _ = request.send().await;
            }
        }
        if let Some(mut child) = self.child.lock().await.take() {
            crate::process::kill_tree(&mut child, Duration::from_secs(2)).await;
        }
        self.waiting.fail_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_silent_http_server_times_out_and_stops() {
        // A server that takes requests and never answers them.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let client = Client::new(Outgoing::Http { client: http, url: format!("http://{address}/mcp"), headers: Vec::new(), session: Mutex::new(None), version: Mutex::new(None) }, None);
        let started = Instant::now();
        let cancel = CancellationToken::new();
        let timed_out = client.request("tools/call", json!({}), Duration::from_millis(300), Duration::from_secs(5), &cancel).await;
        assert!(matches!(timed_out, Err(Failure::Timeout)));
        let stop = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            stop.cancel();
        });
        let cancelled = client.request("tools/call", json!({}), Duration::from_secs(60), Duration::from_secs(60), &cancel).await;
        assert!(matches!(cancelled, Err(Failure::Cancelled)));
        assert!(matches!(client.initialize(Duration::from_millis(300)).await, Err(Failure::Timeout)));
        assert!(matches!(client.list_tools(Duration::from_millis(300)).await, Err(Failure::Timeout)));
        assert!(started.elapsed() < Duration::from_secs(10));
        server.abort();
    }

    #[tokio::test]
    async fn output_that_is_not_utf8_does_not_end_the_streams() {
        let waiting = Waiting::default();
        let (answer, mut answered) = oneshot::channel();
        waiting.answers.lock().unwrap().insert(7, answer);
        let (reply, _replies) = mpsc::unbounded_channel();
        read_messages(&b"\xd6\xd0\xce\xc4\n{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}\n"[..], &waiting, &reply).await;
        assert_eq!(answered.try_recv().unwrap().unwrap(), json!({ "ok": true }));
        let kept: Stderr = Arc::default();
        keep_errors(&b"\xd6\xd0\xce\xc4 failed\r\nlast line\n"[..], kept.clone(), "test".into()).await;
        assert!(kept.lock().unwrap().ends_with("failed\nlast line\n"));
    }
}
