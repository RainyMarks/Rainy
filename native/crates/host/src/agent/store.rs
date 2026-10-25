//! Chats on disk (3.x `store.ts`): `chats/<id>.jsonl` — a header line, then entry, metadata and review lines — and
//! `chats/index.json`. A 3.x or 4.0 Host reads what this one writes and the other way round.

use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rainy_protocol::chat::TranscriptEntry;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{HostError, HostResult};
use crate::settings::ModelSelection;
use crate::store::{create_private_dir, read_json, write_atomic, write_json};

/// Format version of chat files.
pub const CHAT_FORMAT_VERSION: u64 = 1;

/// The entry a fork started from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Parent {
    pub session_id: String,
    pub entry_id: String,
}

/// The persisted summary of one chat; the run status is added by the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredChat {
    pub id: String,
    pub workspace_id: Option<String>,
    pub cwd: String,
    pub title: String,
    pub created_at: f64,
    pub updated_at: f64,
    pub archived: bool,
    pub pinned: bool,
    pub model: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Parent>,
    /// The chat's own permission mode; absent means the default from Settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
}

/// Metadata changes, recorded as `meta` lines; later lines win.
#[derive(Clone, Debug, Default, Serialize)]
pub struct MetaPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    /// `Some(None)` clears the chat's own model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Option<ModelSelection>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
}

impl MetaPatch {
    fn apply(&self, chat: &mut StoredChat) {
        if let Some(title) = &self.title {
            chat.title = title.clone();
        }
        if let Some(archived) = self.archived {
            chat.archived = archived;
        }
        if let Some(pinned) = self.pinned {
            chat.pinned = pinned;
        }
        if let Some(model) = &self.model {
            chat.model = model.clone();
        }
        if let Some(permission) = &self.permission {
            chat.permission = Some(permission.clone());
        }
    }
}

/// A chat file's content.
pub struct ChatFile {
    pub chat: StoredChat,
    pub entries: Vec<TranscriptEntry>,
    /// Write and edit calls whose changes the user reviewed and kept.
    pub kept: Vec<String>,
}

/// What a new chat starts with.
#[derive(Default)]
pub struct NewChat {
    pub workspace_id: Option<String>,
    pub cwd: String,
    pub parent: Option<Parent>,
    pub entries: Vec<TranscriptEntry>,
    pub title: String,
    pub model: Option<ModelSelection>,
    pub permission: Option<String>,
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn not_found(id: &str) -> HostError {
    HostError::new("not-found", format!("Chat {id} not found"))
}

struct State {
    chats: HashMap<String, StoredChat>,
    /// Chats whose file this process has checked to end with a complete line.
    line_ended: HashSet<String>,
    index_pending: bool,
}

/// Reads and writes chat files; one per Host.
pub struct ChatStore {
    dir: PathBuf,
    state: Mutex<State>,
    /// One writer per chat file at a time.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    index_lock: Mutex<()>,
}

/// An entry as stored. 3.x wrote what pi-ai held, which a provider could leave in shapes entries do not take: a
/// tool call's arguments as `null` or another JSON value, and counts the provider did not send as `null` (NaN in
/// JavaScript). Such an entry is brought into shape rather than skipped, so no history is lost; `None` when it is
/// still no entry.
fn parse_entry(entry: &Value) -> Option<TranscriptEntry> {
    if let Ok(parsed) = serde_json::from_value(entry.clone()) {
        return Some(parsed);
    }
    let mut entry = entry.clone();
    repair_entry(&mut entry);
    serde_json::from_value(entry).ok()
}

fn repair_entry(entry: &mut Value) {
    /// Fields that must be numbers or strings: `null` (or another JSON value) becomes the field's empty value.
    fn fill(object: Option<&mut Value>, numbers: &[&str], strings: &[&str]) {
        let Some(Value::Object(object)) = object else { return };
        for key in numbers {
            if object.get(*key).is_some_and(|value| !value.is_number()) {
                object.insert((*key).into(), json!(0));
            }
        }
        for key in strings {
            if object.get(*key).is_some_and(|value| !value.is_string()) {
                object.insert((*key).into(), json!(""));
            }
        }
    }
    fill(Some(entry), &["ts", "tokensBefore", "requests"], &[]);
    if entry["kind"] == "turn" {
        fill(Some(entry), &["durationMs"], &["provider", "model"]);
    }
    fill(entry.pointer_mut("/usage"), &["input", "output", "cacheRead", "cacheWrite"], &[]);
    fill(entry.pointer_mut("/message"), &["timestamp"], &["api", "provider", "model", "stopReason"]);
    fill(entry.pointer_mut("/message/usage"), &["input", "output", "cacheRead", "cacheWrite"], &[]);
    for block in entry.pointer_mut("/message/content").and_then(Value::as_array_mut).into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => fill(Some(block), &[], &["text"]),
            Some("thinking") => fill(Some(block), &[], &["thinking"]),
            Some("toolCall") => {
                fill(Some(block), &[], &["id", "name"]);
                // Arguments sent as a JSON text are read; anything else that is no object becomes no arguments.
                let arguments = match &block["arguments"] {
                    Value::Object(_) => None,
                    Value::String(text) => Some(serde_json::from_str::<Value>(text).ok().filter(Value::is_object).unwrap_or_else(|| json!({}))),
                    _ => Some(json!({})),
                };
                if let (Some(arguments), Some(block)) = (arguments, block.as_object_mut()) {
                    block.insert("arguments".into(), arguments);
                }
            }
            _ => {}
        }
    }
    for part in entry.pointer_mut("/content").and_then(Value::as_array_mut).into_iter().flatten() {
        if part["type"] == "text" {
            fill(Some(part), &[], &["text"]);
        }
    }
    if entry["kind"] == "toolResult" && !entry["isError"].is_boolean() {
        entry["isError"] = json!(false);
    }
}

fn modified_ms(path: &Path) -> f64 {
    std::fs::metadata(path).and_then(|info| info.modified()).ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0.0, |elapsed| elapsed.as_millis() as f64)
}

impl ChatStore {
    /// Load the index, rebuilding it from the chat files when it is missing or unreadable.
    pub fn load(dir: PathBuf) -> HostResult<Arc<Self>> {
        create_private_dir(&dir)?;
        let store = Arc::new(Self { dir, state: Mutex::new(State { chats: HashMap::new(), line_ended: HashSet::new(), index_pending: false }), locks: Mutex::default(), index_lock: Mutex::new(()) });
        let index = read_json::<Value>(&store.dir.join("index.json"));
        let parsed = match index {
            Ok(Some(raw)) if raw["version"] == 1 => serde_json::from_value::<Vec<StoredChat>>(raw["chats"].clone()).ok(),
            Ok(None) => None,
            _ => {
                log::warn!("chat index unreadable, rebuilding");
                None
            }
        };
        match parsed {
            Some(chats) => store.state.lock().unwrap().chats = chats.into_iter().map(|chat| (chat.id.clone(), chat)).collect(),
            None => store.rebuild_index()?,
        }
        Ok(store)
    }

    /// Scan every chat file and rewrite the index.
    fn rebuild_index(&self) -> HostResult<()> {
        let mut chats = HashMap::new();
        for entry in std::fs::read_dir(&self.dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".jsonl") else { continue };
            match self.read_file(id, true) {
                Ok(Some(file)) => {
                    chats.insert(file.chat.id.clone(), file.chat);
                }
                Ok(None) => {}
                Err(error) => log::warn!("skipping chat {name}: {}", error.message),
            }
        }
        self.state.lock().unwrap().chats = chats;
        self.write_index()
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.jsonl"))
    }

    fn lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks.lock().unwrap().entry(id.to_owned()).or_default().clone()
    }

    /// Every chat, newest activity first.
    pub fn list(&self) -> Vec<StoredChat> {
        let mut chats: Vec<StoredChat> = self.state.lock().unwrap().chats.values().cloned().collect();
        chats.sort_by(|left, right| right.updated_at.total_cmp(&left.updated_at));
        chats
    }

    pub fn get(&self, id: &str) -> Option<StoredChat> {
        self.state.lock().unwrap().chats.get(id).cloned()
    }

    /// Create a chat, possibly with entries copied from another (a fork).
    pub fn create(self: &Arc<Self>, new: NewChat) -> HostResult<StoredChat> {
        let now = crate::text::now_ms() as f64;
        let chat = StoredChat {
            id: uuid::Uuid::new_v4().to_string(),
            workspace_id: new.workspace_id,
            cwd: new.cwd,
            title: new.title,
            created_at: now,
            updated_at: now,
            archived: false,
            pinned: false,
            model: new.model,
            parent: new.parent,
            permission: new.permission,
        };
        let mut header = json!({ "type": "header", "version": CHAT_FORMAT_VERSION, "id": chat.id, "workspaceId": chat.workspace_id, "cwd": chat.cwd, "createdAt": now });
        if let Some(parent) = &chat.parent {
            header["parent"] = json!(parent);
        }
        let mut lines = vec![header.to_string()];
        lines.extend(new.entries.iter().map(|entry| json!({ "type": "entry", "entry": entry }).to_string()));
        let meta = MetaPatch {
            title: (!chat.title.is_empty()).then(|| chat.title.clone()),
            model: chat.model.clone().map(Some),
            permission: chat.permission.clone(),
            ..MetaPatch::default()
        };
        let meta = serde_json::to_value(&meta).unwrap_or_default();
        if meta.as_object().is_some_and(|fields| !fields.is_empty()) {
            let mut line = json!({ "type": "meta" });
            line.as_object_mut().unwrap().extend(meta.as_object().unwrap().clone());
            lines.push(line.to_string());
        }
        write_atomic(&self.path(&chat.id), format!("{}\n", lines.join("\n")).as_bytes())?;
        let mut state = self.state.lock().unwrap();
        state.line_ended.insert(chat.id.clone());
        state.chats.insert(chat.id.clone(), chat.clone());
        drop(state);
        self.schedule_index();
        Ok(chat)
    }

    /// A chat's entries in order.
    pub async fn read(&self, id: &str) -> HostResult<ChatFile> {
        let lock = self.lock(id);
        let _guard = lock.lock().await;
        self.read_file(id, false)?.ok_or_else(|| not_found(id))
    }

    fn read_file(&self, id: &str, header_only: bool) -> HostResult<Option<ChatFile>> {
        if !valid_id(id) {
            return Ok(None);
        }
        let path = self.path(id);
        let text = match std::fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut lines = text.split('\n');
        let header: Value = serde_json::from_str(lines.next().unwrap_or_default()).map_err(|error| HostError::other(format!("Chat {id} has no header: {error}")))?;
        if header["type"] != "header" || header["version"] != CHAT_FORMAT_VERSION || !header["id"].is_string() {
            return Err(HostError::other(format!("Chat {id} has an unknown header")));
        }
        let mut chat = StoredChat {
            id: header["id"].as_str().unwrap_or(id).to_owned(),
            workspace_id: header["workspaceId"].as_str().map(str::to_owned),
            cwd: header["cwd"].as_str().unwrap_or_default().to_owned(),
            title: String::new(),
            created_at: header["createdAt"].as_f64().unwrap_or(0.0),
            updated_at: modified_ms(&path),
            archived: false,
            pinned: false,
            model: None,
            parent: serde_json::from_value(header["parent"].clone()).ok(),
            permission: None,
        };
        let (mut entries, mut kept, mut damaged) = (Vec::new(), Vec::new(), 0);
        for line in lines.filter(|line| !line.is_empty()) {
            // A crash can leave a partial line; records appended later start on the next one.
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                damaged += 1;
                continue;
            };
            match record["type"].as_str() {
                Some("meta") => {
                    let patch = MetaPatch {
                        title: record["title"].as_str().map(str::to_owned),
                        archived: record["archived"].as_bool(),
                        pinned: record["pinned"].as_bool(),
                        model: record.get("model").map(|model| serde_json::from_value(model.clone()).ok()),
                        permission: record["permission"].as_str().filter(|mode| ["read-only", "ask", "auto"].contains(mode)).map(str::to_owned),
                    };
                    patch.apply(&mut chat);
                }
                Some("entry") if !header_only => match parse_entry(&record["entry"]) {
                    Some(entry) => entries.push(entry),
                    None => damaged += 1,
                },
                Some("review") if !header_only => kept.extend(record["kept"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned)),
                _ => {}
            }
        }
        if damaged > 0 {
            log::warn!("chat {id}: skipped {damaged} damaged line(s)");
        }
        Ok(Some(ChatFile { chat, entries, kept }))
    }

    /// Append whole lines, starting on a new line when a crash left the last one partial.
    fn append_lines(&self, id: &str, text: &str) -> HostResult<()> {
        let path = self.path(id);
        let checked = self.state.lock().unwrap().line_ended.contains(id);
        let mut file = std::fs::OpenOptions::new().append(true).read(true).create(true).open(&path)?;
        let mut text = text.to_owned();
        if !checked {
            let size = file.metadata()?.len();
            if size > 0 {
                let mut last = [0u8];
                file.seek(SeekFrom::Start(size - 1))?;
                file.read_exact(&mut last)?;
                if last[0] != b'\n' {
                    text.insert(0, '\n');
                }
            }
            self.state.lock().unwrap().line_ended.insert(id.to_owned());
        }
        file.write_all(text.as_bytes())?;
        Ok(())
    }

    fn require(&self, id: &str) -> HostResult<()> {
        if self.state.lock().unwrap().chats.contains_key(id) { Ok(()) } else { Err(not_found(id)) }
    }

    /// Append entries.
    pub async fn append(self: &Arc<Self>, id: &str, entries: &[TranscriptEntry]) -> HostResult<()> {
        if entries.is_empty() {
            return Ok(());
        }
        self.require(id)?;
        let lines: String = entries.iter().map(|entry| format!("{}\n", json!({ "type": "entry", "entry": entry }))).collect();
        {
            let lock = self.lock(id);
            let _guard = lock.lock().await;
            self.append_lines(id, &lines)?;
        }
        if let Some(chat) = self.state.lock().unwrap().chats.get_mut(id) {
            chat.updated_at = crate::text::now_ms() as f64;
        }
        self.schedule_index();
        Ok(())
    }

    /// Record that the user reviewed and kept the changes of some write and edit calls.
    pub async fn keep(&self, id: &str, tool_call_ids: &[String]) -> HostResult<()> {
        if tool_call_ids.is_empty() {
            return Ok(());
        }
        self.require(id)?;
        let lock = self.lock(id);
        let _guard = lock.lock().await;
        self.append_lines(id, &format!("{}\n", json!({ "type": "review", "kept": tool_call_ids })))
    }

    /// Record metadata changes; returns the updated summary.
    pub async fn set_meta(self: &Arc<Self>, id: &str, patch: MetaPatch) -> HostResult<StoredChat> {
        self.require(id)?;
        let mut line = json!({ "type": "meta" });
        if let Value::Object(fields) = serde_json::to_value(&patch).unwrap_or_default() {
            line.as_object_mut().unwrap().extend(fields);
        }
        let chat = {
            let lock = self.lock(id);
            let _guard = lock.lock().await;
            self.append_lines(id, &format!("{line}\n"))?;
            let mut state = self.state.lock().unwrap();
            let chat = state.chats.get_mut(id).ok_or_else(|| not_found(id))?;
            patch.apply(chat);
            chat.clone()
        };
        self.schedule_index();
        Ok(chat)
    }

    /// Give an untitled chat the title derived from its first message; a title the user set first is kept.
    pub async fn set_default_title(self: &Arc<Self>, id: &str, title: &str) -> HostResult<StoredChat> {
        self.require(id)?;
        let lock = self.lock(id);
        let _guard = lock.lock().await;
        let untitled = self.get(id).is_some_and(|chat| chat.title.is_empty());
        if untitled {
            self.append_lines(id, &format!("{}\n", json!({ "type": "meta", "title": title })))?;
            if let Some(chat) = self.state.lock().unwrap().chats.get_mut(id) {
                chat.title = title.to_owned();
            }
            self.schedule_index();
        }
        self.get(id).ok_or_else(|| not_found(id))
    }

    /// Delete a chat file.
    pub async fn delete(self: &Arc<Self>, id: &str) -> HostResult<()> {
        {
            let lock = self.lock(id);
            let _guard = lock.lock().await;
            match std::fs::remove_file(self.path(id)) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
                _ => {}
            }
        }
        let mut state = self.state.lock().unwrap();
        state.chats.remove(id);
        state.line_ended.remove(id);
        drop(state);
        self.locks.lock().unwrap().remove(id);
        self.schedule_index();
        Ok(())
    }

    fn schedule_index(self: &Arc<Self>) {
        {
            let mut state = self.state.lock().unwrap();
            if state.index_pending {
                return;
            }
            state.index_pending = true;
        }
        let store = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            store.state.lock().unwrap().index_pending = false;
            if let Err(error) = store.write_index() {
                log::warn!("chat index write failed: {}", error.message);
            }
        });
    }

    fn write_index(&self) -> HostResult<()> {
        let _guard = self.index_lock.lock().unwrap();
        let chats: Vec<StoredChat> = self.state.lock().unwrap().chats.values().cloned().collect();
        write_json(&self.dir.join("index.json"), &json!({ "version": 1, "chats": chats }))?;
        Ok(())
    }

    /// Write any pending index change now.
    pub fn flush(&self) -> HostResult<()> {
        self.state.lock().unwrap().index_pending = false;
        self.write_index()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_and_reads_3x_files() {
        let dir = std::env::temp_dir().join(format!("rainy-chats-{}", uuid::Uuid::new_v4()));
        let store = ChatStore::load(dir.clone()).unwrap();
        let chat = store.create(NewChat { cwd: "/w".into(), permission: Some("ask".into()), ..NewChat::default() }).unwrap();
        let entry: TranscriptEntry = serde_json::from_value(json!({ "kind": "user", "id": "e1", "ts": 1, "text": "找到 needle 了" })).unwrap();
        store.append(&chat.id, &[entry]).await.unwrap();
        store.set_meta(&chat.id, MetaPatch { title: Some("T".into()), ..MetaPatch::default() }).await.unwrap();
        store.keep(&chat.id, &["c1".into()]).await.unwrap();
        // A partial line from a crash is skipped and later records start on a new line.
        std::fs::OpenOptions::new().append(true).open(store.path(&chat.id)).unwrap().write_all(b"{\"type\":\"entry\",\"en").unwrap();
        store.flush().unwrap();
        let fresh = ChatStore::load(dir.clone()).unwrap();
        fresh.append(&chat.id, &[serde_json::from_value(json!({ "kind": "notice", "id": "e2", "ts": 2, "level": "info", "text": "x" })).unwrap()]).await.unwrap();
        let file = fresh.read(&chat.id).await.unwrap();
        assert_eq!(file.entries.len(), 2);
        assert_eq!((file.chat.title.as_str(), file.chat.permission.as_deref(), file.kept.as_slice()), ("T", Some("ask"), &["c1".to_owned()][..]));
        fresh.delete(&chat.id).await.unwrap();
        assert!(fresh.get(&chat.id).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn keeps_entries_pi_ai_left_in_odd_shapes() {
        let reply = json!({
            "kind": "assistant", "id": "a1", "ts": 1,
            "message": {
                "role": "assistant", "api": "openai-completions", "provider": "p", "model": "m", "stopReason": "toolUse", "timestamp": 1,
                "usage": { "input": null, "output": 5, "cacheRead": null, "cacheWrite": 0, "totalTokens": null },
                "content": [
                    { "type": "text", "text": "x" },
                    { "type": "toolCall", "id": "c1", "name": "read", "arguments": null },
                    { "type": "toolCall", "id": "c2", "name": "read", "arguments": "{\"path\":\"a\"}" },
                    { "type": "toolCall", "id": "c3", "name": "read", "arguments": [1] },
                ],
            },
        });
        let Some(TranscriptEntry::Assistant { message, .. }) = parse_entry(&reply) else { panic!("the reply was dropped") };
        assert_eq!((message.usage.input, message.usage.output), (0.0, 5.0));
        let arguments: Vec<Value> = message.content.iter().filter_map(|block| if let rainy_protocol::chat::AssistantBlock::ToolCall { arguments, .. } = block { Some(Value::Object(arguments.clone())) } else { None }).collect();
        assert_eq!(arguments, [json!({}), json!({ "path": "a" }), json!({})]);
        let turn = json!({ "kind": "turn", "id": "t1", "ts": 2, "durationMs": null, "usage": { "input": 1, "output": null, "cacheRead": 0, "cacheWrite": 0 }, "provider": "p", "model": "m", "requests": 1 });
        assert!(matches!(parse_entry(&turn), Some(TranscriptEntry::Turn { .. })));
        let result = json!({ "kind": "toolResult", "id": "r1", "ts": 3, "toolCallId": "c1", "toolName": "read", "content": [{ "type": "text", "text": null }], "isError": null });
        assert!(matches!(parse_entry(&result), Some(TranscriptEntry::ToolResult { is_error: false, .. })));
        assert!(parse_entry(&json!({ "kind": "unknown", "id": "x" })).is_none());
    }
}
