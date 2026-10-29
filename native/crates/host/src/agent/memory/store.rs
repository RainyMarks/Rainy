//! Versioned project notes shared by the Windows and WSL Hosts of one desktop (3.x `memory/store.ts`):
//! `<carrier state>/project-memory/<project>/memory.v1.json`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::budget::estimate_text;
use crate::error::{HostError, HostResult};
use crate::locale::l;

/// Persisted note length limit.
pub const MAX_NOTE_LENGTH: usize = 2048;

fn default_format() -> u32 {
    4
}

/// Where a note came from: a chat entry on one execution target, and the file it read when there is one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    #[serde(default = "default_format")]
    pub format_version: u32,
    pub session_id: String,
    pub seq: u64,
    pub execution_target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<FileSource>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileSource {
    pub path: String,
    /// `<size>:<mtime in ms>`, as Node.js wrote it.
    pub version: String,
}

fn default_scope() -> String {
    "project".into()
}

/// A generated historical fact; its text carries no instruction authority.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub text: String,
    pub sources: Vec<Source>,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_by_user: Option<bool>,
    /// `project` or `execution-target`.
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn yes() -> bool {
    true
}

/// The validated record on disk; source watermarks keep deleted notes from coming back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub version: u8,
    pub revision: u64,
    pub enabled: bool,
    #[serde(default = "yes")]
    pub generation_enabled: bool,
    pub updated_at: Option<String>,
    pub items: Vec<Item>,
    #[serde(default)]
    pub last_attempt_at: u64,
    pub excluded: Vec<String>,
    pub source_watermarks: BTreeMap<String, u64>,
}

impl Document {
    /// New projects do not organize memory in the background until the user turns it on: it spends model requests.
    pub fn fresh() -> Self {
        Self { version: 1, revision: 0, enabled: true, generation_enabled: false, updated_at: None, items: Vec::new(), last_attempt_at: 0, excluded: Vec::new(), source_watermarks: BTreeMap::new() }
    }

    fn check(&self) -> Result<(), String> {
        let identifier = |text: &str| (1..=160).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte));
        let valid = self.version == 1
            && self.items.len() <= 32
            && self.excluded.len() <= 1024
            && self.excluded.iter().all(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
            && self.items.iter().all(|item| {
                uuid::Uuid::parse_str(&item.id).is_ok()
                    && !item.text.is_empty()
                    && item.text.chars().count() <= MAX_NOTE_LENGTH
                    && (1..=8).contains(&item.sources.len())
                    && item.sources.iter().all(|source| identifier(&source.session_id) && identifier(&source.execution_target_id))
                    && (item.scope == "project" || item.scope == "execution-target")
            });
        if valid { Ok(()) } else { Err("Invalid project memory record.".into()) }
    }
}

/// The hash a deleted note is remembered by, so it is not generated again.
pub fn digest(text: &str) -> String {
    crate::text::sha256_hex(text.trim().split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase())
}

/// Leave out common credential material before model input or note storage.
pub fn redact(text: &str) -> String {
    static PRIVATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----").unwrap());
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(?:sk|ghp|gho|github_pat|xox[baprs])[-_][A-Za-z0-9_-]{16,}\b").unwrap());
    static ASSIGNED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\b(api[_ -]?key|access[_ -]?token|password|secret|authorization)\s*[:=]\s*["']?[^\s,"'}]+"#).unwrap());
    static BEARER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bBearer\s+[a-z0-9._~+/-]+=*").unwrap());
    let text = PRIVATE.replace_all(text, "[private key omitted]");
    let text = TOKEN.replace_all(&text, "[credential omitted]");
    let text = ASSIGNED.replace_all(&text, "${1}=[credential omitted]");
    BEARER.replace_all(&text, "Bearer [credential omitted]").into_owned()
}

/// One source fragment given to the summarizer.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub format_version: u32,
    pub id: String,
    pub session_id: String,
    pub seq: u64,
    pub execution_target_id: String,
    /// `user`, `assistant` or `tool`.
    pub kind: &'static str,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileSource>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProposedNote {
    text: String,
    source_ids: Vec<String>,
    replace_id: Option<String>,
    scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delta {
    notes: Vec<ProposedNote>,
    remove: Vec<String>,
}

/// Admit only notes grounded in the source fragments of this generation; the result still needs a revision check.
pub fn apply_delta(current: &Document, raw: Value, evidence: &[Evidence], max_tokens: u64, now: &str) -> Result<Document, String> {
    let delta: Delta = serde_json::from_value(raw).map_err(|error| format!("Memory generation returned an invalid update: {error}"))?;
    if delta.notes.len() > 8 || delta.remove.len() > 8 {
        return Err("Memory generation returned too many changes.".into());
    }
    let available: HashMap<&str, &Evidence> = evidence.iter().map(|item| (item.id.as_str(), item)).collect();
    let mut existing: Vec<Item> = current.items.clone();
    for id in &delta.remove {
        let at = existing.iter().position(|item| item.id == *id).ok_or("Memory generation refers to an unknown existing note.")?;
        if existing[at].edited_by_user == Some(true) {
            return Err("Generated memory cannot remove a manually edited note.".into());
        }
        existing.remove(at);
    }
    for proposed in delta.notes {
        let text = redact(proposed.text.trim());
        if text.is_empty() || text.chars().count() > 1024 || proposed.source_ids.is_empty() || proposed.source_ids.len() > 8 {
            return Err("Memory generation returned an invalid note.".into());
        }
        if current.excluded.contains(&digest(&text)) {
            continue;
        }
        if !proposed.source_ids.iter().any(|id| available.get(id.as_str()).is_some_and(|source| source.kind == "user" || source.kind == "tool")) {
            return Err("Assistant claims alone cannot become verified project memory.".into());
        }
        let sources = proposed
            .source_ids
            .iter()
            .map(|id| {
                let source = available.get(id.as_str()).ok_or("Memory generation refers to evidence it did not receive.")?;
                Ok(Source { format_version: source.format_version, session_id: source.session_id.clone(), seq: source.seq, execution_target_id: source.execution_target_id.clone(), file: source.file.clone() })
            })
            .collect::<Result<Vec<_>, &str>>()?;
        if let Some(replace) = &proposed.replace_id {
            if !current.items.iter().any(|item| item.id == *replace) {
                return Err("Memory generation replaces an unknown note.".into());
            }
            if existing.iter().any(|item| item.id == *replace && item.edited_by_user == Some(true)) {
                return Err("Generated memory cannot overwrite a manually edited note.".into());
            }
            existing.retain(|item| item.id != *replace);
        }
        let duplicate = existing.iter().find(|item| digest(&item.text) == digest(&text)).cloned();
        if duplicate.as_ref().is_some_and(|item| item.edited_by_user == Some(true)) {
            continue;
        }
        let scope = proposed.scope.filter(|scope| scope == "project" || scope == "execution-target").unwrap_or_else(|| {
            if proposed.source_ids.iter().any(|id| available.get(id.as_str()).is_some_and(|source| source.kind == "tool")) { "execution-target".into() } else { "project".into() }
        });
        let id = duplicate.as_ref().map(|item| item.id.clone()).or(proposed.replace_id).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        existing.retain(|item| item.id != id);
        existing.push(Item { id, text, sources, updated_at: now.to_owned(), edited_by_user: None, scope });
    }
    existing.sort_by(|left, right| {
        right.edited_by_user.unwrap_or(false).cmp(&left.edited_by_user.unwrap_or(false)).then_with(|| right.updated_at.cmp(&left.updated_at)).then_with(|| left.id.cmp(&right.id))
    });
    let mut items = Vec::new();
    let mut tokens = 0;
    for item in existing {
        let size = estimate_text(&item.text);
        if tokens + size > max_tokens || items.len() == 32 {
            continue;
        }
        tokens += size;
        items.push(item);
    }
    let mut watermarks = current.source_watermarks.clone();
    for source in evidence {
        let key = format!("{}/{}", source.execution_target_id, source.session_id);
        let mark = watermarks.entry(key).or_insert(0);
        *mark = (*mark).max(source.seq);
    }
    Ok(Document { items, source_watermarks: watermarks, revision: current.revision + 1, updated_at: Some(now.to_owned()), ..current.clone() })
}

/// The current time as JavaScript's `toISOString` writes it.
pub fn now_iso() -> String {
    crate::text::iso_time(crate::text::now_ms())
}

/// One writer per store; invalid records fail and are never overwritten with empty state.
pub struct Store {
    root: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root, lock: tokio::sync::Mutex::new(()) }
    }

    /// The file of a carrier project.
    pub fn path(&self, project: &str) -> HostResult<PathBuf> {
        let valid = (1..=160).contains(&project.len()) && project.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte)) && project != "." && project != "..";
        if !valid {
            return Err(HostError::invalid("Invalid project identity."));
        }
        Ok(self.root.join(project).join("memory.v1.json"))
    }

    fn read_current(path: &Path) -> HostResult<Document> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let document: Document = serde_json::from_slice(&bytes).map_err(|error| {
                    HostError::other(l(&format!("无法读取项目记忆 {}：{error}", path.display()), &format!("The project memory {} cannot be read: {error}", path.display())))
                })?;
                document.check().map_err(HostError::other)?;
                Ok(document)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Document::fresh()),
            Err(error) => Err(error.into()),
        }
    }

    /// The validated notes, or an empty record for a new project.
    pub async fn read(&self, project: &str) -> HostResult<Document> {
        let path = self.path(project)?;
        let _guard = self.lock.lock().await;
        Self::read_current(&path)
    }

    /// Apply an edit to the latest record and publish it atomically; `None` from the edit leaves it unchanged.
    pub async fn update(&self, project: &str, edit: impl FnOnce(&Document) -> HostResult<Option<Document>>) -> HostResult<Document> {
        let path = self.path(project)?;
        let _guard = self.lock.lock().await;
        let current = Self::read_current(&path)?;
        let Some(next) = edit(&current)? else { return Ok(current) };
        next.check().map_err(HostError::other)?;
        crate::store::write_atomic(&path, format!("{}\n", serde_json::to_string(&next)?).as_bytes())?;
        Ok(next)
    }

    /// Remove one note, or all, keeping source watermarks and remembering the texts so they are not generated again.
    pub async fn remove(&self, project: &str, id: Option<&str>) -> HostResult<Document> {
        self.update(project, |current| {
            let mut excluded = current.excluded.clone();
            for item in current.items.iter().filter(|item| id.is_none_or(|id| item.id == id)) {
                let hash = digest(&item.text);
                if !excluded.contains(&hash) {
                    excluded.push(hash);
                }
            }
            let start = excluded.len().saturating_sub(1024);
            Ok(Some(Document {
                revision: current.revision + 1,
                updated_at: Some(now_iso()),
                excluded: excluded[start..].to_vec(),
                items: current.items.iter().filter(|item| id.is_some_and(|id| item.id != id)).cloned().collect(),
                ..current.clone()
            }))
        })
        .await
    }

    /// Save a correction a background generation cannot overwrite.
    pub async fn edit(&self, project: &str, id: &str, text: &str, expected_revision: u64, max_tokens: u64) -> HostResult<Document> {
        self.update(project, |current| {
            if current.revision != expected_revision {
                return Err(HostError::other(l("项目记忆已更新，请刷新后重试。", "Project memory changed. Refresh and try again.")));
            }
            let old = current.items.iter().find(|item| item.id == id).ok_or_else(|| HostError::other(l("项目记忆条目不存在。", "This project memory note does not exist.")))?;
            let text = redact(text.trim());
            if text.is_empty() {
                return Err(HostError::other(l("项目记忆不能为空。", "A project memory note cannot be empty.")));
            }
            if text.chars().count() > MAX_NOTE_LENGTH {
                return Err(HostError::other(l(&format!("单条项目记忆不能超过 {MAX_NOTE_LENGTH} 个字符。"), &format!("A project memory note can be at most {MAX_NOTE_LENGTH} characters."))));
            }
            let updated_at = now_iso();
            let items: Vec<Item> = current
                .items
                .iter()
                .map(|item| if item.id == id { Item { text: text.clone(), updated_at: updated_at.clone(), edited_by_user: Some(true), ..item.clone() } } else { item.clone() })
                .collect();
            if items.iter().map(|item| estimate_text(&item.text)).sum::<u64>() > max_tokens {
                return Err(HostError::other(l("项目记忆超过总 token 预算，请精简内容。", "Project memory is over its token budget. Shorten the notes.")));
            }
            let mut excluded = current.excluded.clone();
            let hash = digest(&old.text);
            if !excluded.contains(&hash) {
                excluded.push(hash);
            }
            let start = excluded.len().saturating_sub(1024);
            Ok(Some(Document { revision: current.revision + 1, updated_at: Some(updated_at), items, excluded: excluded[start..].to_vec(), ..current.clone() }))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_and_grounds_notes() {
        assert_eq!(redact("password: hunter2 and Bearer abc.def"), "password=[credential omitted] and Bearer [credential omitted]");
        let evidence = vec![Evidence { format_version: 101, id: "s:1".into(), session_id: "s".into(), seq: 1, execution_target_id: "wsl:a".into(), kind: "tool", text: "ok".into(), file: None }];
        let document = apply_delta(&Document::fresh(), json!({ "notes": [{ "text": "Uses uv", "sourceIds": ["s:1"] }], "remove": [] }), &evidence, 1024, "2026-10-22T00:00:00.000Z").unwrap();
        assert_eq!((document.items.len(), document.items[0].scope.as_str(), document.source_watermarks["wsl:a/s"]), (1, "execution-target", 1));
        let claims = vec![Evidence { kind: "assistant", ..evidence[0].clone() }];
        assert!(apply_delta(&Document::fresh(), json!({ "notes": [{ "text": "x", "sourceIds": ["s:1"] }], "remove": [] }), &claims, 1024, "t").is_err());
    }
}
