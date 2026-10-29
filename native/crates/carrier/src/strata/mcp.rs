//! Strata's management server, `tools\strata_mcp.py` (the one Strata's own app reads its model list through), over its
//! standard input and output: the model catalog with what fits this computer, model downloads that run in the
//! background with their progress, and removing a model. It keeps the hardware it read for ten minutes, so a session
//! stays open between calls and ends after a while without any.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};

use super::process::{Owned, Terminal, contain};
use crate::locale::localized;

const PROTOCOL: &str = "2025-06-18";

pub struct Session {
    owned: Owned,
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
        std::fs::create_dir_all(install)?;
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let errors = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
        let mut command = terminal.management(install);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(errors);
        let mut child = command.spawn().with_context(|| format!("Starting {}", terminal.python.display()))?;
        contain(&child);
        let stdin = child.stdin.take().context("No input of Strata's management server")?;
        let stdout = child.stdout.take().context("No output of Strata's management server")?;
        let (sender, answers) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("strata-management".into()).spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                match serde_json::from_str::<Value>(&line) {
                    Ok(message) => {
                        if sender.send(message).is_err() {
                            break;
                        }
                    }
                    Err(_) => log::warn!("Strata's management server wrote a line that is not JSON"),
                }
            }
        })?;
        let mut session = Self { owned: Owned::new(child), stdin, answers, next: 0, last_used: Instant::now(), root: terminal.root.clone() };
        // A fresh interpreter can wait minutes for the antivirus on its first start.
        session.request("initialize", json!({ "protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": { "name": "Rainy", "version": env!("CARGO_PKG_VERSION") } }), Duration::from_secs(300))?;
        session.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(session)
    }

    pub fn alive(&self) -> bool {
        !self.owned.finished()
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.stdin.write_all(&line)?;
        self.stdin.flush()?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        self.last_used = Instant::now();
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!(localized("Strata 的管理服务没有及时回答。", "Strata's management server did not answer in time."));
            }
            match self.answers.recv_timeout(left.min(Duration::from_millis(500))) {
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
        drop(self.stdin);
        if self.owned.wait(Duration::from_secs(5)).is_none() {
            self.owned.stop();
        }
    }
}
