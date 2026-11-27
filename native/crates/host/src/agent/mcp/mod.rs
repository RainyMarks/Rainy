//! MCP servers configured in Settings (3.x `mcp.ts`): one connection per enabled server, its tools as agent tools,
//! and its instructions for the system prompt.

pub mod client;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use rainy_protocol::chat::ResultContent;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use self::client::{Client, Failure};
use super::llm::ToolSpec;
use super::tools::{Tool, ToolOutput, Updates};
use crate::locale::l;
use crate::network::Network;
use crate::settings::McpServerConfig;

/// Per-call timeout; a tool that reports progress keeps running past it, up to the total.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const CALL_TOTAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Wait for an HTTP server to connect and list its tools, both transports together.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// A local server started through uvx or npx can spend its first start downloading packages and an interpreter.
const STDIO_CONNECT_TIMEOUT: Duration = Duration::from_secs(180);
/// An HTTP+SSE stream that drops later than this after connecting is reopened; one that drops sooner is an error.
const SSE_SETTLE: Duration = Duration::from_secs(10);
/// A stdio server running at least this long before exiting resets its backoff counter.
const STDIO_SETTLE: Duration = Duration::from_secs(10);
/// Maximum automatic reconnect attempts for a crashed stdio server.
const STDIO_MAX_RECONNECTS: u32 = 5;
/// Base backoff delay before restarting a crashed stdio server (2s, 4s, 8s, 16s, 32s).
const STDIO_BACKOFF_BASE_SECS: u64 = 2;
/// Hard fallback cap on a single MCP text block when no session `ToolContext::limit_output` is attached.
const FALLBACK_MAX_CHARS: usize = 48_000;

/// Output limiter matching [`super::tools::ToolContext::limit_output`].
pub type OutputLimiter = Arc<dyn Fn(&str, String) -> String + Send + Sync>;

/// The model-facing name of an MCP tool: `mcp__<server>__<tool>` in `[A-Za-z0-9_-]`, at most 64 characters.
pub fn tool_name(server: &str, tool: &str) -> String {
    let name: String = format!("mcp__{server}__{tool}").chars().map(|char| if char.is_ascii_alphanumeric() || char == '_' || char == '-' { char } else { '_' }).collect();
    if name.len() <= 64 {
        return name;
    }
    format!("{}_{}", &name[..55], &crate::text::sha256_hex(&name)[..8])
}

/// A tool a server lists.
#[derive(Clone, Debug)]
pub struct ServerTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// The server marks it as not changing its environment (`readOnlyHint`).
    pub read_only: bool,
}

struct Connection {
    config: McpServerConfig,
    /// `connecting`, `ready`, `error` or `disabled`.
    state: &'static str,
    error: Option<String>,
    client: Option<Arc<Client>>,
    tools: Vec<ServerTool>,
    instructions: String,
    /// Consecutive stdio auto-reconnect attempts since the server last stayed up past [`STDIO_SETTLE`].
    reconnect_attempts: u32,
    /// Cancelled when the connection is replaced or closed, ending a connection attempt.
    retired: CancellationToken,
}

/// Keeps one connection per enabled server.
pub struct McpManager {
    network: Arc<Network>,
    runtime: Option<tokio::runtime::Handle>,
    connections: Mutex<Vec<Arc<Mutex<Connection>>>>,
    listeners: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

impl McpManager {
    pub fn new(network: Arc<Network>) -> Arc<Self> {
        Arc::new(Self {
            network,
            runtime: tokio::runtime::Handle::try_current().ok(),
            connections: Mutex::default(),
            listeners: Mutex::default(),
        })
    }

    fn runtime_handle(&self) -> Option<tokio::runtime::Handle> {
        self.runtime.clone().or_else(|| tokio::runtime::Handle::try_current().ok())
    }

    pub fn on_change(&self, listener: impl Fn() + Send + Sync + 'static) {
        self.listeners.lock().unwrap().push(Arc::new(listener));
    }

    fn notify(&self) {
        let listeners = self.listeners.lock().unwrap().clone();
        for listener in listeners {
            listener();
        }
    }

    /// Bring connections in line with the configured servers. When an enabled server is replaced in place,
    /// the new connection waits for the retired client to close first so stdio processes do not race on ports or locks.
    pub fn sync(self: &Arc<Self>, servers: &[McpServerConfig]) {
        let mut connections = self.connections.lock().unwrap();
        let mut replacing: std::collections::HashMap<String, Arc<Client>> = std::collections::HashMap::new();
        connections.retain(|connection| {
            let mut held = connection.lock().unwrap();
            match servers.iter().find(|server| server.enabled && server.same_connection(&held.config)) {
                Some(server) => {
                    held.config = server.clone();
                    true
                }
                None => {
                    let name = held.config.name.clone();
                    let still_enabled = servers.iter().any(|server| server.enabled && server.name == name);
                    if still_enabled && let Some(client) = held.client.take() {
                        held.retired.cancel();
                        replacing.insert(name, client);
                    } else {
                        self.retire(&held);
                    }
                    false
                }
            }
        });
        for server in servers {
            if connections.iter().any(|connection| connection.lock().unwrap().config.name == server.name) {
                continue;
            }
            let connection = Arc::new(Mutex::new(Connection {
                config: server.clone(),
                state: if server.enabled { "connecting" } else { "disabled" },
                error: None,
                client: None,
                tools: Vec::new(),
                instructions: String::new(),
                reconnect_attempts: 0,
                retired: CancellationToken::new(),
            }));
            let previous = replacing.remove(&server.name);
            if server.enabled {
                self.spawn_connect_after(connection.clone(), previous, Duration::ZERO);
            } else if let Some(old) = previous {
                self.close_client_async(old);
            }
            connections.push(connection);
        }
        drop(connections);
        for (_, old) in replacing {
            self.close_client_async(old);
        }
        self.notify();
    }

    fn close_client_async(&self, client: Arc<Client>) {
        if let Some(handle) = self.runtime_handle() {
            handle.spawn(async move { client.close().await });
        } else {
            log::warn!("[mcp] no Tokio runtime handle available to close retired client asynchronously");
        }
    }

    fn retire(&self, connection: &Connection) {
        connection.retired.cancel();
        if let Some(client) = connection.client.clone() {
            self.close_client_async(client);
        }
    }

    /// Connect again the HTTP servers that failed, for example after the proxy settings changed.
    pub fn retry_web_servers(self: &Arc<Self>) {
        let mut retried = false;
        for connection in self.connections.lock().unwrap().iter() {
            let mut held = connection.lock().unwrap();
            if held.config.transport != "streamable-http" || held.state != "error" {
                continue;
            }
            held.state = "connecting";
            held.error = None;
            drop(held);
            retried = true;
            self.spawn_connect_after(connection.clone(), None, Duration::ZERO);
        }
        if retried {
            self.notify();
        }
    }

    /// Live state of every configured server, its tools marked as Settings allows them.
    pub fn status(&self) -> Vec<Value> {
        self.connections
            .lock()
            .unwrap()
            .iter()
            .map(|connection| {
                let held = connection.lock().unwrap();
                let tools: Vec<Value> = held
                    .tools
                    .iter()
                    .map(|tool| json!({ "name": tool.name, "description": tool.description, "enabled": held.config.allows(&tool.name, &tool_name(&held.config.name, &tool.name)) }))
                    .collect();
                let mut status = json!({ "name": held.config.name, "state": held.state, "tools": tools });
                if let Some(error) = &held.error {
                    status["error"] = json!(error);
                }
                status
            })
            .collect()
    }

    /// Instructions of connected servers, for the system prompt.
    pub fn instructions(&self) -> Vec<(String, String)> {
        self.connections
            .lock()
            .unwrap()
            .iter()
            .filter_map(|connection| {
                let held = connection.lock().unwrap();
                (held.state == "ready" && !held.instructions.is_empty()).then(|| (held.config.name.clone(), held.instructions.clone()))
            })
            .collect()
    }

    /// Model-facing names of connected tools their server marks read-only.
    pub fn read_only_tools(&self) -> HashSet<String> {
        let mut names = HashSet::new();
        for connection in self.connections.lock().unwrap().iter() {
            let held = connection.lock().unwrap();
            if held.state == "ready" {
                names.extend(held.tools.iter().filter(|tool| tool.read_only).map(|tool| tool_name(&held.config.name, &tool.name)));
            }
        }
        names
    }

    /// Whether a tool's server is connected and Settings still allows it; a run checks this before each call.
    pub fn allows(&self, name: &str) -> bool {
        self.connections.lock().unwrap().iter().any(|connection| {
            let held = connection.lock().unwrap();
            held.state == "ready" && held.client.is_some() && held.tools.iter().any(|tool| tool_name(&held.config.name, &tool.name) == name && held.config.allows(&tool.name, name))
        })
    }

    /// The allowed tools of connected servers, deduplicated by model-facing name, with optional output limiting.
    pub fn tools(&self, limiter: Option<OutputLimiter>) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for connection in self.connections.lock().unwrap().iter() {
            let held = connection.lock().unwrap();
            let Some(client) = held.client.clone().filter(|_| held.state == "ready") else { continue };
            let server = held.config.name.clone();
            for tool in &held.tools {
                let name = tool_name(&server, &tool.name);
                if !held.config.allows(&tool.name, &name) || !seen.insert(name.clone()) {
                    continue;
                }
                let description = if tool.description.is_empty() { format!("{} from MCP server {server}", tool.name) } else { tool.description.clone() };
                tools.push(Arc::new(McpTool {
                    client: client.clone(),
                    server: server.clone(),
                    tool: tool.name.clone(),
                    spec: ToolSpec { name, description, parameters: tool.input_schema.clone(), freeform: None },
                    limiter: limiter.clone(),
                }));
            }
        }
        tools
    }

    /// Close every connection.
    pub async fn close(&self) {
        let connections: Vec<_> = std::mem::take(&mut *self.connections.lock().unwrap());
        let clients: Vec<Arc<Client>> = connections
            .iter()
            .filter_map(|connection| {
                let held = connection.lock().unwrap();
                held.retired.cancel();
                held.client.clone()
            })
            .collect();
        futures::future::join_all(clients.iter().map(|client| client.close())).await;
    }

    fn spawn_connect_after(self: &Arc<Self>, connection: Arc<Mutex<Connection>>, previous: Option<Arc<Client>>, delay: Duration) {
        let Some(handle) = self.runtime_handle() else {
            log::warn!("[mcp] no Tokio runtime handle available to connect server");
            return;
        };
        let manager = self.clone();
        handle.spawn(async move {
            if let Some(old) = previous {
                old.close().await;
            }
            if !delay.is_zero() {
                let retired = connection.lock().unwrap().retired.clone();
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = retired.cancelled() => return,
                }
            }
            manager.connect(connection).await;
        });
    }

    async fn connect(self: Arc<Self>, connection: Arc<Mutex<Connection>>) {
        let (config, retired) = {
            let held = connection.lock().unwrap();
            (held.config.clone(), held.retired.clone())
        };
        if retired.is_cancelled() {
            return;
        }
        // Pass `retired` into `open` so a stdio child process is explicitly closed via `kill_tree` on cancellation
        // instead of leaking when a `tokio::select!` drops the future.
        let attempt = self.open(&config, &retired).await;
        if retired.is_cancelled() {
            if let Ok((client, _, _)) = attempt {
                client.close().await;
            }
            return;
        }
        match attempt {
            Ok((client, tools, sse)) => {
                let installed = {
                    let mut held = connection.lock().unwrap();
                    if held.retired.is_cancelled() {
                        false
                    } else {
                        held.instructions = client.instructions();
                        held.tools = tools;
                        held.client = Some(client.clone());
                        held.state = "ready";
                        held.error = None;
                        true
                    }
                };
                if !installed {
                    client.close().await;
                    return;
                }
                self.notify();
                let connected = std::time::Instant::now();
                let is_stdio = config.transport == "stdio";
                let manager = self.clone();
                let Some(handle) = self.runtime_handle() else { return };
                handle.spawn(async move {
                    tokio::select! {
                        () = client.closed.cancelled() => {}
                        () = retired.cancelled() => return,
                    }
                    let elapsed = connected.elapsed();
                    let reopen_sse = sse && elapsed > SSE_SETTLE;
                    let backoff = {
                        let mut held = connection.lock().unwrap();
                        if held.client.as_ref().is_some_and(|held| Arc::ptr_eq(held, &client)) {
                            held.client = None;
                        }
                        if reopen_sse {
                            log::info!("[mcp:{}] the HTTP+SSE stream closed; connecting again", held.config.name);
                            held.state = "connecting";
                            held.error = None;
                            Some(Duration::ZERO)
                        } else if is_stdio {
                            if elapsed >= STDIO_SETTLE {
                                held.reconnect_attempts = 0;
                            }
                            if held.reconnect_attempts < STDIO_MAX_RECONNECTS {
                                let attempt = held.reconnect_attempts;
                                held.reconnect_attempts += 1;
                                let delay = stdio_backoff_delay(attempt);
                                log::info!(
                                    "[mcp:{}] stdio server exited; reconnecting in {}s (attempt {}/{STDIO_MAX_RECONNECTS})",
                                    held.config.name,
                                    delay.as_secs(),
                                    held.reconnect_attempts
                                );
                                held.state = "connecting";
                                held.error = Some(l(
                                    &format!("MCP 服务器意外退出，{} 秒后自动重连…", delay.as_secs()),
                                    &format!("The MCP server exited unexpectedly; reconnecting in {}s…", delay.as_secs()),
                                ));
                                Some(delay)
                            } else {
                                held.state = "error";
                                held.error = Some(l(
                                    "MCP 服务器多次意外退出，已停止自动重连。",
                                    "The MCP server exited repeatedly; automatic reconnect stopped.",
                                ));
                                None
                            }
                        } else {
                            held.state = "error";
                            held.error = Some(l("MCP 服务器断开了连接。", "The server closed the connection."));
                            None
                        }
                    };
                    manager.notify();
                    if let Some(delay) = backoff {
                        manager.spawn_connect_after(connection, None, delay);
                    }
                });
            }
            Err(message) => {
                log::info!("[mcp:{}] {message}", config.name);
                let retry_delay = {
                    let mut held = connection.lock().unwrap();
                    if config.transport == "stdio" && held.reconnect_attempts > 0 && held.reconnect_attempts < STDIO_MAX_RECONNECTS {
                        let delay = stdio_backoff_delay(held.reconnect_attempts);
                        held.reconnect_attempts += 1;
                        held.state = "connecting";
                        held.error = Some(message);
                        Some(delay)
                    } else {
                        held.state = "error";
                        held.error = Some(message);
                        None
                    }
                };
                self.notify();
                if let Some(delay) = retry_delay {
                    self.spawn_connect_after(connection, None, delay);
                }
            }
        }
    }

    /// Connect, shake hands and list the tools; whether the server speaks the older HTTP+SSE transport.
    async fn open(&self, config: &McpServerConfig, retired: &CancellationToken) -> Result<(Arc<Client>, Vec<ServerTool>, bool), String> {
        let headers: Vec<(String, String)> = config.headers.iter().flatten().map(|(name, value)| (name.clone(), value.clone())).collect();
        if config.transport == "stdio" {
            let command = config.command.clone().unwrap_or_default();
            let mut overlay = std::collections::BTreeMap::new();
            if let Some(env) = &config.env {
                overlay.extend(env.clone());
            }
            let env = crate::agent::util::scrubbed_child_env(&overlay);
            let program = crate::process::resolve_executable(&command, Some(&env)).unwrap_or_else(|| command.clone().into());
            let cwd = crate::env::home_dir();
            let (client, stderr) = Client::stdio(&program, config.args.as_deref().unwrap_or_default(), &env, &cwd, &config.name).map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    l(&format!("找不到启动命令 {command}。请检查命令名称，或填写完整路径。"), &format!("The command {command} was not found. Check its name, or enter its full path."))
                } else {
                    error.to_string()
                }
            })?;
            let ready = async {
                client.initialize_cancellable(STDIO_CONNECT_TIMEOUT, retired).await?;
                client.list_tools_cancellable(STDIO_CONNECT_TIMEOUT, retired).await
            };
            return match ready.await {
                Ok(tools) if !retired.is_cancelled() => Ok((client, parse_tools(&tools), false)),
                Ok(_) => {
                    client.close().await;
                    Err(Failure::Cancelled.to_string())
                }
                Err(failure) => {
                    client.close().await;
                    let output = stderr.lock().unwrap().trim().lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" ");
                    Err(match failure {
                        Failure::Closed => l(
                            &format!("启动命令运行后立即退出了。{}", if output.is_empty() { "请检查命令、参数和环境变量。".to_owned() } else { format!("输出：{output}") }),
                            &format!("The command exited right after starting. {}", if output.is_empty() { "Check the command, arguments and environment variables.".to_owned() } else { format!("Output: {output}") }),
                        ),
                        other => other.to_string(),
                    })
                }
            };
        }
        let url = url::Url::parse(config.url.as_deref().unwrap_or_default()).map_err(|error| error.to_string())?;
        let deadline = std::time::Instant::now() + CONNECT_TIMEOUT;
        let client = Client::http(&self.network, &url, headers.clone()).await;
        let streamable = async {
            client.initialize_cancellable(CONNECT_TIMEOUT, retired).await?;
            client.list_tools_cancellable(CONNECT_TIMEOUT, retired).await
        };
        match streamable.await {
            Ok(tools) if !retired.is_cancelled() => Ok((client, parse_tools(&tools), false)),
            Ok(_) => {
                client.close().await;
                Err(Failure::Cancelled.to_string())
            }
            // A server from before Streamable HTTP refuses its POST with a 4xx status and speaks HTTP+SSE at the same
            // address. Authorization failures are reported as they are.
            Err(Failure::Http(status, text)) if (400..500).contains(&status) && status != 401 && status != 403 && !retired.is_cancelled() => {
                let first = Failure::Http(status, text).to_string();
                log::info!("[mcp:{}] {first}; trying HTTP+SSE", config.name);
                client.close().await;
                let left = deadline.saturating_duration_since(std::time::Instant::now()).max(Duration::from_secs(5));
                let sse = tokio::select! {
                    res = Client::sse(&self.network, &url, headers, left) => res,
                    () = retired.cancelled() => return Err(Failure::Cancelled.to_string()),
                };
                let sse = match sse {
                    Ok(sse) => sse,
                    // An address that is no SSE endpoint either adds nothing to the first error.
                    Err(Failure::Http(404 | 405, _)) => return Err(first),
                    Err(Failure::Other(message)) if message.to_lowercase().contains("content type") => return Err(first),
                    Err(other) => return Err(other.to_string()),
                };
                let ready = async {
                    sse.initialize_cancellable(left, retired).await?;
                    sse.list_tools_cancellable(left, retired).await
                };
                match ready.await {
                    Ok(tools) if !retired.is_cancelled() => {
                        log::info!("[mcp:{}] connected over HTTP+SSE", config.name);
                        Ok((sse, parse_tools(&tools), true))
                    }
                    Ok(_) => {
                        sse.close().await;
                        Err(Failure::Cancelled.to_string())
                    }
                    Err(failure) => {
                        sse.close().await;
                        Err(failure.to_string())
                    }
                }
            }
            Err(failure) => {
                client.close().await;
                Err(failure.to_string())
            }
        }
    }
}

fn parse_tools(listed: &[Value]) -> Vec<ServerTool> {
    listed
        .iter()
        .filter_map(|tool| {
            Some(ServerTool {
                name: tool["name"].as_str()?.to_owned(),
                description: tool["description"].as_str().unwrap_or_default().to_owned(),
                input_schema: tool.get("inputSchema").filter(|schema| schema.is_object()).cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                read_only: tool.pointer("/annotations/readOnlyHint") == Some(&Value::Bool(true)),
            })
        })
        .collect()
}

/// Exponential backoff delay for stdio reconnect attempt `attempt` (0-based): 2s, 4s, 8s, 16s, 32s.
fn stdio_backoff_delay(attempt: u32) -> Duration {
    Duration::from_secs(STDIO_BACKOFF_BASE_SECS.saturating_mul(1u64 << attempt.min(5)))
}

fn limit_mcp_text(tool: &str, text: String, limiter: Option<&OutputLimiter>) -> String {
    if let Some(limit) = limiter {
        return limit(tool, text);
    }
    let total_chars = text.chars().count();
    if total_chars <= FALLBACK_MAX_CHARS {
        return text;
    }
    let head = FALLBACK_MAX_CHARS * 3 / 4;
    let tail = FALLBACK_MAX_CHARS - head;
    let head_byte = text.char_indices().nth(head).map_or(text.len(), |(idx, _)| idx);
    let tail_byte = text.char_indices().nth(total_chars.saturating_sub(tail)).map_or(text.len(), |(idx, _)| idx);
    format!("{}\n[Output limited ({} chars omitted)]\n{}", &text[..head_byte], total_chars - FALLBACK_MAX_CHARS, &text[tail_byte..])
}

/// One server tool as the model calls it.
struct McpTool {
    client: Arc<Client>,
    server: String,
    tool: String,
    spec: ToolSpec,
    limiter: Option<OutputLimiter>,
}

impl Tool for McpTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let result = self
                .client
                .request("tools/call", json!({ "name": self.tool, "arguments": args }), CALL_TIMEOUT, CALL_TOTAL_TIMEOUT, &cancel)
                .await
                .map_err(|failure| match failure {
                    Failure::Cancelled => "tool call aborted".to_owned(),
                    other => other.to_string(),
                })?;
            let mut content = Vec::new();
            let mut text_parts = Vec::new();
            for block in result["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text") => text_parts.extend(block["text"].as_str().map(str::to_owned)),
                    Some("image") => {
                        if let (Some(data), Some(mime_type)) = (block["data"].as_str(), block["mimeType"].as_str()) {
                            content.push(ResultContent::Image { data: data.to_owned(), mime_type: mime_type.to_owned() });
                        }
                    }
                    Some("resource") => text_parts.extend(block.pointer("/resource/text").and_then(Value::as_str).map(str::to_owned)),
                    _ => {}
                }
            }
            if text_parts.is_empty()
                && content.is_empty()
                && let Some(structured) = result.get("structuredContent").filter(|value| !value.is_null())
            {
                text_parts.push(serde_json::to_string_pretty(structured).unwrap_or_default());
            }
            if !text_parts.is_empty() {
                let limited = limit_mcp_text(&self.spec.name, text_parts.join("\n"), self.limiter.as_ref());
                content.insert(0, ResultContent::Text { text: limited });
            }
            if content.is_empty() {
                content.push(ResultContent::Text { text: "(no output)".into() });
            }
            Ok(ToolOutput { content, details: Some(json!({ "server": self.server, "tool": self.tool })), is_error: result["isError"] == true, terminate: false })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_tools_like_3x() {
        assert_eq!(tool_name("my.server", "do it"), "mcp__my_server__do_it");
        let long = tool_name("server", &"x".repeat(80));
        assert_eq!(long.len(), 64);
        assert!(long.starts_with("mcp__server__xxx"));
    }

    #[test]
    fn limits_oversized_mcp_output_and_computes_backoff() {
        let custom: OutputLimiter = Arc::new(|tool, text| format!("{tool}:{}", text.len().min(10)));
        assert_eq!(limit_mcp_text("mcp__a__b", "0123456789abcdef".into(), Some(&custom)), "mcp__a__b:10");
        let huge = "a".repeat(FALLBACK_MAX_CHARS + 5000);
        let cut = limit_mcp_text("mcp__a__b", huge, None);
        assert!(cut.contains("[Output limited") && cut.len() < FALLBACK_MAX_CHARS + 128);
        assert_eq!(stdio_backoff_delay(0), Duration::from_secs(2));
        assert_eq!(stdio_backoff_delay(1), Duration::from_secs(4));
        assert_eq!(stdio_backoff_delay(2), Duration::from_secs(8));
        assert_eq!(stdio_backoff_delay(4), Duration::from_secs(32));
    }
}
