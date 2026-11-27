#![allow(dead_code)]
//! Shared WebSocket, JSON-RPC session, and `rainy-host` process harness for integration tests.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use axum::http::{StatusCode, header};
use base64::Engine as _;
use rainy_protocol::frames::ServerFrame;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// Minimal RFC 6455 test client for authenticating and speaking JSON-RPC to `rainy-host`.
pub struct WsClient {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl WsClient {
    /// Attempt a raw HTTP WebSocket upgrade; returns `(status_code, Option<WsClient>)`.
    pub async fn handshake(port: u16, path: &str, cookie: Option<&str>, origin: Option<&str>) -> (u16, Option<Self>) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.expect("connect host");
        let mut key_bytes = [0u8; 16];
        getrandom::fill(&mut key_bytes).expect("random");
        let ws_key = base64::engine::general_purpose::STANDARD.encode(key_bytes);
        let mut req = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {ws_key}\r\nSec-WebSocket-Version: 13\r\n"
        );
        if let Some(cookie) = cookie {
            req.push_str(&format!("Cookie: {cookie}\r\n"));
        }
        if let Some(origin) = origin {
            req.push_str(&format!("Origin: {origin}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await.expect("write upgrade");

        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut tmp).await.expect("read upgrade response");
            assert!(n > 0, "EOF during WebSocket handshake");
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let header_text = String::from_utf8_lossy(&buf[..header_end]);
        let code: u16 = header_text
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if code != 101 {
            return (code, None);
        }
        let leftover = buf[header_end..].to_vec();
        (code, Some(Self { stream, buf: leftover }))
    }

    pub async fn connect(port: u16, cookie: &str) -> Self {
        let origin = format!("http://127.0.0.1:{port}");
        let (code, client) = Self::handshake(port, "/rpc", Some(cookie), Some(&origin)).await;
        assert_eq!(code, 101, "expected 101 upgrade");
        client.expect("upgraded ws client")
    }

    async fn send_frame(&mut self, opcode: u8, payload: &[u8]) {
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode);
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | (len as u8));
        } else if len <= u16::MAX as usize {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mut mask = [0u8; 4];
        getrandom::fill(&mut mask).expect("mask");
        frame.extend_from_slice(&mask);
        for (i, &b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i % 4]);
        }
        self.stream.write_all(&frame).await.expect("write ws frame");
    }

    pub async fn send_text(&mut self, text: &str) {
        self.send_frame(0x1, text.as_bytes()).await;
    }

    async fn read_exact_buffered(&mut self, n: usize) -> Vec<u8> {
        let mut tmp = [0u8; 8192];
        while self.buf.len() < n {
            let got = self.stream.read(&mut tmp).await.expect("read ws bytes");
            assert!(got > 0, "unexpected WebSocket EOF");
            self.buf.extend_from_slice(&tmp[..got]);
        }
        self.buf.drain(..n).collect()
    }

    pub async fn recv_text(&mut self) -> Option<String> {
        let mut message = Vec::new();
        loop {
            let head = self.read_exact_buffered(2).await;
            let fin = (head[0] & 0x80) != 0;
            let opcode = head[0] & 0x0f;
            let masked = (head[1] & 0x80) != 0;
            let mut len = (head[1] & 0x7f) as usize;
            if len == 126 {
                let ext = self.read_exact_buffered(2).await;
                len = u16::from_be_bytes([ext[0], ext[1]]) as usize;
            } else if len == 127 {
                let ext = self.read_exact_buffered(8).await;
                len = u64::from_be_bytes(ext.try_into().unwrap()) as usize;
            }
            let mask = if masked {
                let m = self.read_exact_buffered(4).await;
                Some([m[0], m[1], m[2], m[3]])
            } else {
                None
            };
            let mut payload = self.read_exact_buffered(len).await;
            if let Some(m) = mask {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= m[i % 4];
                }
            }
            match opcode {
                0x8 => return None,
                0x9 => self.send_frame(0xA, &payload).await,
                0xA => {}
                0x1 | 0x0 => {
                    message.extend_from_slice(&payload);
                    if fin {
                        return Some(String::from_utf8(message).expect("utf8 ws text"));
                    }
                }
                _ => {}
            }
        }
    }
}

pub struct RpcSession {
    ws: WsClient,
    next_id: u64,
    events: Vec<(String, Value)>,
}

impl RpcSession {
    pub fn new(ws: WsClient) -> Self {
        Self { ws, next_id: 1, events: Vec::new() }
    }

    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, rainy_protocol::RpcError> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = json!({ "id": id, "method": method, "params": params });
        self.ws.send_text(&frame.to_string()).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let text = tokio::time::timeout(remaining, self.ws.recv_text())
                .await
                .expect("rpc call timed out")
                .expect("rpc socket closed");
            match ServerFrame::parse(&text) {
                Some(ServerFrame::Response { id: resp_id, result }) if resp_id == id => return result,
                Some(ServerFrame::Event { event, data }) => self.events.push((event, data)),
                _ => {}
            }
        }
    }

    pub async fn next_event(&mut self, timeout: Duration) -> (String, Value) {
        if !self.events.is_empty() {
            return self.events.remove(0);
        }
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let text = tokio::time::timeout(remaining, self.ws.recv_text())
                .await
                .expect("event timed out")
                .expect("rpc socket closed");
            if let Some(ServerFrame::Event { event, data }) = ServerFrame::parse(&text) {
                return (event, data);
            }
        }
    }
}

pub struct HostProcess {
    child: Child,
    pub port: u16,
    pub launch_url: String,
    _home: tempfile::TempDir,
}

impl HostProcess {
    pub fn spawn() -> Self {
        Self::spawn_with_user_home(None)
    }

    /// A Host whose user folder (`USERPROFILE`, `HOME`) is `user_home`.
    pub fn spawn_with_user_home(user_home: Option<&Path>) -> Self {
        let home = tempfile::tempdir().expect("temp home");
        let app_root = home.path().join("app");
        std::fs::create_dir_all(app_root.join("resources")).expect("create resources");
        std::fs::create_dir_all(app_root.join("dist").join("renderer")).expect("create renderer");
        std::fs::write(app_root.join("dist").join("renderer").join("index.html"), "<!doctype html><head></head><body>ok</body>").expect("write index");
        let bin = env!("CARGO_BIN_EXE_rainy-host");
        let mut command = Command::new(bin);
        if let Some(user_home) = user_home {
            command.env("USERPROFILE", user_home).env("HOME", user_home);
        }
        let mut child = command
            .env("RAINY_HOME", home.path())
            .env("RAINY_APP_ROOT", &app_root)
            .env("RAINY_PORT", "0")
            .env_remove("RAINY_CARRIER_CREDENTIALS")
            .env_remove("RAINY_CARRIER_PROXY")
            .env_remove("RAINY_CARRIER_PREVIEW")
            .env_remove("RAINY_CONFIGURE_DEEPSEEK")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn rainy-host");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let ready = loop {
            line.clear();
            let n = reader.read_line(&mut line).expect("read control line");
            assert!(n > 0, "rainy-host exited before printing ready");
            if let Some(rest) = line.trim().strip_prefix("RAINY_CONTROL ") {
                let msg: Value = serde_json::from_str(rest).expect("valid control json");
                if msg["type"] == "ready" {
                    break msg;
                }
                if msg["type"] == "fatal" {
                    panic!("rainy-host fatal startup error: {}", msg["message"]);
                }
            }
        };
        let launch_url = ready["url"].as_str().expect("ready url").to_owned();
        let parsed = url::Url::parse(&launch_url).expect("parse ready url");
        let port = parsed.port().expect("ready port");
        // Drain remaining stdout so the host never blocks on control writes.
        std::thread::spawn(move || {
            let mut sink = String::new();
            while reader.read_line(&mut sink).is_ok_and(|n| n > 0) {
                sink.clear();
            }
        });
        Self { child, port, launch_url, _home: home }
    }

    /// Exchange the launch token for the `rainy_auth` HttpOnly cookie value (`rainy_auth=...`).
    pub async fn auth_cookie(&self) -> String {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap();
        let resp = client.get(&self.launch_url).send().await.expect("launch url");
        assert_eq!(resp.status(), StatusCode::FOUND);
        let set_cookie = resp.headers().get(header::SET_COOKIE).expect("set-cookie").to_str().unwrap().to_owned();
        assert!(set_cookie.contains("HttpOnly"), "cookie must be HttpOnly: {set_cookie}");
        assert!(set_cookie.contains("SameSite=Strict"), "cookie must be SameSite=Strict: {set_cookie}");
        set_cookie.split(';').next().unwrap().trim().to_owned()
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        if let Some(mut stdin) = self.child.stdin.take() {
            let _ = writeln!(stdin, r#"{{"type":"stop"}}"#);
            let _ = stdin.flush();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
