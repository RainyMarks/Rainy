//! A language server reached through the Host's `/rainy/ide/lsp` WebSocket (`src/editor/lsp.ts`): one JSON-RPC
//! message per text frame. The Host starts the server for the project folder and checks every file URI it carries.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow};
use async_tungstenite::tungstenite::Message;
use async_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use futures::channel::oneshot;
use futures::{FutureExt as _, StreamExt as _};
use serde_json::{Value, json};

use crate::locale::localized;
use crate::rpc::RpcClient;

/// How long a request may take before it is abandoned; the first answer of pyright or clangd on a cold project can be
/// slow (3.x `REQUEST_TIMEOUT_MS`).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The close code with which the Host asks for a new connection: it stopped the server to start it in a changed
/// environment (`IDE_LANGUAGE_RESTART_CODE`).
pub const RESTART_CODE: u16 = 1012;

/// What the server sends on its own.
#[derive(Debug, Clone, PartialEq)]
pub enum LspIncoming {
    Notification { method: String, params: Value },
    /// The connection ended; `restart` when the Host asks for a new one.
    Closed { reason: String, restart: bool },
}

/// The answer to a request the server makes of the client, or the message of a method the client does not implement.
pub type Answer = Box<dyn Fn(&str, &Value) -> Result<Value, String> + Send + Sync>;

type Pending = oneshot::Sender<Result<Value, String>>;

/// Why a connection ended when the Host gave no reason; the status bar shows it.
fn closed() -> String {
    localized("语言服务连接已断开。", "Language service connection closed.")
}

struct Inner {
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, Pending>>,
    outgoing: async_channel::Sender<String>,
    closed: AtomicBool,
}

/// One language-server connection; cheap to clone.
#[derive(Clone)]
pub struct LspClient {
    inner: Arc<Inner>,
}

/// The `/rainy/ide/lsp` route of a project root and server language.
pub fn route(workspace_id: &str, language: &str, root_id: Option<&str>) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("workspaceId", workspace_id).append_pair("language", language);
    if let Some(root_id) = root_id.filter(|root| !root.is_empty()) {
        query.append_pair("rootId", root_id);
    }
    format!("/rainy/ide/lsp?{}", query.finish())
}

impl LspClient {
    /// Open the connection on its own thread; notifications and the end of the connection arrive on the channel.
    pub fn connect(rpc: &RpcClient, route: String, answer: Answer) -> (Self, async_channel::Receiver<LspIncoming>) {
        let (outgoing, queue) = async_channel::unbounded();
        let (events, receiver) = async_channel::unbounded();
        let inner = Arc::new(Inner { next_id: AtomicI64::new(1), pending: Mutex::default(), outgoing, closed: AtomicBool::new(false) });
        let worker = inner.clone();
        let rpc = rpc.clone();
        let spawned = thread::Builder::new().name("rainy-lsp".into()).spawn(move || {
            smol::block_on(async move {
                let (reason, restart) = match rpc.socket(&route).await {
                    Ok(socket) => serve(&worker, socket, &queue, &events, &answer).await,
                    Err(error) => (format!("{error:#}"), false),
                };
                worker.closed.store(true, Ordering::SeqCst);
                worker.outgoing.close();
                for (_, pending) in worker.pending.lock().unwrap().drain() {
                    let _ = pending.send(Err(reason.clone()));
                }
                let _ = events.send(LspIncoming::Closed { reason, restart }).await;
            })
        });
        if let Err(error) = spawned {
            log::warn!("The language service thread did not start: {error}");
            inner.closed.store(true, Ordering::SeqCst);
            inner.outgoing.close();
        }
        (Self { inner }, receiver)
    }

    /// Send a request; resolves with its result, or the error message, after at most [`REQUEST_TIMEOUT`].
    pub fn request(&self, method: &str, params: Value) -> impl Future<Output = Result<Value>> + Send + 'static + use<> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);
        let sent = self.inner.outgoing.try_send(frame).is_ok();
        let inner = self.inner.clone();
        let method = method.to_owned();
        async move {
            if !sent {
                inner.pending.lock().unwrap().remove(&id);
                return Err(anyhow!(closed()));
            }
            let timeout = futures::FutureExt::fuse(smol::Timer::after(REQUEST_TIMEOUT));
            let answer = rx.fuse();
            futures::pin_mut!(timeout, answer);
            futures::select! {
                result = answer => match result {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(message)) => Err(anyhow!(message)),
                    Err(_) => Err(anyhow!(closed())),
                },
                _ = timeout => {
                    inner.pending.lock().unwrap().remove(&id);
                    // The server may still be working on it; tell it the answer is no longer wanted.
                    let cancel = json!({ "jsonrpc": "2.0", "method": "$/cancelRequest", "params": { "id": id } }).to_string();
                    let _ = inner.outgoing.try_send(cancel);
                    Err(anyhow!(localized(&format!("{method} 请求超时。"), &format!("{method} timed out."))))
                }
            }
        }
    }

    /// Send a notification; dropped when the connection has ended.
    pub fn notify(&self, method: &str, params: Value) {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string();
        let _ = self.inner.outgoing.try_send(frame);
    }

    /// Close the connection; the Host stops the server.
    pub fn close(&self) {
        self.inner.outgoing.close();
    }
}

/// Carry frames until the socket or the queue closes; returns why the connection ended.
async fn serve(
    inner: &Inner,
    socket: crate::rpc::Socket,
    queue: &async_channel::Receiver<String>,
    events: &async_channel::Sender<LspIncoming>,
    answer: &Answer,
) -> (String, bool) {
    let (mut sink, mut stream) = socket.split();
    loop {
        futures::select_biased! {
            incoming = stream.next().fuse() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if let Some(reply) = dispatch(inner, events, answer, text.as_str()).await
                        && sink.send(Message::text(reply)).await.is_err()
                    {
                        return (closed(), false);
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    let (reason, restart) = frame.map_or((String::new(), false), |frame| {
                        (frame.reason.to_string(), frame.code == CloseCode::from(RESTART_CODE))
                    });
                    let reason = if reason.is_empty() { closed() } else { reason };
                    return (reason, restart);
                }
                None | Some(Err(_)) => return (closed(), false),
                Some(Ok(_)) => {}
            },
            outgoing = queue.recv().fuse() => match outgoing {
                Ok(frame) => {
                    if sink.send(Message::text(frame)).await.is_err() {
                        return (closed(), false);
                    }
                }
                Err(_) => {
                    let _ = sink.close(None).await;
                    return (closed(), false);
                }
            },
        }
    }
}

/// Handle one message from the server; returns the reply to a server request.
async fn dispatch(inner: &Inner, events: &async_channel::Sender<LspIncoming>, answer: &Answer, text: &str) -> Option<String> {
    // The Host only relays JSON; anything else is dropped.
    let message: Value = serde_json::from_str(text).ok()?;
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").filter(|id| !id.is_null());
    match (method, id) {
        (Some(method), Some(id)) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let reply = match answer(method, &params) {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err(message) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": message } }),
            };
            Some(reply.to_string())
        }
        (Some(method), None) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let _ = events.send(LspIncoming::Notification { method: method.to_owned(), params }).await;
            None
        }
        (None, Some(id)) => {
            let id = id.as_i64()?;
            let pending = inner.pending.lock().unwrap().remove(&id)?;
            let result = match message.get("error") {
                Some(error) => Err(error.get("message").and_then(Value::as_str).map_or_else(|| localized("语言服务器出错。", "Language server error"), str::to_owned)),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = pending.send(result);
            None
        }
        (None, None) => None,
    }
}

/// The file URI of a Host path (`directoryUri` in `src/editor/uri.ts`): `C:\a b` → `file:///C:/a%20b`, `/home/x` →
/// `file:///home/x`, `\\server\share` → `file://server/share`.
pub fn file_uri(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized.strip_suffix('/').filter(|rest| !rest.is_empty()).unwrap_or(&normalized);
    let bytes = normalized.as_bytes();
    // A drive root keeps its slash: `C:\` is `file:///C:/`.
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return format!("file:///{normalized}/");
    }
    let drive = bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1..3) == Some(b":/");
    let prefix = if drive {
        "file:///"
    } else if normalized.starts_with("//") {
        "file:"
    } else {
        "file://"
    };
    let parts: Vec<String> = normalized
        .split('/')
        .enumerate()
        .map(|(index, part)| if drive && index == 0 { part.to_owned() } else { encode_component(part) })
        .collect();
    format!("{prefix}{}", parts.join("/"))
}

/// `encodeURIComponent`.
fn encode_component(part: &str) -> String {
    let mut out = String::new();
    for byte in part.bytes() {
        let keep = byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte);
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The comparison key of a file URI (`uriKey`): servers may encode a drive colon (`c%3A`) or change its case, and Windows
/// paths compare case-insensitively. Other URIs are returned unchanged.
pub fn uri_key(uri: &str) -> String {
    let Ok(url) = url::Url::parse(uri) else { return uri.to_owned() };
    if url.scheme() != "file" {
        return uri.to_owned();
    }
    let raw = url.path();
    let path = percent_decode(raw).unwrap_or_else(|| raw.to_owned());
    let host = url.host_str().unwrap_or_default().to_lowercase();
    let bytes = path.as_bytes();
    let windows = (bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':') || !host.is_empty();
    format!("file://{host}{}", if windows { path.to_lowercase() } else { path })
}

/// The Host path of a file URI: `file:///C:/a%20b` → `C:/a b`, `file:///home/x` → `/home/x`.
pub fn uri_path(uri: &str) -> Option<String> {
    let url = url::Url::parse(uri).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    let path = percent_decode(url.path())?;
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    Some(match url.host_str().filter(|host| !host.is_empty()) {
        Some(host) => format!("//{host}{path}"),
        None if drive => path[1..].to_owned(),
        None => path,
    })
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spells_file_uris_like_the_editor() {
        assert_eq!(file_uri("C:\\Users\\a b\\x.py"), "file:///C:/Users/a%20b/x.py");
        assert_eq!(file_uri("/home/rog/项目/x.py"), "file:///home/rog/%E9%A1%B9%E7%9B%AE/x.py");
        assert_eq!(file_uri("C:\\proj\\"), "file:///C:/proj");
        assert_eq!(file_uri("\\\\server\\share\\x"), "file://server/share/x");
        assert_eq!(file_uri("D:\\"), "file:///D:/");
        // Relative or odd paths are encoded, never cut inside a character.
        assert_eq!(file_uri("a中.py"), "file://a%E4%B8%AD.py");
        assert_eq!(file_uri("C"), "file://C");
    }

    #[test]
    fn compares_uris_across_spellings() {
        assert_eq!(uri_key("file:///c%3A/Users/A.py"), uri_key("file:///C:/users/a.py"));
        assert_ne!(uri_key("file:///home/A.py"), uri_key("file:///home/a.py"));
        assert_eq!(uri_key("untitled:1"), "untitled:1");
    }

    #[test]
    fn reads_host_paths_back() {
        assert_eq!(uri_path("file:///C:/a%20b/x.py").as_deref(), Some("C:/a b/x.py"));
        assert_eq!(uri_path("file:///c%3A/x.py").as_deref(), Some("c:/x.py"));
        assert_eq!(uri_path("file:///home/rog/x.py").as_deref(), Some("/home/rog/x.py"));
        assert_eq!(uri_path("https://example.com"), None);
    }

    #[test]
    fn routes_carry_the_root_only_when_named() {
        assert_eq!(route("w 1", "python", None), "/rainy/ide/lsp?workspaceId=w+1&language=python");
        assert_eq!(route("w", "cpp", Some("r2")), "/rainy/ide/lsp?workspaceId=w&language=cpp&rootId=r2");
    }
}
