//! Per-project editor snapshots and unsaved-text recovery under `<home>/ide` (3.x `ide/state.ts`): tabs, expanded
//! folders, unsaved buffers, layout and run settings, saved only over the revision the page last saw.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::paths::relative_path;
use crate::projects::Projects;
use crate::store::{read_json, write_json};
use crate::text::sha256_hex;

const MAX_STATE_BYTES: usize = 32 * 1024 * 1024;
const MAX_BUFFER_BYTES: usize = 5 * 1024 * 1024;
const MAX_TABS: usize = 64;
const MAX_BUFFERS: usize = 32;
const MAX_EXPANDED_PATHS: usize = 10_000;

/// The file name of one project's snapshot; identities that are not plain file names are hashed.
pub fn file_name(id: &str) -> String {
    let reserved = {
        let lower = id.to_lowercase();
        ["con", "prn", "aux", "nul"].contains(&lower.as_str())
            || ((lower.starts_with("com") || lower.starts_with("lpt")) && lower.len() == 4 && lower.as_bytes()[3].is_ascii_digit())
    };
    let plain = (1..=128).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-') && !reserved;
    if plain { format!("{id}.json") } else { format!("sha256-{}.json", sha256_hex(id)) }
}

fn empty_state() -> Value {
    json!({ "version": 1, "revision": 0, "data": {
        "lastSessionId": null, "tabs": [], "activePath": null, "expandedPaths": [], "buffers": [],
        // agentWidth 0: the AI pane follows the window width until the user resizes it.
        "layout": { "sidebarWidth": 240, "agentWidth": 0, "bottomHeight": 220, "sidebarVisible": true, "agentVisible": false, "bottomVisible": false, "bottomTab": "terminal" },
    } })
}

fn invalid() -> HostError {
    HostError::invalid("The IDE state request contains invalid editor fields.")
}

fn relative(value: &Value, allow_root: bool) -> HostResult<String> {
    let path = value.as_str().ok_or_else(invalid)?;
    relative_path(path, allow_root).map_err(|_| invalid())?;
    Ok(path.to_owned())
}

fn root_id(value: &Value) -> HostResult<Option<String>> {
    match value {
        Value::Null => Ok(None),
        Value::String(id) if (1..=128).contains(&id.len()) => Ok(Some(id.clone())),
        _ => Err(invalid()),
    }
}

fn valid_dimension(value: &Value) -> bool {
    value.is_null() || value.as_f64().is_some_and(|n| n.is_finite() && (0.0..=100_000.0).contains(&n))
}

fn check_layout(layout: &Value) -> HostResult<()> {
    let object = layout.as_object().ok_or_else(invalid)?;
    for key in ["sidebarWidth", "agentWidth", "bottomHeight"] {
        if let Some(value) = object.get(key) {
            if !valid_dimension(value) {
                return Err(invalid());
            }
        }
    }
    for key in ["sidebarVisible", "agentVisible", "bottomVisible"] {
        if let Some(value) = object.get(key) {
            if !value.is_null() && !value.is_boolean() {
                return Err(invalid());
            }
        }
    }
    if let Some(value) = object.get("bottomTab") {
        if !value.is_null() && !value.as_str().is_some_and(|tab| (1..=64).contains(&tab.len())) {
            return Err(invalid());
        }
    }
    Ok(())
}

fn entry_identity(entry: &Value) -> HostResult<String> {
    Ok(format!("{}:{}", root_id(&entry["rootId"])?.unwrap_or_else(|| "primary".into()), relative(&entry["path"], false)?))
}

fn check_tab(tab: &Value) -> HostResult<(String, String)> {
    if !tab.is_object() {
        return Err(invalid());
    }
    let kind = tab["kind"].as_str().filter(|kind| *kind == "file" || *kind == "diff").ok_or_else(invalid)?;
    if !(tab["view"].is_null() || tab["view"].as_str().is_some_and(|view| (1..=64).contains(&view.len()))) {
        return Err(invalid());
    }
    if let Some(cursor) = tab.get("cursor").filter(|v| !v.is_null()) {
        if !cursor.is_object() && !cursor.is_number() {
            return Err(invalid());
        }
    }
    if let Some(scroll) = tab.get("scroll").filter(|v| !v.is_null()) {
        if !scroll.is_object() && !scroll.is_number() {
            return Err(invalid());
        }
    }
    Ok((kind.to_owned(), entry_identity(tab)?))
}

fn check_buffer(buffer: &Value) -> HostResult<String> {
    if !buffer.is_object() {
        return Err(invalid());
    }
    buffer["content"].as_str().ok_or_else(invalid)?;
    if !(buffer["baseVersion"].is_null() || buffer["baseVersion"].as_str().is_some_and(|version| (1..=1024).contains(&version.len()))) || !buffer["bom"].is_boolean() {
        return Err(invalid());
    }
    buffer["eol"].as_str().filter(|eol| ["lf", "crlf", "mixed"].contains(eol)).ok_or_else(invalid)?;
    entry_identity(buffer)
}

/// Check a snapshot: validate known key fields and their types while preserving unknown fields written by newer or older
/// versions (3.x/4.x forward compatibility).
pub fn check_data(data: &Value) -> HostResult<()> {
    if !data.is_object() {
        return Err(invalid());
    }
    if !(data["lastSessionId"].is_null() || data["lastSessionId"].as_str().is_some_and(|id| (1..=512).contains(&id.len()))) {
        return Err(invalid());
    }
    let mut tabs = HashSet::new();
    let tab_array = data["tabs"].as_array().ok_or_else(invalid)?;
    for tab in tab_array {
        let (kind, id) = check_tab(tab)?;
        if !tabs.insert(format!("{kind}:{id}")) {
            return Err(HostError::invalid("An editor tab may appear only once."));
        }
    }
    for path in data["expandedPaths"].as_array().ok_or_else(invalid)? {
        relative(path, false)?;
    }
    if let Some(roots) = data.get("expandedRoots").filter(|roots| !roots.is_null()) {
        for entry in roots.as_array().ok_or_else(invalid)? {
            if !entry.is_object() {
                return Err(invalid());
            }
            root_id(&entry["rootId"])?.ok_or_else(invalid)?;
            relative(&entry["path"], true)?;
        }
    }
    let mut buffers = HashSet::new();
    for buffer in data["buffers"].as_array().ok_or_else(invalid)? {
        if !buffers.insert(check_buffer(buffer)?) {
            return Err(HostError::invalid("A file may have only one recovery buffer."));
        }
    }
    if !data["activePath"].is_null() {
        let active = json!({ "path": data["activePath"], "rootId": data["activeRootId"] });
        let wanted = entry_identity(&active)?;
        if !tab_array.iter().any(|tab| entry_identity(tab).is_ok_and(|known| known == wanted)) {
            return Err(HostError::invalid("The active path must name an open tab."));
        }
    }
    check_layout(&data["layout"])?;
    if let Some(execution) = data.get("execution").filter(|execution| !execution.is_null()) {
        super::execution::check_configuration(execution)?;
    }
    Ok(())
}

/// Salvage valid fields from a damaged or incompatible IDE state snapshot, falling back to `empty_state()`.
fn salvage_state(raw: Option<Value>) -> Value {
    let mut base = empty_state();
    let Some(Value::Object(record)) = raw else { return base };
    if let Some(rev) = record.get("revision").and_then(Value::as_u64) {
        base["revision"] = json!(rev);
    }
    let Some(Value::Object(src)) = record.get("data") else { return base };
    let default_layout = base["data"]["layout"].as_object().cloned().unwrap_or_default();
    let mut out = serde_json::Map::new();
    // Preserve unknown top-level data keys.
    for (k, v) in src {
        if !["lastSessionId", "tabs", "activePath", "activeRootId", "expandedPaths", "expandedRoots", "buffers", "layout", "execution"].contains(&k.as_str()) {
            out.insert(k.clone(), v.clone());
        }
    }
    let last_session = src.get("lastSessionId").and_then(Value::as_str).filter(|id| (1..=512).contains(&id.len())).map(|id| json!(id)).unwrap_or(Value::Null);
    out.insert("lastSessionId".into(), last_session);

    let mut seen_tabs = HashSet::new();
    let mut valid_tabs = Vec::new();
    let mut tab_ids = HashSet::new();
    if let Some(items) = src.get("tabs").and_then(Value::as_array) {
        for tab in items.iter().take(MAX_TABS) {
            if let Ok((kind, id)) = check_tab(tab) {
                if seen_tabs.insert(format!("{kind}:{id}")) {
                    tab_ids.insert(id);
                    valid_tabs.push(tab.clone());
                }
            }
        }
    }
    out.insert("tabs".into(), Value::Array(valid_tabs));

    let active_valid = src.get("activePath").filter(|v| !v.is_null()).and_then(|path| {
        let probe = json!({ "path": path, "rootId": src.get("activeRootId").cloned().unwrap_or(Value::Null) });
        entry_identity(&probe).ok().filter(|id| tab_ids.contains(id))
    });
    if active_valid.is_some() {
        out.insert("activePath".into(), src["activePath"].clone());
        if let Some(active_root) = src.get("activeRootId") {
            out.insert("activeRootId".into(), active_root.clone());
        }
    } else {
        out.insert("activePath".into(), Value::Null);
    }

    let expanded_paths: Vec<Value> = src
        .get("expandedPaths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| relative(p, false).is_ok())
        .take(MAX_EXPANDED_PATHS)
        .cloned()
        .collect();
    out.insert("expandedPaths".into(), Value::Array(expanded_paths));

    if let Some(items) = src.get("expandedRoots").and_then(Value::as_array) {
        let valid_roots: Vec<Value> = items
            .iter()
            .filter(|e| e.is_object() && root_id(&e["rootId"]).ok().flatten().is_some() && relative(&e["path"], true).is_ok())
            .take(MAX_EXPANDED_PATHS)
            .cloned()
            .collect();
        out.insert("expandedRoots".into(), Value::Array(valid_roots));
    }

    let mut seen_buffers = HashSet::new();
    let mut valid_buffers = Vec::new();
    if let Some(items) = src.get("buffers").and_then(Value::as_array) {
        for buf in items.iter().take(MAX_BUFFERS) {
            if let Ok(id) = check_buffer(buf) {
                if buf["content"].as_str().map_or(0, str::len) <= MAX_BUFFER_BYTES && seen_buffers.insert(id) {
                    valid_buffers.push(buf.clone());
                }
            }
        }
    }
    out.insert("buffers".into(), Value::Array(valid_buffers));

    let mut layout = default_layout;
    if let Some(Value::Object(src_layout)) = src.get("layout") {
        for (k, v) in src_layout {
            let valid = match k.as_str() {
                "sidebarWidth" | "agentWidth" | "bottomHeight" => valid_dimension(v) && !v.is_null(),
                "sidebarVisible" | "agentVisible" | "bottomVisible" => v.is_boolean(),
                "bottomTab" => v.as_str().is_some_and(|tab| (1..=64).contains(&tab.len())),
                _ => true,
            };
            if valid {
                layout.insert(k.clone(), v.clone());
            }
        }
    }
    out.insert("layout".into(), Value::Object(layout));

    if let Some(execution) = src.get("execution").filter(|e| !e.is_null()) {
        if super::execution::check_configuration(execution).is_ok() {
            out.insert("execution".into(), execution.clone());
        }
    }

    let data = Value::Object(out);
    if check_data(&data).is_ok() {
        base["data"] = data;
        base
    } else {
        empty_state()
    }
}

pub struct StateStore {
    directory: PathBuf,
    projects: Arc<Projects>,
    records: Mutex<HashMap<String, Value>>,
    selection: Mutex<Option<String>>,
    writes: Mutex<()>,
}

impl StateStore {
    pub fn open(directory: PathBuf, projects: Arc<Projects>) -> Self {
        let selection = match read_json::<Value>(&directory.join("selection.json")) {
            Ok(Some(selection)) => selection.get("workspaceId").and_then(Value::as_str).map(str::to_owned),
            Ok(None) => None,
            Err(error) => {
                log::warn!("IDE selection is unreadable and was ignored: {error}");
                None
            }
        };
        Self { directory, projects, records: Mutex::default(), selection: Mutex::new(selection), writes: Mutex::new(()) }
    }

    fn current(&self, workspace_id: &str) -> HostResult<Value> {
        if self.projects.get(workspace_id).is_none() {
            return Err(crate::projects::missing());
        }
        if let Some(record) = self.records.lock().unwrap().get(workspace_id) {
            return Ok(record.clone());
        }
        let path = self.directory.join(file_name(workspace_id));
        let record = match read_json::<Value>(&path) {
            Ok(Some(record)) if record["version"] == 1 && record["revision"].is_u64() && check_data(&record["data"]).is_ok() => record,
            Ok(Some(raw)) => {
                log::warn!("IDE state for workspace {workspace_id} has invalid fields; backing up and salvaging.");
                let backup = self.directory.join(format!("{}.damaged-{}", file_name(workspace_id), crate::text::now_ms()));
                let _ = std::fs::copy(&path, &backup);
                let salvaged = salvage_state(Some(raw));
                let _ = write_json(&path, &salvaged);
                salvaged
            }
            Err(error) => {
                log::warn!("IDE state for workspace {workspace_id} is unreadable ({error}); backing up and rebuilding.");
                let backup = self.directory.join(format!("{}.damaged-{}", file_name(workspace_id), crate::text::now_ms()));
                let _ = std::fs::copy(&path, &backup);
                let salvaged = empty_state();
                let _ = write_json(&path, &salvaged);
                salvaged
            }
            Ok(None) => empty_state(),
        };
        self.records.lock().unwrap().insert(workspace_id.to_owned(), record.clone());
        Ok(record)
    }

    pub fn get(&self, workspace_id: &str) -> HostResult<Value> {
        self.current(workspace_id)
    }

    /// The project selected last, while it is still registered.
    pub fn selection(&self) -> Option<String> {
        self.selection.lock().unwrap().clone().filter(|id| self.projects.get(id).is_some())
    }

    pub fn set_selection(&self, workspace_id: Option<&str>) -> HostResult<Value> {
        let _guard = self.writes.lock().unwrap();
        if workspace_id.is_some_and(|id| self.projects.get(id).is_none()) {
            return Err(crate::projects::missing());
        }
        let next = json!({ "version": 1, "workspaceId": workspace_id });
        write_json(&self.directory.join("selection.json"), &next)?;
        *self.selection.lock().unwrap() = workspace_id.map(str::to_owned);
        Ok(next)
    }

    /// Save a complete snapshot only when `base` is still the committed revision.
    pub fn replace(&self, workspace_id: &str, base: u64, data: Value) -> HostResult<Value> {
        let _guard = self.writes.lock().unwrap();
        check_data(&data)?;
        let too_large = data["tabs"].as_array().map_or(0, Vec::len) > MAX_TABS
            || data["buffers"].as_array().map_or(0, Vec::len) > MAX_BUFFERS
            || data["expandedPaths"].as_array().map_or(0, Vec::len) + data["expandedRoots"].as_array().map_or(0, Vec::len) > MAX_EXPANDED_PATHS
            || data["buffers"].as_array().into_iter().flatten().any(|buffer| buffer["content"].as_str().map_or(0, str::len) > MAX_BUFFER_BYTES)
            || data.to_string().len() > MAX_STATE_BYTES;
        if too_large {
            return Err(HostError::new("state-too-large", l("项目的恢复快照超过了存储上限。", "The workspace recovery snapshot exceeds the configured storage budget.")));
        }
        let current = self.current(workspace_id)?;
        if current["revision"].as_u64() != Some(base) {
            let message = l("项目的编辑器状态已被另一个窗口保存。请先恢复当前快照，再覆盖它。", "The workspace IDE state was saved by another client. Recover the current snapshot before replacing it.");
            return Err(HostError::new("revision-conflict", message).with("currentState", current));
        }
        let next = json!({ "version": 1, "revision": base + 1, "data": data });
        write_json(&self.directory.join(file_name(workspace_id)), &next)?;
        self.records.lock().unwrap().insert(workspace_id.to_owned(), next.clone());
        Ok(next)
    }

    /// The `state.*` operations.
    pub fn handle(&self, request: &Value) -> HostResult<Value> {
        let workspace = || request["workspaceId"].as_str().filter(|id| !id.is_empty()).ok_or_else(invalid);
        match request["op"].as_str() {
            Some("state.read") => self.get(workspace()?),
            Some("state.save") => self.replace(workspace()?, request["baseRevision"].as_u64().ok_or_else(invalid)?, request["data"].clone()),
            Some("state.selection.read") => Ok(json!({ "version": 1, "workspaceId": self.selection() })),
            Some("state.selection.save") => match request.get("workspaceId") {
                None | Some(Value::Null) => self.set_selection(None),
                Some(Value::String(id)) if !id.is_empty() => self.set_selection(Some(id)),
                _ => Err(invalid()),
            },
            _ => Err(invalid()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_state_files_like_3x() {
        assert_eq!(file_name("abc-123"), "abc-123.json");
        assert!(file_name("con").starts_with("sha256-"));
        assert!(file_name("a/b").starts_with("sha256-"));
    }

    #[test]
    fn checks_snapshots() {
        let mut data = empty_state()["data"].clone();
        data["tabs"] = json!([{ "path": "a.py", "kind": "file" }, { "path": "b.py", "rootId": "r1", "kind": "file", "view": "rich" }]);
        data["activePath"] = json!("b.py");
        data["activeRootId"] = json!("r1");
        data["buffers"] = json!([{ "path": "a.py", "content": "x", "baseVersion": null, "bom": false, "eol": "lf" }]);
        assert!(check_data(&data).is_ok());
        data["activeRootId"] = Value::Null;
        assert!(check_data(&data).is_err());
        data["activePath"] = json!("../x");
        assert!(check_data(&data).is_err());
    }

    #[test]
    fn preserves_unknown_fields_and_salvages_damaged_state() {
        let mut data = empty_state()["data"].clone();
        data["futureTopKey"] = json!({ "enabled": true });
        data["tabs"] = json!([{ "path": "a.py", "kind": "file", "view": "futureView", "futureTabKey": 42 }]);
        data["layout"]["futurePaneWidth"] = json!(320);
        data["activePath"] = json!("a.py");
        assert!(check_data(&data).is_ok());
        // Invalid known field type is still rejected.
        let mut bad_layout = data.clone();
        bad_layout["layout"]["sidebarWidth"] = json!("wide");
        assert!(check_data(&bad_layout).is_err());

        // Salvage keeps valid tabs/unknown keys and repairs invalid layout fields.
        let dir = tempfile::tempdir().unwrap();
        let projects_file = dir.path().join("projects.json");
        let projects = Arc::new(Projects::load(projects_file).unwrap());
        let project_dir = dir.path().join("proj");
        std::fs::create_dir_all(&project_dir).unwrap();
        let added = projects.open(project_dir.to_str().unwrap(), None).unwrap();
        let state_dir = dir.path().join("ide");
        std::fs::create_dir_all(&state_dir).unwrap();
        let state_file = state_dir.join(file_name(&added.id));
        // Corrupted JSON triggers backup + clean rebuild.
        std::fs::write(&state_file, "{not valid json").unwrap();
        let store = StateStore::open(state_dir.clone(), projects.clone());
        let loaded = store.get(&added.id).unwrap();
        assert_eq!(loaded["version"], 1);
        assert_eq!(loaded["revision"], 0);
        let backups: Vec<_> = std::fs::read_dir(&state_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".damaged-"))
            .collect();
        assert_eq!(backups.len(), 1);
        // Reject non-string workspaceId on state.selection.save.
        assert!(store.handle(&json!({ "op": "state.selection.save", "workspaceId": 123 })).is_err());
        assert!(store.handle(&json!({ "op": "state.selection.save", "workspaceId": Value::Null })).is_ok());
    }
}
