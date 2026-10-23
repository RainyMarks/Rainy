//! What a file held before the agent's write or edit changed it, kept so the user can undo the change (3.x
//! `snapshots.ts`): `<home>/snapshots/<chat>/<hash of call id>.{json,bin}`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// Why a change could not be undone: `changed`, `unavailable` or `failed`.
#[derive(Debug)]
pub struct RevertFailure {
    pub reason: &'static str,
    pub message: String,
    pub path: String,
}

#[derive(Serialize, Deserialize)]
struct Record {
    version: u8,
    path: String,
    /// Hash of the content before the change; `None` when the change created the file.
    before: Option<String>,
    after: String,
}

fn digest(bytes: &[u8]) -> String {
    crate::text::sha256_hex(bytes)
}

/// Tool call ids come from providers; their hash makes a safe file name.
fn stem(call_id: &str) -> String {
    digest(call_id.as_bytes())[..40].to_owned()
}

pub struct Snapshots {
    root: PathBuf,
}

impl Snapshots {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Record a change after it was written; `before` is `None` when the file did not exist.
    pub fn record(&self, session: &str, call_id: &str, path: &Path, before: Option<&[u8]>, after: &[u8]) -> std::io::Result<()> {
        let directory = self.root.join(session);
        crate::store::create_private_dir(&directory)?;
        let stem = stem(call_id);
        if let Some(before) = before {
            crate::store::write_atomic(&directory.join(format!("{stem}.bin")), before)?;
        }
        let record = Record { version: 1, path: path.to_string_lossy().into_owned(), before: before.map(digest), after: digest(after) };
        crate::store::write_atomic(&directory.join(format!("{stem}.json")), serde_json::to_string(&record)?.as_bytes())
    }

    fn find(&self, sessions: &[String], call_id: &str) -> Option<(PathBuf, Record)> {
        let stem = stem(call_id);
        sessions.iter().find_map(|session| {
            let directory = self.root.join(session);
            let text = std::fs::read(directory.join(format!("{stem}.json"))).ok()?;
            let record: Record = serde_json::from_slice(&text).ok().filter(|record: &Record| record.version == 1)?;
            Some((directory, record))
        })
    }

    /// What a file held before one change: bytes, `Some(None)` when the change created it, `None` when not kept.
    /// `sessions` are the chat and the chats it was forked from, nearest first.
    pub fn original(&self, sessions: &[String], call_id: &str) -> Option<Option<Vec<u8>>> {
        let (directory, record) = self.find(sessions, call_id)?;
        let Some(before) = record.before else { return Some(None) };
        let bytes = std::fs::read(directory.join(format!("{}.bin", stem(call_id)))).ok()?;
        (digest(&bytes) == before).then_some(Some(bytes))
    }

    /// Put a file back to its content before one change, if it still holds what that change wrote.
    pub fn revert(&self, sessions: &[String], call_id: &str) -> Result<String, RevertFailure> {
        let stem = stem(call_id);
        let (directory, record) = self.find(sessions, call_id).ok_or(RevertFailure { reason: "unavailable", path: String::new(), message: "The original content is no longer kept.".into() })?;
        let path = record.path.clone();
        let changed = |message: &str| RevertFailure { reason: "changed", path: path.clone(), message: message.into() };
        let current = std::fs::read(&record.path).map_err(|_| changed("The file was moved or deleted after the change."))?;
        if digest(&current) != record.after {
            return Err(changed("The file changed after the change was made."));
        }
        let failed = |error: std::io::Error| RevertFailure { reason: "failed", path: path.clone(), message: error.to_string() };
        match &record.before {
            None => std::fs::remove_file(&record.path).map_err(failed)?,
            Some(before) => {
                let original = std::fs::read(directory.join(format!("{stem}.bin"))).map_err(failed)?;
                if digest(&original) != *before {
                    return Err(RevertFailure { reason: "unavailable", path, message: "The saved original content is damaged.".into() });
                }
                std::fs::write(&record.path, original).map_err(failed)?;
            }
        }
        let _ = std::fs::remove_file(directory.join(format!("{stem}.json")));
        let _ = std::fs::remove_file(directory.join(format!("{stem}.bin")));
        Ok(record.path)
    }

    /// A chat's snapshots go away with it.
    pub fn remove(&self, session: &str) {
        let _ = std::fs::remove_dir_all(self.root.join(session));
    }

    /// Delete snapshots older than `retention`.
    pub fn sweep(&self, retention: Duration) {
        sweep(&self.root, retention);
    }
}

/// Delete files older than `retention` one level below `root`, and the folders left empty.
pub fn sweep(root: &Path, retention: Duration) {
    let cutoff = SystemTime::now() - retention;
    let Ok(sessions) = std::fs::read_dir(root) else { return };
    for session in sessions.flatten() {
        let directory = session.path();
        for file in std::fs::read_dir(&directory).into_iter().flatten().flatten() {
            if file.metadata().and_then(|info| info.modified()).is_ok_and(|modified| modified < cutoff) {
                let _ = std::fs::remove_file(file.path());
            }
        }
        let _ = std::fs::remove_dir(&directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverts_only_unchanged_files() {
        let root = std::env::temp_dir().join(format!("rainy-snap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("a.txt");
        std::fs::write(&file, "new").unwrap();
        let snapshots = Snapshots::new(root.join("snapshots"));
        snapshots.record("s1", "call|1", &file, Some(b"old"), b"new").unwrap();
        let fork = vec!["s2".to_owned(), "s1".to_owned()];
        assert_eq!(snapshots.original(&fork, "call|1"), Some(Some(b"old".to_vec())));
        std::fs::write(&file, "edited").unwrap();
        assert_eq!(snapshots.revert(&fork, "call|1").unwrap_err().reason, "changed");
        std::fs::write(&file, "new").unwrap();
        snapshots.revert(&fork, "call|1").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        assert!(snapshots.original(&fork, "call|1").is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
