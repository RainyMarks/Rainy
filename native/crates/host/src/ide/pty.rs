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

/// Decodes UTF-8 across chunk boundaries, with optional Windows OEMCP (GBK/BIG5/Shift-JIS/EUC-KR) fallback when a
/// console program emits legacy code-page bytes instead of UTF-8.
#[derive(Default)]
pub struct Utf8Stream {
    pending: Vec<u8>,
    legacy: Option<&'static encoding_rs::Encoding>,
    /// Once invalid UTF-8 is observed and decoded via `legacy`, keep decoding high-byte runs with the legacy code page
    /// so subsequent 2-byte CJK characters split across chunks (e.g. GBK "通" 0xCD 0xA8, "目录" 0xC4 0xBF 0xC2 0xBC)
    /// are not mistaken for valid 2-byte UTF-8 Latin/diacritic code points.
    legacy_mode: bool,
}

impl Utf8Stream {
    /// Stream decoder configured for local console/ConPTY output (UTF-8 with Windows OEMCP fallback).
    pub fn for_console() -> Self {
        Self::with_legacy(crate::process::legacy_encoding())
    }

    pub fn with_legacy(legacy: Option<&'static encoding_rs::Encoding>) -> Self {
        Self { pending: Vec::new(), legacy, legacy_mode: false }
    }

    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        self.drain(false)
    }

    pub fn finish(&mut self) -> String {
        self.drain(true)
    }

    fn drain(&mut self, eof: bool) -> String {
        let mut out = String::new();
        loop {
            if self.pending.is_empty() {
                return out;
            }
            if self.legacy_mode {
                if let Some(encoding) = self.legacy {
                    // Drain any leading ASCII / ANSI escape bytes directly.
                    let ascii_end = self.pending.iter().position(|&b| b >= 0x80).unwrap_or(self.pending.len());
                    if ascii_end > 0 {
                        out.push_str(std::str::from_utf8(&self.pending[..ascii_end]).expect("ascii"));
                        self.pending.drain(..ascii_end);
                        if self.pending.is_empty() {
                            return out;
                        }
                    }
                    if !self.decode_legacy_run(encoding, eof, &mut out) {
                        return out;
                    }
                    continue;
                }
            }
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    out.push_str(text);
                    self.pending.clear();
                    return out;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        // If a legacy code page is active and the valid prefix ends in a high-byte run immediately
                        // followed by an invalid UTF-8 byte (e.g. GBK "目录" + another GBK char), backtrack to the
                        // start of that contiguous high-byte run so the whole run decodes consistently as legacy.
                        let emit_until = if self.legacy.is_some() && error.error_len().is_some() {
                            self.pending[..valid].iter().rposition(|&b| b < 0x80).map_or(0, |pos| pos + 1)
                        } else {
                            valid
                        };
                        if emit_until > 0 {
                            out.push_str(std::str::from_utf8(&self.pending[..emit_until]).expect("valid prefix"));
                            self.pending.drain(..emit_until);
                        }
                    }
                    match error.error_len() {
                        None if !eof => return out,
                        None => {
                            if let Some(encoding) = self.legacy {
                                let (decoded, _) = encoding.decode_without_bom_handling(&self.pending);
                                out.push_str(&decoded);
                            } else {
                                out.push_str(&String::from_utf8_lossy(&self.pending));
                            }
                            self.pending.clear();
                            return out;
                        }
                        Some(length) => {
                            let Some(encoding) = self.legacy else {
                                out.push('\u{fffd}');
                                self.pending.drain(..length);
                                continue;
                            };
                            if !self.decode_legacy_run(encoding, eof, &mut out) {
                                return out;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Returns `false` when more bytes are needed before deciding the trailing sequence.
    fn decode_legacy_run(&mut self, encoding: &'static encoding_rs::Encoding, eof: bool, out: &mut String) -> bool {
        while let Some(&first) = self.pending.first() {
            if first < 0x80 {
                break;
            }
            if self.pending.len() < 2 {
                if !eof {
                    return false;
                }
                out.push('\u{fffd}');
                self.pending.clear();
                return true;
            }
            let (pair, had_errors) = encoding.decode_without_bom_handling(&self.pending[..2]);
            if !had_errors {
                self.legacy_mode = true;
                out.push_str(&pair);
                self.pending.drain(..2);
                continue;
            }
            // GB18030 / GBK 4-byte sequence (second byte in 0x30..=0x39).
            if encoding == encoding_rs::GBK && (0x30..=0x39).contains(&self.pending[1]) {
                if self.pending.len() < 4 {
                    if !eof {
                        return false;
                    }
                } else {
                    let (quad, quad_err) = encoding.decode_without_bom_handling(&self.pending[..4]);
                    if !quad_err {
                        self.legacy_mode = true;
                        out.push_str(&quad);
                        self.pending.drain(..4);
                        continue;
                    }
                }
            }
            out.push('\u{fffd}');
            self.pending.drain(..1);
        }
        true
    }
}

pub struct Pty {
    pub pid: u32,
    job: Option<crate::process::ProcessJob>,
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
        let job = crate::process::ProcessJob::new();
        if pid != 0 {
            if !job.as_ref().is_some_and(|job| job.assign_pid(pid)) {
                crate::process::contain_pid(pid);
            }
        }
        let mut reader = pair.master.try_clone_reader().map_err(|error| HostError::other(error.to_string()))?;
        let writer = pair.master.take_writer().map_err(|error| HostError::other(error.to_string()))?;
        let master = Arc::new(Mutex::new(Some(pair.master)));
        let (exit_sender, exit) = watch::channel(None);
        let (done_sender, output_done) = watch::channel(false);
        std::thread::spawn(move || {
            let mut decoder = Utf8Stream::for_console();
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
        Ok(Arc::new(Self { pid, job, writer: Mutex::new(Some(writer)), master, exit, output_done }))
    }

    pub fn write(&self, data: &str) -> HostResult<()> {
        let mut writer = self.writer.lock().unwrap();
        let writer = writer.as_mut().ok_or_else(|| HostError::other("This terminal is not running."))?;
        // Write large pastes in bounded chunks to avoid saturating the ConPTY/PTY input buffer in one call.
        for chunk in data.as_bytes().chunks(16 * 1024) {
            writer.write_all(chunk).map_err(|error| HostError::other(error.to_string()))?;
        }
        writer.flush().map_err(|error| HostError::other(error.to_string()))
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
        if let Some(job) = &self.job {
            job.terminate();
        }
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

    #[test]
    fn decodes_gbk_conpty_output_across_chunk_boundaries_and_ansi() {
        let (encoded, _, had_errors) = encoding_rs::GBK.encode("\x1b[32m联通测试目录\x1b[0m OK");
        assert!(!had_errors);
        // Feed one byte at a time to stress chunk boundaries on multi-byte GBK sequences.
        let mut stream = Utf8Stream::with_legacy(Some(encoding_rs::GBK));
        let mut collected = String::new();
        for byte in encoded.iter() {
            collected.push_str(&stream.push(&[*byte]));
        }
        collected.push_str(&stream.finish());
        assert_eq!(collected, "\x1b[32m联通测试目录\x1b[0m OK");

        // Pure UTF-8 still decodes cleanly even with GBK fallback enabled.
        let mut utf8_stream = Utf8Stream::with_legacy(Some(encoding_rs::GBK));
        let utf8 = "你好世界 UTF-8 🚀".as_bytes();
        let mut utf8_out = String::new();
        for chunk in utf8.chunks(2) {
            utf8_out.push_str(&utf8_stream.push(chunk));
        }
        utf8_out.push_str(&utf8_stream.finish());
        assert_eq!(utf8_out, "你好世界 UTF-8 🚀");
    }
}
