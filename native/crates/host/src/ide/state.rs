//! Per-project editor snapshots and unsaved-text recovery under `<home>/ide` (3.x `ide/state.ts`): tabs, expanded
//! folders, unsaved buffers, layout and run settings, saved only over the revision the page last saw.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::error::{HostError, HostResult};
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

fn only_keys(value: &Value, allowed: &[&str]) -> HostResult<()> {
    let object = value.as_object().ok_or_else(invalid)?;
    if object.keys().all(|key| allowed.contains(&key.as_str())) { Ok(()) } else { Err(invalid()) }
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

/// Check a snapshot the page sends: only the documented fields, paths inside the project, one tab and one buffer per
/// file, and the active path among the tabs.
pub fn check_data(data: &Value) -> HostResult<()> {
    only_keys(data, &["lastSessionId", "tabs", "activePath", "activeRootId", "expandedPaths", "expandedRoots", "buffers", "layout", "execution"])?;
    if !(data["lastSessionId"].is_null() || data["lastSessionId"].as_str().is_some_and(|id| (1..=512).contains(&id.len()))) {
        return Err(invalid());
    }
    let identity = |entry: &Value| -> HostResult<String> { Ok(format!("{}:{}", root_id(&entry["rootId"])?.unwrap_or_else(|| "primary".into()), relative(&entry["path"], false)?)) };
    let mut tabs = HashSet::new();
    for tab in data["tabs"].as_array().ok_or_else(invalid)? {
        only_keys(tab, &["path", "rootId", "kind", "cursor", "scroll", "view"])?;
        let kind = tab["kind"].as_str().filter(|kind| *kind == "file" || *kind == "diff").ok_or_else(invalid)?;
        if !(tab["view"].is_null() || tab["view"].as_str().is_some_and(|view| ["source", "preview", "split", "hex", "rich"].contains(&view))) {
            return Err(invalid());
        }
        if !tabs.insert(format!("{kind}:{}", identity(tab)?)) {
            return Err(HostError::invalid("An editor tab may appear only once."));
        }
    }
    for path in data["expandedPaths"].as_array().ok_or_else(invalid)? {
        relative(path, false)?;
    }
    if let Some(roots) = data.get("expandedRoots").filter(|roots| !roots.is_null()) {
        for entry in roots.as_array().ok_or_else(invalid)? {
            only_keys(entry, &["rootId", "path"])?;
            root_id(&entry["rootId"])?.ok_or_else(invalid)?;
            relative(&entry["path"], true)?;
        }
    }
    let mut buffers = HashSet::new();
    for buffer in data["buffers"].as_array().ok_or_else(invalid)? {
        only_keys(buffer, &["path", "rootId", "content", "baseVersion", "bom", "eol"])?;
        buffer["content"].as_str().ok_or_else(invalid)?;
        if !(buffer["baseVersion"].is_null() || buffer["baseVersion"].as_str().is_some_and(|version| (1..=1024).contains(&version.len()))) || !buffer["bom"].is_boolean() {
            return Err(invalid());
        }
        buffer["eol"].as_str().filter(|eol| ["lf", "crlf", "mixed"].contains(eol)).ok_or_else(invalid)?;
        if !buffers.insert(identity(buffer)?) {
            return Err(HostError::invalid("A file may have only one recovery buffer."));
        }
    }
    if !data["activePath"].is_null() {
        let active = json!({ "path": data["activePath"], "rootId": data["activeRootId"] });
        let wanted = identity(&active)?;
        if !data["tabs"].as_array().unwrap().iter().any(|tab| identity(tab).is_ok_and(|known| known == wanted)) {
            return Err(HostError::invalid("The active path must name an open tab."));
        }
    }
    only_keys(&data["layout"], &["sidebarWidth", "agentWidth", "bottomHeight", "sidebarVisible", "agentVisible", "bottomVisible", "bottomTab"])?;
    if let Some(execution) = data.get("execution").filter(|execution| !execution.is_null()) {
        super::execution::check_configuration(execution)?;
    }
    Ok(())
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
            return Err(HostError::new("workspace-not-found", "The selected workspace is no longer registered."));
        }
        if let Some(record) = self.records.lock().unwrap().get(workspace_id) {
            return Ok(record.clone());
        }
        let unreadable = || HostError::new("io-error", "The saved IDE state of this project has an unsupported format; it was left unchanged.");
        let record = match read_json::<Value>(&self.directory.join(file_name(workspace_id))) {
            Ok(Some(record)) if record["version"] == 1 && record["revision"].is_u64() && check_data(&record["data"]).is_ok() => record,
            Ok(Some(_)) | Err(_) => return Err(unreadable()),
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
            return Err(HostError::new("workspace-not-found", "The selected workspace is no longer registered."));
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
            return Err(HostError::new("state-too-large", "The workspace recovery snapshot exceeds the configured storage budget."));
        }
        let current = self.current(workspace_id)?;
        if current["revision"].as_u64() != Some(base) {
            return Err(HostError::new("revision-conflict", "The workspace IDE state was saved by another client. Recover the current snapshot before replacing it.").with("currentState", current));
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
            Some("state.selection.save") => self.set_selection(request["workspaceId"].as_str()),
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
}
