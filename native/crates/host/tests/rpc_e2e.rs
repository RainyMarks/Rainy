//! End-to-end tests driving the real `rainy-host` binary with a local Mock LLM HTTP server over `/rpc`:
//! covers loopback auth & origin checks, one-shot `/rainy/ide/office-pdf`, streaming `StreamPatch` delivery,
//! read-before-write enforcement, `ask`-mode command approval via `chat.approve`, and context compaction.

mod common;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use common::{HostProcess, RpcSession, WsClient};
use rainy_protocol::chat::AssistantMessage;
use rainy_protocol::stream::{StreamPatch, apply_stream_patch};
use serde_json::{Value, json};

// ───────────────────────────── Local Mock LLM Server ─────────────────────────────

#[derive(Clone)]
enum MockAction {
    /// Stream text in timed chunks so Host emits incremental `StreamPatch` events.
    StreamText { chunks: Vec<String>, delay_ms: u64 },
    /// Emit one tool call with given id, name and JSON arguments string.
    ToolCall { id: String, name: String, arguments: String },
}

#[derive(Clone, Default)]
struct MockState {
    script: Arc<Mutex<VecDeque<MockAction>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn mock_completions(State(state): State<MockState>, body: String) -> Response {
    if let Ok(parsed) = serde_json::from_str::<Value>(&body) {
        state.requests.lock().unwrap().push(parsed);
    }
    let action = state.script.lock().unwrap().pop_front().unwrap_or(MockAction::StreamText {
        chunks: vec!["(default mock reply)".into()],
        delay_ms: 0,
    });
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(16);
    tokio::spawn(async move {
        let send_chunk = |tx: &tokio::sync::mpsc::Sender<Result<String, std::io::Error>>, val: Value| {
            let line = format!("data: {}\n\n", val);
            let tx = tx.clone();
            async move {
                let _ = tx.send(Ok(line)).await;
            }
        };
        match action {
            MockAction::StreamText { chunks, delay_ms } => {
                for (idx, piece) in chunks.into_iter().enumerate() {
                    if idx > 0 && delay_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }
                    send_chunk(
                        &tx,
                        json!({
                            "id": "chatcmpl-mock",
                            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": piece }, "finish_reason": null }]
                        }),
                    )
                    .await;
                }
                send_chunk(
                    &tx,
                    json!({
                        "id": "chatcmpl-mock",
                        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                        "usage": { "prompt_tokens": 120, "completion_tokens": 24, "total_tokens": 144 }
                    }),
                )
                .await;
            }
            MockAction::ToolCall { id, name, arguments } => {
                send_chunk(
                    &tx,
                    json!({
                        "id": "chatcmpl-tool",
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "role": "assistant",
                                "tool_calls": [{
                                    "index": 0,
                                    "id": id,
                                    "type": "function",
                                    "function": { "name": name, "arguments": arguments }
                                }]
                            },
                            "finish_reason": null
                        }]
                    }),
                )
                .await;
                send_chunk(
                    &tx,
                    json!({
                        "id": "chatcmpl-tool",
                        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
                        "usage": { "prompt_tokens": 150, "completion_tokens": 30, "total_tokens": 180 }
                    }),
                )
                .await;
            }
        }
        let _ = tx.send(Ok("data: [DONE]\n\n".to_owned())).await;
    });
    let stream = futures::stream::poll_fn(move |cx| rx.poll_recv(cx));
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    response
}

async fn start_mock_llm(script: Vec<MockAction>) -> (u16, MockState) {
    let state = MockState { script: Arc::new(Mutex::new(VecDeque::from(script))), requests: Arc::default() };
    let app = Router::new().route("/v1/chat/completions", post(mock_completions)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (port, state)
}

// ───────────────────────────── E2E Test Cases ─────────────────────────────

#[tokio::test]
async fn rpc_auth_origin_and_one_shot_office_pdf_e2e() {
    let host = HostProcess::spawn();
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).no_proxy().build().unwrap();
    let base = format!("http://127.0.0.1:{}", host.port);

    // 1. Bad launch token is refused.
    let bad_token = client.get(format!("{base}/?token=definitely-wrong")).send().await.unwrap();
    assert_eq!(bad_token.status(), StatusCode::FORBIDDEN);

    // 2. Unauthenticated HTTP route and WebSocket upgrade are refused.
    let unauth_pdf = client.get(format!("{base}/rainy/ide/office-pdf?token=abc")).send().await.unwrap();
    assert_eq!(unauth_pdf.status(), StatusCode::FORBIDDEN);
    let (ws_no_cookie, _) = WsClient::handshake(host.port, "/rpc", None, Some(&base)).await;
    assert_eq!(ws_no_cookie, 403);

    // 3. Exchange valid launch token for HttpOnly cookie.
    let cookie = host.auth_cookie().await;

    // 4. Forged Origin on /rpc upgrade is refused even with valid cookie.
    let (ws_bad_origin, _) = WsClient::handshake(host.port, "/rpc", Some(&cookie), Some("http://evil.example")).await;
    assert_eq!(ws_bad_origin, 403);

    // 5. Valid Cookie + same Origin upgrades /rpc and answers `app.info`.
    let (ws_ok, ws) = WsClient::handshake(host.port, "/rpc", Some(&cookie), Some(&base)).await;
    assert_eq!(ws_ok, 101);
    let mut rpc = RpcSession::new(ws.unwrap());
    let info = rpc.call("app.info", Value::Null).await.expect("app.info");
    assert_eq!(info["name"], "Rainy");

    // 6. One-shot `/rainy/ide/office-pdf` upload rejects non-PDF, accepts valid PDF, serves once and deletes.
    let bad_upload = client
        .post(format!("{base}/rainy/ide/office-pdf"))
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "application/pdf")
        .body("NOT A PDF")
        .send()
        .await
        .unwrap();
    assert_eq!(bad_upload.status(), StatusCode::BAD_REQUEST);

    let pdf_bytes = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n%%EOF\n".to_vec();
    let upload = client
        .post(format!("{base}/rainy/ide/office-pdf"))
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "application/pdf")
        .body(pdf_bytes.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::OK);
    let reg: rainy_protocol::OfficePdfRegistration = upload.json().await.unwrap();
    assert!(reg.url.starts_with("/rainy/ide/office-pdf?token="));

    let first_get = client
        .get(format!("{base}{}", reg.url))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(first_get.status(), StatusCode::OK);
    assert_eq!(first_get.headers().get(header::CONTENT_TYPE).unwrap(), "application/pdf");
    assert!(first_get.headers().get(header::CONTENT_SECURITY_POLICY).unwrap().to_str().unwrap().contains("sandbox"));
    assert_eq!(first_get.bytes().await.unwrap().as_ref(), pdf_bytes.as_slice());

    // Replaying the consumed one-shot token returns 404.
    let second_get = client
        .get(format!("{base}{}", reg.url))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(second_get.status(), StatusCode::NOT_FOUND);
}

fn assistant_text(msg: &AssistantMessage) -> String {
    msg.content
        .iter()
        .filter_map(|b| match b {
            rainy_protocol::chat::AssistantBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn mock_llm_rpc_streaming_patch_read_before_write_approval_and_compaction_e2e() {
    let shell_tool = if cfg!(windows) { "pwsh" } else { "bash" };
    let script = vec![
        // Turn 1: multi-chunk streamed reply (delay 75ms > 50ms throttle -> emits `StreamPatch` deltas).
        MockAction::StreamText {
            chunks: vec!["Hello ".into(), "Rainy ".into(), "流式".into(), "补丁!".into()],
            delay_ms: 75,
        },
        // Turn 2a: attempt `edit` BEFORE reading `note.txt` -> Host must reject (read-before-write).
        MockAction::ToolCall {
            id: "call_edit_unseen".into(),
            name: "edit".into(),
            arguments: json!({ "file_path": "note.txt", "old_string": "before", "new_string": "bad" }).to_string(),
        },
        // Turn 2b: now `read` `note.txt`.
        MockAction::ToolCall {
            id: "call_read_note".into(),
            name: "read".into(),
            arguments: json!({ "file_path": "note.txt" }).to_string(),
        },
        // Turn 2c: now `edit` `note.txt` succeeds.
        MockAction::ToolCall {
            id: "call_edit_ok".into(),
            name: "edit".into(),
            arguments: json!({ "file_path": "note.txt", "old_string": "before", "new_string": "after" }).to_string(),
        },
        // Turn 2d: final text for Turn 2.
        MockAction::StreamText { chunks: vec!["Edited note.txt safely.".into()], delay_ms: 0 },
        // Turn 3a: shell command under `ask` mode -> must pause for user approval.
        MockAction::ToolCall {
            id: "call_shell_ask".into(),
            name: shell_tool.into(),
            arguments: json!({ "command": "echo e2e-approved", "description": "Echo test" }).to_string(),
        },
        // Turn 3b: final text after approval.
        MockAction::StreamText { chunks: vec!["Command ran after approval.".into()], delay_ms: 0 },
    ];
    let (llm_port, mock_state) = start_mock_llm(script).await;
    let host = HostProcess::spawn();
    let cookie = host.auth_cookie().await;
    let base = format!("http://127.0.0.1:{}", host.port);
    let (code, ws) = WsClient::handshake(host.port, "/rpc", Some(&cookie), Some(&base)).await;
    assert_eq!(code, 101);
    let mut rpc = RpcSession::new(ws.unwrap());

    // Prepare workspace with `note.txt`.
    let project = tempfile::tempdir().unwrap();
    let note_path = project.path().join("note.txt");
    std::fs::write(&note_path, "before\n").unwrap();
    let ws_info = rpc
        .call("ide", json!({ "op": "workspaces.open", "path": project.path().to_string_lossy() }))
        .await
        .expect("open workspace");
    let workspace_id = ws_info["workspaceId"].as_str().unwrap().to_owned();

    // Configure & select mock OpenAI-compatible local model.
    rpc.call(
        "models.configure",
        json!({
            "provider": "mock-e2e",
            "label": "Mock E2E",
            "baseURL": format!("http://127.0.0.1:{llm_port}/v1"),
            "api": "openai-completions",
            "model": "mock-1",
            "local": true,
            "contextWindow": 16000,
            "maxTokens": 2048
        }),
    )
    .await
    .expect("configure model");
    rpc.call("models.select", json!({ "provider": "mock-e2e", "model": "mock-1" }))
        .await
        .expect("select model");

    // Create chat in `ask` permission mode.
    let created = rpc
        .call("sessions.create", json!({ "workspaceId": workspace_id, "permission": "ask" }))
        .await
        .expect("create session");
    let session_id = created["id"].as_str().unwrap().to_owned();

    // ── Turn 1: Verify streaming `StreamPatch` events and final committed assistant entry ──
    rpc.call("chat.send", json!({ "sessionId": session_id, "text": "Stream a greeting" }))
        .await
        .expect("send turn 1");
    let mut streamed: Option<AssistantMessage> = None;
    let mut committed_text: Option<String> = None;
    let mut saw_patch = false;
    loop {
        let (event, data) = rpc.next_event(Duration::from_secs(15)).await;
        if event == "session.stream" && data["sessionId"] == session_id {
            if let Some(patch_val) = data.get("patch") {
                let patch: StreamPatch = serde_json::from_value(patch_val.clone()).expect("valid StreamPatch");
                if let Some(copy) = streamed.as_mut() {
                    assert!(apply_stream_patch(copy, &patch), "StreamPatch must apply cleanly");
                    saw_patch = true;
                }
            } else if let Some(msg_val) = data.get("message") {
                if !msg_val.is_null() {
                    streamed = Some(serde_json::from_value(msg_val.clone()).expect("valid AssistantMessage"));
                }
            }
        }
        if event == "session.entry" && data["sessionId"] == session_id && data["entry"]["kind"] == "assistant" {
            let msg: AssistantMessage = serde_json::from_value(data["entry"]["message"].clone()).expect("assistant entry");
            committed_text = Some(assistant_text(&msg));
        }
        if event == "session.state" && data["sessionId"] == session_id && data["status"] == "idle" {
            break;
        }
    }
    assert!(saw_patch, "expected at least one incremental StreamPatch during streaming");
    let streamed_text = assistant_text(streamed.as_ref().expect("streamed message"));
    assert!(
        "Hello Rainy 流式补丁!".starts_with(&streamed_text) && streamed_text.len() > "Hello ".len(),
        "streamed text must be a non-trivial prefix of full reply, got: {streamed_text}"
    );
    assert_eq!(committed_text.as_deref(), Some("Hello Rainy 流式补丁!"));

    // ── Turn 2: Read-before-write blocks blind edit, then read + edit succeeds ──
    rpc.call("chat.send", json!({ "sessionId": session_id, "text": "Update note.txt" }))
        .await
        .expect("send turn 2");
    let mut saw_unseen_edit_error = false;
    let mut saw_ok_edit = false;
    loop {
        let (event, data) = rpc.next_event(Duration::from_secs(15)).await;
        if event == "session.entry" && data["sessionId"] == session_id {
            let entry = &data["entry"];
            if entry["kind"] == "toolResult" && entry["toolCallId"] == "call_edit_unseen" {
                assert_eq!(entry["isError"], true, "edit before read must fail");
                let text = entry["content"][0]["text"].as_str().unwrap_or_default();
                assert!(text.contains("has not been read"), "unexpected error text: {text}");
                saw_unseen_edit_error = true;
            }
            if entry["kind"] == "toolResult" && entry["toolCallId"] == "call_edit_ok" {
                assert_eq!(entry["isError"], false, "edit after read must succeed");
                saw_ok_edit = true;
            }
        }
        if event == "session.state" && data["sessionId"] == session_id && data["status"] == "idle" {
            break;
        }
    }
    assert!(saw_unseen_edit_error, "missing failed toolResult for edit-before-read");
    assert!(saw_ok_edit, "missing succeeded toolResult for edit-after-read");
    assert_eq!(std::fs::read_to_string(&note_path).unwrap().replace("\r\n", "\n"), "after\n");

    // ── Turn 3: Shell command pauses for approval in `ask` mode, runs after `chat.approve` ──
    rpc.call("chat.send", json!({ "sessionId": session_id, "text": "Run echo command" }))
        .await
        .expect("send turn 3");
    let mut approved = false;
    let mut saw_shell_output = false;
    loop {
        let (event, data) = rpc.next_event(Duration::from_secs(15)).await;
        if event == "session.state" && data["sessionId"] == session_id {
            if !approved {
                if let Some(approvals) = data["approvals"].as_array().filter(|a| !a.is_empty()) {
                    assert_eq!(approvals[0]["toolCallId"], "call_shell_ask");
                    assert_eq!(approvals[0]["reason"], "command");
                    rpc.call(
                        "chat.approve",
                        json!({ "sessionId": session_id, "toolCallId": "call_shell_ask", "decision": "allow" }),
                    )
                    .await
                    .expect("approve shell");
                    approved = true;
                }
            }
            if approved && data["status"] == "idle" {
                break;
            }
        }
        if event == "session.entry" && data["sessionId"] == session_id {
            let entry = &data["entry"];
            if entry["kind"] == "toolResult" && entry["toolCallId"] == "call_shell_ask" {
                assert_eq!(entry["isError"], false);
                let text = entry["content"][0]["text"].as_str().unwrap_or_default();
                assert!(text.contains("e2e-approved"), "unexpected shell output: {text}");
                saw_shell_output = true;
            }
        }
    }
    assert!(approved, "shell command in ask mode must trigger pending approval");
    assert!(saw_shell_output, "approved shell command must run and produce output");

    // ── Compaction: compact the accumulated multi-turn chat via `chat.compact` ──
    // Send two extra turns so the transcript has older turns beyond the kept tail.
    {
        let mut script = mock_state.script.lock().unwrap();
        script.push_back(MockAction::StreamText { chunks: vec!["Padding reply four. ".repeat(120)], delay_ms: 0 });
        script.push_back(MockAction::StreamText { chunks: vec!["Padding reply five. ".repeat(120)], delay_ms: 0 });
    }
    for prompt in ["Extra context message four ".repeat(80), "Extra context message five ".repeat(80)] {
        rpc.call("chat.send", json!({ "sessionId": session_id, "text": prompt })).await.expect("send padding");
        loop {
            let (event, data) = rpc.next_event(Duration::from_secs(15)).await;
            if event == "session.state" && data["sessionId"] == session_id && data["status"] == "idle" {
                break;
            }
        }
    }
    let compact_res = rpc.call("chat.compact", json!({ "sessionId": session_id })).await;
    assert!(compact_res.is_ok(), "chat.compact failed: {compact_res:?}");
    let snapshot = rpc.call("sessions.get", json!({ "sessionId": session_id })).await.expect("sessions.get");
    let has_compaction = snapshot["entries"]
        .as_array()
        .is_some_and(|entries| entries.iter().any(|e| e["kind"] == "compaction"));
    assert!(has_compaction, "expected a compaction entry after chat.compact");
    assert!(Path::new(&note_path).is_file());
    assert!(mock_state.requests.lock().unwrap().len() >= 7);
}

#[tokio::test]
async fn bulk_pins_archives_and_deletes_many_chats_e2e() {
    let host = HostProcess::spawn();
    let cookie = host.auth_cookie().await;
    let mut rpc = RpcSession::new(WsClient::connect(host.port, &cookie).await);
    let mut ids = Vec::new();
    for _ in 0..3 {
        let chat = rpc.call("sessions.create", json!({})).await.expect("sessions.create");
        ids.push(chat["id"].as_str().unwrap().to_owned());
    }
    let listed = |list: &Value, id: &str| list.as_array().unwrap().iter().find(|chat| chat["id"] == id).cloned();

    // A chat named twice is done once.
    let pinned = rpc.call("sessions.bulk", json!({ "action": "pin", "sessionIds": [&ids[0], &ids[1], &ids[2], &ids[0]] })).await.expect("bulk pin");
    assert_eq!(pinned, json!({ "done": 3, "failed": [] }));
    let archived = rpc.call("sessions.bulk", json!({ "action": "archive", "sessionIds": [&ids[0], &ids[1]] })).await.expect("bulk archive");
    assert_eq!(archived["done"], 2);
    let list = rpc.call("sessions.list", json!({})).await.unwrap();
    for id in &ids {
        assert_eq!(listed(&list, id).unwrap()["pinned"], true);
    }
    assert_eq!(listed(&list, &ids[0]).unwrap()["archived"], true);
    assert_eq!(listed(&list, &ids[2]).unwrap()["archived"], false);

    // One that cannot be done is reported and does not stop the others; an id that is not a chat's never reaches the disk.
    let deleted = rpc.call("sessions.bulk", json!({ "action": "delete", "sessionIds": [&ids[0], "../outside", &ids[1]] })).await.expect("bulk delete");
    assert_eq!(deleted["done"], 2);
    assert_eq!(deleted["failed"].as_array().unwrap().len(), 1);
    assert_eq!(deleted["failed"][0]["sessionId"], "../outside");
    let list = rpc.call("sessions.list", json!({})).await.unwrap();
    assert!(listed(&list, &ids[0]).is_none() && listed(&list, &ids[1]).is_none());
    assert!(listed(&list, &ids[2]).is_some());
    let restored = rpc.call("sessions.bulk", json!({ "action": "unpin", "sessionIds": [&ids[2], &ids[0]] })).await.expect("bulk unpin");
    assert_eq!(restored["done"], 1);
    assert_eq!(restored["failed"][0]["sessionId"], ids[0]);

    // An unknown action or an empty list is refused as a whole.
    assert!(rpc.call("sessions.bulk", json!({ "action": "rename", "sessionIds": [&ids[2]] })).await.is_err());
    assert!(rpc.call("sessions.bulk", json!({ "action": "archive", "sessionIds": [] })).await.is_err());
}
