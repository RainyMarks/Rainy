//! Workspace state persistence, layout sizes, and crash-safe unsaved-buffer recovery.

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::*;
use rainy_protocol::{IdeWorkspaceState, RpcError};
use serde_json::{Value, json};

use super::keys::split_key;
use super::{Ide, PaneLayout};

impl Ide {
    /// The pane sizes of the shown project: those changed in this window, else those saved; a size neither knows is
    /// `None`. None without a project.
    pub fn layout(&self) -> Option<PaneLayout> {
        let id = self.workspace_id()?;
        let changed = self.layout.as_ref().filter(|(project, _)| *project == id).map(|(_, layout)| *layout).unwrap_or_default();
        let saved = self.state.as_ref().filter(|_| !self.loading).map(|state| &state.data["layout"]);
        let read = |key: &str| saved.and_then(|layout| layout[key].as_f64()).map(|value| value as f32);
        Some(PaneLayout {
            sidebar_width: changed.sidebar_width.or_else(|| read("sidebarWidth")),
            agent_width: changed.agent_width.or_else(|| read("agentWidth")),
            bottom_height: changed.bottom_height.or_else(|| read("bottomHeight")),
        })
    }

    /// Change pane sizes of the shown project; they are saved with its state a moment later. Without a project nothing
    /// is kept.
    pub fn set_layout(&mut self, patch: PaneLayout, cx: &mut Context<Self>) {
        let Some(id) = self.workspace_id() else { return };
        let mut layout = self.layout.take().filter(|(project, _)| *project == id).map(|(_, layout)| layout).unwrap_or_default();
        layout.sidebar_width = patch.sidebar_width.or(layout.sidebar_width);
        layout.agent_width = patch.agent_width.or(layout.agent_width);
        layout.bottom_height = patch.bottom_height.or(layout.bottom_height);
        self.layout = Some((id, layout));
        self.schedule_state_save(cx);
    }

    /// Save tabs, the active file and expanded folders a moment after the last change.
    pub(super) fn schedule_state_save(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
            cx.background_executor().timer(Duration::from_millis(600)).await;
            let _ = this.update(cx, |this, cx| this.flush_state(cx));
        }));
    }

    /// Write the editor state now (`state.save`), keeping every field this interface does not use.
    pub(super) fn flush_state(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.state_request(cx) else { return };
        cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
            let result = request.await;
            let _ = this.update(cx, |this, _| {
                let _ = this.state_saved(result);
            });
        })
        .detach();
    }

    /// Write the editor state with its unsaved text now and resolve once the Host stored it, as before Rainy restarts
    /// into an update (3.x `rainy:flush-workbench`). A save that lost to another window's is written again on top of it.
    pub fn flush_now(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.save_task = None;
        let locale = self.locale(cx);
        let closed = move |_| crate::i18n::text(locale, "ide.ideClosed").to_owned();
        cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
            let mut attempts = 0;
            loop {
                let request = this.update(cx, |this, cx| this.state_request(cx)).map_err(closed)?;
                let Some(request) = request else { return Ok(()) };
                let result = request.await;
                let saved = this.update(cx, |this, _| this.state_saved(result)).map_err(closed)?;
                attempts += 1;
                match saved {
                    Ok(()) => return Ok(()),
                    Err((error, true)) if attempts < 2 => log::info!("The editor state is saved again after a conflict: {}", error.message),
                    Err((error, _)) => return Err(crate::i18n::error_text(locale, &error)),
                }
            }
        })
    }

    /// Take the Host's answer to `state.save`: the saved state, or the error and whether the state another window saved
    /// replaced this one's base.
    fn state_saved(&mut self, result: Result<IdeWorkspaceState, RpcError>) -> Result<(), (Box<RpcError>, bool)> {
        match result {
            Ok(saved) => {
                self.state = Some(saved);
                Ok(())
            }
            Err(error) => {
                let current = error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("currentState"))
                    .cloned()
                    .and_then(|current| serde_json::from_value::<IdeWorkspaceState>(current).ok());
                let replaced = current.is_some();
                if let Some(current) = current {
                    self.state = Some(current);
                }
                Err((Box::new(error), replaced))
            }
        }
    }

    /// The `state.save` request for what differs from the saved state, if anything does.
    fn state_request(&mut self, cx: &mut Context<Self>) -> Option<impl Future<Output = Result<IdeWorkspaceState, RpcError>> + use<>> {
        let (Some(id), Some(state)) = (self.workspace_id(), self.state.clone()) else { return None };
        let mut data = if state.data.is_object() { state.data.clone() } else { json!({}) };
        let previous: HashMap<String, Value> = data["tabs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tab| Some((self.key_of_stored(tab["path"].as_str()?, tab["rootId"].as_str()), tab.clone())))
            .collect();
        data["tabs"] = Value::Array(
            self.tabs
                .iter()
                .map(|key| {
                    let kind = if self.diff_tabs.contains(key) { "diff" } else { "file" };
                    let mut tab = previous.get(key).cloned().unwrap_or_else(|| json!({}));
                    if !tab.is_object() {
                        tab = json!({});
                    }
                    let place = Self::stored(key);
                    tab["path"] = place["path"].clone();
                    match place.get("rootId") {
                        Some(root) => tab["rootId"] = root.clone(),
                        None => drop(tab.as_object_mut().and_then(|tab| tab.remove("rootId"))),
                    }
                    tab["kind"] = json!(kind);
                    match self.views.get(key) {
                        Some(view) => tab["view"] = json!(view),
                        None => drop(tab.as_object_mut().and_then(|tab| tab.remove("view"))),
                    }
                    tab
                })
                .collect(),
        );
        let object = data.as_object_mut()?;
        match &self.active {
            Some(key) => {
                let place = Self::stored(key);
                object.insert("activePath".into(), place["path"].clone());
                match place.get("rootId") {
                    Some(root) => drop(object.insert("activeRootId".into(), root.clone())),
                    None => drop(object.remove("activeRootId")),
                }
            }
            None => {
                object.insert("activePath".into(), Value::Null);
                object.remove("activeRootId");
            }
        }
        let mut buffers: Vec<Value> = self
            .files
            .iter()
            .filter(|(_, file)| file.dirty)
            .filter_map(|(key, file)| {
                let content = file.editor.as_ref()?.read(cx).text().to_string();
                let mut buffer = Self::stored(key);
                buffer["content"] = json!(content);
                buffer["baseVersion"] = json!(file.document.version);
                buffer["bom"] = json!(file.document.bom);
                buffer["eol"] = json!(file.document.eol);
                Some(buffer)
            })
            .collect();
        buffers.extend(self.recovery.iter().filter(|(key, _)| !self.files.contains_key(*key) && self.tabs.contains(key)).map(|(_, buffer)| buffer.clone()));
        object.insert("buffers".into(), Value::Array(buffers));
        let (primary, attached): (Vec<&String>, Vec<&String>) = self.expanded.iter().partition(|key| split_key(key).0.is_none());
        object.insert("expandedPaths".into(), json!(primary));
        if attached.is_empty() {
            object.remove("expandedRoots");
        } else {
            object.insert("expandedRoots".into(), Value::Array(attached.iter().map(|key| Self::stored(key)).collect()));
        }
        let store = self.store.read(cx);
        let in_project = |chat: &&String| store.summaries.iter().any(|s| s.id == **chat && s.workspace_id.as_deref() == Some(id.as_str()));
        if let Some(selected) = store.selected.as_ref().filter(in_project) {
            data["lastSessionId"] = json!(selected);
        }
        if let Some((_, layout)) = self.layout.as_ref().filter(|(project, _)| *project == id) {
            if !data["layout"].is_object() {
                data["layout"] = json!({});
            }
            for (key, value) in [("sidebarWidth", layout.sidebar_width), ("agentWidth", layout.agent_width), ("bottomHeight", layout.bottom_height)] {
                if let Some(value) = value {
                    data["layout"][key] = json!(value.round() as i64);
                }
            }
        }
        if data == state.data {
            return None;
        }
        Some(self.request::<IdeWorkspaceState>(json!({ "op": "state.save", "workspaceId": id, "baseRevision": state.revision, "data": data }), cx))
    }
}
