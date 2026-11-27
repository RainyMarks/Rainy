//! Git/review diff tabs and hunk-level accept/revert with save confirmation.

use gpui_kit::component::diff::{DiffFile, DiffState};
use gpui_kit::*;
use rainy_protocol::{IdeFileDiff, IdeFileDocument, RpcError};
use serde_json::{Value, json};

use super::keys::{language_of, split_key};
use super::{DiffTab, Ide, IdeEvent, ide_error};

/// A unified diff of two texts with every line as context, so the viewer can fold unchanged runs itself.
pub fn unified_patch(path: &str, base: &str, current: &str) -> String {
    let diff = similar::TextDiff::from_lines(base, current);
    let mut out = String::new();
    let mut unified = diff.unified_diff();
    unified.context_radius(usize::MAX / 4).header(&format!("a/{path}"), &format!("b/{path}"));
    out.push_str(&unified.to_string());
    out
}

impl Ide {
    /// Show a file against its Git base in its tab.
    pub fn open_diff(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.contains(&key) {
            self.tabs.push(key.clone());
        }
        self.diff_tabs.insert(key.clone());
        self.active = Some(key.clone());
        cx.emit(IdeEvent::ActiveChanged);
        self.schedule_state_save(cx);
        self.load_diff(key, window, cx);
        cx.notify();
    }

    /// Show the editor of a tab that showed a diff.
    pub fn show_file(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        self.diff_tabs.remove(&key);
        self.diffs.remove(&key);
        self.open(key, true, window, cx);
    }

    /// Read a diff again (`files.diff`).
    pub fn load_diff(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let mut params = self.location(&key);
        params.insert("op".into(), json!("files.diff"));
        let request = self.request::<IdeFileDiff>(Value::Object(params), cx);
        self.diffs.entry(key.clone()).or_default();
        let workspace = self.workspace_id();
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                if this.workspace_id() != workspace || !this.diff_tabs.contains(&key) {
                    return;
                }
                let mut tab = DiffTab::default();
                match result {
                    Ok(diff) if diff.status == "unavailable" => tab.error = Some(diff.reason.unwrap_or_default().into()),
                    Ok(diff) => {
                        let base = diff.base.unwrap_or_default();
                        let current = diff.current.unwrap_or_default();
                        let patch = unified_patch(&diff.path, &base, &current);
                        tab.status = diff.status;
                        tab.hunks = crate::ui::ide::hunk_diff::compute_hunks(&base, &current);
                        tab.base = base;
                        tab.current = current;
                        match DiffFile::parse(&patch) {
                            Ok(files) => {
                                let language = language_of(&diff.path);
                                let files: Vec<DiffFile> = files.into_iter().map(|file| file.with_language(language)).collect();
                                tab.state = Some(cx.new(|cx| DiffState::new(files, cx)));
                            }
                            Err(error) => tab.error = Some(error.to_string().into()),
                        }
                    }
                    Err(error) => tab.error = Some(ide_error(this.locale(cx), &error)),
                }
                this.diffs.insert(key, tab);
                cx.notify();
            });
        })
        .detach();
    }

    /// Close the comparison tab of a file, as after its changes were undone.
    pub fn close_review(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.diff_tabs.contains(key) {
            self.close(key, cx);
        }
    }

    /// Show the agent's changes to a file as a comparison of the text before them with the text now.
    pub fn show_review(&mut self, key: String, base: String, current: String, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.contains(&key) {
            self.tabs.push(key.clone());
        }
        self.diff_tabs.insert(key.clone());
        self.active = Some(key.clone());
        let (_, path) = split_key(&key);
        let patch = unified_patch(path, &base, &current);
        let hunks = crate::ui::ide::hunk_diff::compute_hunks(&base, &current);
        let mut tab = DiffTab { status: "modified".into(), base, current, hunks, ..DiffTab::default() };
        match DiffFile::parse(&patch) {
            Ok(files) => {
                let language = language_of(path);
                let files: Vec<DiffFile> = files.into_iter().map(|file| file.with_language(language)).collect();
                tab.state = Some(cx.new(|cx| DiffState::new(files, cx)));
            }
            Err(error) => tab.error = Some(error.to_string().into()),
        }
        self.diffs.insert(key, tab);
        cx.emit(IdeEvent::ActiveChanged);
        self.schedule_state_save(cx);
        cx.notify();
    }

    /// Rebuild a `DiffTab`'s visual `DiffState` and hunks after a hunk-level accept/revert.
    fn update_diff_tab(&mut self, key: &str, base: String, current: String, cx: &mut Context<Self>) {
        let (_, path) = split_key(key);
        let patch = unified_patch(path, &base, &current);
        let hunks = crate::ui::ide::hunk_diff::compute_hunks(&base, &current);
        let status = if hunks.is_empty() { "unchanged".to_owned() } else { "modified".to_owned() };
        let mut tab = DiffTab { status, base, current, hunks, ..DiffTab::default() };
        if let Ok(files) = DiffFile::parse(&patch) {
            let language = language_of(path);
            let files: Vec<DiffFile> = files.into_iter().map(|file| file.with_language(language)).collect();
            tab.state = Some(cx.new(|cx| DiffState::new(files, cx)));
        }
        self.diffs.insert(key.to_owned(), tab);
        cx.notify();
    }

    /// Accept (`accept == true`) or revert (`accept == false`) one diff hunk and write back to the workspace file.
    /// Waits for `save_task` confirmation before updating the diff tab or showing a success toast so UI and disk never diverge.
    pub fn apply_diff_hunk(&mut self, key: String, hunk_index: usize, accept: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((base, current)) = self.diffs.get(&key).map(|tab| (tab.base.clone(), tab.current.clone())) else { return };
        let Some((next_base, next_current)) = crate::ui::ide::hunk_diff::apply_hunk_decision(&base, &current, hunk_index, accept) else {
            return;
        };
        let workspace = self.workspace_id();
        if accept {
            if self.is_dirty(&key) {
                let save = self.save_task(Some(key.clone()), false, window, cx);
                cx.spawn_in(window, async move |this, cx| {
                    if !save.await {
                        return;
                    }
                    let _ = this.update_in(cx, |this, window, cx| {
                        if this.workspace_id() == workspace {
                            this.update_diff_tab(&key, next_base, next_current, cx);
                            use gpui_kit::component::WindowExt as _;
                            window.push_notification(gpui_kit::component::notification::Notification::success(crate::ui::tr(cx, "native.diffHunkApplied")), cx);
                        }
                    });
                })
                .detach();
            } else {
                self.update_diff_tab(&key, next_base, next_current, cx);
                use gpui_kit::component::WindowExt as _;
                window.push_notification(gpui_kit::component::notification::Notification::success(crate::ui::tr(cx, "native.diffHunkApplied")), cx);
            }
            return;
        }
        // Revert hunk: update open editor buffer (and wait for save), or write back through Host `files.save` directly.
        if self.files.get(&key).and_then(|f| f.editor.as_ref()).is_some() {
            let previous_text = self.files.get(&key).and_then(|f| f.editor.as_ref()).map(|ed| ed.read(cx).text().to_string());
            self.set_text(&key, &next_current, window, cx);
            let save = self.save_task(Some(key.clone()), false, window, cx);
            cx.spawn_in(window, async move |this, cx| {
                let ok = save.await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.workspace_id() != workspace {
                        return;
                    }
                    if ok {
                        this.update_diff_tab(&key, next_base, next_current, cx);
                        use gpui_kit::component::WindowExt as _;
                        window.push_notification(gpui_kit::component::notification::Notification::success(crate::ui::tr(cx, "native.diffHunkApplied")), cx);
                    } else if let Some(prev) = previous_text {
                        // Restore editor content if saving failed so the editor matches disk/previous state.
                        this.set_text(&key, &prev, window, cx);
                    }
                });
            })
            .detach();
            return;
        }
        let mut read_params = self.location(&key);
        read_params.insert("op".into(), json!("files.read"));
        let read_call = self.store.read(cx).call("ide", Value::Object(read_params));
        let store = self.store.clone();
        let save_location = self.location(&key);
        cx.spawn_in(window, async move |this, cx| {
            let version = match read_call.await.and_then(|value| {
                serde_json::from_value::<IdeFileDocument>(value).map_err(|e| RpcError { code: "invalid".into(), message: e.to_string(), data: None })
            }) {
                Ok(doc) => doc.version,
                Err(error) => {
                    let _ = cx.update(|window, cx| {
                        use gpui_kit::component::WindowExt as _;
                        let text = crate::ui::trf(cx, "native.diffHunkFailed", &[("error", &crate::ui::error_text(cx, &error))]);
                        window.push_notification(gpui_kit::component::notification::Notification::error(text), cx);
                    });
                    return;
                }
            };
            let mut save_params = save_location;
            save_params.insert("op".into(), json!("files.save"));
            save_params.insert("content".into(), json!(next_current));
            save_params.insert("expectedVersion".into(), json!(version));
            let save_call = store.read_with(cx, |store, _| store.call("ide", Value::Object(save_params)));
            match save_call.await {
                Ok(_) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        if this.workspace_id() == workspace {
                            this.update_diff_tab(&key, next_base, next_current, cx);
                            cx.emit(IdeEvent::Saved);
                            use gpui_kit::component::WindowExt as _;
                            window.push_notification(
                                gpui_kit::component::notification::Notification::success(crate::ui::tr(cx, "native.diffHunkApplied")),
                                cx,
                            );
                        }
                    });
                }
                Err(error) => {
                    let _ = cx.update(|window, cx| {
                        use gpui_kit::component::WindowExt as _;
                        let text = crate::ui::trf(cx, "native.diffHunkFailed", &[("error", &crate::ui::error_text(cx, &error))]);
                        window.push_notification(gpui_kit::component::notification::Notification::error(text), cx);
                    });
                }
            }
        })
        .detach();
    }
}
