//! Language servers over the `/rainy/ide/lsp` WebSocket (3.x `ide/language.ts`): one server process per editor
//! connection, initialized for the project folder, and messages naming files outside it refused.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::files::Files;
use super::framing::{Decoder, encode};
use super::state::StateStore;
use super::tools::{Kit, Tools};
use crate::activity::Activity;
use crate::error::{HostError, HostResult};
use crate::paths::{contains, realpath};
use crate::process::child_env;
use crate::runtime::Runtime;

const MAX_CONNECTIONS: usize = 8;
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
/// Close code telling the editor to connect again: the project's interpreter environment changed.
pub const RESTART_CODE: u16 = 1012;

struct Spec {
    root: PathBuf,
    argv: Vec<String>,
    environment: BTreeMap<String, String>,
}

struct Connection {
    id: u64,
    workspace_id: String,
    root_id: Option<String>,
    language: String,
    argv: Vec<String>,
    environment: BTreeMap<String, String>,
    stop: CancellationToken,
    /// The close code and reason the socket gets when the Host stops the server.
    close: Mutex<Option<(u16, &'static str)>>,
}

pub struct Languages {
    tools: Tools,
    files: Arc<Files>,
    runtime: Arc<Runtime>,
    state: Arc<StateStore>,
    activity: Arc<Activity>,
    connections: Mutex<Vec<Arc<Connection>>>,
    next: AtomicU64,
    closing: AtomicBool,
}

fn file_uri(path: &Path) -> String {
    Url::from_file_path(path).map(String::from).unwrap_or_else(|_| format!("file://{}", path.display()))
}

/// Refuse a document URI that is not a file inside the project, also through links.
fn check_uri(root: &Path, spelling: &str) -> HostResult<()> {
    let refused = || HostError::invalid("Language document is outside its workspace");
    let url = Url::parse(spelling).map_err(|_| refused())?;
    if url.scheme() != "file" {
        return Err(HostError::invalid("Editor language documents must use file URIs"));
    }
    let path = url.to_file_path().map_err(|_| refused())?;
    let path = crate::paths::normalize(&path);
    if !contains(root, &path) {
        return Err(refused());
    }
    let mut existing = path.as_path();
    loop {
        match realpath(existing) {
            Ok(real) => return if contains(root, &real) { Ok(()) } else { Err(refused()) },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => existing = existing.parent().ok_or_else(refused)?,
            Err(error) => return Err(error.into()),
        }
    }
}

/// Check an editor message and bind initialization to the project folder.
fn prepare(mut message: Value, root: &Path) -> HostResult<Value> {
    let invalid = || HostError::invalid("Invalid editor language message");
    let object = message.as_object().ok_or_else(invalid)?;
    let id_ok = match object.get("id") {
        None => object.get("method").is_some_and(Value::is_string),
        Some(id) => id.is_number() || id.is_string() || id.is_null(),
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || !id_ok {
        return Err(invalid());
    }
    match object.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let params = message.get_mut("params").and_then(Value::as_object_mut).ok_or_else(|| HostError::invalid("Language initialization parameters are missing"))?;
            let uri = file_uri(root);
            let name = root.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
            params.insert("processId".into(), Value::Null);
            params.insert("rootPath".into(), json!(root));
            params.insert("rootUri".into(), json!(uri));
            params.insert("workspaceFolders".into(), json!([{ "uri": uri, "name": name }]));
            return Ok(message);
        }
        Some("workspace/didChangeWorkspaceFolders") => return Err(HostError::invalid("Open another workspace language connection to change the project")),
        _ => {}
    }
    let mut pending = vec![&message["params"]];
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(items) => pending.extend(items.iter()),
            Value::Object(fields) => {
                for (key, child) in fields {
                    match child {
                        Value::String(spelling) if ["uri", "oldUri", "newUri", "rootUri"].contains(&key.as_str()) => check_uri(root, spelling)?,
                        Value::Array(_) | Value::Object(_) => pending.push(child),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(message)
}

impl Languages {
    pub fn new(tools: Tools, files: Arc<Files>, runtime: Arc<Runtime>, state: Arc<StateStore>, activity: Arc<Activity>) -> Arc<Self> {
        Arc::new(Self { tools, files, runtime, state, activity, connections: Mutex::default(), next: AtomicU64::new(1), closing: AtomicBool::new(false) })
    }

    fn resolve(&self, workspace_id: &str, language: &str, root_id: Option<&str>) -> HostResult<Spec> {
        self.activity.assert_can_start()?;
        if self.closing.load(Ordering::Relaxed) {
            return Err(HostError::other("Editor language services are closing"));
        }
        let root = self.files.resolve_workspace(workspace_id, root_id)?.root;
        let environment = self.runtime.resolve_workspace(workspace_id).map(|resolved| resolved.environment).unwrap_or_default();
        let argv = match language {
            "python" => vec![Tools::require(self.tools.ty(), Kit::Python)?.display().to_string(), "server".into()],
            "javascript" | "typescript" => vec![Tools::require(self.tools.tsc(), Kit::Web)?.display().to_string(), "--lsp".into(), "--stdio".into()],
            "c" | "cpp" => {
                let clangd = crate::process::resolve_executable("clangd", Some(&child_env(&environment))).ok_or_else(|| crate::process::executable_missing("clangd"))?;
                let mut argv = vec![clangd.display().to_string(), "--background-index=0".into()];
                if let Some(directory) = self.compile_commands(workspace_id, &root) {
                    argv.push(format!("--compile-commands-dir={}", directory.display()));
                }
                argv
            }
            _ => return Err(HostError::invalid("Unknown editor language")),
        };
        Ok(Spec { root, argv, environment })
    }

    /// The build folder of the project's active CMake run profile, where clangd finds `compile_commands.json`.
    fn compile_commands(&self, workspace_id: &str, root: &Path) -> Option<PathBuf> {
        let state = self.state.get(workspace_id).ok()?;
        let configuration = &state["data"]["execution"];
        let profiles = configuration["profiles"].as_array()?;
        let native = |profile: &&Value| profile["language"] == "c" || profile["language"] == "cpp";
        let active = profiles.iter().find(|profile| profile["name"] == configuration["activeProfile"]).filter(native);
        let profile = active.or_else(|| profiles.iter().find(native))?;
        (profile["build"]["kind"] == "cmake").then(|| super::resolve::workspace_path(root, profile["build"]["buildDirectory"].as_str()?, None).ok()).flatten()
    }

    /// Serve one accepted socket whose query names the project, folder and language.
    pub async fn accept(self: Arc<Self>, socket: WebSocket, url: Url) {
        let query: std::collections::HashMap<String, String> = url.query_pairs().map(|(name, value)| (name.into_owned(), value.into_owned())).collect();
        let (mut sink, mut stream) = socket.split();
        let close = |code: u16, reason: &'static str| Message::Close(Some(CloseFrame { code, reason: reason.into() }));
        let valid_keys = query.keys().all(|key| ["workspaceId", "language", "rootId"].contains(&key.as_str()));
        let (Some(workspace_id), Some(language)) = (query.get("workspaceId").filter(|_| valid_keys), query.get("language")) else {
            let _ = sink.send(close(1008, "Invalid editor language request")).await;
            return;
        };
        let root_id = query.get("rootId").cloned();
        let spec = match self.resolve(workspace_id, language, root_id.as_deref()) {
            Ok(spec) => spec,
            // The interface offers the download of tools that are not there.
            Err(error) if error.code == super::tools::MISSING_REASON => {
                let _ = sink.send(close(super::tools::MISSING_CODE, super::tools::MISSING_REASON)).await;
                return;
            }
            Err(error) => {
                log::info!("IDE language request rejected: {}", error.message);
                let _ = sink.send(close(1008, "Invalid editor language request")).await;
                return;
            }
        };
        if self.connections.lock().unwrap().len() >= MAX_CONNECTIONS {
            let _ = sink.send(close(1013, "Editor language capacity reached")).await;
            return;
        }
        let mut env = child_env(&spec.environment);
        env.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
        let mut command = crate::process::command(&spec.argv[0], &spec.argv[1..], &spec.root, &env);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                log::warn!("IDE language startup failed: {}: {error}", spec.argv[0]);
                let _ = sink.send(Message::Text(json!({ "jsonrpc": "2.0", "method": "window/logMessage", "params": { "type": 1, "message": format!("{}: {error}", spec.argv[0]) } }).to_string().into())).await;
                let _ = sink.send(close(1011, "Language server could not start")).await;
                return;
            }
        };
        let connection = Arc::new(Connection {
            id: self.next.fetch_add(1, Ordering::Relaxed),
            workspace_id: workspace_id.clone(),
            root_id,
            language: language.clone(),
            argv: spec.argv.clone(),
            environment: spec.environment.clone(),
            stop: CancellationToken::new(),
            close: Mutex::new(None),
        });
        self.connections.lock().unwrap().push(connection.clone());
        let (mut stdin, mut stdout, mut stderr) = (child.stdin.take().expect("piped"), child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
        tokio::spawn(async move {
            let mut sink = [0u8; 8192];
            while matches!(stderr.read(&mut sink).await, Ok(read) if read > 0) {}
        });
        let (to_socket, mut socket_queue) = tokio::sync::mpsc::unbounded_channel::<Message>();
        let writer = tokio::spawn(async move {
            while let Some(message) = socket_queue.recv().await {
                let closing = matches!(message, Message::Close(_));
                if sink.send(message).await.is_err() || closing {
                    break;
                }
            }
        });
        // Server → editor.
        let server_output = {
            let to_socket = to_socket.clone();
            let stop = connection.stop.clone();
            tokio::spawn(async move {
                let mut decoder = Decoder::new(MAX_MESSAGE_BYTES);
                let mut buffer = vec![0u8; 65536];
                loop {
                    let read = tokio::select! {
                        read = stdout.read(&mut buffer) => read,
                        () = stop.cancelled() => break,
                    };
                    match read {
                        Ok(0) | Err(_) => break,
                        Ok(read) => match decoder.push(&buffer[..read]) {
                            Ok(messages) => {
                                for message in messages {
                                    let _ = to_socket.send(Message::Text(message.to_string().into()));
                                }
                            }
                            Err(error) => {
                                log::warn!("Editor language output failed: {error}");
                                stop.cancel();
                                break;
                            }
                        },
                    }
                }
            })
        };
        // Editor → server, in order.
        let root = spec.root.clone();
        let input_stop = connection.stop.clone();
        let editor_input = {
            let to_socket = to_socket.clone();
            tokio::spawn(async move {
                loop {
                    let message = tokio::select! {
                        message = stream.next() => message,
                        () = input_stop.cancelled() => break,
                    };
                    let text = match message {
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(Message::Binary(_))) => {
                            let _ = to_socket.send(Message::Close(Some(CloseFrame { code: 1011, reason: "Editor language input exceeds its message budget".into() })));
                            break;
                        }
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        Some(Ok(_)) => continue,
                    };
                    if text.len() > MAX_MESSAGE_BYTES {
                        break;
                    }
                    let admitted = serde_json::from_str::<Value>(text.as_str()).map_err(HostError::from).and_then(|message| prepare(message, &root));
                    match admitted {
                        Ok(message) => {
                            if stdin.write_all(&encode(&message)).await.is_err() || stdin.flush().await.is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = to_socket.send(Message::Text(json!({ "jsonrpc": "2.0", "method": "window/logMessage", "params": { "type": 1, "message": error.message } }).to_string().into()));
                            let _ = to_socket.send(Message::Close(Some(CloseFrame { code: 1011, reason: "Editor language connection failed".into() })));
                            break;
                        }
                    }
                }
                input_stop.cancel();
            })
        };
        let status = tokio::select! {
            status = child.wait() => Some(status),
            () = connection.stop.cancelled() => None,
        };
        connection.stop.cancel();
        if status.is_none() {
            crate::process::kill_tree(&mut child, Duration::from_secs(2)).await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), server_output).await;
        editor_input.abort();
        let requested = *connection.close.lock().unwrap();
        let (code, reason) = match (requested, status) {
            (Some(requested), _) => requested,
            (None, Some(Ok(status))) if status.success() => (1000, "Language server exited"),
            (None, Some(_)) => (1011, "Language server stopped"),
            (None, None) => (1000, "Editor language connection closed"),
        };
        let _ = to_socket.send(close(code, reason));
        drop(to_socket);
        let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
        self.connections.lock().unwrap().retain(|known| known.id != connection.id);
    }

    /// Stop the language servers of one project.
    pub fn close_workspace(&self, workspace_id: &str) {
        for connection in self.connections.lock().unwrap().iter().filter(|connection| connection.workspace_id == workspace_id) {
            connection.stop.cancel();
        }
    }

    /// Restart the project's servers whose process choice changed, such as the PATH after another interpreter was
    /// selected; their sockets close with [`RESTART_CODE`] so the editor connects again.
    pub fn refresh_workspace(&self, workspace_id: &str) {
        let connections: Vec<Arc<Connection>> = self.connections.lock().unwrap().iter().filter(|connection| connection.workspace_id == workspace_id).cloned().collect();
        for connection in connections {
            let Ok(next) = self.resolve(workspace_id, &connection.language, connection.root_id.as_deref()) else { continue };
            if next.argv != connection.argv || next.environment != connection.environment {
                *connection.close.lock().unwrap() = Some((RESTART_CODE, "Language environment changed"));
                connection.stop.cancel();
            }
        }
    }

    pub fn close(&self) {
        self.closing.store(true, Ordering::Relaxed);
        for connection in self.connections.lock().unwrap().iter() {
            connection.stop.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_initialization_and_refuses_outside_documents() {
        let dir = tempfile::tempdir().unwrap();
        let root = realpath(dir.path()).unwrap();
        std::fs::write(root.join("a.py"), "").unwrap();
        let initialized = prepare(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "processId": 7, "capabilities": {} } }), &root).unwrap();
        assert_eq!(initialized["params"]["processId"], Value::Null);
        assert_eq!(initialized["params"]["rootUri"], file_uri(&root));
        let inside = json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": { "textDocument": { "uri": file_uri(&root.join("a.py")), "text": "" } } });
        assert!(prepare(inside, &root).is_ok());
        let outside = json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": { "textDocument": { "uri": file_uri(&root.parent().unwrap().join("x.py")) } } });
        assert!(prepare(outside, &root).is_err());
        assert!(prepare(json!({ "jsonrpc": "1.0", "method": "x" }), &root).is_err());
        assert!(prepare(json!({ "jsonrpc": "2.0", "method": "workspace/didChangeWorkspaceFolders", "params": {} }), &root).is_err());
    }
}
