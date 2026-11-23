//! End-to-end tests driving the real `rainy-host` binary over `/rpc` with cross-platform
//! mock stdio servers speaking `codex app-server --stdio` and ACP (`agy acp`).
//!
//! Verifies: probe/model discovery, session creation without API keys, streaming text + thinking
//! patches, tool progress/results, bidirectional approval (allow / allow-session / deny),
//! thread/session resume, model & reasoning switching, external change reporting (`undoable: false`),
//! `chat.abort` interruption, and Antigravity ACP primary-agent compliance (no injected system prompt).

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use axum::http::{StatusCode, header};
use base64::Engine as _;
use rainy_protocol::chat::AssistantMessage;
use rainy_protocol::frames::ServerFrame;
use rainy_protocol::stream::{StreamPatch, apply_stream_patch};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

// ───────────────────────────── Minimal RFC 6455 test client ─────────────────────────────

struct WsClient {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl WsClient {
    async fn connect(port: u16, cookie: &str) -> Self {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.expect("connect host");
        let mut key_bytes = [0u8; 16];
        getrandom::fill(&mut key_bytes).expect("random");
        let ws_key = base64::engine::general_purpose::STANDARD.encode(key_bytes);
        let origin = format!("http://127.0.0.1:{port}");
        let req = format!(
            "GET /rpc HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {ws_key}\r\nSec-WebSocket-Version: 13\r\nCookie: {cookie}\r\nOrigin: {origin}\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await.expect("write upgrade");
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut tmp).await.expect("read upgrade");
            assert!(n > 0, "EOF during WebSocket handshake");
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let header_text = String::from_utf8_lossy(&buf[..header_end]);
        assert!(header_text.contains(" 101 "), "expected 101 upgrade, got: {header_text}");
        Self { stream, buf: buf[header_end..].to_vec() }
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

    async fn send_text(&mut self, text: &str) {
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

    async fn recv_text(&mut self) -> Option<String> {
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

struct RpcSession {
    ws: WsClient,
    next_id: u64,
    events: Vec<(String, Value)>,
}

impl RpcSession {
    fn new(ws: WsClient) -> Self {
        Self { ws, next_id: 1, events: Vec::new() }
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, rainy_protocol::RpcError> {
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

    async fn next_event(&mut self, timeout: Duration) -> (String, Value) {
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

// ───────────────────────────── Host process harness ─────────────────────────────

struct HostProcess {
    child: Child,
    port: u16,
    launch_url: String,
    _home: tempfile::TempDir,
}

impl HostProcess {
    fn spawn() -> Self {
        let home = tempfile::tempdir().expect("temp home");
        let app_root = home.path().join("app");
        std::fs::create_dir_all(app_root.join("resources")).expect("create resources");
        std::fs::create_dir_all(app_root.join("dist").join("renderer")).expect("create renderer");
        std::fs::write(app_root.join("dist").join("renderer").join("index.html"), "<!doctype html><head></head><body>ok</body>").expect("write index");
        let bin = env!("CARGO_BIN_EXE_rainy-host");
        let mut child = Command::new(bin)
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
        std::thread::spawn(move || {
            let mut sink = String::new();
            while reader.read_line(&mut sink).is_ok_and(|n| n > 0) {
                sink.clear();
            }
        });
        Self { child, port, launch_url, _home: home }
    }

    async fn auth_cookie(&self) -> String {
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).no_proxy().build().unwrap();
        let resp = client.get(&self.launch_url).send().await.expect("launch url");
        assert_eq!(resp.status(), StatusCode::FOUND);
        let set_cookie = resp.headers().get(header::SET_COOKIE).expect("set-cookie").to_str().unwrap().to_owned();
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

// ───────────────────────────── Cross-platform mock CLI builder ─────────────────────────────

fn find_python() -> PathBuf {
    for candidate in ["python3", "python"] {
        if let Ok(out) = Command::new(candidate).arg("--version").output()
            && out.status.success()
        {
            return PathBuf::from(candidate);
        }
    }
    panic!("python3 or python must be available on PATH to run external_agent_e2e");
}

/// Write a Python mock server and an executable platform wrapper (`.cmd` on Windows, executable shell script on Unix).
fn write_mock_cli(dir: &Path, name: &str, script: &str) -> PathBuf {
    let py_path = dir.join(format!("{name}.py"));
    std::fs::write(&py_path, script).expect("write mock python");
    let python = find_python();
    #[cfg(windows)]
    {
        let wrapper = dir.join(format!("{name}.cmd"));
        let body = format!("@echo off\r\n\"{}\" -u \"{}\" %*\r\n", python.display(), py_path.display());
        std::fs::write(&wrapper, body).expect("write cmd wrapper");
        wrapper
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let wrapper = dir.join(name);
        let body = format!("#!/bin/sh\nexec \"{}\" -u \"{}\" \"$@\"\n", python.display(), py_path.display());
        std::fs::write(&wrapper, body).expect("write sh wrapper");
        let mut perms = std::fs::metadata(&wrapper).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&wrapper, perms).unwrap();
        wrapper
    }
}

const MOCK_CODEX_PY: &str = r#"
import json, os, sys, time

if "--version" in sys.argv:
    print("codex-cli 0.170.0-mock")
    sys.exit(0)

assert "app-server" in sys.argv, f"expected app-server subcommand, got {sys.argv}"

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def wait_response(expected_id):
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        msg = json.loads(raw)
        if msg.get("id") == expected_id and "result" in msg:
            return msg["result"]
    return {}

thread_id = "thread-codex-1"
turn_seq = 0
log_path = os.path.join(os.getcwd(), "codex_mock_log.jsonl")

def record(entry):
    with open(log_path, "a", encoding="utf-8") as f:
        f.write(json.dumps(entry) + "\n")

for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    msg = json.loads(raw)
    method = msg.get("method")
    req_id = msg.get("id")
    params = msg.get("params") or {}
    if method:
        record({"method": method, "params": params})

    if method == "initialize":
        send({"id": req_id, "result": {"codexHome": "/tmp/codex", "platformFamily": "test", "platformOs": "test", "userAgent": "mock"}})
    elif method == "initialized":
        pass
    elif method == "model/list":
        send({"id": req_id, "result": {
            "data": [{
                "id": "gpt-5.3-codex",
                "model": "gpt-5.3-codex",
                "displayName": "GPT-5.3 Codex",
                "hidden": False,
                "isDefault": True,
                "defaultReasoningEffort": "medium",
                "supportedReasoningEfforts": [
                    {"reasoningEffort": "low", "description": "Fast"},
                    {"reasoningEffort": "medium", "description": "Balanced"},
                    {"reasoningEffort": "high", "description": "Deep"}
                ]
            }]
        }})
    elif method == "thread/start":
        send({"id": req_id, "result": {
            "thread": {"id": thread_id},
            "model": params.get("model") or "gpt-5.3-codex",
            "modelProvider": "openai",
            "cwd": params.get("cwd") or os.getcwd(),
            "approvalPolicy": params.get("approvalPolicy") or "on-request",
            "approvalsReviewer": "user",
            "sandbox": {"type": "workspaceWrite"}
        }})
    elif method == "thread/resume":
        tid = params.get("threadId") or thread_id
        send({"id": req_id, "result": {
            "thread": {"id": tid},
            "model": params.get("model") or "gpt-5.3-codex",
            "modelProvider": "openai",
            "cwd": params.get("cwd") or os.getcwd(),
            "approvalPolicy": params.get("approvalPolicy") or "on-request",
            "approvalsReviewer": "user",
            "sandbox": {"type": "workspaceWrite"}
        }})
    elif method == "turn/start":
        turn_seq += 1
        turn_id = f"turn-{turn_seq}"
        tid = params.get("threadId") or thread_id
        text = ""
        for item in params.get("input") or []:
            if item.get("type") == "text":
                text += item.get("text", "")
        send({"id": req_id, "result": {"turn": {"id": turn_id, "status": "inProgress"}}})
        send({"method": "turn/started", "params": {"threadId": tid, "turn": {"id": turn_id}}})

        if "slow_turn" in text:
            send({"method": "item/agentMessage/delta", "params": {"threadId": tid, "turnId": turn_id, "itemId": "msg-slow", "delta": "working..."}})
            for inner_raw in sys.stdin:
                inner_raw = inner_raw.strip()
                if not inner_raw:
                    continue
                inner = json.loads(inner_raw)
                if inner.get("method") == "turn/interrupt":
                    record({"method": "turn/interrupt", "params": inner.get("params")})
                    send({"id": inner.get("id"), "result": {}})
                    send({"method": "turn/completed", "params": {"threadId": tid, "turn": {"id": turn_id, "status": "interrupted"}}})
                    break
        elif "deny_file" in text:
            send({"method": "item/started", "params": {"threadId": tid, "turnId": turn_id, "item": {"type": "fileChange", "id": "fc-deny", "changes": [{"path": "blocked.txt", "kind": "create"}]}}})
            send({"id": "srv-file-1", "method": "item/fileChange/requestApproval", "params": {"threadId": tid, "turnId": turn_id, "itemId": "fc-deny", "grantRoot": "blocked.txt", "reason": "Write blocked.txt", "startedAtMs": 1}})
            decision = wait_response("srv-file-1")
            record({"approval_decision": decision})
            send({"method": "item/completed", "params": {"threadId": tid, "turnId": turn_id, "item": {"type": "fileChange", "id": "fc-deny", "status": "declined", "changes": []}}})
            send({"method": "item/agentMessage/delta", "params": {"threadId": tid, "turnId": turn_id, "itemId": "msg-deny", "delta": f"Decision={decision.get('decision')}"}})
            send({"method": "turn/completed", "params": {"threadId": tid, "turn": {"id": turn_id, "status": "completed"}}})
        else:
            send({"method": "item/reasoning/textDelta", "params": {"threadId": tid, "turnId": turn_id, "itemId": "rsn-1", "delta": "Planning step..."}})
            time.sleep(0.08)
            send({"method": "item/agentMessage/delta", "params": {"threadId": tid, "turnId": turn_id, "itemId": "msg-1", "delta": "Hello "}})
            time.sleep(0.08)
            send({"method": "item/agentMessage/delta", "params": {"threadId": tid, "turnId": turn_id, "itemId": "msg-1", "delta": "Codex!"}})
            send({"method": "item/started", "params": {"threadId": tid, "turnId": turn_id, "item": {"type": "commandExecution", "id": "cmd-1", "command": "echo hi", "cwd": os.getcwd()}}})
            send({"id": "srv-cmd-1", "method": "item/commandExecution/requestApproval", "params": {"threadId": tid, "turnId": turn_id, "itemId": "cmd-1", "command": "echo hi", "cwd": os.getcwd(), "startedAtMs": 1}})
            decision = wait_response("srv-cmd-1")
            record({"approval_decision": decision})
            send({"method": "item/commandExecution/outputDelta", "params": {"threadId": tid, "turnId": turn_id, "itemId": "cmd-1", "delta": "hi\n"}})
            send({"method": "item/completed", "params": {"threadId": tid, "turnId": turn_id, "item": {"type": "commandExecution", "id": "cmd-1", "aggregatedOutput": "hi\n", "exitCode": 0, "status": "completed"}}})
            with open(os.path.join(os.getcwd(), "created_by_codex.txt"), "w", encoding="utf-8") as f:
                f.write("hello from codex\n")
            send({"method": "item/started", "params": {"threadId": tid, "turnId": turn_id, "item": {"type": "fileChange", "id": "fc-1", "changes": [{"path": "created_by_codex.txt", "kind": "create"}]}}})
            send({"method": "item/completed", "params": {"threadId": tid, "turnId": turn_id, "item": {"type": "fileChange", "id": "fc-1", "status": "completed", "changes": [{"path": "created_by_codex.txt", "kind": "create"}]}}})
            send({"method": "turn/diff/updated", "params": {"threadId": tid, "turnId": turn_id, "diff": "--- /dev/null\n+++ b/created_by_codex.txt\n@@ -0,0 +1 @@\n+hello from codex\n"}})
            send({"method": "thread/tokenUsage/updated", "params": {"threadId": tid, "turnId": turn_id, "tokenUsage": {"last": {"inputTokens": 80, "outputTokens": 20, "cachedInputTokens": 10, "cacheWriteInputTokens": 0, "reasoningOutputTokens": 5, "totalTokens": 115}, "total": {"inputTokens": 80, "outputTokens": 20, "cachedInputTokens": 10, "cacheWriteInputTokens": 0, "reasoningOutputTokens": 5, "totalTokens": 115}}}})
            send({"method": "item/agentMessage/delta", "params": {"threadId": tid, "turnId": turn_id, "itemId": "msg-2", "delta": f"Done ({decision.get('decision')})."}})
            send({"method": "turn/completed", "params": {"threadId": tid, "turn": {"id": turn_id, "status": "completed"}}})
    elif method == "turn/interrupt":
        send({"id": req_id, "result": {}})
"#;

const MOCK_ACP_PY: &str = r#"
import json, os, sys, time

if "--version" in sys.argv:
    print("agy 1.6.0-mock")
    sys.exit(0)

assert "acp" in sys.argv, f"expected acp subcommand, got {sys.argv}"

def send(obj):
    obj.setdefault("jsonrpc", "2.0")
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def wait_response(expected_id):
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        msg = json.loads(raw)
        if msg.get("id") == expected_id and "result" in msg:
            return msg["result"]
    return {}

session_id = "acp-sess-1"
log_path = os.path.join(os.getcwd(), "acp_mock_log.jsonl")

def record(entry):
    with open(log_path, "a", encoding="utf-8") as f:
        f.write(json.dumps(entry) + "\n")

for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    msg = json.loads(raw)
    method = msg.get("method")
    req_id = msg.get("id")
    params = msg.get("params") or {}
    if method:
        record({"method": method, "params": params})

    if method == "initialize":
        caps = params.get("clientCapabilities") or {}
        assert caps.get("fs", {}).get("readTextFile") is False
        assert caps.get("terminal") is False
        send({"id": req_id, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": True}}})
    elif method == "session/new":
        # Compliance check: Rainy must never inject custom systemPrompt or Rainy tool definitions.
        assert "systemPrompt" not in params
        assert "tools" not in params
        send({"id": req_id, "result": {
            "sessionId": session_id,
            "models": {
                "currentModelId": "gemini-3-pro",
                "availableModels": [
                    {"modelId": "gemini-3-pro", "name": "Gemini 3 Pro"},
                    {"modelId": "gemini-3-flash", "name": "Gemini 3 Flash"}
                ]
            },
            "modes": {
                "availableModes": [
                    {"id": "default", "name": "Default"},
                    {"id": "accept-edits", "name": "Accept Edits"},
                    {"id": "plan", "name": "Plan"}
                ]
            },
            "configOptions": [{
                "id": "reasoning_effort",
                "category": "thought_level",
                "currentValue": "high",
                "options": [
                    {"value": "low", "name": "Low"},
                    {"value": "high", "name": "High"}
                ]
            }]
        }})
    elif method == "session/resume":
        send({"id": req_id, "result": {"sessionId": params.get("sessionId") or session_id}})
    elif method == "session/set_mode":
        send({"id": req_id, "result": {}})
    elif method == "session/set_model":
        send({"id": req_id, "result": {}})
    elif method == "session/set_config_option":
        send({"id": req_id, "result": {}})
    elif method == "session/prompt":
        text = ""
        for blk in params.get("prompt") or []:
            if blk.get("type") == "text":
                text += blk.get("text", "")
        sid = params.get("sessionId") or session_id

        if "slow_turn" in text:
            send({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "thinking..."}}}})
            for inner_raw in sys.stdin:
                inner_raw = inner_raw.strip()
                if not inner_raw:
                    continue
                inner = json.loads(inner_raw)
                if inner.get("method") == "session/cancel":
                    record({"method": "session/cancel", "params": inner.get("params")})
                    send({"id": req_id, "result": {"stopReason": "cancelled"}})
                    break
        elif "reject_perm" in text:
            send({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "tool_call", "toolCallId": "acp-tc-2", "title": "Run rm", "kind": "execute", "rawInput": {"command": "rm -rf tmp"}}}})
            send({"id": "acp-perm-2", "method": "session/request_permission", "params": {
                "sessionId": sid,
                "toolCall": {"toolCallId": "acp-tc-2", "title": "Run rm", "kind": "execute"},
                "options": [
                    {"optionId": "opt-once", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "opt-reject", "name": "Reject", "kind": "reject_once"}
                ]
            }})
            ans = wait_response("acp-perm-2")
            record({"permission_outcome": ans})
            send({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "tool_call_update", "toolCallId": "acp-tc-2", "status": "cancelled", "content": [{"type": "text", "text": "denied"}]}}})
            send({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": f"Outcome={ans.get('outcome', {}).get('optionId')}"}}}})
            send({"id": req_id, "result": {"stopReason": "end_turn"}})
        else:
            send({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "Analyzing with Antigravity..."}}}})
            time.sleep(0.08)
            send({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hello "}}}})
            time.sleep(0.08)
            send({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Antigravity!"}}}})
            send({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "tool_call", "toolCallId": "acp-tc-1", "title": "Edit app.py", "kind": "edit", "locations": [{"path": "app.py"}]}}})
            send({"id": "acp-perm-1", "method": "session/request_permission", "params": {
                "sessionId": sid,
                "toolCall": {"toolCallId": "acp-tc-1", "title": "Edit app.py", "kind": "edit", "locations": [{"path": "app.py"}]},
                "options": [
                    {"optionId": "opt-once", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "opt-always", "name": "Always allow", "kind": "allow_always"},
                    {"optionId": "opt-reject", "name": "Reject", "kind": "reject_once"}
                ]
            }})
            ans = wait_response("acp-perm-1")
            record({"permission_outcome": ans})
            send({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "tool_call_update", "toolCallId": "acp-tc-1", "status": "completed", "content": [{"type": "text", "text": "Edited app.py"}]}}})
            send({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "usage_update", "inputTokens": 50, "outputTokens": 15}}})
            send({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": f" ({ans.get('outcome', {}).get('optionId')})"}}}})
            send({"id": req_id, "result": {"stopReason": "end_turn"}})
"#;

#[tokio::test]
async fn codex_app_server_probe_stream_approval_resume_and_abort() {
    let host = HostProcess::spawn();
    let cookie = host.auth_cookie().await;
    let ws = WsClient::connect(host.port, &cookie).await;
    let mut rpc = RpcSession::new(ws);

    let work = tempfile::tempdir().expect("workdir");
    let mock_bin = write_mock_cli(work.path(), "mock-codex", MOCK_CODEX_PY);

    // 1. Configure & probe mock Codex binary (no LLM API key configured!).
    let cfg_res = rpc
        .call(
            "external.configure",
            json!({
                "config": {
                    "kind": "codex",
                    "enabled": true,
                    "binary": mock_bin.to_string_lossy(),
                    "args": [],
                    "defaultModel": "gpt-5.3-codex",
                    "defaultThinking": "medium"
                }
            }),
        )
        .await
        .expect("external.configure");
    assert!(cfg_res["agents"].as_array().unwrap().iter().any(|a| a["kind"] == "codex"));

    let probe_res = rpc
        .call("external.probe", json!({ "kind": "codex", "cwd": work.path().to_string_lossy() }))
        .await
        .expect("external.probe");
    let codex_status = probe_res["agents"].as_array().unwrap().iter().find(|a| a["kind"] == "codex").expect("codex row");
    assert_eq!(codex_status["installed"], true);
    assert_eq!(codex_status["version"], "codex-cli 0.170.0-mock");
    assert_eq!(codex_status["models"][0]["id"], "gpt-5.3-codex");
    assert_eq!(codex_status["models"][0]["reasoningEfforts"], json!(["low", "medium", "high"]));

    // 2. Create a Codex external session.
    let created = rpc
        .call(
            "sessions.create",
            json!({
                "cwd": work.path().to_string_lossy(),
                "permission": "ask",
                "externalBackend": { "kind": "codex" }
            }),
        )
        .await
        .expect("sessions.create");
    let session_id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["externalBackend"]["kind"], "codex");

    // 3. Turn 1: streaming thinking + text + command approval (allow) + fileChange.
    rpc.call("chat.send", json!({ "sessionId": session_id, "text": "Build demo" })).await.expect("chat.send turn 1");

    let mut streamed: Option<AssistantMessage> = None;
    let mut saw_patch = false;
    let mut approved_cmd = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        let (event, data) = rpc.next_event(Duration::from_secs(10)).await;
        if event == "session.stream" && data["sessionId"] == session_id {
            if let Some(msg) = data.get("message").filter(|v| !v.is_null()) {
                streamed = Some(serde_json::from_value(msg.clone()).expect("whole stream message"));
            } else if let Some(patch_val) = data.get("patch") {
                let patch: StreamPatch = serde_json::from_value(patch_val.clone()).expect("stream patch");
                if let Some(cur) = streamed.as_mut() {
                    assert!(apply_stream_patch(cur, &patch));
                    saw_patch = true;
                }
            }
        } else if event == "session.state" && data["sessionId"] == session_id {
            if !approved_cmd
                && let Some(arr) = data["approvals"].as_array()
                && let Some(first) = arr.first()
            {
                assert_eq!(first["reason"], "command");
                let call_id = first["toolCallId"].as_str().unwrap().to_owned();
                approved_cmd = true;
                rpc.call("chat.approve", json!({ "sessionId": session_id, "toolCallId": call_id, "decision": "allow" }))
                    .await
                    .expect("approve command");
            }
            if approved_cmd && data["status"] == "idle" {
                break;
            }
        }
    }
    assert!(approved_cmd, "must have received and answered command approval");
    assert!(saw_patch || streamed.is_some(), "must have streamed updates");

    let snap = rpc.call("sessions.get", json!({ "sessionId": session_id })).await.expect("sessions.get");
    assert_eq!(snap["summary"]["externalThreadId"], "thread-codex-1");
    let entries_str = snap["entries"].to_string();
    assert!(entries_str.contains("Planning step..."), "missing thinking: {entries_str}");
    assert!(entries_str.contains("Hello Codex!"), "missing streamed text: {entries_str}");
    assert!(entries_str.contains("Done (accept)."), "missing post-approval reply: {entries_str}");

    let changes = rpc.call("chat.changes", json!({ "sessionId": session_id })).await.expect("chat.changes");
    let changes_arr = changes.as_array().unwrap();
    assert!(changes_arr.iter().any(|c| c["display"] == "created_by_codex.txt" && c["undoable"] == false), "changes={changes}");

    // 4. Turn 2: thread/resume + file change approval denied.
    rpc.call("chat.send", json!({ "sessionId": session_id, "text": "deny_file please" })).await.expect("chat.send turn 2");
    let mut denied_file = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        let (event, data) = rpc.next_event(Duration::from_secs(10)).await;
        if event == "session.state" && data["sessionId"] == session_id {
            if !denied_file
                && let Some(arr) = data["approvals"].as_array()
                && let Some(first) = arr.first()
            {
                assert_eq!(first["reason"], "file-change");
                let call_id = first["toolCallId"].as_str().unwrap().to_owned();
                denied_file = true;
                rpc.call("chat.approve", json!({ "sessionId": session_id, "toolCallId": call_id, "decision": "deny", "reason": "no" }))
                    .await
                    .expect("deny file");
            }
            if denied_file && data["status"] == "idle" {
                break;
            }
        }
    }
    assert!(denied_file, "must have denied file change");
    let snap2 = rpc.call("sessions.get", json!({ "sessionId": session_id })).await.expect("sessions.get 2");
    assert!(snap2["entries"].to_string().contains("Decision=decline"));

    // 5. Turn 3: chat.abort triggers turn/interrupt and returns to idle cleanly.
    rpc.call("chat.send", json!({ "sessionId": session_id, "text": "slow_turn" })).await.expect("chat.send slow");
    let mut aborted = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        let (event, data) = rpc.next_event(Duration::from_secs(10)).await;
        if !aborted && event == "session.stream" && data["sessionId"] == session_id {
            aborted = true;
            rpc.call("chat.abort", json!({ "sessionId": session_id })).await.expect("chat.abort");
        }
        if aborted && event == "session.state" && data["sessionId"] == session_id && data["status"] == "idle" {
            break;
        }
    }
    assert!(aborted, "must have aborted slow turn");

    // Verify mock server received thread/resume and turn/interrupt.
    let log = std::fs::read_to_string(work.path().join("codex_mock_log.jsonl")).expect("codex log");
    assert!(log.contains("\"thread/start\""), "log={log}");
    assert!(log.contains("\"thread/resume\""), "log={log}");
    assert!(log.contains("\"turn/interrupt\""), "log={log}");
}

#[tokio::test]
async fn acp_antigravity_probe_stream_permission_resume_and_abort() {
    let host = HostProcess::spawn();
    let cookie = host.auth_cookie().await;
    let ws = WsClient::connect(host.port, &cookie).await;
    let mut rpc = RpcSession::new(ws);

    let work = tempfile::tempdir().expect("workdir");
    let mock_bin = write_mock_cli(work.path(), "mock-agy", MOCK_ACP_PY);

    // 1. Configure & probe mock Antigravity ACP binary.
    rpc.call(
        "external.configure",
        json!({
            "config": {
                "kind": "antigravity",
                "enabled": true,
                "binary": mock_bin.to_string_lossy(),
                "args": ["acp"],
                "defaultModel": "gemini-3-pro",
                "defaultThinking": "high"
            }
        }),
    )
    .await
    .expect("configure antigravity");

    let probe_res = rpc
        .call("external.probe", json!({ "kind": "antigravity", "cwd": work.path().to_string_lossy() }))
        .await
        .expect("probe antigravity");
    let agy_status = probe_res["agents"].as_array().unwrap().iter().find(|a| a["kind"] == "antigravity").expect("agy row");
    assert_eq!(agy_status["installed"], true);
    assert_eq!(agy_status["version"], "agy 1.6.0-mock");
    assert_eq!(agy_status["models"][0]["id"], "gemini-3-pro");
    assert_eq!(agy_status["models"][0]["reasoningEfforts"], json!(["low", "high"]));

    // 2. Create Antigravity session & verify /swarm is rejected (compliance).
    let created = rpc
        .call(
            "sessions.create",
            json!({
                "cwd": work.path().to_string_lossy(),
                "permission": "ask",
                "externalBackend": { "kind": "antigravity" }
            }),
        )
        .await
        .expect("sessions.create agy");
    let session_id = created["id"].as_str().unwrap().to_owned();
    assert!(rpc.call("chat.send", json!({ "sessionId": session_id, "text": "/swarm forbidden" })).await.is_err());

    // 3. Turn 1: thought + message chunks + permission request answered with `always` (allow_always).
    rpc.call("chat.send", json!({ "sessionId": session_id, "text": "Refactor app.py" })).await.expect("send agy 1");
    let mut answered_perm = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        let (event, data) = rpc.next_event(Duration::from_secs(10)).await;
        if event == "session.state" && data["sessionId"] == session_id {
            if !answered_perm
                && let Some(arr) = data["approvals"].as_array()
                && let Some(first) = arr.first()
            {
                assert_eq!(first["reason"], "file-change");
                let call_id = first["toolCallId"].as_str().unwrap().to_owned();
                answered_perm = true;
                rpc.call("chat.approve", json!({ "sessionId": session_id, "toolCallId": call_id, "decision": "always" }))
                    .await
                    .expect("approve always");
            }
            if answered_perm && data["status"] == "idle" {
                break;
            }
        }
    }
    assert!(answered_perm, "must have answered ACP permission request");
    let snap = rpc.call("sessions.get", json!({ "sessionId": session_id })).await.expect("sessions.get agy");
    assert_eq!(snap["summary"]["externalThreadId"], "acp-sess-1");
    let entries_str = snap["entries"].to_string();
    assert!(entries_str.contains("Analyzing with Antigravity..."), "missing thought: {entries_str}");
    assert!(entries_str.contains("Hello Antigravity!"), "missing message: {entries_str}");
    assert!(entries_str.contains("(opt-always)"), "missing always optionId: {entries_str}");

    // 4. Turn 2: model switch + permission denied (`reject_once`).
    rpc.call(
        "chat.setModel",
        json!({ "sessionId": session_id, "provider": "external:antigravity", "model": "gemini-3-flash", "thinking": "low" }),
    )
    .await
    .expect("chat.setModel");
    rpc.call("chat.send", json!({ "sessionId": session_id, "text": "reject_perm" })).await.expect("send agy 2");
    let mut rejected_perm = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        let (event, data) = rpc.next_event(Duration::from_secs(10)).await;
        if event == "session.state" && data["sessionId"] == session_id {
            if !rejected_perm
                && let Some(arr) = data["approvals"].as_array()
                && let Some(first) = arr.first()
            {
                let call_id = first["toolCallId"].as_str().unwrap().to_owned();
                rejected_perm = true;
                rpc.call("chat.approve", json!({ "sessionId": session_id, "toolCallId": call_id, "decision": "deny" }))
                    .await
                    .expect("deny perm");
            }
            if rejected_perm && data["status"] == "idle" {
                break;
            }
        }
    }
    assert!(rejected_perm);
    let snap2 = rpc.call("sessions.get", json!({ "sessionId": session_id })).await.expect("sessions.get agy 2");
    assert!(snap2["entries"].to_string().contains("Outcome=opt-reject"));

    // 5. Turn 3: abort triggers `session/cancel`.
    rpc.call("chat.send", json!({ "sessionId": session_id, "text": "slow_turn" })).await.expect("send agy slow");
    let mut aborted = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        let (event, data) = rpc.next_event(Duration::from_secs(10)).await;
        if !aborted && event == "session.stream" && data["sessionId"] == session_id {
            aborted = true;
            rpc.call("chat.abort", json!({ "sessionId": session_id })).await.expect("abort agy");
        }
        if aborted && event == "session.state" && data["sessionId"] == session_id && data["status"] == "idle" {
            break;
        }
    }
    assert!(aborted);

    let log = std::fs::read_to_string(work.path().join("acp_mock_log.jsonl")).expect("acp log");
    assert!(log.contains("\"session/set_model\""), "log={log}");
    assert!(log.contains("\"gemini-3-flash\""), "log={log}");
    assert!(log.contains("\"session/cancel\""), "log={log}");
}
