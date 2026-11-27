//! File listing, open/read/save/reload lifecycle, CRUD operations, and disk change polling.

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::component::input::{EditorState, InputEvent, Position};
use gpui_kit::*;
use rainy_protocol::{FileEntry, IdeDirectory, IdeFileChange, IdeFileDocument, RpcError};
use serde_json::{Value, json};

use super::keys::{child_key, file_key, language_of, parent_key, root_key, split_key};
use super::{Ide, IdeEvent, Listing, OpenFile, ide_error};

impl Ide {
    /// List one directory; emits `FilesChanged` and notifies only when the listing actually changed.
    pub fn list(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let mut params = self.location(&key);
        params.insert("op".into(), json!("files.list"));
        let request = self.request::<IdeDirectory>(Value::Object(params), cx);
        let workspace = self.workspace_id();
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                if this.workspace_id() != workspace {
                    return;
                }
                let listing = match result {
                    Ok(directory) => {
                        let mut entries = directory.entries;
                        entries.sort_by(|a, b| b.is_directory().cmp(&a.is_directory()).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                        Listing { entries, error: None }
                    }
                    Err(error) => Listing { entries: Vec::new(), error: Some(ide_error(this.locale(cx), &error)) },
                };
                if this.listings.get(&key) == Some(&listing) {
                    return;
                }
                this.listings.insert(key, listing);
                cx.emit(IdeEvent::FilesChanged);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn toggle_directory(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key.clone());
            self.list(key, window, cx);
        }
        self.schedule_state_save(cx);
        cx.emit(IdeEvent::FilesChanged);
        cx.notify();
    }

    /// Open a file in a tab, or show its tab; `focus` makes it the active tab.
    pub fn open(&mut self, key: String, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open_task(key, focus, window, cx).detach();
    }

    /// `open`, done once the file has been read.
    pub fn open_task(&mut self, key: String, focus: bool, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        if !self.tabs.contains(&key) {
            self.tabs.push(key.clone());
        }
        if focus {
            self.active = Some(key.clone());
            cx.emit(IdeEvent::ActiveChanged);
            self.schedule_state_save(cx);
        }
        cx.notify();
        if let Some(file) = self.files.get(&key) {
            if focus && let Some(editor) = file.editor.clone() {
                editor.update(cx, |state, cx| state.focus(window, cx));
            }
            return Task::ready(());
        }
        if focus {
            self.focus_next = Some(key.clone());
        }
        self.read_count += 1;
        let read = self.read_count;
        self.reads.insert(key.clone(), read);
        let mut params = self.location(&key);
        params.insert("op".into(), json!("files.read"));
        let request = self.request::<IdeFileDocument>(Value::Object(params), cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.reads.get(&key) != Some(&read) {
                    return;
                }
                this.reads.remove(&key);
                match result {
                    Ok(document) => this.accept_document(key, document, window, cx),
                    Err(error) => {
                        this.tabs.retain(|tab| *tab != key);
                        if this.active.as_deref() == Some(key.as_str()) {
                            this.active = this.tabs.last().cloned();
                            cx.emit(IdeEvent::ActiveChanged);
                        }
                        this.error = Some(ide_error(this.locale(cx), &error));
                        cx.notify();
                    }
                }
            });
        })
    }

    fn accept_document(&mut self, key: String, mut document: IdeFileDocument, window: &mut Window, cx: &mut Context<Self>) {
        let recovered = self
            .recovery
            .remove(&key)
            .and_then(|buffer| Some((buffer["content"].as_str()?.to_owned(), buffer["baseVersion"].as_str().map(str::to_owned))))
            .filter(|(content, _)| document.read_only_reason.is_none() && Some(content) != document.content.as_ref());
        let mut conflict = None;
        if let Some((_, Some(base))) = &recovered
            && *base != document.version
        {
            conflict = Some(std::mem::replace(&mut document.version, base.clone()));
        }
        let (editor, subscription, saved) = match document.content.clone().filter(|_| document.read_only_reason.is_none()) {
            Some(disk) => {
                let text = recovered.as_ref().map_or(disk.clone(), |(content, _)| content.clone());
                let language = language_of(&document.path);
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(language)
                        .line_number(true)
                        .searchable(true)
                        .default_value(text.clone())
                });
                let watched = key.clone();
                let subscription = cx.subscribe(&editor, move |this: &mut Ide, editor, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change)
                        && let Some(file) = this.files.get_mut(&watched)
                    {
                        let dirty = editor.read(cx).text() != file.saved.as_str();
                        let changed = dirty != file.dirty;
                        if changed {
                            file.dirty = dirty;
                            cx.notify();
                        }
                        if dirty || changed {
                            this.schedule_state_save(cx);
                        }
                    }
                });
                (Some(editor), Some(subscription), disk)
            }
            None => (None, None, String::new()),
        };
        if let (Some(editor), Some(target)) = (&editor, self.language_target(&key)) {
            let servers = crate::store::language::LanguageServers::global(cx);
            servers.update(cx, |servers, cx| servers.attach(&key, editor, target, cx));
        }
        let revealed = key.clone();
        let dirty = recovered.is_some() && editor.is_some();
        self.files.insert(
            key.clone(),
            OpenFile { document, editor, saved, dirty, conflict: conflict.is_some(), disk_version: conflict, _subscription: subscription },
        );
        self.apply_reveal(&revealed, window, cx);
        if self.focus_next.as_deref() == Some(revealed.as_str()) {
            self.focus_next = None;
            if let Some(editor) = self.files.get(&revealed).and_then(|file| file.editor.clone()) {
                editor.update(cx, |state, cx| state.focus(window, cx));
            }
        }
        cx.notify();
    }

    /// Open a file with the cursor at a zero-based line and UTF-16 column, as search results give them.
    pub fn reveal(&mut self, key: String, line: u32, column: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.diff_tabs.remove(&key);
        self.reveals.insert(key.clone(), (line, column));
        self.open(key.clone(), true, window, cx);
        self.apply_reveal(&key, window, cx);
    }

    fn apply_reveal(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.files.get(key).and_then(|file| file.editor.clone()) else { return };
        let Some((line, column)) = self.reveals.remove(key) else { return };
        editor.update(cx, |state, cx| {
            let text = state.value();
            let character = text.lines().nth(line as usize).map_or(0, |text| {
                let mut units = 0;
                text.chars().take_while(|c| {
                    units += c.len_utf16() as u32;
                    units <= column
                })
                .count() as u32
            });
            state.set_cursor_position(Position { line, character }, window, cx);
        });
    }

    /// Save the active file, or `key`; `overwrite` replaces a version changed on disk since it was read.
    pub fn save(&mut self, key: Option<String>, overwrite: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.save_task(key, overwrite, window, cx).detach();
    }

    /// Save every file with unsaved edits, as before a run (3.x `saveAll`); resolves whether all were saved.
    pub fn save_all(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        let dirty: Vec<String> = self.files.iter().filter(|(_, file)| file.dirty).map(|(key, _)| key.clone()).collect();
        let saves: Vec<Task<bool>> = dirty.into_iter().map(|key| self.save_task(Some(key), false, window, cx)).collect();
        cx.background_spawn(async move {
            let mut all = true;
            for save in saves {
                all &= save.await;
            }
            all
        })
    }

    /// `save`, resolving whether the file was saved (or had nothing to save).
    pub fn save_task(&mut self, key: Option<String>, overwrite: bool, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        let Some(key) = key.or_else(|| self.active.clone()) else { return Task::ready(true) };
        let Some(file) = self.files.get(&key) else { return Task::ready(true) };
        let Some(editor) = &file.editor else { return Task::ready(true) };
        if !file.dirty && !overwrite {
            return Task::ready(true);
        }
        let text = editor.read(cx).text().to_string();
        let expected = match (&file.disk_version, overwrite) {
            (Some(disk), true) => disk.clone(),
            _ => file.document.version.clone(),
        };
        let mut params = self.location(&key);
        params.insert("op".into(), json!("files.save"));
        params.insert("content".into(), json!(text));
        params.insert("expectedVersion".into(), json!(expected));
        let call = self.store.read(cx).call("ide", Value::Object(params));
        let workspace = self.workspace_id();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            this.update_in(cx, |this, window, cx| {
                let shown = this.workspace_id() == workspace;
                match result.and_then(|value| serde_json::from_value::<IdeFileDocument>(value).map_err(|e| RpcError { code: "invalid".into(), message: e.to_string(), data: None })) {
                    Ok(document) => {
                        if shown && let Some(file) = this.files.get_mut(&key) {
                            file.saved = text;
                            file.document = document;
                            file.dirty = file.editor.as_ref().is_some_and(|editor| editor.read(cx).text() != file.saved.as_str());
                            file.conflict = false;
                            file.disk_version = None;
                        }
                        cx.emit(IdeEvent::Saved);
                        if shown {
                            let servers = crate::store::language::LanguageServers::global(cx);
                            servers.update(cx, |servers, _| servers.saved(&key));
                        }
                        if this.closing.remove(&key) && shown {
                            this.close(&key, cx);
                        }
                        cx.notify();
                        true
                    }
                    Err(error) => {
                        this.closing.remove(&key);
                        if shown
                            && error.code == "version-conflict"
                            && let Some(file) = this.files.get_mut(&key)
                        {
                            file.conflict = true;
                            file.disk_version = error.data.as_ref().and_then(|data| data["currentVersion"].as_str()).map(str::to_owned);
                        }
                        use gpui_kit::component::WindowExt as _;
                        window.push_notification(crate::ui::error_notification(cx, &error), cx);
                        cx.notify();
                        false
                    }
                }
            })
            .unwrap_or(false)
        })
    }

    /// Save a tab, then close it.
    pub fn save_and_close(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        self.closing.insert(key.clone());
        self.save(Some(key), false, window, cx);
    }

    /// Read a file again from disk, dropping unsaved edits.
    pub fn reload(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.read_again(key, true, window, cx);
    }

    /// Read an open file again; detaches from LSP cleanly if re-reading fails.
    fn read_again(&mut self, key: &str, discard: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.files.contains_key(key) {
            return;
        }
        self.read_count += 1;
        let read = self.read_count;
        self.reads.insert(key.to_owned(), read);
        let mut params = self.location(key);
        params.insert("op".into(), json!("files.read"));
        let request = self.request::<IdeFileDocument>(Value::Object(params), cx);
        let key = key.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.reads.get(&key) != Some(&read) {
                    return;
                }
                this.reads.remove(&key);
                let Some(file) = this.files.get_mut(&key) else { return };
                if !discard {
                    if file.dirty {
                        if let Ok(document) = &result
                            && document.version != file.document.version
                        {
                            file.conflict = true;
                            file.disk_version = Some(document.version.clone());
                        }
                        cx.notify();
                        return;
                    }
                    if result.as_ref().is_ok_and(|document| document.version == file.document.version) {
                        return;
                    }
                }
                this.files.remove(&key);
                match result {
                    Ok(document) => this.accept_document(key, document, window, cx),
                    Err(error) => {
                        let servers = crate::store::language::LanguageServers::global(cx);
                        servers.update(cx, |servers, cx| servers.detach(&key, cx));
                        this.tabs.retain(|tab| *tab != key);
                        if this.active.as_deref() == Some(key.as_str()) {
                            this.active = this.tabs.last().cloned();
                            cx.emit(IdeEvent::ActiveChanged);
                        }
                        this.error = Some(ide_error(this.locale(cx), &error));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Choose how a tab shows its file; `None` is the default for the file type.
    pub fn set_view(&mut self, key: &str, view: Option<&str>, cx: &mut Context<Self>) {
        match view {
            Some(view) => drop(self.views.insert(key.to_owned(), view.to_owned())),
            None => drop(self.views.remove(key)),
        }
        self.schedule_state_save(cx);
        cx.notify();
    }

    /// Replace the unsaved text of an open file, as a viewer that edits it (rich Markdown) does.
    pub fn set_text(&mut self, key: &str, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.files.get(key).and_then(|file| file.editor.clone()) else { return };
        if editor.read(cx).text() == text {
            return;
        }
        editor.update(cx, |state, cx| state.set_value(text.to_owned(), window, cx));
        if let Some(file) = self.files.get_mut(key) {
            file.dirty = text != file.saved;
        }
        self.schedule_state_save(cx);
        cx.notify();
    }

    /// Close a tab; unsaved edits are the caller's to confirm first.
    pub fn close(&mut self, key: &str, cx: &mut Context<Self>) {
        let index = self.tabs.iter().position(|tab| tab == key);
        self.tabs.retain(|tab| tab != key);
        if self.files.remove(key).is_some() {
            let servers = crate::store::language::LanguageServers::global(cx);
            servers.update(cx, |servers, cx| servers.detach(key, cx));
        }
        self.reads.remove(key);
        self.diff_tabs.remove(key);
        self.diffs.remove(key);
        self.views.remove(key);
        if self.active.as_deref() == Some(key) {
            self.active = index.and_then(|index| self.tabs.get(index.min(self.tabs.len().saturating_sub(1))).cloned());
            cx.emit(IdeEvent::ActiveChanged);
        }
        self.schedule_state_save(cx);
        cx.notify();
    }

    /// The listed entry of a key.
    pub fn entry(&self, key: &str) -> Option<&FileEntry> {
        let (_, path) = split_key(key);
        self.listings.get(&parent_key(key))?.entries.iter().find(|entry| entry.path == path)
    }

    /// Whether a tab has unsaved edits.
    pub fn is_dirty(&self, key: &str) -> bool {
        self.files.get(key).is_some_and(|file| file.dirty)
    }

    /// List every shown directory again.
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.clone() else { return };
        for root in workspace.roots() {
            self.list(file_key("", Some(&root)), window, cx);
        }
        for key in self.expanded.clone() {
            self.list(key, window, cx);
        }
    }

    /// Run a file operation, show its failure, and list the touched directory again.
    fn file_operation(
        &mut self,
        op: &str,
        key: &str,
        extra: Value,
        then: impl FnOnce(&mut Self, Value, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut params = self.location(key);
        params.insert("op".into(), json!(op));
        if let Value::Object(extra) = extra {
            params.extend(extra);
        }
        let call = self.store.read(cx).call("ide", Value::Object(params));
        let parent = parent_key(key);
        let workspace = self.workspace_id();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let shown = this.workspace_id() == workspace;
                match result {
                    Ok(value) if shown => then(this, value, window, cx),
                    Ok(_) => {}
                    Err(error) => {
                        use gpui_kit::component::WindowExt as _;
                        let notification = match error.code.as_str() {
                            "already-exists" => gpui_kit::component::notification::Notification::error(crate::ui::tr(cx, "ide.ideAlreadyExists")),
                            _ => crate::ui::error_notification(cx, &error),
                        };
                        window.push_notification(notification, cx);
                    }
                }
                if shown {
                    this.list(parent, window, cx);
                }
            });
        })
        .detach();
    }

    /// Create a file or folder in a directory; a new file opens.
    pub fn create(&mut self, directory: String, name: String, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        let key = child_key(&directory, name.trim().trim_matches('/'));
        if !split_key(&directory).1.is_empty() && self.expanded.insert(directory.clone()) {
            self.schedule_state_save(cx);
        }
        let (op, extra) = if folder { ("files.mkdir", json!({})) } else { ("files.create", json!({ "content": "" })) };
        let opened = key.clone();
        self.file_operation(op, &key, extra, move |this, _, window, cx| {
            if !folder {
                this.open(opened, true, window, cx);
            }
        }, window, cx);
    }

    /// Give a file or folder a new name in the same directory.
    pub fn rename(&mut self, key: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry(&key).cloned() else { return };
        let (_, path) = split_key(&key);
        let destination = match path.rsplit_once('/') {
            Some((parent, _)) => format!("{parent}/{}", name.trim()),
            None => name.trim().to_owned(),
        };
        let renamed = child_key(&parent_key(&key), name.trim());
        let was_open = self.tabs.contains(&key);
        let old = key.clone();
        self.file_operation("files.rename", &key, json!({ "destination": destination, "expectedVersion": entry.version }), move |this, _, window, cx| {
            let gone: Vec<String> = this.tabs.iter().filter(|tab| **tab == old || tab.starts_with(&format!("{old}/"))).cloned().collect();
            for tab in gone {
                this.close(&tab, cx);
            }
            if was_open {
                this.open(renamed, true, window, cx);
            }
        }, window, cx);
    }

    /// What deleting a path would remove, with the token that confirms it (`files.deletePreview`).
    pub fn delete_preview(&self, key: &str, cx: &App) -> impl Future<Output = Result<Value, RpcError>> + use<> {
        let mut params = self.location(key);
        params.insert("op".into(), json!("files.deletePreview"));
        self.store.read(cx).call("ide", Value::Object(params))
    }

    /// Delete a path confirmed by its preview token; its tabs close.
    pub fn delete(&mut self, key: String, token: String, window: &mut Window, cx: &mut Context<Self>) {
        let old = key.clone();
        self.file_operation("files.delete", &key, json!({ "token": token }), move |this, _, _, cx| {
            let gone: Vec<String> = this.tabs.iter().filter(|tab| **tab == old || tab.starts_with(&format!("{old}/"))).cloned().collect();
            for tab in gone {
                this.close(&tab, cx);
            }
        }, window, cx);
    }

    /// The Host noticed a change of an open file or a run (`ide.hint`).
    pub(super) fn on_host_event(&mut self, name: &str, data: &Value, cx: &mut Context<Self>) {
        let files_hint = name == "ide.hint"
            && data.get("workspaceId").and_then(Value::as_str) == self.workspace_id().as_deref()
            && data.get("kind").and_then(Value::as_str) == Some("files");
        if files_hint || name == "session.changes" {
            self.check_files(cx);
            let window = self.window;
            self.relist = Some(cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
                cx.background_executor().timer(Duration::from_millis(300)).await;
                let (Some(window), Some(ide)) = (window, this.upgrade()) else { return };
                let _ = window.update(cx, |_, window, cx| ide.update(cx, |ide, cx| ide.refresh(window, cx)));
            }));
        }
    }

    /// Compare open files with the disk; emits `FilesChanged` and notifies only when a file actually changed or conflicted.
    pub fn check_files(&mut self, cx: &mut Context<Self>) {
        let mut by_root: HashMap<Option<String>, Vec<String>> = HashMap::new();
        for key in self.files.keys() {
            let (root, path) = split_key(key);
            by_root.entry(root.map(str::to_owned)).or_default().push(path.to_owned());
        }
        let workspace = self.workspace_id();
        for (root, paths) in by_root {
            let mut params = json!({ "op": "files.changes", "workspaceId": workspace, "paths": paths });
            if let Some(root) = &root {
                params["rootId"] = json!(root);
            }
            let request = self.request::<Vec<IdeFileChange>>(params, cx);
            let window = self.window;
            let workspace = workspace.clone();
            cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
                let Ok(changes) = request.await else { return };
                let changed = this.update(cx, |this, cx| {
                    if this.workspace_id() != workspace {
                        return Vec::new();
                    }
                    let mut conflicted = false;
                    let changed: Vec<String> = changes
                        .into_iter()
                        .filter_map(|change| {
                            let key = root_key(root.as_deref(), &change.path);
                            let file = this.files.get_mut(&key)?;
                            if change.version.as_deref() == Some(file.document.version.as_str()) {
                                return None;
                            }
                            if file.dirty {
                                if !file.conflict || file.disk_version != change.version {
                                    file.conflict = true;
                                    file.disk_version = change.version.clone();
                                    conflicted = true;
                                }
                                None
                            } else {
                                Some(key)
                            }
                        })
                        .collect();
                    if !changed.is_empty() || conflicted {
                        cx.emit(IdeEvent::FilesChanged);
                        cx.notify();
                    }
                    changed
                });
                let (Ok(changed), Some(window), Some(ide)) = (changed, window, this.upgrade()) else { return };
                for key in changed {
                    let _ = window.update(cx, |_, window, cx| ide.update(cx, |ide, cx| ide.read_again(&key, false, window, cx)));
                }
            })
            .detach();
        }
    }
}
