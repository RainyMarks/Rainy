//! Client of the Host's `/rpc` WebSocket (`src/renderer/rpc.ts`).
//!
//! The launch URL is opened once to exchange its token for the HttpOnly session cookie; the socket then carries JSON
//! requests, answers and broadcast events. A lost connection is opened again with a growing delay (250 ms up to 5 s):
//! requests made while it is down wait and are sent once it is open, and requests sent on the lost connection fail.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use async_tungstenite::tungstenite::client::IntoClientRequest as _;
use async_tungstenite::tungstenite::http::HeaderValue;
use async_tungstenite::tungstenite::protocol::WebSocketConfig;
use async_tungstenite::tungstenite::Message;
use futures::channel::oneshot;
use futures::{FutureExt as _, StreamExt as _};
use rainy_protocol::{RequestFrame, RpcError, ServerFrame};
use serde::de::DeserializeOwned;
use serde_json::Value;
use smol::io::{AsyncReadExt as _, AsyncWriteExt as _};
use smol::net::TcpStream;

use crate::control::HostReady;

/// Whether the interface is connected to the Host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Open,
    Closed,
}

/// What the client reports to the interface.
#[derive(Debug, Clone, PartialEq)]
pub enum RpcEvent {
    State(ConnectionState),
    Event { name: String, data: Value },
}

type Answer = oneshot::Sender<Result<Value, RpcError>>;

struct Inner {
    port: u16,
    launch_path: String,
    cookie: Mutex<Option<String>>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Answer>>,
    outgoing: async_channel::Sender<(u64, String)>,
    stopped: AtomicBool,
}

/// The interface's single Host connection; cheap to clone.
#[derive(Clone)]
pub struct RpcClient {
    inner: Arc<Inner>,
}

fn disconnected() -> RpcError {
    RpcError { code: "disconnected".into(), message: "The connection to Rainy was lost, so this action did not finish.".into(), data: None }
}

impl RpcClient {
    /// Connect to a ready Host; events and connection changes arrive on the returned channel.
    pub fn connect(ready: &HostReady) -> Result<(Self, async_channel::Receiver<RpcEvent>)> {
        let launch_path = ready.url.strip_prefix(&format!("http://127.0.0.1:{}", ready.port)).unwrap_or("/").to_owned();
        let (outgoing, queue) = async_channel::unbounded();
        let (events, receiver) = async_channel::unbounded();
        let inner = Arc::new(Inner {
            port: ready.port,
            launch_path: if launch_path.is_empty() { "/".into() } else { launch_path },
            cookie: Mutex::new(None),
            next_id: AtomicU64::new(1),
            pending: Mutex::default(),
            outgoing,
            stopped: AtomicBool::new(false),
        });
        let worker = inner.clone();
        thread::Builder::new().name("rainy-rpc".into()).spawn(move || smol::block_on(run(worker, queue, events)))?;
        Ok((Self { inner }, receiver))
    }

    /// Call a Host method; resolves with its result or a [`RpcError`].
    pub fn call(&self, method: &str, params: Value) -> impl Future<Output = Result<Value, RpcError>> + Send + 'static + use<> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = serde_json::to_string(&RequestFrame { id, method, params: &params }).expect("serializable frame");
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);
        let sent = self.inner.outgoing.try_send((id, frame));
        async move {
            if sent.is_err() {
                return Err(disconnected());
            }
            rx.await.unwrap_or_else(|_| Err(disconnected()))
        }
    }

    /// Call a Host method and decode its result.
    pub fn request<T: DeserializeOwned + Send + 'static>(
        &self,
        method: &str,
        params: Value,
    ) -> impl Future<Output = Result<T, RpcError>> + Send + 'static + use<T> {
        let call = self.call(method, params);
        async move {
            let value = call.await?;
            serde_json::from_value(value).map_err(|error| RpcError { code: "invalid".into(), message: error.to_string(), data: None })
        }
    }

    /// GET a Host route with the session cookie, such as `/rainy/ide/raw?…`; returns the status and body.
    pub async fn get(&self, path: &str) -> Result<(u16, Vec<u8>)> {
        let cookie = self.inner.cookie.lock().unwrap().clone().ok_or_else(|| anyhow!("Not connected to the Host."))?;
        let response = http_get(self.inner.port, path, Some(&cookie)).await?;
        Ok((response.status, response.body))
    }

    /// Stop reconnecting; pending requests fail.
    pub fn close(&self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        self.inner.outgoing.close();
    }
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// A minimal HTTP/1.1 GET over loopback; the Host answers with `Connection: close` semantics when asked.
async fn http_get(port: u16, path: &str, cookie: Option<&str>) -> Result<Response> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if let Some(cookie) = cookie {
        request.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| anyhow!("Malformed HTTP response"))?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines.next().and_then(|line| line.split_whitespace().nth(1)).and_then(|code| code.parse().ok()).context("HTTP status")?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':').map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned())))
        .collect();
    let mut body = raw[split + 4..].to_vec();
    if headers.iter().any(|(name, value)| name == "transfer-encoding" && value.eq_ignore_ascii_case("chunked")) {
        body = dechunk(&body)?;
    }
    Ok(Response { status, headers, body })
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = data.windows(2).position(|w| w == b"\r\n").ok_or_else(|| anyhow!("Malformed chunk"))?;
        let size_text = std::str::from_utf8(&data[..end])?.split(';').next().unwrap_or("0").trim();
        let size = usize::from_str_radix(size_text, 16)?;
        data = &data[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size + 2 {
            bail!("Truncated chunk");
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

/// Exchange the launch token for the session cookie (`name=value`).
async fn session_cookie(inner: &Inner) -> Result<String> {
    let response = http_get(inner.port, &inner.launch_path, None).await?;
    if response.status != 302 {
        bail!("The Host refused the launch token ({}).", response.status);
    }
    response
        .headers
        .iter()
        .find(|(name, _)| name == "set-cookie")
        .and_then(|(_, value)| value.split(';').next())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("The Host did not set a session cookie."))
}

type Socket = async_tungstenite::WebSocketStream<TcpStream>;

async fn open_socket(inner: &Inner) -> Result<Socket> {
    let cookie = {
        let known = inner.cookie.lock().unwrap().clone();
        match known {
            Some(cookie) => cookie,
            None => {
                let cookie = session_cookie(inner).await?;
                *inner.cookie.lock().unwrap() = Some(cookie.clone());
                cookie
            }
        }
    };
    let origin = format!("http://127.0.0.1:{}", inner.port);
    let mut request = format!("ws://127.0.0.1:{}/rpc", inner.port).into_client_request()?;
    request.headers_mut().insert("Cookie", HeaderValue::from_str(&cookie)?);
    request.headers_mut().insert("Origin", HeaderValue::from_str(&origin)?);
    let stream = TcpStream::connect(("127.0.0.1", inner.port)).await?;
    stream.set_nodelay(true)?;
    // Snapshots of long chats with images arrive as one message; allow them.
    let config = WebSocketConfig::default().max_message_size(Some(1 << 30)).max_frame_size(Some(1 << 30));
    let (socket, _) = async_tungstenite::client_async_with_config(request, stream, Some(config)).await?;
    Ok(socket)
}

async fn run(inner: Arc<Inner>, queue: async_channel::Receiver<(u64, String)>, events: async_channel::Sender<RpcEvent>) {
    let mut delay = Duration::from_millis(250);
    // A frame taken from the queue while the socket was failing; sent first on the next connection.
    let mut held: Option<(u64, String)> = None;
    while !inner.stopped.load(Ordering::SeqCst) {
        let _ = events.send(RpcEvent::State(ConnectionState::Connecting)).await;
        match open_socket(&inner).await {
            Ok(socket) => {
                delay = Duration::from_millis(250);
                let _ = events.send(RpcEvent::State(ConnectionState::Open)).await;
                held = serve(&inner, socket, &queue, &events, held.take()).await;
            }
            Err(error) => {
                log::warn!("Host connection failed: {error:#}");
                // A rejected cookie (the Host restarted on the same port) is fetched again.
                *inner.cookie.lock().unwrap() = None;
            }
        }
        let _ = events.send(RpcEvent::State(ConnectionState::Closed)).await;
        if queue.is_closed() && queue.is_empty() {
            break;
        }
        smol::Timer::after(delay).await;
        delay = (delay * 2).min(Duration::from_secs(5));
    }
    for (_, answer) in inner.pending.lock().unwrap().drain() {
        let _ = answer.send(Err(disconnected()));
    }
}

/// Carry frames until the socket closes; returns a frame taken from the queue but not sent.
async fn serve(
    inner: &Inner,
    socket: Socket,
    queue: &async_channel::Receiver<(u64, String)>,
    events: &async_channel::Sender<RpcEvent>,
    held: Option<(u64, String)>,
) -> Option<(u64, String)> {
    let (mut sink, mut stream) = socket.split();
    let mut sent: HashSet<u64> = HashSet::new();
    let mut unsent = None;
    if let Some((id, frame)) = held {
        if sink.send(Message::text(frame.clone())).await.is_err() {
            return Some((id, frame));
        }
        sent.insert(id);
    }
    loop {
        futures::select_biased! {
            incoming = stream.next().fuse() => match incoming {
                Some(Ok(Message::Text(text))) => dispatch(inner, events, &mut sent, text.as_str()).await,
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            outgoing = queue.recv().fuse() => match outgoing {
                Ok((id, frame)) => {
                    // A request whose caller gave up still goes out; its answer is dropped.
                    if sink.send(Message::text(frame.clone())).await.is_err() {
                        unsent = Some((id, frame));
                        break;
                    }
                    sent.insert(id);
                }
                Err(_) => {
                    let _ = sink.close(None).await;
                    break;
                }
            },
        }
    }
    // Requests sent on the lost connection never get answers.
    let mut pending = inner.pending.lock().unwrap();
    for id in sent {
        if let Some(answer) = pending.remove(&id) {
            let _ = answer.send(Err(disconnected()));
        }
    }
    unsent
}

async fn dispatch(inner: &Inner, events: &async_channel::Sender<RpcEvent>, sent: &mut HashSet<u64>, text: &str) {
    match ServerFrame::parse(text) {
        Some(ServerFrame::Event { event, data }) => {
            let _ = events.send(RpcEvent::Event { name: event, data }).await;
        }
        Some(ServerFrame::Response { id, result }) => {
            sent.remove(&id);
            if let Some(answer) = inner.pending.lock().unwrap().remove(&id) {
                let _ = answer.send(result);
            }
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_chunked_bodies() {
        assert_eq!(dechunk(b"3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n").unwrap(), b"abcde");
        assert!(dechunk(b"5\r\nab").is_err());
    }
}
