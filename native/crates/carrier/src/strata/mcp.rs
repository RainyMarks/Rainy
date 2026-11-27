//! Strata's management server, `tools\strata_mcp.py` (the one Strata's own app reads its model list through), over its
//! standard input and output: the model catalog with what fits this computer, model downloads that run in the
//! background with their progress, and removing a model. It keeps the hardware it read for ten minutes, so a session
//! stays open between calls and ends after a while without any.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};

use super::process::{Owned, Terminal, contain};
use crate::locale::localized;

const PROTOCOL: &str = "2025-06-18";

/// Concurrent handle that aborts an in-flight management server call and terminates its process tree without waiting
/// on any session RPC mutex.
#[derive(Clone)]
pub(crate) struct AbortHandle {
    owned: Option<Arc<Owned>>,
    aborted: Arc<AtomicBool>,
}

impl AbortHandle {
    pub(crate) fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
        if let Some(owned) = &self.owned {
            owned.stop();
        }
    }

    pub(crate) fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn new_for_test() -> Self {
        Self { owned: None, aborted: Arc::new(AtomicBool::new(false)) }
    }
}

pub struct Session {
    owned: Arc<Owned>,
    aborted: Arc<AtomicBool>,
    stdin: ChildStdin,
    answers: Receiver<Value>,
    next: u64,
    pub last_used: Instant,
    /// The Control folder it runs from.
    pub root: PathBuf,
}

impl Session {
    /// Start the server for the run configs in `install` and agree on the protocol.
    pub fn start(terminal: &Terminal, install: &Path, log: &Path) -> Result<Self> {
        Self::start_with_hook(terminal, install, log, |_| {})
    }

    /// Start the server and hand its [`AbortHandle`] to `on_started` before the initial `initialize` handshake waits,
    /// so `close()` can interrupt even a slow first startup immediately.
    pub(crate) fn start_with_hook(
        terminal: &Terminal,
        install: &Path,
        log: &Path,
        on_started: impl FnOnce(AbortHandle),
    ) -> Result<Self> {
        std::fs::create_dir_all(install)?;
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let errors = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
        let mut command = terminal.management(install);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(errors);
        let mut child = command.spawn().with_context(|| format!("Starting {}", terminal.python.display()))?;
        contain(&child);
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let owned = Arc::new(Owned::new(child));
        let aborted = Arc::new(AtomicBool::new(false));
        let stdin = stdin.context("No input of Strata's management server")?;
        let stdout = stdout.context("No output of Strata's management server")?;
        let (sender, answers) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("strata-management".into()).spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut raw = Vec::new();
            loop {
                raw.clear();
                match reader.read_until(b'\n', &mut raw) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&raw);
                        match serde_json::from_str::<Value>(line.trim()) {
                            Ok(message) => {
                                if sender.send(message).is_err() {
                                    break;
                                }
                            }
                            Err(_) => log::warn!("Strata's management server wrote a line that is not JSON"),
                        }
                    }
                }
            }
        })?;
        let mut session = Self {
            owned: owned.clone(),
            aborted: aborted.clone(),
            stdin,
            answers,
            next: 0,
            last_used: Instant::now(),
            root: terminal.root.clone(),
        };
        on_started(AbortHandle { owned: Some(owned), aborted });
        // A fresh interpreter can wait minutes for the antivirus on its first start.
        session.request(
            "initialize",
            json!({ "protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": { "name": "Rainy", "version": env!("CARGO_PKG_VERSION") } }),
            Duration::from_secs(300),
        )?;
        session.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(session)
    }

    pub(crate) fn abort_handle(&self) -> AbortHandle {
        AbortHandle {
            owned: Some(self.owned.clone()),
            aborted: self.aborted.clone(),
        }
    }

    pub fn alive(&self) -> bool {
        !self.aborted.load(Ordering::SeqCst) && !self.owned.finished()
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.stdin.write_all(&line)?;
        self.stdin.flush()?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        if self.aborted.load(Ordering::SeqCst) {
            bail!(localized("Strata 的管理服务已停止。", "Strata's management server was stopped."));
        }
        self.next += 1;
        let id = self.next;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        self.last_used = Instant::now();
        let deadline = Instant::now() + timeout;
        loop {
            if self.aborted.load(Ordering::SeqCst) {
                bail!(localized("Strata 的管理服务已停止。", "Strata's management server was stopped."));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!(localized("Strata 的管理服务没有及时回答。", "Strata's management server did not answer in time."));
            }
            match self.answers.recv_timeout(left.min(Duration::from_millis(200))) {
                // Answers of an earlier call that timed out, and notifications, are passed over.
                Ok(message) if message.get("id").and_then(Value::as_u64) == Some(id) => {
                    self.last_used = Instant::now();
                    if let Some(error) = message.get("error") {
                        bail!(error.get("message").and_then(Value::as_str).unwrap_or("Strata's management server refused the request").to_owned());
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    bail!(localized("Strata 的管理服务意外退出了。", "Strata's management server ended unexpectedly."))
                }
            }
        }
    }

    /// Call a tool: its answer, or the error it reported.
    pub fn call(&mut self, tool: &str, arguments: Value, timeout: Duration) -> Result<Value> {
        let result = self.request("tools/call", json!({ "name": tool, "arguments": arguments }), timeout)?;
        let content = result.get("structuredContent").cloned().filter(Value::is_object).or_else(|| {
            let text = result.get("content")?.as_array()?.iter().find_map(|item| item.get("text").and_then(Value::as_str))?;
            serde_json::from_str::<Value>(text).ok()
        });
        let content = content.ok_or_else(|| anyhow!("Strata's management server gave no answer to {tool}"))?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            bail!(content.get("error").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| content.to_string()));
        }
        Ok(content)
    }

    pub fn stop(self) {
        self.aborted.store(true, Ordering::SeqCst);
        drop(self.stdin);
        if self.owned.wait(Duration::from_secs(5)).is_none() {
            self.owned.stop();
        }
    }
}
