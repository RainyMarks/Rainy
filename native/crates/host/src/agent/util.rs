//! Shared internal utilities for the agent engine: UTF-8 safe truncation, incremental JSONL tail reading,
//! bounded HTTP body reading and strict child-process environment scrubbing.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

use futures::StreamExt as _;

/// Largest valid UTF-8 character boundary at or before `max_bytes`.
pub fn floor_char_boundary(text: &str, max_bytes: usize) -> usize {
    if max_bytes >= text.len() {
        return text.len();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Smallest valid UTF-8 character boundary at or after `min_bytes`.
pub fn ceil_char_boundary(text: &str, min_bytes: usize) -> usize {
    if min_bytes >= text.len() {
        return text.len();
    }
    let mut start = min_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

/// Truncate `text` to at most `max_bytes` UTF-8 bytes without splitting a character.
pub fn truncate_bytes_slice(text: &str, max_bytes: usize) -> &str {
    &text[..floor_char_boundary(text, max_bytes)]
}

/// Truncate `text` to at most `max_bytes` UTF-8 bytes, appending `…` when cut.
pub fn truncate_bytes_ellipsis(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    format!("{}…", truncate_bytes_slice(text, max_bytes))
}

/// Read an HTTP response body up to `max_bytes` bytes (stopping early if the server streams more).
pub async fn read_http_body_limited(response: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>, reqwest::Error> {
    let mut body = response.bytes_stream();
    let mut kept = Vec::new();
    while kept.len() < max_bytes {
        let Some(chunk) = body.next().await else { break };
        let chunk = chunk?;
        let room = max_bytes - kept.len();
        kept.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
    Ok(kept)
}

/// Additional sensitive environment variable filter layered on top of `crate::process::sensitive` for agent
/// child processes (MCP servers, hooks): strips internal `RAINY_*` host variables and credential/auth names.
pub fn extra_sensitive_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if upper == "RAINY_PHP_EXTENSION_DIR" {
        return false;
    }
    if upper.starts_with("RAINY_") {
        return true;
    }
    if upper.contains("CREDENTIAL") || upper.contains("AUTHORIZATION") || upper.contains("COOKIE") {
        return true;
    }
    for token in ["AUTH", "PASS", "PWD"] {
        if upper == token || upper.starts_with(&format!("{token}_")) || upper.ends_with(&format!("_{token}")) || upper.contains(&format!("_{token}_")) {
            return true;
        }
    }
    false
}

/// Child environment starting from `crate::process::scrubbed_env()`, further stripping internal `RAINY_*` and
/// extra credential-shaped variables, then applying `overlay` explicitly configured for the child.
pub fn scrubbed_child_env(overlay: &BTreeMap<String, String>) -> BTreeMap<OsString, OsString> {
    let mut env = crate::process::scrubbed_env();
    env.retain(|name, _| !extra_sensitive_env(&name.to_string_lossy()));
    for (name, value) in overlay {
        if cfg!(windows) {
            env.retain(|known, _| !known.to_string_lossy().eq_ignore_ascii_case(name));
        }
        env.insert(name.into(), value.into());
    }
    env
}

/// Platform file identity when available (Unix `(dev, ino)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileId {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

fn file_id(_metadata: &std::fs::Metadata) -> Option<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Some(FileId { dev: _metadata.dev(), ino: _metadata.ino() })
    }
    #[cfg(not(unix))]
    None
}

/// Incremental reader for append-only JSONL files that detects truncation, atomic replacement and in-place
/// same-length overwrites reliably on both Windows and Unix.
#[derive(Default)]
pub struct JsonlTail {
    read: u64,
    head: Vec<u8>,
    tail_guard: Vec<u8>,
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
    file_id: Option<FileId>,
}

/// Result of advancing a [`JsonlTail`].
pub struct TailUpdate {
    /// True when the file was rewritten or truncated and caller state must be reset before ingesting `added`.
    pub reset: bool,
    /// Complete `\n`-terminated text appended since the last offset (empty when nothing new is ready).
    pub added: String,
    /// New byte offset after the last complete line.
    pub offset: u64,
}

impl JsonlTail {
    pub fn update(&mut self, path: &Path) -> std::io::Result<TailUpdate> {
        let mut file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        let length = metadata.len();
        let created = metadata.created().ok();
        let modified = metadata.modified().ok();
        let id = file_id(&metadata);

        // Fast path: exact same length, timestamps and file identity.
        if self.read > 0 && length == self.read && modified == self.modified && created == self.created && id == self.file_id {
            return Ok(TailUpdate { reset: false, added: String::new(), offset: self.read });
        }

        let mut head = vec![0u8; length.min(64) as usize];
        file.read_exact(&mut head)?;
        let shared = self.head.len().min(head.len());

        let mut reset = false;
        if self.read > 0 {
            let identity_changed = id != self.file_id || created != self.created || head[..shared] != self.head[..shared];
            let truncated = length < self.read;
            // Same length with a changed mtime means an in-place or equal-length atomic rewrite.
            let equal_length_rewritten = length == self.read && modified != self.modified;
            let tail_changed = if !truncated && !identity_changed && !equal_length_rewritten && !self.tail_guard.is_empty() {
                let start = self.read.saturating_sub(self.tail_guard.len() as u64);
                file.seek(SeekFrom::Start(start))?;
                let mut current_tail = vec![0u8; self.tail_guard.len()];
                file.read_exact(&mut current_tail).is_err() || current_tail != self.tail_guard
            } else {
                false
            };
            if truncated || identity_changed || equal_length_rewritten || tail_changed {
                *self = Self::default();
                reset = true;
            }
        }

        self.head = head;
        self.created = created;
        self.modified = modified;
        self.file_id = id;

        if length == self.read {
            return Ok(TailUpdate { reset, added: String::new(), offset: self.read });
        }

        file.seek(SeekFrom::Start(self.read))?;
        let mut added = Vec::new();
        (&mut file).take(length - self.read).read_to_end(&mut added)?;
        // A partial line still being written is deferred until its trailing `\n` lands.
        let Some(end) = added.iter().rposition(|byte| *byte == b'\n') else {
            return Ok(TailUpdate { reset, added: String::new(), offset: self.read });
        };
        let consumed = end + 1;
        let new_offset = self.read + consumed as u64;
        if consumed >= 64 {
            self.tail_guard = added[consumed - 64..consumed].to_vec();
        } else {
            let start = new_offset.saturating_sub(64);
            let mut guard = vec![0u8; (new_offset - start) as usize];
            if file.seek(SeekFrom::Start(start)).is_ok() && file.read_exact(&mut guard).is_ok() {
                self.tail_guard = guard;
            }
        }
        self.read = new_offset;
        let text = String::from_utf8_lossy(&added[..end]).into_owned();
        Ok(TailUpdate { reset, added: text, offset: self.read })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_utf8_without_splitting_multibyte_chars() {
        let text = "你好世界abc";
        assert_eq!(truncate_bytes_slice(text, 5), "你");
        assert_eq!(truncate_bytes_slice(text, 6), "你好");
        assert_eq!(truncate_bytes_ellipsis(text, 6), "你好…");
        assert_eq!(truncate_bytes_ellipsis(text, 20), text);
    }

    #[test]
    fn jsonl_tail_detects_append_and_equal_length_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("chat.jsonl");
        std::fs::write(&file, "{\"type\":\"header\",\"id\":\"1\"}\n{\"a\":1}\n").unwrap();
        let mut tail = JsonlTail::default();
        let first = tail.update(&file).unwrap();
        assert!(!first.reset);
        assert!(first.added.contains("{\"a\":1}"));
        let off1 = first.offset;

        // Appending only yields the newly appended line.
        let mut f = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
        std::io::Write::write_all(&mut f, b"{\"b\":2}\n").unwrap();
        drop(f);
        let second = tail.update(&file).unwrap();
        assert!(!second.reset);
        assert_eq!(second.added, "{\"b\":2}");
        assert!(second.offset > off1);

        // Overwriting earlier content while keeping header identical and length >= previous triggers reset.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&file, "{\"type\":\"header\",\"id\":\"1\"}\n{\"a\":9}\n{\"b\":8}\n{\"c\":3}\n").unwrap();
        let third = tail.update(&file).unwrap();
        assert!(third.reset);
        assert!(third.added.contains("{\"a\":9}") && third.added.contains("{\"c\":3}"));
    }

    #[test]
    fn extra_sensitive_env_strips_rainy_internal_and_auth_vars() {
        assert!(extra_sensitive_env("RAINY_HOME"));
        assert!(extra_sensitive_env("RAINY_PORT"));
        assert!(extra_sensitive_env("RAINY_SSH_EXTRA_ENV"));
        assert!(!extra_sensitive_env("RAINY_PHP_EXTENSION_DIR"));
        assert!(extra_sensitive_env("MYSQL_PWD"));
        assert!(extra_sensitive_env("GOOGLE_APPLICATION_CREDENTIALS"));
        assert!(extra_sensitive_env("HTTP_AUTHORIZATION"));
        assert!(extra_sensitive_env("SESSION_COOKIE"));
        assert!(!extra_sensitive_env("PATH"));
        assert!(!extra_sensitive_env("COMPASS"));
        assert!(!extra_sensitive_env("AUTHOR"));
    }
}
