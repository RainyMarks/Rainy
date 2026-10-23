//! Programs in a pseudo-terminal (ConPTY on Windows), for terminals and runs (3.x `spawnTerminal`).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::watch;

use crate::error::{HostError, HostResult};

/// How a program ended.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

/// Decodes UTF-8 across chunk boundaries; bytes that can never be UTF-8 become U+FFFD.
#[derive(Default)]
pub struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    out.push_str(text);
                    self.pending.clear();
                    return out;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    out.push_str(std::str::from_utf8(&self.pending[..valid]).expect("valid prefix"));
                    match error.error_len() {
                        // An incomplete sequence at the end waits for the next chunk.
                        None => {
                            self.pending.drain(..valid);
                            return out;
                        }
                        Some(length) => {
                            out.push('\u{fffd}');
                            self.pending.drain(..valid + length);
                        }
                    }
                }
            }
        }
    }

    pub fn finish(&mut self) -> String {
        let rest = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        rest
    }
}

pub struct Pty {
    pub pid: u32,
    writer: Mutex<Option<Box<dyn std::io::Write + Send>>>,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    exit: watch::Receiver<Option<Outcome>>,
    output_done: watch::Receiver<bool>,
}

impl Pty {
    /// Start `argv` in a new pseudo-terminal of `cols`×`rows`; output arrives on `output` as text.
    pub fn spawn(argv: &[String], cwd: &Path, env: &BTreeMap<OsString, OsString>, cols: u16, rows: u16, output: Arc<dyn Fn(String) + Send + Sync>) -> HostResult<Arc<Self>> {
        let (program, args) = argv.split_first().ok_or_else(|| HostError::invalid("A terminal needs a program."))?;
        let system = native_pty_system();
        let pair = system.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).map_err(|error| HostError::other(format!("The terminal could not be created: {error}")))?;
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.cwd(cwd);
        command.env_clear();
        for (name, value) in env {
            command.env(name, value);
        }
        command.env("TERM", "xterm-256color");
        let mut child = pair.slave.spawn_command(command).map_err(|error| HostError::new("dependency-unavailable", format!("The program could not start: {error}")))?;
        drop(pair.slave);
        let pid = child.process_id().unwrap_or(0);
        let mut reader = pair.master.try_clone_reader().map_err(|error| HostError::other(error.to_string()))?;
        let writer = pair.master.take_writer().map_err(|error| HostError::other(error.to_string()))?;
        let master = Arc::new(Mutex::new(Some(pair.master)));
        let (exit_sender, exit) = watch::channel(None);
        let (done_sender, output_done) = watch::channel(false);
        std::thread::spawn(move || {
            let mut decoder = Utf8Stream::default();
            let mut buffer = [0u8; 16384];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let text = decoder.push(&buffer[..read]);
                        if !text.is_empty() {
                            output(text);
                        }
                    }
                }
            }
            let rest = decoder.finish();
            if !rest.is_empty() {
                output(rest);
            }
            let _ = done_sender.send(true);
        });
        let closing = master.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            let outcome = match status {
                Ok(status) => Outcome { exit_code: Some(status.exit_code() as i32), signal: status.signal().map(|signal| format!("SIG{}", signal.trim_start_matches("SIG").to_uppercase())) },
                Err(_) => Outcome::default(),
            };
            // Closing the pseudo-console flushes its last output and ends the reader on Windows.
            std::thread::sleep(std::time::Duration::from_millis(50));
            closing.lock().unwrap().take();
            let _ = exit_sender.send(Some(outcome));
        });
        Ok(Arc::new(Self { pid, writer: Mutex::new(Some(writer)), master, exit, output_done }))
    }

    pub fn write(&self, data: &str) -> HostResult<()> {
        let mut writer = self.writer.lock().unwrap();
        let writer = writer.as_mut().ok_or_else(|| HostError::other("This terminal is not running."))?;
        writer.write_all(data.as_bytes()).and_then(|()| writer.flush()).map_err(|error| HostError::other(error.to_string()))
    }

    /// A size change; one reaching a program that already ended does nothing.
    pub fn resize(&self, cols: u16, rows: u16) {
        if let Some(master) = self.master.lock().unwrap().as_ref() {
            let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }

    /// Wait for the program to end and for its last output.
    pub async fn wait(&self) -> Outcome {
        let mut exit = self.exit.clone();
        let outcome = loop {
            if let Some(outcome) = exit.borrow().clone() {
                break outcome;
            }
            if exit.changed().await.is_err() {
                break Outcome::default();
            }
        };
        let mut done = self.output_done.clone();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), done.wait_for(|done| *done)).await;
        outcome
    }

    pub fn running(&self) -> bool {
        self.exit.borrow().is_none()
    }

    /// Stop the terminal's process tree and wait for it.
    pub async fn terminate(&self) {
        if self.running() && self.pid != 0 {
            crate::process::kill_pid_tree(self.pid).await;
        }
        self.writer.lock().unwrap().take();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), self.wait()).await;
        self.master.lock().unwrap().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_split_characters() {
        let mut stream = Utf8Stream::default();
        let bytes = "雨天".as_bytes();
        assert_eq!(stream.push(&bytes[..2]), "");
        assert_eq!(stream.push(&bytes[2..4]), "雨");
        assert_eq!(stream.push(&bytes[4..]), "天");
        assert_eq!(stream.push(b"a\xffb"), "a\u{fffd}b");
    }
}
