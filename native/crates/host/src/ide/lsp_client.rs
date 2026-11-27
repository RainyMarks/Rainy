//! Shared stdio JSON-RPC LSP client used by both the pooled diagnostics service (`language.rs`)
//! and one-shot TypeScript formatting (`format.rs`), with process-group cleanup on drop.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::framing::{Decoder, encode};
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::process::{ProcessJob, contain_child};

/// Map a file extension + server language to the exact LSP `languageId` (`tsx` -> `typescriptreact`, etc.).
pub fn document_language_id<'a>(path: &Path, server_language: &'a str) -> &'a str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("tsx") => "typescriptreact",
        Some("jsx") => "javascriptreact",
        _ => server_language,
    }
}

/// Framed LSP client over a child's stdio with `VecDeque` message buffering and process-tree cleanup on drop.
pub struct LspStdioClient {
    child: Option<tokio::process::Child>,
    job: Option<ProcessJob>,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    decoder: Decoder,
    received: VecDeque<Value>,
    next_id: u64,
}

impl LspStdioClient {
    pub fn spawn(
        program: impl AsRef<std::ffi::OsStr>,
        args: &[impl AsRef<std::ffi::OsStr>],
        cwd: &Path,
        env: &BTreeMap<OsString, OsString>,
        max_message_bytes: usize,
        drain_stderr: bool,
    ) -> std::io::Result<Self> {
        let mut command = crate::process::command(program, args, cwd, env);
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        if drain_stderr {
            command.stderr(Stdio::piped());
        } else {
            command.stderr(Stdio::null());
        }
        let mut child = command.spawn()?;
        let job = ProcessJob::new();
        if !job.as_ref().is_some_and(|job| job.assign_child(&child)) {
            contain_child(&child);
        }
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        if let Some(mut stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut sink = [0u8; 4096];
                while matches!(stderr.read(&mut sink).await, Ok(read) if read > 0) {}
            });
        }
        Ok(Self {
            child: Some(child),
            job,
            stdin,
            stdout,
            decoder: Decoder::new(max_message_bytes),
            received: VecDeque::new(),
            next_id: 1,
        })
    }

    pub async fn send(&mut self, message: &Value) -> HostResult<()> {
        let bytes = encode(message);
        self.stdin.write_all(&bytes).await.map_err(|error| HostError::new("io-error", error.to_string()))?;
        self.stdin.flush().await.map_err(|error| HostError::new("io-error", error.to_string()))
    }

    pub async fn notify(&mut self, method: &str, params: Value) -> HostResult<()> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params })).await
    }

    pub async fn read_more(&mut self) -> HostResult<()> {
        let mut chunk = [0u8; 32768];
        let read = self.stdout.read(&mut chunk).await.map_err(|error| HostError::new("io-error", error.to_string()))?;
        if read == 0 {
            return Err(HostError::new("io-error", l("语言服务器已停止。", "Language server stopped.")));
        }
        let messages = self.decoder.push(&chunk[..read]).map_err(|error| HostError::new("io-error", error))?;
        self.received.extend(messages);
        Ok(())
    }

    pub fn pop_received(&mut self) -> Option<Value> {
        self.received.pop_front()
    }

    /// Send a JSON-RPC request and wait for its matching response, answering server-initiated requests with `null`
    /// and forwarding notifications to `on_notification`.
    pub async fn request(
        &mut self,
        method: &str,
        params: Value,
        mut on_notification: impl FnMut(&Value),
    ) -> HostResult<Result<Value, (i64, String)>> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        loop {
            while let Some(message) = self.received.pop_front() {
                if message.get("method").is_some() {
                    if let Some(req_id) = message.get("id") {
                        let _ = self.send(&json!({ "jsonrpc": "2.0", "id": req_id, "result": Value::Null })).await;
                    } else {
                        on_notification(&message);
                    }
                } else if message.get("id").and_then(Value::as_u64) == Some(id) {
                    if let Some(err) = message.get("error") {
                        let code = err.get("code").and_then(Value::as_i64).unwrap_or(-1);
                        let msg = err.get("message").and_then(Value::as_str).unwrap_or("LSP error").to_owned();
                        return Ok(Err((code, msg)));
                    }
                    return Ok(Ok(message.get("result").cloned().unwrap_or(Value::Null)));
                }
            }
            self.read_more().await?;
        }
    }

    pub async fn shutdown(mut self) {
        let id = self.next_id;
        self.next_id += 1;
        let _ = tokio::time::timeout(
            Duration::from_millis(300),
            self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": "shutdown", "params": Value::Null })),
        )
        .await;
        let _ = tokio::time::timeout(
            Duration::from_millis(200),
            self.send(&json!({ "jsonrpc": "2.0", "method": "exit", "params": Value::Null })),
        )
        .await;
        if let Some(job) = self.job.take() {
            job.terminate();
        }
        if let Some(mut child) = self.child.take() {
            crate::process::kill_tree(&mut child, Duration::from_secs(1)).await;
        }
    }
}

impl Drop for LspStdioClient {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.terminate();
        }
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            if let Some(pid) = child.id()
                && pid > 1
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
