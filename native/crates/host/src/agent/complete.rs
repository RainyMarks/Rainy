//! `@` completion (3.x `complete.ts`): files and folders under a working directory, and other chats.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use serde_json::{Value, json};

use super::store::StoredChat;

const MAX_RESULTS: usize = 20;
const MAX_ENTRIES: usize = 50_000;
/// Index entries are reused for this long.
const INDEX_TTL_MS: u64 = 10_000;
/// Maximum cached directory trees held in memory (LRU evicted).
const MAX_CACHED_INDEXES: usize = 8;
const EXCLUDED: [&str; 15] =
    [".git", "node_modules", "dist", "build", "out", "coverage", "target", ".next", ".nuxt", ".turbo", ".venv", "__pycache__", ".pytest_cache", ".mypy_cache", ".gradle"];

#[derive(Clone)]
struct Indexed {
    /// Relative to the root, with `/`.
    path: String,
    /// Lowercased `path` cached at index build time to avoid re-lowercasing 50k paths per keystroke.
    lower: String,
    directory: bool,
}

struct CachedIndex {
    built_at: u64,
    last_used: u64,
    entries: Arc<Vec<Indexed>>,
}

static INDEXES: LazyLock<Mutex<HashMap<PathBuf, CachedIndex>>> = LazyLock::new(Mutex::default);

fn index(root: &Path) -> Arc<Vec<Indexed>> {
    let now = crate::text::now_ms();
    {
        let mut cache = INDEXES.lock().unwrap();
        if let Some(entry) = cache.get_mut(root)
            && now.saturating_sub(entry.built_at) < INDEX_TTL_MS
        {
            entry.last_used = now;
            return entry.entries.clone();
        }
    }
    let mut found = Vec::new();
    let mut builder = ignore::WalkBuilder::new(root);
    builder.hidden(false).follow_links(false).require_git(false);
    builder.filter_entry(|entry| {
        if entry.depth() == 0 {
            return true;
        }
        if entry.file_type().is_some_and(|kind| kind.is_dir()) {
            let name = entry.file_name().to_string_lossy();
            return !EXCLUDED.contains(&name.as_ref());
        }
        true
    });
    for entry in builder.build().flatten() {
        if entry.depth() == 0 {
            continue;
        }
        if found.len() >= MAX_ENTRIES {
            break;
        }
        let Some(kind) = entry.file_type() else { continue };
        let Some(relative) = crate::paths::relative_to(root, entry.path()).filter(|rel| !rel.is_empty()) else {
            continue;
        };
        if kind.is_dir() {
            let lower = relative.to_lowercase();
            found.push(Indexed { path: relative, lower, directory: true });
        } else if kind.is_file() {
            let lower = relative.to_lowercase();
            found.push(Indexed { path: relative, lower, directory: false });
        }
    }
    let entries = Arc::new(found);
    let mut cache = INDEXES.lock().unwrap();
    cache.insert(root.to_path_buf(), CachedIndex { built_at: now, last_used: now, entries: entries.clone() });
    while cache.len() > MAX_CACHED_INDEXES {
        let Some(oldest) = cache.iter().min_by_key(|(_, entry)| entry.last_used).map(|(key, _)| key.clone()) else {
            break;
        };
        cache.remove(&oldest);
    }
    entries
}

fn score(lower: &str, query_lower: &str) -> Option<u8> {
    let name = lower.rsplit('/').next().unwrap_or(lower);
    if name == query_lower {
        return Some(0);
    }
    if name.starts_with(query_lower) {
        return Some(1);
    }
    if name.contains(query_lower) {
        return Some(2);
    }
    if lower.contains(query_lower) {
        return Some(3);
    }
    let mut rest = lower;
    for char in query_lower.chars() {
        let at = rest.find(char)?;
        rest = &rest[at + char.len_utf8()..];
    }
    Some(4)
}

fn quote(path: &str) -> String {
    if path.chars().any(char::is_whitespace) { format!("\"{path}\"") } else { path.to_owned() }
}

/// Files and folders matching a query (at most 20). A query containing `/` lists that folder; otherwise the whole
/// tree is ranked.
pub fn files(root: &Path, query: &str) -> Vec<Value> {
    let normalized = query.replace('\\', "/");
    let normalized = normalized.strip_prefix('"').unwrap_or(&normalized);
    let all = index(root);
    let slash = normalized.rfind('/');
    let matches: Vec<&Indexed> = if normalized.is_empty() || slash.is_some() {
        let (directory, fragment) = match slash {
            Some(at) => (&normalized[..=at], normalized[at + 1..].to_lowercase()),
            None => ("", String::new()),
        };
        let mut matches: Vec<&Indexed> = all
            .iter()
            .filter(|entry| {
                entry.path.strip_prefix(directory).is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
                    && entry.lower.rsplit('/').next().is_some_and(|name| name.starts_with(&fragment))
            })
            .collect();
        matches.sort_by(|left, right| right.directory.cmp(&left.directory).then_with(|| left.path.cmp(&right.path)));
        matches
    } else {
        let query_lower = normalized.to_lowercase();
        let mut ranked: Vec<(u8, &Indexed)> = all.iter().filter_map(|entry| Some((score(&entry.lower, &query_lower)?, entry))).collect();
        ranked.sort_by(|(left_rank, left), (right_rank, right)| left_rank.cmp(right_rank).then(left.path.len().cmp(&right.path.len())).then_with(|| left.path.cmp(&right.path)));
        ranked.into_iter().map(|(_, entry)| entry).collect()
    };
    matches
        .into_iter()
        .take(MAX_RESULTS)
        .map(|entry| {
            let suffix = if entry.directory { "/" } else { "" };
            json!({
                "kind": if entry.directory { "directory" } else { "file" },
                "insert": quote(&format!("{}{suffix}", entry.path)),
                "label": format!("{}{suffix}", entry.path.rsplit('/').next().unwrap_or(&entry.path)),
                "detail": entry.path,
            })
        })
        .collect()
}

/// Other chats matching a query (at most 5), inserted as `@[title](rainy-session:<id>)`.
pub fn chats(chats: &[StoredChat], query: &str, exclude: Option<&str>) -> Vec<Value> {
    let query = query.to_lowercase();
    chats
        .iter()
        .filter(|chat| Some(chat.id.as_str()) != exclude && !chat.title.is_empty() && (query.is_empty() || chat.title.to_lowercase().contains(&query)))
        .take(5)
        .map(|chat| {
            let title: String = chat.title.chars().map(|char| if char == ']' || char == '\n' { ' ' } else { char }).collect();
            let updated = crate::text::iso_time(chat.updated_at as u64);
            json!({ "kind": "session", "insert": format!("[{title}](rainy-session:{})", chat.id), "label": chat.title, "detail": updated.get(..10).unwrap_or(&updated) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_and_lists_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/agent")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/x")).unwrap();
        std::fs::write(dir.path().join("src/agent/session.rs"), "").unwrap();
        std::fs::write(dir.path().join("my notes.md"), "").unwrap();
        let found = files(dir.path(), "sess");
        assert_eq!(found[0]["insert"], "src/agent/session.rs");
        let listed = files(dir.path(), "src/");
        assert_eq!(listed[0]["insert"], "src/agent/");
        let root = files(dir.path(), "");
        assert!(root.iter().any(|item| item["insert"] == "\"my notes.md\""));
        assert!(!root.iter().any(|item| item["detail"].as_str().unwrap().starts_with("node_modules")));
    }

    #[test]
    fn respects_gitignore_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored_dir/\n*.log\n").unwrap();
        std::fs::create_dir_all(dir.path().join("ignored_dir")).unwrap();
        std::fs::write(dir.path().join("ignored_dir/secret.txt"), "").unwrap();
        std::fs::write(dir.path().join("debug.log"), "").unwrap();
        std::fs::write(dir.path().join("visible.rs"), "").unwrap();
        let all = files(dir.path(), "");
        assert!(all.iter().any(|item| item["detail"] == "visible.rs"));
        assert!(!all.iter().any(|item| item["detail"] == "debug.log"));
        assert!(!all.iter().any(|item| item["detail"].as_str().unwrap().starts_with("ignored_dir")));
    }
}
