//! Client of the Host's `/rpc` WebSocket (`src/renderer/rpc.ts`).
//!
//! The launch URL is opened once to exchange its token for the HttpOnly session cookie; the socket then carries JSON
//! requests, answers and broadcast events. A lost connection is opened again with a growing delay (250 ms up to 5 s):
//! requests made while it is down wait and are sent once it is open, and requests sent on the lost connection fail.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use async_tungstenite::tungstenite::Message;
use async_tungstenite::tungstenite::client::IntoClientRequest as _;
use async_tungstenite::tungstenite::http::HeaderValue;
use async_tungstenite::tungstenite::protocol::WebSocketConfig;
use futures::channel::oneshot;
use futures::{FutureExt as _, StreamExt as _};
use rainy_protocol::{RequestFrame, RpcError, ServerFrame};
use serde::de::DeserializeOwned;
use serde_json::Value;
use smol::io::{AsyncReadExt as _, AsyncWriteExt as _};
use smol::net::TcpStream;

use crate::control::HostReady;
use crate::fs_util;
use crate::http;
use crate::locale::localized;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_IO_TIMEOUT: Duration = Duration::from_secs(120);
const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const WS_SEND_TIMEOUT: Duration = Duration::from_secs(15);
const WS_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const WS_PING_INTERVAL: Duration = Duration::from_secs(15);
const WS_IDLE_DEADLINE: Duration = Duration::from_secs(45);

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
type CookieWaiters = Mutex<Option<Vec<oneshot::Sender<Result<String, String>>>>>;

struct Inner {
    port: u16,
    launch_path: String,
    cookie: Mutex<Option<String>>,
    /// Coalesces concurrent cookie exchanges so only one request consumes the launch token at a time.
    cookie_waiters: CookieWaiters,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Answer>>,
    outgoing: async_channel::Sender<(u64, String)>,
    stop: async_channel::Sender<()>,
    stopped: AtomicBool,
}

/// The interface's single Host connection; cheap to clone.
#[derive(Clone)]
pub struct RpcClient {
    inner: Arc<Inner>,
}

fn disconnected() -> RpcError {
    let message = localized("与 Rainy 的连接已断开，这次操作没有完成。", "The connection to Rainy was lost, so this action did not finish.");
    RpcError { code: "disconnected".into(), message, data: None }
}

fn timed_out() -> anyhow::Error {
    anyhow!(localized("与执行后端的通信超时。", "Communication with the execution Host timed out."))
}

async fn with_timeout<T>(duration: Duration, future: impl Future<Output = Result<T>>) -> Result<T> {
    let timer = futures::FutureExt::fuse(smol::Timer::after(duration));
    let work = future.fuse();
    futures::pin_mut!(timer, work);
    futures::select_biased! {
        result = work => result,
        _ = timer => Err(timed_out()),
    }
}

async fn connect_loopback(port: u16) -> Result<TcpStream> {
    with_timeout(CONNECT_TIMEOUT, async move { Ok(TcpStream::connect(("127.0.0.1", port)).await?) }).await
}

impl RpcClient {
    /// Connect to a ready Host; events and connection changes arrive on the returned channel.
    pub fn connect(ready: &HostReady) -> Result<(Self, async_channel::Receiver<RpcEvent>)> {
        let launch_path = ready.url.strip_prefix(&format!("http://127.0.0.1:{}", ready.port)).unwrap_or("/").to_owned();
        let (outgoing, queue) = async_channel::unbounded();
        let (events, receiver) = async_channel::unbounded();
        let (stop, stop_rx) = async_channel::bounded(1);
        let inner = Arc::new(Inner {
            port: ready.port,
            launch_path: if launch_path.is_empty() { "/".into() } else { launch_path },
            cookie: Mutex::new(None),
            cookie_waiters: Mutex::new(None),
            next_id: AtomicU64::new(1),
            pending: Mutex::default(),
            outgoing,
            stop,
            stopped: AtomicBool::new(false),
        });
        let worker = inner.clone();
        thread::Builder::new().name("rainy-rpc".into()).spawn(move || smol::block_on(run(worker, queue, events, stop_rx)))?;
        Ok((Self { inner }, receiver))
    }

    /// Call a Host method; resolves with its result or a [`RpcError`].
    pub fn call(&self, method: &str, params: Value) -> impl Future<Output = Result<Value, RpcError>> + Send + 'static + use<> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = serde_json::to_string(&RequestFrame { id, method, params: &params }).expect("serializable frame");
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);
        let sent = self.inner.outgoing.try_send((id, frame));
        if sent.is_err() {
            self.inner.pending.lock().unwrap().remove(&id);
        }
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
        let cookie = self.inner.cookie.lock().unwrap().clone().ok_or_else(|| anyhow!(localized("尚未连接到执行后端。", "Not connected to the Host.")))?;
        let response = http_get(self.inner.port, path, Some(&cookie)).await?;
        Ok((response.status, response.body))
    }

    /// POST raw bytes to a Host route with the session cookie, such as `/rainy/ide/office-pdf`; returns the status and body.
    pub async fn post(&self, path: &str, content_type: &str, body: &[u8]) -> Result<(u16, Vec<u8>)> {
        let cookie = self.inner.cookie.lock().unwrap().clone().ok_or_else(|| anyhow!(localized("尚未连接到执行后端。", "Not connected to the Host.")))?;
        let response = http_post(self.inner.port, path, &cookie, content_type, body).await?;
        Ok((response.status, response.body))
    }

    /// Stream a file as a POST request body to a Host route with the session cookie, avoiding loading large files into RAM.
    pub async fn post_file(&self, path: &str, content_type: &str, file: &std::path::Path) -> Result<(u16, Vec<u8>)> {
        let cookie = self.inner.cookie.lock().unwrap().clone().ok_or_else(|| anyhow!(localized("尚未连接到执行后端。", "Not connected to the Host.")))?;
        let response = http_post_file(self.inner.port, path, &cookie, content_type, file).await?;
        Ok((response.status, response.body))
    }

    /// Stream a GET response from a Host route directly to a local file with bounded memory; returns the HTTP status code.
    pub async fn get_to_file(&self, path: &str, destination: &std::path::Path) -> Result<u16> {
        let cookie = self.inner.cookie.lock().unwrap().clone().ok_or_else(|| anyhow!(localized("尚未连接到执行后端。", "Not connected to the Host.")))?;
        http_get_to_file(self.inner.port, path, Some(&cookie), destination).await
    }

    /// Open another WebSocket to a Host route, such as `/rainy/ide/lsp?…`.
    pub(crate) async fn socket(&self, route: &str) -> Result<Socket> {
        open_route(&self.inner, route).await
    }

    /// Stop reconnecting; pending requests fail immediately.
    pub fn close(&self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        self.inner.outgoing.close();
        self.inner.stop.close();
    }
}

fn valid_header_token(value: &str) -> bool {
    !value.contains(['\r', '\n', '\0'])
}

/// A minimal HTTP/1.1 GET over loopback; the Host answers with `Connection: close` semantics when asked.
async fn http_get(port: u16, path: &str, cookie: Option<&str>) -> Result<http::Response> {
    if !path.starts_with('/') || !valid_header_token(path) {
        bail!(localized("无效的请求路径。", "Invalid HTTP request path."));
    }
    let mut stream = connect_loopback(port).await?;
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if let Some(cookie) = cookie {
        request.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    request.push_str("\r\n");
    with_timeout(HTTP_TIMEOUT, async {
        stream.write_all(request.as_bytes()).await?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
        http::parse(&raw, || localized("执行后端返回了无效的 HTTP 回答。", "The execution Host sent an invalid HTTP answer."))
    })
    .await
}

/// A minimal HTTP/1.1 POST over loopback carrying the session cookie and same-origin headers.
async fn http_post(port: u16, path: &str, cookie: &str, content_type: &str, body: &[u8]) -> Result<http::Response> {
    if !path.starts_with('/') || !valid_header_token(path) || !valid_header_token(content_type) {
        bail!(localized("无效的 HTTP 请求参数。", "Invalid HTTP request parameters."));
    }
    let mut stream = connect_loopback(port).await?;
    let header = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nCookie: {cookie}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    with_timeout(HTTP_TIMEOUT, async {
        stream.write_all(header.as_bytes()).await?;
        stream.write_all(body).await?;
        stream.flush().await?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
        http::parse(&raw, || localized("执行后端返回了无效的 HTTP 回答。", "The execution Host sent an invalid HTTP answer."))
    })
    .await
}

/// Stream a local file into an HTTP/1.1 POST over loopback in 256 KiB chunks.
async fn http_post_file(port: u16, path: &str, cookie: &str, content_type: &str, file: &std::path::Path) -> Result<http::Response> {
    use std::io::Read as _;
    if !path.starts_with('/') || !valid_header_token(path) || !valid_header_token(content_type) {
        bail!(localized("无效的 HTTP 请求参数。", "Invalid HTTP request parameters."));
    }
    let metadata = std::fs::symlink_metadata(file)?;
    if !metadata.is_file() {
        bail!(localized("无法上传非常规文件。", "Cannot upload a non-regular file."));
    }
    let total = metadata.len();
    let mut reader = std::fs::File::open(file)?;
    let mut stream = connect_loopback(port).await?;
    let header = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nCookie: {cookie}\r\nContent-Type: {content_type}\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
    );
    with_timeout(STREAM_IO_TIMEOUT, async {
        stream.write_all(header.as_bytes()).await?;
        Ok(())
    })
    .await?;
    let mut buffer = vec![0u8; 256 * 1024];
    let mut remaining = total;
    while remaining > 0 {
        let want = (buffer.len() as u64).min(remaining) as usize;
        let read = reader.read(&mut buffer[..want])?;
        if read == 0 {
            bail!(localized("上传过程中文件被截断。", "The file was truncated while uploading."));
        }
        with_timeout(STREAM_IO_TIMEOUT, async {
            stream.write_all(&buffer[..read]).await?;
            Ok(())
        })
        .await?;
        remaining -= read as u64;
    }
    with_timeout(STREAM_IO_TIMEOUT, async {
        stream.flush().await?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
        http::parse(&raw, || localized("执行后端返回了无效的 HTTP 回答。", "The execution Host sent an invalid HTTP answer."))
    })
    .await
}

/// The longest chunk-size line (with its extensions) a streamed answer may send.
const MAX_CHUNK_LINE: usize = 4096;

/// Stream an HTTP/1.1 GET response directly to `destination` without buffering the whole body in memory.
/// Only 2xx responses are atomically written to `destination` via a sibling temporary file.
async fn http_get_to_file(port: u16, path: &str, cookie: Option<&str>, destination: &std::path::Path) -> Result<u16> {
    use std::io::Write as _;
    if !path.starts_with('/') || !valid_header_token(path) {
        bail!(localized("无效的请求路径。", "Invalid HTTP request path."));
    }
    let mut stream = connect_loopback(port).await?;
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if let Some(cookie) = cookie {
        request.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    request.push_str("\r\n");
    with_timeout(HTTP_TIMEOUT, async {
        stream.write_all(request.as_bytes()).await?;
        stream.flush().await?;
        Ok(())
    })
    .await?;

    let invalid = || localized("执行后端返回了无效的 HTTP 回答。", "The execution Host sent an invalid HTTP answer.");
    let mut head_buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let split = loop {
        if let Some(pos) = head_buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos;
        }
        if head_buf.len() > 64 * 1024 {
            bail!(invalid());
        }
        let n = with_timeout(HTTP_TIMEOUT, async { Ok(stream.read(&mut chunk).await?) }).await?;
        if n == 0 {
            bail!(invalid());
        }
        head_buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&head_buf[..split]);
    let mut lines = head.split("\r\n");
    let Some(status) = lines.next().and_then(|line| line.split_whitespace().nth(1)).and_then(|code| code.parse::<u16>().ok()) else {
        bail!(invalid());
    };
    let mut chunked = false;
    let mut content_length: Option<u64> = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            if name.eq_ignore_ascii_case("transfer-encoding") && value.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if name.eq_ignore_ascii_case("content-length") {
                let parsed = value.trim().parse::<u64>().map_err(|_| anyhow!(invalid()))?;
                if let Some(prev) = content_length
                    && prev != parsed
                {
                    bail!(invalid());
                }
                content_length = Some(parsed);
            }
        }
    }
    // Non-2xx responses must not create or overwrite the destination file.
    if !(200..300).contains(&status) {
        return Ok(status);
    }

    let parent = destination.parent().ok_or_else(|| anyhow!(localized("目标文件路径无效。", "Invalid destination file path.")))?;
    let name = destination.file_name().and_then(|n| n.to_str()).unwrap_or("download");
    let temporary = parent.join(format!(".{name}.{}.tmp", crate::text::uuid_v4_lossy()));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;

    let initial = &head_buf[split + 4..];
    let outcome: Result<()> = async {
        if chunked {
            let mut buf = initial.to_vec();
            let mut io_buf = vec![0u8; 256 * 1024];
            loop {
                while !buf.windows(2).any(|w| w == b"\r\n") {
                    if buf.len() > MAX_CHUNK_LINE {
                        bail!(invalid());
                    }
                    let n = with_timeout(STREAM_IO_TIMEOUT, async { Ok(stream.read(&mut io_buf[..MAX_CHUNK_LINE]).await?) }).await?;
                    if n == 0 {
                        bail!(invalid());
                    }
                    buf.extend_from_slice(&io_buf[..n]);
                }
                let line_end = buf.windows(2).position(|w| w == b"\r\n").unwrap();
                let size_text = std::str::from_utf8(&buf[..line_end]).map_err(|_| anyhow!(invalid()))?;
                let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| anyhow!(invalid()))?;
                buf.drain(..line_end + 2);
                if size == 0 {
                    // Require the terminating CRLF (allowing optional trailer header lines before the empty CRLF).
                    let mut trailer_bytes = 0usize;
                    loop {
                        while !buf.windows(2).any(|w| w == b"\r\n") {
                            if buf.len() > MAX_CHUNK_LINE || trailer_bytes + buf.len() > MAX_CHUNK_LINE {
                                bail!(invalid());
                            }
                            let n = with_timeout(STREAM_IO_TIMEOUT, async { Ok(stream.read(&mut io_buf[..MAX_CHUNK_LINE]).await?) }).await?;
                            if n == 0 {
                                bail!(invalid());
                            }
                            buf.extend_from_slice(&io_buf[..n]);
                        }
                        let line_end = buf.windows(2).position(|w| w == b"\r\n").unwrap();
                        trailer_bytes = trailer_bytes.saturating_add(line_end + 2);
                        if trailer_bytes > MAX_CHUNK_LINE {
                            bail!(invalid());
                        }
                        buf.drain(..line_end + 2);
                        if line_end == 0 {
                            break;
                        }
                    }
                    break;
                }
                let mut left = size;
                if !buf.is_empty() {
                    let take = left.min(buf.len());
                    file.write_all(&buf[..take])?;
                    buf.drain(..take);
                    left -= take;
                }
                while left > 0 {
                    let want = left.min(io_buf.len());
                    let n = with_timeout(STREAM_IO_TIMEOUT, async { Ok(stream.read(&mut io_buf[..want]).await?) }).await?;
                    if n == 0 {
                        bail!(invalid());
                    }
                    file.write_all(&io_buf[..n])?;
                    left -= n;
                }
                while buf.len() < 2 {
                    let want = 2 - buf.len();
                    let n = with_timeout(STREAM_IO_TIMEOUT, async { Ok(stream.read(&mut io_buf[..want]).await?) }).await?;
                    if n == 0 {
                        bail!(invalid());
                    }
                    buf.extend_from_slice(&io_buf[..n]);
                }
                if &buf[..2] != b"\r\n" {
                    bail!(invalid());
                }
                buf.drain(..2);
            }
        } else {
            let mut written = 0u64;
            if !initial.is_empty() {
                let take = content_length.map_or(initial.len(), |limit| (limit as usize).min(initial.len()));
                file.write_all(&initial[..take])?;
                written += take as u64;
            }
            let mut io_buf = vec![0u8; 256 * 1024];
            loop {
                if let Some(limit) = content_length
                    && written >= limit
                {
                    break;
                }
                let n = with_timeout(STREAM_IO_TIMEOUT, async { Ok(stream.read(&mut io_buf).await?) }).await?;
                if n == 0 {
                    break;
                }
                let take = content_length.map_or(n, |limit| (limit.saturating_sub(written) as usize).min(n));
                file.write_all(&io_buf[..take])?;
                written += take as u64;
            }
            if let Some(limit) = content_length
                && written < limit
            {
                bail!(invalid());
            }
        }
        file.flush()?;
        file.sync_all()?;
        Ok(())
    }
    .await;
    drop(file);
    match outcome {
        Ok(()) => {
            if let Err(error) = fs_util::rename_retried(&temporary, destination) {
                let _ = std::fs::remove_file(&temporary);
                return Err(error.into());
            }
            Ok(status)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}

/// Exchange the launch token for the session cookie (`name=value`).
async fn session_cookie(inner: &Inner) -> Result<String> {
    let response = http_get(inner.port, &inner.launch_path, None).await?;
    if response.status != 302 {
        let status = response.status;
        bail!(localized(&format!("执行后端拒绝了启动令牌（HTTP {status}）。"), &format!("The execution Host refused the launch token (HTTP {status}).")));
    }
    response
        .headers
        .iter()
        .find(|(name, _)| name == "set-cookie")
        .and_then(|(_, value)| value.split(';').next())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!(localized("执行后端没有设置会话 Cookie。", "The execution Host did not set a session cookie.")))
}

/// Obtain or exchange the session cookie while coalescing concurrent callers so the launch token is never raced.
async fn ensure_cookie(inner: &Inner) -> Result<String> {
    let waiter = {
        let mut waiters = inner.cookie_waiters.lock().unwrap();
        if let Some(cookie) = inner.cookie.lock().unwrap().clone() {
            return Ok(cookie);
        }
        match waiters.as_mut() {
            Some(list) => {
                let (tx, rx) = oneshot::channel();
                list.push(tx);
                Some(rx)
            }
            None => {
                *waiters = Some(Vec::new());
                None
            }
        }
    };
    if let Some(rx) = waiter {
        return rx
            .await
            .map_err(|_| anyhow!(localized("会话 Cookie 获取被中断。", "Session cookie exchange was interrupted.")))?
            .map_err(|message| anyhow!(message));
    }
    struct Guard<'a>(&'a Inner, bool);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            if !self.1 {
                // Dropped before completing (e.g. cancelled by close()): release waiters cleanly.
                let _ = self.0.cookie_waiters.lock().unwrap().take();
            }
        }
    }
    let mut guard = Guard(inner, false);
    let result = session_cookie(inner).await;
    if let Ok(cookie) = &result {
        *inner.cookie.lock().unwrap() = Some(cookie.clone());
    }
    let pending = inner.cookie_waiters.lock().unwrap().take().unwrap_or_default();
    guard.1 = true;
    match &result {
        Ok(cookie) => {
            for tx in pending {
                let _ = tx.send(Ok(cookie.clone()));
            }
        }
        Err(error) => {
            let text = format!("{error:#}");
            for tx in pending {
                let _ = tx.send(Err(text.clone()));
            }
        }
    }
    result
}

pub(crate) type Socket = async_tungstenite::WebSocketStream<TcpStream>;

async fn open_socket(inner: &Inner) -> Result<Socket> {
    open_route(inner, "/rpc").await
}

/// A WebSocket to a Host route (`/rpc`, `/rainy/ide/lsp?…`) with the session cookie.
async fn open_route(inner: &Inner, route: &str) -> Result<Socket> {
    let cookie = ensure_cookie(inner).await?;
    let origin = format!("http://127.0.0.1:{}", inner.port);
    let mut request = format!("ws://127.0.0.1:{}{route}", inner.port).into_client_request()?;
    request.headers_mut().insert("Cookie", HeaderValue::from_str(&cookie)?);
    request.headers_mut().insert("Origin", HeaderValue::from_str(&origin)?);
    let stream = connect_loopback(inner.port).await?;
    stream.set_nodelay(true)?;
    // Snapshots of long chats with images arrive as one message; allow them.
    let config = WebSocketConfig::default().max_message_size(Some(1 << 30)).max_frame_size(Some(1 << 30));
    let (socket, _) = with_timeout(WS_HANDSHAKE_TIMEOUT, async move {
        Ok(async_tungstenite::client_async_with_config(request, stream, Some(config)).await?)
    })
    .await?;
    Ok(socket)
}

async fn run(
    inner: Arc<Inner>,
    queue: async_channel::Receiver<(u64, String)>,
    events: async_channel::Sender<RpcEvent>,
    stop: async_channel::Receiver<()>,
) {
    let mut delay = Duration::from_millis(250);
    // A frame taken from the queue while the socket was failing; sent first on the next connection.
    let mut held: Option<(u64, String)> = None;
    while !inner.stopped.load(Ordering::SeqCst) {
        let _ = events.send(RpcEvent::State(ConnectionState::Connecting)).await;
        let opened = {
            let connect = open_socket(&inner).fuse();
            let stopping = stop.recv().fuse();
            futures::pin_mut!(connect, stopping);
            futures::select_biased! {
                _ = stopping => break,
                result = connect => result,
            }
        };
        match opened {
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
        if inner.stopped.load(Ordering::SeqCst) || (queue.is_closed() && queue.is_empty()) {
            break;
        }
        let timer = futures::FutureExt::fuse(smol::Timer::after(delay));
        let stopping = stop.recv().fuse();
        futures::pin_mut!(timer, stopping);
        futures::select_biased! {
            _ = stopping => break,
            _ = timer => {}
        }
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
        if with_timeout(WS_SEND_TIMEOUT, async { sink.send(Message::text(frame.clone())).await.map_err(|e| anyhow!("{e}")) })
            .await
            .is_err()
        {
            return Some((id, frame));
        }
        sent.insert(id);
    }
    let mut last_incoming = Instant::now();
    let mut next_ping = futures::FutureExt::fuse(smol::Timer::after(WS_PING_INTERVAL));
    loop {
        futures::select_biased! {
            incoming = stream.next().fuse() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    last_incoming = Instant::now();
                    dispatch(inner, events, &mut sent, text.as_str()).await;
                }
                Some(Ok(Message::Ping(payload))) => {
                    last_incoming = Instant::now();
                    if with_timeout(WS_SEND_TIMEOUT, async { sink.send(Message::Pong(payload)).await.map_err(|e| anyhow!("{e}")) })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Some(Ok(Message::Pong(_))) => {
                    last_incoming = Instant::now();
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {
                    last_incoming = Instant::now();
                }
            },
            outgoing = queue.recv().fuse() => match outgoing {
                Ok((id, frame)) => {
                    // A request whose caller gave up still goes out; its answer is dropped.
                    if with_timeout(WS_SEND_TIMEOUT, async { sink.send(Message::text(frame.clone())).await.map_err(|e| anyhow!("{e}")) })
                        .await
                        .is_err()
                    {
                        unsent = Some((id, frame));
                        break;
                    }
                    sent.insert(id);
                }
                Err(_) => {
                    let _ = with_timeout(WS_CLOSE_TIMEOUT, async { sink.close(None).await.map_err(|e| anyhow!("{e}")) }).await;
                    break;
                }
            },
            _ = next_ping => {
                if last_incoming.elapsed() >= WS_IDLE_DEADLINE {
                    log::warn!("Host WebSocket did not answer keepalive ping within {} s; reconnecting", WS_IDLE_DEADLINE.as_secs());
                    let _ = with_timeout(WS_CLOSE_TIMEOUT, async { sink.close(None).await.map_err(|e| anyhow!("{e}")) }).await;
                    break;
                }
                if with_timeout(WS_SEND_TIMEOUT, async { sink.send(Message::Ping(Vec::new().into())).await.map_err(|e| anyhow!("{e}")) })
                    .await
                    .is_err()
                {
                    break;
                }
                next_ping = futures::FutureExt::fuse(smol::Timer::after(WS_PING_INTERVAL));
            }
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
    use ::core::prelude::v1::test;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn posts_bytes_and_streams_files_with_cookie() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap() == 0 {
                        break;
                    }
                    head.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&head);
                let content_len: usize = text
                    .lines()
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse().ok()).flatten()
                    })
                    .unwrap_or(0);
                let mut body = vec![0u8; content_len];
                if content_len > 0 {
                    stream.read_exact(&mut body).unwrap();
                }
                if text.starts_with("POST /upload ") {
                    assert!(text.contains("Cookie: rainy_auth=tok\r\n"));
                    assert!(text.contains("Content-Type: application/pdf\r\n"));
                    let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    stream.write_all(reply.as_bytes()).unwrap();
                    stream.write_all(&body).unwrap();
                } else if text.starts_with("GET /chunked ") {
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n")
                        .unwrap();
                } else if text.starts_with("GET /missing ") {
                    stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 5\r\nConnection: close\r\n\r\nerror").unwrap();
                }
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("sample.pdf");
        std::fs::write(&pdf, b"%PDF-1.7\nbody").unwrap();

        smol::block_on(async {
            let resp = http_post(port, "/upload", "rainy_auth=tok", "application/pdf", b"%PDF-bytes").await.unwrap();
            assert_eq!(resp.status, 200);
            assert_eq!(resp.body, b"%PDF-bytes");

            let resp = http_post_file(port, "/upload", "rainy_auth=tok", "application/pdf", &pdf).await.unwrap();
            assert_eq!(resp.status, 200);
            assert_eq!(resp.body, b"%PDF-1.7\nbody");

            let out = dir.path().join("out.bin");
            let status = http_get_to_file(port, "/chunked", Some("rainy_auth=tok"), &out).await.unwrap();
            assert_eq!(status, 200);
            assert_eq!(std::fs::read(&out).unwrap(), b"hello world");

            // Non-2xx status leaves existing file untouched.
            let status = http_get_to_file(port, "/missing", Some("rainy_auth=tok"), &out).await.unwrap();
            assert_eq!(status, 404);
            assert_eq!(std::fs::read(&out).unwrap(), b"hello world");

            // Rejects CRLF injection in path or content-type.
            assert!(http_post(port, "/bad\r\nX: 1", "rainy_auth=tok", "application/pdf", b"").await.is_err());
            assert!(http_post(port, "/ok", "rainy_auth=tok", "text/plain\r\nX: 1", b"").await.is_err());
        });
        server.join().unwrap();
    }

    #[test]
    fn an_endless_chunk_size_line_is_refused() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request);
            socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
            // A size line that never ends, on a connection that stays open.
            for _ in 0..64 {
                if socket.write_all(&[b'f'; 4096]).is_err() {
                    return;
                }
            }
            thread::sleep(Duration::from_secs(10));
        });
        let destination = std::env::temp_dir().join(format!("rainy-chunked-{}", std::process::id()));
        let started = std::time::Instant::now();
        let result = smol::block_on(http_get_to_file(port, "/file", None, &destination));
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(5), "the answer must be refused before the server gives up");
        assert!(!destination.exists(), "failed download must not leave partial destination file");
        drop(server);
    }

    #[test]
    fn close_wakes_reconnect_backoff_and_call_cleans_pending() {
        // Closed port ensures reconnect backoff is entered quickly.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let ready = HostReady { url: format!("http://127.0.0.1:{port}/?token=x"), port, pid: 1, home: "/h".into() };
        let (client, _events) = RpcClient::connect(&ready).unwrap();
        let pending_call = client.call("ping", Value::Null);
        let started = Instant::now();
        client.close();
        let err = smol::block_on(pending_call).unwrap_err();
        assert_eq!(err.code, "disconnected");
        assert!(started.elapsed() < Duration::from_secs(2), "close() took {:?}", started.elapsed());
        // Calling after close must not leak entries in `pending`.
        let err2 = smol::block_on(client.call("ping", Value::Null)).unwrap_err();
        assert_eq!(err2.code, "disconnected");
        assert!(client.inner.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn http_get_to_file_rejects_bad_content_length_and_truncated_chunked_tail() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let responses: Vec<&'static [u8]> = vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: bad\r\nConnection: close\r\n\r\nhello",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\nX-Check: 1\r\n\r\n",
        ];
        let server = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap() == 0 {
                        break;
                    }
                    head.push(byte[0]);
                }
                stream.write_all(response).unwrap();
                stream.flush().unwrap();
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.bin");
        std::fs::write(&target, b"keep").unwrap();

        smol::block_on(async {
            // Malformed Content-Length must fail without clobbering existing destination.
            assert!(http_get_to_file(port, "/bad-len", Some("rainy_auth=tok"), &target).await.is_err());
            assert_eq!(std::fs::read(&target).unwrap(), b"keep");

            // Chunked response truncated right after `0\r\n` (missing terminating `\r\n`) must fail.
            assert!(http_get_to_file(port, "/trunc-chunk", Some("rainy_auth=tok"), &target).await.is_err());
            assert_eq!(std::fs::read(&target).unwrap(), b"keep");

            // Complete chunked response with a trailer line and terminating `\r\n` succeeds.
            assert_eq!(http_get_to_file(port, "/ok-trailer", Some("rainy_auth=tok"), &target).await.unwrap(), 200);
            assert_eq!(std::fs::read(&target).unwrap(), b"hello");
        });

        server.join().unwrap();
    }
}
