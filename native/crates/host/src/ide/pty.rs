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
    /// True only while decoding the current contiguous non-ASCII run after invalid UTF-8 was seen; resets to `false`
    /// as soon as an ASCII/control byte (`< 0x80`) arrives so subsequent UTF-8 output in the same terminal stays UTF-8.
    in_legacy_run: bool,
}

impl Utf8Stream {
    /// Stream decoder configured for local console/ConPTY output (UTF-8 with Windows OEMCP fallback).
    pub fn for_console() -> Self {
        Self::with_legacy(crate::process::legacy_encoding())
    }

    pub fn with_legacy(legacy: Option<&'static encoding_rs::Encoding>) -> Self {
        Self { pending: Vec::new(), legacy, in_legacy_run: false }
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
            if self.in_legacy_run {
                if let Some(encoding) = self.legacy {
                    if self.pending[0] < 0x80 {
                        // End of contiguous legacy high-byte run: resume UTF-8 preference for subsequent runs.
                        self.in_legacy_run = false;
                        continue;
                    }
                    if !self.decode_legacy_run(encoding, eof, &mut out) {
                        return out;
                    }
                    continue;
                }
                self.in_legacy_run = false;
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
                self.in_legacy_run = false;
                break;
            }
            if self.pending.len() < 2 {
                if !eof {
                    return false;
                }
                out.push('\u{fffd}');
                self.pending.clear();
                self.in_legacy_run = false;
                return true;
            }
            let (pair, had_errors) = encoding.decode_without_bom_handling(&self.pending[..2]);
            if !had_errors {
                self.in_legacy_run = true;
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
                        self.in_legacy_run = true;
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

const PTY_WRITE_QUEUE_CAPACITY: usize = 64;

pub struct Pty {
    pub pid: u32,
    job: Option<crate::process::ProcessJob>,
    writer_tx: Mutex<Option<std::sync::mpsc::SyncSender<Vec<u8>>>>,
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
        let mut reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => {
                if let Some(job) = &job {
                    job.terminate();
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(HostError::other(error.to_string()));
            }
        };
        let mut writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(error) => {
                if let Some(job) = &job {
                    job.terminate();
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(HostError::other(error.to_string()));
            }
        };
        let (writer_tx, writer_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(PTY_WRITE_QUEUE_CAPACITY);
        std::thread::spawn(move || {
            while let Ok(bytes) = writer_rx.recv() {
                for chunk in bytes.chunks(16 * 1024) {
                    if writer.write_all(chunk).is_err() {
                        return;
                    }
                }
                if writer.flush().is_err() {
                    return;
                }
            }
        });
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
                Ok(status) => {
                    let signal = status.signal().map(|signal| format!("SIG{}", signal.trim_start_matches("SIG").to_uppercase()));
                    let exit_code = if signal.is_some() { None } else { Some(status.exit_code() as i32) };
                    Outcome { exit_code, signal }
                }
                Err(_) => Outcome::default(),
            };
            // Closing the pseudo-console flushes its last output and ends the reader on Windows.
            std::thread::sleep(std::time::Duration::from_millis(50));
            closing.lock().unwrap().take();
            let _ = exit_sender.send(Some(outcome));
        });
        Ok(Arc::new(Self { pid, job, writer_tx: Mutex::new(Some(writer_tx)), master, exit, output_done }))
    }

    pub fn write(&self, data: &str) -> HostResult<()> {
        let tx = self.writer_tx.lock().unwrap().clone().ok_or_else(|| HostError::other("This terminal is not running."))?;
        tx.try_send(data.as_bytes().to_vec()).map_err(|err| match err {
            std::sync::mpsc::TrySendError::Full(_) => HostError::other("The terminal input buffer is full; retry shortly."),
            std::sync::mpsc::TrySendError::Disconnected(_) => HostError::other("This terminal is not running."),
        })
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
        // Drop the writer channel first so the writer thread exits without blocking on a full ConPTY pipe.
        self.writer_tx.lock().unwrap().take();
        if let Some(job) = &self.job {
            job.terminate();
        }
        if self.running() && self.pid != 0 {
            crate::process::kill_pid_tree(self.pid).await;
        }
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

        // After a legacy GBK segment ends at an ASCII boundary, subsequent UTF-8 output must still decode as UTF-8.
        let mut mixed = Utf8Stream::with_legacy(Some(encoding_rs::GBK));
        let (gbk_line, _, _) = encoding_rs::GBK.encode("联通目录\n");
        let mut mixed_out = mixed.push(&gbk_line);
        mixed_out.push_str(&mixed.push("接下来是中文 UTF-8 🎉\n".as_bytes()));
        mixed_out.push_str(&mixed.finish());
        assert_eq!(mixed_out, "联通目录\n接下来是中文 UTF-8 🎉\n");
    }
}
