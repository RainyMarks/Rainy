//! The editor area (`IdeEditor.tsx` and the tab strip of `Workbench.tsx`): open files as tabs, the active one in an
//! editor or a viewer, and what a window without a project or without open files shows.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::diff::Diff;
use gpui_kit::component::input::{Editor, EditorMode, EditorState, Enter, IndentInline, InputEvent, InputModeKind as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use rainy_carrier::ExecutionTarget;
use serde_json::{Value, json};

use crate::store::Store;
use crate::store::ide::{Ide, file_name, split_key};
use crate::ui::ide::viewers::{self, PageRequest};
use crate::ui::ide::ai_edit::{self, AiEdit, AiEditEvent};
use crate::ui::ide::{AiEditSelection, MarkdownPreview, MarkdownSplit, choose_folder, editor_commands, file_icon};
use crate::ui::{tr, trf};
use crate::web::{PaneEvent, WebPane};

/// A file shown in a page: which viewer, for which version of the file, and the text the page sent last.
struct Viewer {
    kind: &'static str,
    version: String,
    pane: Entity<WebPane>,
    /// Monotonic counter of when this viewer was last accessed, for LRU hibernation.
    last_used: u64,
    /// The editor whose text a Markdown page shows. Reading the file again from disk gives the tab a new editor; a page
    /// left on the old one would keep the old text, and its next edit would write that over the new file.
    editor: Option<EntityId>,
    /// The page's own edit, so it is not sent back to it.
    from_page: Option<String>,
    /// The split view's scrolling, for the preview and its source editor to follow each other.
    scroll: ScrollSync,
    _subscriptions: Vec<Subscription>,
    /// Changes and scrolling of `editor`, replaced together with it.
    _editor_subscriptions: Vec<Subscription>,
}

/// Last known scroll position of a viewer pane so it can be restored after LRU hibernation.
#[derive(Clone, Copy, Debug)]
enum SavedScroll {
    Line(f64),
    Top(f64),
}

/// Maximum number of active WebView2 viewer panes kept alive simultaneously (LRU eviction above this).
const MAX_ACTIVE_VIEWERS: usize = 4;
/// Maximum number of decoded fallback image entries retained in memory.
const MAX_PICTURES: usize = 16;

/// Where the split view's source editor was last seen, and when the preview last moved it.
#[derive(Default)]
struct ScrollSync {
    /// The editor's top line, 1-based with the fraction of the line scrolled past.
    line: Option<f64>,
    /// The editor scrolls to the preview's line a frame or two after the preview reports it; that scroll is not sent
    /// back, or the preview would be pulled to where the editor stops, such as above its own end.
    page_at: Option<Instant>,
}

/// How long after the preview scrolled the editor the editor's own scrolling is taken as that one.
const PAGE_SCROLL_QUIET: Duration = Duration::from_millis(250);

/// An image file's decoded bytes, fetched from the Host's `/rainy/ide/raw` route, for when its page cannot be shown.
enum Picture {
    Loading,
    Ready(Arc<Image>),
    /// What went wrong, and the Host's own answer as a detail.
    Failed(SharedString, Option<SharedString>),
}

/// What the editor area asks of the window around it.
pub enum EditorAreaEvent {
    /// Add an image a page made to the AI composer.
    AttachImage { name: String, bytes: Vec<u8> },
}

impl EventEmitter<EditorAreaEvent> for EditorArea {}

pub struct EditorArea {
    ide: Entity<Ide>,
    /// Pictures by file key and version, so a changed file is fetched again.
    pictures: HashMap<(String, String), Picture>,
    /// Pages by file key (bounded to `MAX_ACTIVE_VIEWERS` via LRU hibernation).
    viewers: HashMap<String, Viewer>,
    /// Monotonic tick for WebPane LRU tracking.
    viewer_tick: u64,
    /// Saved scroll positions by file key, restored when a hibernated WebPane is recreated.
    saved_scroll: HashMap<String, SavedScroll>,
    /// The tab strip, scrolled so the active tab is in view when it changes.
    tabs_scroll: ScrollHandle,
    scrolled_to: Option<String>,
    /// Frames left to bring the active tab into view; the strip has no size until it was laid out once.
    scroll_retries: u8,
    /// Ctrl+K on a file's editor.
    ai_edit: Option<Entity<AiEdit>>,
    /// The project map, shown in place of the open files, for the project it was opened for.
    map: Option<(String, Entity<WebPane>)>,
    map_shown: bool,
    /// The chat the map page was told about last, so it follows the chat selected now.
    map_session: Option<Option<String>>,
    /// The heading at the top of each rich editor, by file key.
    outline: HashMap<String, i64>,
    /// An export to start once the file's page is ready.
    pending_export: Option<(String, &'static str)>,
    /// True to show interactive per-hunk diff review (Accept/Revert Hunk); false for side-by-side Diff view.
    diff_hunk_mode: bool,
    _subscriptions: Vec<Subscription>,
}

fn image_format(path: &str) -> Option<ImageFormat> {
    Some(match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "bmp" => ImageFormat::Bmp,
        "ico" => ImageFormat::Ico,
        "svg" => ImageFormat::Svg,
        "tif" | "tiff" => ImageFormat::Tiff,
        _ => return None,
    })
}

use rainy_carrier::text::percent_encode as encode;

impl EditorArea {
    pub fn new(ide: Entity<Ide>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe(&ide, |this, _, cx| {
                this.follow_editors(cx);
                cx.notify();
            }),
            // The map lists the footprint and marks the changes of the chat selected now.
            cx.observe(&Store::global(cx), |this, _, cx| {
                if this.map_session.is_some() {
                    this.send_session(false, cx);
                }
            }),
            // Choosing a tab or opening a file brings the files back in place of the map.
            cx.subscribe(&ide, |this, _, event: &crate::store::ide::IdeEvent, cx| {
                if *event == crate::store::ide::IdeEvent::ActiveChanged && this.map_shown {
                    this.map_shown = false;
                    cx.notify();
                }
                // Pages address their project; another project's file under the same key gets a page of its own.
                if *event == crate::store::ide::IdeEvent::WorkspaceChanged {
                    this.viewers.clear();
                    this.saved_scroll.clear();
                    this.pictures.clear();
                }
            }),
        ];
        Self { ide, pictures: HashMap::new(), viewers: HashMap::new(), viewer_tick: 0, saved_scroll: HashMap::new(), tabs_scroll: ScrollHandle::new(), scrolled_to: None, scroll_retries: 0, ai_edit: None, map: None, map_shown: false, map_session: None, outline: HashMap::new(), pending_export: None, diff_hunk_mode: true, _subscriptions: subscriptions }
    }

    fn next_viewer_tick(&mut self) -> u64 {
        self.viewer_tick = self.viewer_tick.wrapping_add(1);
        self.viewer_tick
    }

    /// Evict least-recently-used WebPane instances when more than `MAX_ACTIVE_VIEWERS` are alive,
    /// preserving any known scroll position so switching back restores the reader's place.
    fn evict_lru_viewers(&mut self, keep_key: &str) {
        while self.viewers.len() > MAX_ACTIVE_VIEWERS {
            let victim = self
                .viewers
                .iter()
                .filter(|(k, _)| k.as_str() != keep_key)
                .min_by_key(|(_, viewer)| viewer.last_used)
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else { break };
            if let Some(removed) = self.viewers.remove(&victim) {
                if let Some(line) = removed.scroll.line {
                    self.saved_scroll.insert(victim, SavedScroll::Line(line));
                }
            }
        }
    }

    /// The page of a file view, created on first use and again when the view or the file's version changes.
    fn viewer(&mut self, key: &str, kind: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<WebPane>> {
        let ide = self.ide.read(cx);
        let file = ide.files.get(key)?;
        // Pages of the editor's text follow it as it changes; the others show one version of the file's bytes.
        let live = viewers::shows_text(kind);
        let version = file.document.version.clone();
        if self.viewers.get(key).is_some_and(|viewer| viewer.kind == kind && (live || viewer.version == version)) {
            let tick = self.next_viewer_tick();
            if let Some(viewer) = self.viewers.get_mut(key) {
                viewer.last_used = tick;
                return Some(viewer.pane.clone());
            }
        }
        // Snapshot the previous pane's scroll position when another active viewer is being switched away from.
        for (other_key, other) in &self.viewers {
            if other_key != key {
                if let Some(view) = other.pane.read(cx).view() {
                    let probe = "(() => { const el = document.scrollingElement || document.documentElement; const top = el ? el.scrollTop : window.scrollY; if (Number.isFinite(top) && top > 0 && window.ipc) { window.ipc.postMessage(JSON.stringify({ type: 'scroll-top', top })); } })()";
                    view.execute(probe, |_| {});
                }
            }
        }
        // A hex page chosen in place of the file's own view offers the way back to it.
        let back = kind == "hex"
            && ide.views.get(key).map(String::as_str) == Some("hex")
            && viewers::page_view(&file.document.path, file.document.read_only_reason.as_deref(), None).map(|(shown, _)| shown) != Some("hex");
        let launch = Store::global(cx).read(cx).launch_url()?;
        let workspace = ide.workspace.clone()?;
        // The desktop opens Office files in their own application only where it can reach them.
        let windows = matches!(Store::global(cx).read(cx).target, None | Some(ExecutionTarget::Windows { .. }));
        let host_path = windows.then(|| {
            let (root, path) = split_key(key);
            let base = workspace.roots().iter().find(|known| Some(known.root_id.as_str()) == root || (root.is_none() && known.primary)).map_or(workspace.path.clone(), |known| known.path.clone());
            format!("{}\\{}", base.trim_end_matches(['\\', '/']), path.replace('/', "\\"))
        });
        let url = viewers::viewer_url(&launch, kind, key, &workspace.workspace_id, file.document.bytes as u64, &version, host_path.as_deref(), back);
        let editor = file.editor.clone().filter(|_| live);
        let pane = viewers::create_pane(url, kind, window, cx);
        let owner = key.to_owned();
        let subscriptions = vec![cx.subscribe_in(&pane, window, move |this, _, event: &PaneEvent, window, cx| this.on_page(&owner, event, window, cx))];
        let editor_subscriptions = editor.as_ref().map(|editor| self.watch_editor(key, kind, editor, cx)).unwrap_or_default();
        let tick = self.next_viewer_tick();
        self.viewers.insert(
            key.to_owned(),
            Viewer {
                kind,
                version,
                pane: pane.clone(),
                last_used: tick,
                editor: editor.as_ref().map(Entity::entity_id),
                from_page: None,
                scroll: ScrollSync::default(),
                _subscriptions: subscriptions,
                _editor_subscriptions: editor_subscriptions,
            },
        );
        self.evict_lru_viewers(key);
        Some(pane)
    }

    /// Follow the editor of a Markdown or SVG file: its changes go to the page, and in the split view its scrolling too.
    fn watch_editor(&mut self, key: &str, kind: &'static str, editor: &Entity<EditorState>, cx: &mut Context<Self>) -> Vec<Subscription> {
        let owner = key.to_owned();
        let mut subscriptions = vec![cx.subscribe(editor, move |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.send_text(&owner, cx);
            }
        })];
        // The editor has no scroll event, but it is notified whenever its offset changes (wheel, scroll bar, caret).
        if kind == "markdown" {
            let owner = key.to_owned();
            subscriptions.push(cx.observe(editor, move |this, editor, cx| this.editor_scrolled(&owner, &editor, false, cx)));
        }
        subscriptions
    }

    /// Markdown and SVG pages follow their file's current editor. Reading a file again from disk (the conflict bar's Reload, or
    /// an unedited buffer following a change on disk) replaces the editor; the page then gets the new text.
    fn follow_editors(&mut self, cx: &mut Context<Self>) {
        let ide = self.ide.read(cx);
        let replaced: Vec<(String, &'static str, Option<Entity<EditorState>>)> = self
            .viewers
            .iter()
            .filter(|(_, viewer)| viewers::shows_text(viewer.kind))
            .filter_map(|(key, viewer)| {
                // A tab whose file is not open has nothing to follow; its page goes with the tab.
                let editor = ide.files.get(key)?.editor.clone();
                (editor.as_ref().map(Entity::entity_id) != viewer.editor).then(|| (key.clone(), viewer.kind, editor))
            })
            .collect();
        for (key, kind, editor) in replaced {
            let subscriptions = editor.as_ref().map(|editor| self.watch_editor(&key, kind, editor, cx)).unwrap_or_default();
            if let Some(viewer) = self.viewers.get_mut(&key) {
                viewer.editor = editor.as_ref().map(Entity::entity_id);
                viewer._editor_subscriptions = subscriptions;
                viewer.from_page = None;
                viewer.scroll = ScrollSync::default();
            }
            self.send_text(&key, cx);
        }
    }

    /// Hand a Markdown or SVG page the file's current text, unless the page wrote it itself.
    fn send_text(&mut self, key: &str, cx: &mut Context<Self>) {
        let ide = self.ide.read(cx);
        let Some(file) = ide.files.get(key) else { return };
        let Some(editor) = file.editor.as_ref() else { return };
        let text = editor.read(cx).value().to_string();
        let version = file.document.version.clone();
        let Some(viewer) = self.viewers.get_mut(key) else { return };
        if !viewers::shows_text(viewer.kind) || viewer.from_page.as_deref() == Some(text.as_str()) {
            return;
        }
        viewer.from_page = None;
        if let Some(view) = viewer.pane.read(cx).view() {
            view.execute(&viewers::set_text_script(&text, &version), |_| {});
        }
    }

    /// Whether a file's tab shows its source editor next to the preview.
    fn split_shown(&self, key: &str, cx: &App) -> bool {
        self.ide.read(cx).views.get(key).map(String::as_str) == Some("split") && self.viewers.get(key).is_some_and(|viewer| viewer.kind == "markdown")
    }

    /// The split view's source editor was notified: when it scrolled, the preview scrolls to its top line, unless the
    /// preview has just scrolled it there. `force` sends the line even when it did not change, as to a page just loaded.
    fn editor_scrolled(&mut self, key: &str, editor: &Entity<EditorState>, force: bool, cx: &mut Context<Self>) {
        if !self.split_shown(key, cx) {
            return;
        }
        let Some(line) = top_line(editor.read(cx)) else { return };
        let Some(viewer) = self.viewers.get_mut(key) else { return };
        if viewer.scroll.line == Some(line) && !force {
            return;
        }
        viewer.scroll.line = Some(line);
        if viewer.scroll.page_at.is_some_and(|at| at.elapsed() < PAGE_SCROLL_QUIET) {
            return;
        }
        if let (Some(script), Some(view)) = (scroll_script(line), viewer.pane.read(cx).view()) {
            view.execute(&script, |_| {});
        }
    }

    /// The user scrolled the split view's preview: the source editor puts the line at the preview's top at its own top.
    fn page_scrolled(&mut self, key: &str, line: f64, cx: &mut Context<Self>) {
        if !self.split_shown(key, cx) {
            return;
        }
        let Some(editor) = self.ide.read(cx).files.get(key).and_then(|file| file.editor.clone()) else { return };
        if let Some(viewer) = self.viewers.get_mut(key) {
            viewer.scroll.page_at = Some(Instant::now());
        }
        editor.update(cx, |state, cx| {
            let Some(height) = state.line_height().map(f32::from).filter(|height| *height > 0.) else { return };
            let map = state.display_map();
            let row = row_at_line(line, map.buffer_line_count(), |index| map.buffer_line_to_display_row_range(index), |index| map.buffer_line_to_display_row(index));
            let offset = state.scroll_offset();
            state.set_scroll_offset(point(offset.x, px(-(row as f32) * height)), cx);
        });
    }

    /// What a page asked for.
    fn on_page(&mut self, key: &str, event: &PaneEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The preview's scroll position is read here, before the general parser, which does not know it.
        if let PaneEvent::Message(text) = event {
            if let Some(line) = scroll_message(text) {
                self.saved_scroll.insert(key.to_owned(), SavedScroll::Line(line));
                if let Some(viewer) = self.viewers.get_mut(key) {
                    viewer.scroll.line = Some(line);
                }
                self.page_scrolled(key, line, cx);
                return;
            }
            if let Some(top) = scroll_top_message(text) {
                self.saved_scroll.insert(key.to_owned(), SavedScroll::Top(top));
                return;
            }
        }
        match event {
            PaneEvent::Message(text) => match viewers::parse_message(text) {
                Some(PageRequest::Ready) => {
                    // A page that (re)loaded has no text yet, whatever it sent before.
                    if let Some(viewer) = self.viewers.get_mut(key) {
                        viewer.from_page = None;
                    }
                    self.send_text(key, cx);
                    // The preview of a split view starts where the editor is; otherwise restore any hibernated scroll position.
                    if self.split_shown(key, cx)
                        && let Some(editor) = self.ide.read(cx).files.get(key).and_then(|file| file.editor.clone())
                    {
                        self.editor_scrolled(key, &editor, true, cx);
                    } else if let Some(saved) = self.saved_scroll.get(key).copied()
                        && let Some(view) = self.viewers.get(key).and_then(|viewer| viewer.pane.read(cx).view())
                    {
                        match saved {
                            SavedScroll::Line(line) => {
                                if let Some(script) = scroll_script(line) {
                                    view.execute(&script, |_| {});
                                }
                            }
                            SavedScroll::Top(top) => {
                                if let Some(script) = restore_scroll_top_script(top) {
                                    view.execute(&script, |_| {});
                                }
                            }
                        }
                    }
                    if self.pending_export.as_ref().is_some_and(|(pending, _)| pending == key)
                        && let Some((_, format)) = self.pending_export.take()
                        && let Some(view) = self.viewers.get(key).and_then(|viewer| viewer.pane.read(cx).view())
                    {
                        view.execute(&viewers::export_script(format), |_| {});
                    }
                }
                Some(PageRequest::Outline(index)) => {
                    self.outline.insert(key.to_owned(), index);
                    cx.notify();
                }
                Some(PageRequest::Notice { error, text }) => {
                    use gpui_kit::component::notification::Notification;
                    window.push_notification(if error { Notification::error(text) } else { Notification::success(text) }, cx);
                }
                Some(PageRequest::Change(text)) => {
                    if let Some(viewer) = self.viewers.get_mut(key) {
                        viewer.from_page = Some(text.clone());
                    }
                    let key = key.to_owned();
                    self.ide.update(cx, |ide, cx| ide.set_text(&key, &text, window, cx));
                }
                Some(PageRequest::Save) => {
                    let key = key.to_owned();
                    self.ide.update(cx, |ide, cx| ide.save(Some(key), false, window, cx));
                }
                Some(PageRequest::OpenUrl(url)) => viewers::open_page_link(&url, window, cx),
                Some(PageRequest::OpenFile { path, root, line }) => {
                    let key = crate::store::ide::root_key(root.as_deref(), &path);
                    self.ide.update(cx, |ide, cx| match line {
                        Some(line) => ide.reveal(key, (line as u32).saturating_sub(1), 0, window, cx),
                        None => ide.open(key, true, window, cx),
                    });
                }
                Some(PageRequest::Call { id, name, params }) => self.answer(key, id, &name, &params, window, cx),
                Some(PageRequest::Attach { name, bytes }) => cx.emit(EditorAreaEvent::AttachImage { name, bytes }),
                Some(PageRequest::View(view)) => {
                    let key = key.to_owned();
                    self.ide.update(cx, |ide, cx| ide.set_view(&key, view.as_deref(), cx));
                }
                Some(PageRequest::Reload) => {
                    let key = key.to_owned();
                    if !self.ide.read(cx).is_dirty(&key) {
                        self.ide.update(cx, |ide, cx| ide.reload(&key, window, cx));
                    }
                }
                Some(PageRequest::Chord(keys)) => {
                    if let Some(action) = viewers::chord_action(&keys, self.viewers.get(key).map_or("", |viewer| viewer.kind)) {
                        self.run_page_action(action, window, cx);
                    }
                }
                Some(PageRequest::OpenKey { .. } | PageRequest::Ask(_)) | None => {}
            },
            PaneEvent::Shortcut(shortcut) => {
                if let Some(action) = viewers::shortcut_action(*shortcut, self.viewers.get(key).map_or("", |viewer| viewer.kind)) {
                    self.run_page_action(action, window, cx);
                }
            }
            PaneEvent::NewWindow(url) => viewers::open_page_link(url, window, cx),
            PaneEvent::Ready | PaneEvent::Failed(_) => cx.notify(),
        }
    }

    /// Run a workbench key pressed in a page. The keyboard comes back to the window; the Markdown view keys are this
    /// area's own and run here, since GPUI's focus may be anywhere while a page had the keyboard and a dispatched action
    /// would not reach the area.
    fn run_page_action(&mut self, action: Box<dyn Action>, window: &mut Window, cx: &mut Context<Self>) {
        crate::web::reclaim_focus(window);
        if action.as_any().is::<MarkdownPreview>() {
            self.toggle_markdown_view("preview", cx);
        } else if action.as_any().is::<MarkdownSplit>() {
            self.toggle_markdown_view("split", cx);
        } else {
            window.dispatch_action(action, cx);
        }
    }

    /// Requests only the desktop can carry out for a page.
    fn answer(&mut self, key: &str, id: u64, name: &str, params: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.viewers.get(key).map(|viewer| viewer.pane.clone()) else { return };
        let respond = move |result: Value, cx: &mut App| {
            if let Some(view) = pane.read(cx).view() {
                view.execute(&viewers::resolve_script(id, &result), |_| {});
            }
        };
        match name {
            "open-external" => {
                let raw = params["path"].as_str().unwrap_or_default();
                let windows = matches!(Store::global(cx).read(cx).target, None | Some(ExecutionTarget::Windows { .. }));
                let roots: Vec<String> = self
                    .ide
                    .read(cx)
                    .workspace
                    .as_ref()
                    .map(|ws| {
                        let mut paths: Vec<String> = ws.roots().iter().map(|r| r.path.clone()).collect();
                        if !paths.iter().any(|p| p == &ws.path) {
                            paths.push(ws.path.clone());
                        }
                        paths
                    })
                    .unwrap_or_default();
                let validated = if windows { validate_open_external_path(raw, &roots) } else { None };
                match validated {
                    Some(safe_path) => {
                        if params["reveal"] == true {
                            cx.reveal_path(&safe_path);
                        } else {
                            cx.open_with_system(&safe_path);
                        }
                        respond(json!({}), cx);
                    }
                    None => {
                        log::warn!("Blocked open-external request for path outside workspace or unsafe type: {raw}");
                        respond(json!({ "error": tr(cx, "native.openExternalDenied").to_string() }), cx);
                    }
                }
            }
            // An exported Markdown document: ask where, then write the page or print it to PDF.
            "export-save" => {
                let pdf = params["format"] == "pdf";
                let html = params["html"].as_str().unwrap_or_default().to_owned();
                let name = crate::web::export::file_name(params["name"].as_str().unwrap_or_default());
                let extension = if pdf { "pdf" } else { "html" };
                let title = tr(cx, if pdf { "viewers.exportPdf" } else { "viewers.exportHtml" }).trim_end_matches('…').to_owned();
                let folder = self.export_folder(key, cx);
                // Debug builds under test save without the dialog, which would come up over the tester's own windows.
                let qa_folder = if cfg!(debug_assertions) { std::env::var_os("RAINY_QA_EXPORT_DIR").map(std::path::PathBuf::from) } else { None };
                let qa_path = qa_folder.map(|folder| folder.join(format!("{name}.{extension}")));
                let chosen = qa_path.is_none().then(|| {
                    crate::ui::save_dialog::save(
                        window,
                        crate::ui::save_dialog::SaveDialog {
                            title,
                            folder,
                            name: format!("{name}.{extension}"),
                            type_name: extension.to_uppercase(),
                            extension: extension.to_owned(),
                        },
                    )
                });
                let parent = crate::web::hwnd(window);
                let profile = Store::global(cx).read(cx).paths.user_data.join("WebView2");
                cx.spawn_in(window, async move |_, cx| {
                    let answer = match qa_path {
                        Some(path) => Ok(Ok(Some(path))),
                        None => match chosen {
                            Some(chosen) => chosen.recv().await,
                            None => Ok(Ok(None)),
                        },
                    };
                    let path = match answer {
                        Ok(Ok(Some(path))) => path,
                        Ok(Ok(None)) => {
                            let _ = cx.update(|_, cx| respond(json!({}), cx));
                            return;
                        }
                        Ok(Err(error)) => {
                            let _ = cx.update(|_, cx| respond(json!({ "error": error.to_string() }), cx));
                            return;
                        }
                        // The dialog's thread ended without an answer.
                        Err(_) => {
                            let _ = cx.update(|_, cx| respond(json!({ "error": tr(cx, "viewers.exportDialogFailed").to_string() }), cx));
                            return;
                        }
                    };
                    let bytes = if pdf {
                        let (sender, receiver) = async_channel::bounded(1);
                        let started = cx.update(|_, _| match parent {
                            Some(parent) => {
                                crate::web::export::print_to_pdf(parent, profile, html, move |result| {
                                    let _ = sender.try_send(result);
                                });
                                true
                            }
                            None => false,
                        });
                        if !matches!(started, Ok(true)) {
                            let _ = cx.update(|_, cx| respond(json!({ "error": tr(cx, "viewers.exportNoWindow").to_string() }), cx));
                            return;
                        }
                        let made = receiver.recv().await;
                        let Ok(made) = cx.update(|_, cx| {
                            crate::web::export::close_finished();
                            match made {
                                Ok(made) => made.map_err(|failure| pdf_failure(&failure, cx)),
                                // The print callback was dropped without running.
                                Err(_) => Err(tr(cx, "viewers.exportPdfNotMade").to_string()),
                            }
                        }) else {
                            return;
                        };
                        made
                    } else {
                        Ok(html.into_bytes())
                    };
                    let target = path.clone();
                    let written = match bytes {
                        Ok(bytes) => cx.background_spawn(async move { std::fs::write(&target, bytes).map_err(|error| error.to_string()) }).await,
                        Err(error) => Err(error),
                    };
                    let result = match written {
                        Ok(()) => json!({ "path": path.to_string_lossy() }),
                        Err(error) => json!({ "error": error }),
                    };
                    let _ = cx.update(|_, cx| respond(result, cx));
                })
                .detach();
            }
            // Slides and old Word files are drawn as the PDF the installed Office makes; delivered via the Host's
            // single-use `/rainy/ide/office-pdf?token=...` route so up to 200 MB documents never round-trip as base64.
            "office-pdf" => {
                let extension = params["extension"].as_str().unwrap_or_default().trim_start_matches('.').to_ascii_lowercase();
                if crate::web::office::kind(&extension).is_none() {
                    respond(json!({ "error": "Unsupported Office document extension", "code": "unsupported", "missing": false, "encrypted": false }), cx);
                    return;
                }
                let workspace_id = params["workspaceId"].as_str().map(str::to_owned);
                let root_id = params["rootId"].as_str().map(str::to_owned);
                let rel_path = params["path"].as_str().map(str::to_owned);
                let version = params["version"].as_str().map(str::to_owned);
                let legacy_base64 = params["base64"].as_str().map(str::to_owned);
                let store = Store::global(cx);
                let (rpc, windows) = {
                    let s = store.read(cx);
                    (s.rpc.clone(), matches!(s.target, None | Some(ExecutionTarget::Windows { .. })))
                };
                // On Windows, locate the file directly inside the active workspace to avoid downloading it through HTTP.
                let local_source = if windows {
                    if let Some(rel) = rel_path.as_deref() {
                        self.ide.read(cx).workspace.as_ref().and_then(|ws| {
                            let ws_roots = ws.roots();
                            let base = ws_roots
                                .iter()
                                .find(|r| root_id.as_deref() == Some(r.root_id.as_str()) || (root_id.is_none() && r.primary))
                                .map_or(ws.path.as_str(), |r| r.path.as_str());
                            let candidate = std::path::Path::new(base).join(rel.replace('/', "\\"));
                            let roots: Vec<String> = ws_roots.iter().map(|r| r.path.clone()).chain(std::iter::once(ws.path.clone())).collect();
                            validate_workspace_file(&candidate.to_string_lossy(), &roots)
                        })
                    } else {
                        None
                    }
                } else {
                    None
                };
                let raw_query = match (workspace_id.as_deref(), rel_path.as_deref(), version.as_deref()) {
                    (Some(ws), Some(path), Some(ver)) => {
                        let mut q = format!("/rainy/ide/raw?w={}&p={}&v={}", encode(ws), encode(path), encode(ver));
                        if let Some(r) = root_id.as_deref() {
                            q.push_str(&format!("&r={}", encode(r)));
                        }
                        Some(q)
                    }
                    _ => None,
                };
                cx.spawn(async move |_, cx: &mut AsyncApp| {
                    let converted = cx
                        .background_spawn({
                            let rpc = rpc.clone();
                            async move {
                                if let Some(local) = local_source {
                                    return crate::web::office::convert_file_to_pdf(&local, &extension, None);
                                }
                                if let (Some(query), Some(rpc)) = (raw_query, rpc) {
                                    let folder = crate::web::office::create_staging_dir()?;
                                    let input = folder.join(format!("document.{extension}"));
                                    match rpc.get_to_file(&query, &input).await {
                                        Ok(200..=299) => return crate::web::office::convert_file_to_pdf(&input, &extension, Some(folder)),
                                        Ok(status) => {
                                            crate::web::office::ConvertedFile::cleanup(folder);
                                            return Err(crate::web::office::Failure {
                                                code: "failed",
                                                error: format!("HTTP {status}"),
                                                missing: false,
                                                encrypted: false,
                                            });
                                        }
                                        Err(error) => {
                                            crate::web::office::ConvertedFile::cleanup(folder);
                                            return Err(crate::web::office::Failure {
                                                code: "failed",
                                                error: error.to_string(),
                                                missing: false,
                                                encrypted: false,
                                            });
                                        }
                                    }
                                }
                                if let Some(b64) = legacy_base64 {
                                    let bytes = base64::engine::general_purpose::STANDARD.decode(&b64).unwrap_or_default();
                                    let folder = crate::web::office::create_staging_dir()?;
                                    let input = folder.join(format!("document.{extension}"));
                                    if let Err(error) = std::fs::write(&input, &bytes) {
                                        crate::web::office::ConvertedFile::cleanup(folder);
                                        return Err(crate::web::office::Failure {
                                            code: "failed",
                                            error: error.to_string(),
                                            missing: false,
                                            encrypted: false,
                                        });
                                    }
                                    return crate::web::office::convert_file_to_pdf(&input, &extension, Some(folder));
                                }
                                Err(crate::web::office::Failure {
                                    code: "failed",
                                    error: "Missing document location".to_owned(),
                                    missing: false,
                                    encrypted: false,
                                })
                            }
                        })
                        .await;
                    let result = match converted {
                        Ok(converted) => {
                            let delivered: Result<Value, String> = if let Some(rpc) = rpc {
                                if windows {
                                    let pdf_str = converted.pdf_path.to_string_lossy().to_string();
                                    let reg = rpc.call("ide", json!({ "op": "officePdf.register", "path": pdf_str, "deleteAfterRead": true })).await;
                                    match reg {
                                        Ok(value) => Ok(value),
                                        Err(error) => {
                                            let folder = converted.folder.clone();
                                            cx.background_spawn(async move { crate::web::office::ConvertedFile::cleanup(folder) }).detach();
                                            Err(error.message)
                                        }
                                    }
                                } else {
                                    let pdf_path = converted.pdf_path.clone();
                                    let folder = converted.folder.clone();
                                    cx.background_spawn(async move {
                                        let uploaded = rpc.post_file("/rainy/ide/office-pdf", "application/pdf", &pdf_path).await;
                                        crate::web::office::ConvertedFile::cleanup(folder);
                                        match uploaded {
                                            Ok((200..=299, body)) => serde_json::from_slice::<Value>(&body).map_err(|e| e.to_string()),
                                            Ok((status, body)) => Err(format!("HTTP {status} {}", String::from_utf8_lossy(&body))),
                                            Err(error) => Err(error.to_string()),
                                        }
                                    })
                                    .await
                                }
                            } else {
                                let folder = converted.folder.clone();
                                cx.background_spawn(async move { crate::web::office::ConvertedFile::cleanup(folder) }).detach();
                                Err("Host disconnected".to_owned())
                            };
                            match delivered {
                                Ok(reg) => json!({
                                    "url": reg["url"],
                                    "token": reg["token"],
                                    "application": converted.application,
                                }),
                                Err(error) => json!({
                                    "code": "failed",
                                    "error": error,
                                    "missing": false,
                                    "encrypted": false,
                                }),
                            }
                        }
                        Err(failure) => json!({ "code": failure.code, "error": failure.error, "missing": failure.missing, "encrypted": failure.encrypted }),
                    };
                    cx.update(|cx| respond(result, cx));
                })
                .detach();
            }
            _ => respond(json!({ "error": format!("Unknown request: {name}"), "missing": false }), cx),
        }
    }

    /// Where the export dialog opens: the document's own folder when this computer can reach it, else Documents.
    fn export_folder(&self, key: &str, cx: &App) -> std::path::PathBuf {
        let windows = matches!(Store::global(cx).read(cx).target, None | Some(ExecutionTarget::Windows { .. }));
        let own = windows
            .then(|| self.ide.read(cx).language_target(key))
            .flatten()
            .and_then(|target| std::path::Path::new(&target.path).parent().map(std::path::Path::to_path_buf))
            .filter(|folder| folder.is_dir());
        own.unwrap_or_else(|| {
            let home = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from).unwrap_or_default();
            let documents = home.join("Documents");
            if documents.is_dir() { documents } else { home }
        })
    }

    /// The heading at the top of a file's rich editor, once its page told it.
    pub fn outline_index(&self, key: &str) -> Option<i64> {
        self.viewers.get(key).filter(|viewer| viewer.kind.starts_with("markdown"))?;
        self.outline.get(key).copied()
    }

    /// Go to a heading: the page scrolls to it, the source editor puts the cursor on its line.
    pub fn reveal_heading(&mut self, key: &str, index: usize, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.ide.read(cx).views.get(key).map(String::as_str) == Some("source");
        let page = (!source).then(|| self.viewers.get(key).and_then(|viewer| viewer.pane.read(cx).view())).flatten();
        match page {
            Some(view) => view.execute(&viewers::reveal_heading_script(index), |_| {}),
            None => {
                let key = key.to_owned();
                self.ide.update(cx, |ide, cx| ide.reveal(key, line.saturating_sub(1) as u32, 0, window, cx));
            }
        }
    }

    /// Whether the shown file is Markdown with an editor.
    fn markdown_active(&self, cx: &App) -> Option<String> {
        let ide = self.ide.read(cx);
        let key = ide.active.clone()?;
        let file = ide.files.get(&key)?;
        (file.editor.is_some() && viewers::preview_kind(&file.document.path) == Some("markdown")).then_some(key)
    }

    /// Export the shown Markdown document; its page renders it, so a document shown as source gets its page first.
    pub fn export_markdown(&mut self, format: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::notification::Notification;
        let Some(key) = self.markdown_active(cx) else { return };
        window.push_notification(Notification::info(tr(cx, "viewers.exporting")), cx);
        let page = self.viewers.get(&key).filter(|viewer| viewer.kind.starts_with("markdown")).and_then(|viewer| viewer.pane.read(cx).view());
        match page {
            Some(view) => view.execute(&viewers::export_script(format), |_| {}),
            None => {
                self.pending_export = Some((key.clone(), format));
                self.ide.update(cx, |ide, cx| ide.set_view(&key, None, cx));
            }
        }
    }

    /// Bring the open files back in place of the project map.
    pub fn hide_map(&mut self, cx: &mut Context<Self>) {
        if self.map_shown {
            self.map_shown = false;
            cx.notify();
        }
    }

    /// Ctrl+/ as in Typora: switch a Markdown tab between the rich view and its source.
    pub fn toggle_markdown_source(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.markdown_active(cx) else { return };
        let source = self.ide.read(cx).views.get(&key).map(String::as_str) == Some("source");
        self.ide.update(cx, |ide, cx| ide.set_view(&key, if source { None } else { Some("source") }, cx));
    }

    /// The picture of an open image file, fetched on first use.
    fn picture(&mut self, key: &str, cx: &mut Context<Self>) -> Option<&Picture> {
        let ide = self.ide.read(cx);
        let file = ide.files.get(key)?;
        let format = image_format(&file.document.path)?;
        let entry = (key.to_owned(), file.document.version.clone());
        if !self.pictures.contains_key(&entry) {
            let (root, path) = split_key(key);
            let mut query = format!(
                "/rainy/ide/raw?w={}&p={}&v={}",
                encode(&file.document.workspace_id),
                encode(path),
                encode(&file.document.version)
            );
            if let Some(root) = root {
                query.push_str(&format!("&r={}", encode(root)));
            }
            let rpc = Store::global(cx).read(cx).rpc.clone();
            while self.pictures.len() >= MAX_PICTURES {
                let active_key = ide.active.as_deref();
                let victim = self.pictures.keys().find(|(k, _)| Some(k.as_str()) != active_key).cloned();
                match victim {
                    Some(victim) => {
                        self.pictures.remove(&victim);
                    }
                    None => break,
                }
            }
            self.pictures.insert(entry.clone(), Picture::Loading);
            let fetched = entry.clone();
            let (offline, failed) = (tr(cx, "viewers.hostOffline"), tr(cx, "viewers.imageReadFailed"));
            let missing = tr(cx, "viewers.imageMissing");
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let result = match rpc {
                    Some(rpc) => Some(cx.background_spawn(async move { rpc.get(&query).await }).await),
                    None => None,
                };
                // The Host's answer is English and technical, so it is only a detail under the reason.
                let picture = match result {
                    None => Picture::Failed(offline, None),
                    Some(Ok((200..=299, bytes))) => Picture::Ready(Arc::new(Image::from_bytes(format, bytes))),
                    Some(Ok((status, body))) => {
                        let detail = format!("HTTP {status} {}", String::from_utf8_lossy(&body)).trim().to_owned();
                        Picture::Failed(if matches!(status, 404 | 409) { missing } else { failed }, Some(detail.into()))
                    }
                    Some(Err(error)) => Picture::Failed(failed, Some(error.to_string().into())),
                };
                let _ = this.update(cx, |this, cx| {
                    this.pictures.insert(fetched, picture);
                    cx.notify();
                });
            })
            .detach();
        }
        self.pictures.get(&entry)
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ide = self.ide.read(cx);
        let theme = cx.theme().clone();
        let tabs: Vec<(String, bool, bool)> =
            ide.tabs.iter().map(|key| (key.clone(), ide.active.as_deref() == Some(key.as_str()), ide.is_dirty(key))).collect();
        let handle = self.ide.clone();
        h_flex()
            .id("editor-tabs")
            .w_full()
            .min_w_0()
            .h(px(36.))
            .flex_shrink_0()
            .overflow_x_scroll()
            .track_scroll(&self.tabs_scroll)
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .children(tabs.into_iter().enumerate().map(move |(index, (key, active, dirty))| {
                let name = SharedString::from(file_name(&key).to_owned());
                let (activate, close) = (handle.clone(), handle.clone());
                let (activated, closed) = (key.clone(), key.clone());
                h_flex()
                    .id(("tab", index))
                    .h_full()
                    .pl_3()
                    .pr_1()
                    .gap_1p5()
                    .items_center()
                    .flex_shrink_0()
                    .text_sm()
                    .border_r_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .relative()
                    .when(active, |this| {
                        // The tab shown is marked with a line of the accent along its top.
                        this.bg(theme.tab_active)
                            .text_color(theme.tab_active_foreground)
                            .child(div().absolute().top_0().left_0().right_0().h(px(2.)).bg(theme.primary))
                    })
                    .when(!active, |this| this.text_color(theme.muted_foreground).hover(|style| style.bg(theme.list_hover)))
                    .child(Icon::new(file_icon(&name)).small())
                    .child(name)
                    .when(dirty, |this| this.child(div().text_xs().text_color(theme.warning).child("●")))
                    .child(Button::new(("tab-close", index)).ghost().xsmall().icon(IconName::Close).on_click(move |_, window, cx| {
                        close_tab(close.clone(), closed.clone(), window, cx);
                    }))
                    .on_click(move |_, _, cx| {
                        let key = activated.clone();
                        activate.update(cx, |ide, cx| ide.activate(key, cx));
                    })
            }))
    }

    fn render_diff(&self, key: String, cx: &mut Context<Self>) -> AnyElement {
        use crate::ui::ide::hunk_diff::HunkLineKind;

        let theme = cx.theme().clone();
        let ide = self.ide.read(cx);
        let tab = ide.diffs.get(&key);
        let state = tab.and_then(|tab| tab.state.clone());
        let error = tab.and_then(|tab| tab.error.clone());
        let hunks = tab.map(|tab| tab.hunks.clone()).unwrap_or_default();
        let hunk_count = hunks.len();
        let loaded = state.is_some() || tab.is_some_and(|tab| !tab.status.is_empty());
        let hunk_mode = self.diff_hunk_mode;
        let (show, reload) = (self.ide.clone(), self.ide.clone());
        let (shown, reloaded) = (key.clone(), key.clone());
        let (_, path) = split_key(&key);
        let body = match (state, error) {
            (_, Some(error)) => {
                centered(div().child(format!("{} {}", tr(cx, "ide.ideDiffUnavailable"), error)), &theme).into_any_element()
            }
            (Some(_), None) if hunk_mode => {
                if hunks.is_empty() {
                    centered(div().child(tr(cx, "native.diffNoChanges")), &theme).into_any_element()
                } else {
                    let ide_handle = self.ide.clone();
                    let diff_key = key.clone();
                    let added_bg = theme.success.opacity(if theme.is_dark() { 0.14 } else { 0.12 });
                    let removed_bg = theme.danger.opacity(if theme.is_dark() { 0.14 } else { 0.12 });
                    let added_fg = if theme.is_dark() { theme.success } else { Hsla { l: theme.success.l.min(0.32), ..theme.success } };
                    let removed_fg = if theme.is_dark() { theme.danger } else { Hsla { l: theme.danger.l.min(0.35), ..theme.danger } };
                    let accept_label = tr(cx, "native.diffAcceptHunk");
                    let revert_label = tr(cx, "native.diffRevertHunk");
                    let card_radius = theme.radius;
                    let card_border = theme.border;
                    let card_bg = theme.background;
                    let header_bg = theme.muted.opacity(0.45);
                    let muted_fg = theme.muted_foreground;
                    let text_fg = theme.foreground;
                    let mono_font = theme.mono_font_family.clone();
                    v_flex()
                        .id("diff-hunks")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .p_3()
                        .gap_3()
                        .children(hunks.into_iter().map(move |hunk| {
                            let idx = hunk.index;
                            let accept_ide = ide_handle.clone();
                            let revert_ide = ide_handle.clone();
                            let accept_key = diff_key.clone();
                            let revert_key = diff_key.clone();
                            let accept_label = accept_label.clone();
                            let revert_label = revert_label.clone();
                            let mono = mono_font.clone();
                            v_flex()
                                .rounded(card_radius)
                                .border_1()
                                .border_color(card_border)
                                .bg(card_bg)
                                .overflow_hidden()
                                .child(
                                    h_flex()
                                        .px_2p5()
                                        .py_1()
                                        .gap_2()
                                        .items_center()
                                        .bg(header_bg)
                                        .border_b_1()
                                        .border_color(card_border)
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .whitespace_nowrap()
                                                .font_family(mono.clone())
                                                .text_xs()
                                                .text_color(muted_fg)
                                                .child(hunk.header),
                                        )
                                        .child(
                                            h_flex()
                                                .flex_shrink_0()
                                                .gap_1p5()
                                                .text_xs()
                                                .font_family(mono.clone())
                                                .when(hunk.added > 0, |this| this.child(div().text_color(added_fg).child(format!("+{}", hunk.added))))
                                                .when(hunk.removed > 0, |this| this.child(div().text_color(removed_fg).child(format!("-{}", hunk.removed)))),
                                        )
                                        .child(
                                            Button::new(("hunk-accept", idx))
                                                .outline()
                                                .xsmall()
                                                .icon(IconName::Check)
                                                .label(accept_label)
                                                .on_click(move |_, window, cx| {
                                                    let key = accept_key.clone();
                                                    accept_ide.update(cx, |ide, cx| ide.apply_diff_hunk(key, idx, true, window, cx));
                                                }),
                                        )
                                        .child(
                                            Button::new(("hunk-revert", idx))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::Undo)
                                                .label(revert_label)
                                                .on_click(move |_, window, cx| {
                                                    let key = revert_key.clone();
                                                    revert_ide.update(cx, |ide, cx| ide.apply_diff_hunk(key, idx, false, window, cx));
                                                }),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .py_1()
                                        .font_family(mono)
                                        .text_xs()
                                        .children(hunk.lines.into_iter().map(move |line| {
                                            let (bg, mark, fg) = match line.kind {
                                                HunkLineKind::Added => (Some(added_bg), "+", added_fg),
                                                HunkLineKind::Removed => (Some(removed_bg), "-", removed_fg),
                                                HunkLineKind::Context => (None, " ", text_fg),
                                            };
                                            let old_no = line.old_line.map(|n| n.to_string()).unwrap_or_default();
                                            let new_no = line.new_line.map(|n| n.to_string()).unwrap_or_default();
                                            h_flex()
                                                .w_full()
                                                .px_2()
                                                .gap_2()
                                                .items_start()
                                                .when_some(bg, |this, bg| this.bg(bg))
                                                .child(div().w(px(34.)).flex_shrink_0().text_right().text_color(muted_fg.opacity(0.7)).child(old_no))
                                                .child(div().w(px(34.)).flex_shrink_0().text_right().text_color(muted_fg.opacity(0.7)).child(new_no))
                                                .child(div().w(px(12.)).flex_shrink_0().text_color(fg).child(mark))
                                                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_color(text_fg).child(line.text))
                                        })),
                                )
                        }))
                        .into_any_element()
                }
            }
            (Some(state), None) => div().flex_1().min_h_0().child(Diff::new(&state)).into_any_element(),
            (None, None) if loaded => centered(div().child(tr(cx, "native.diffNoChanges")), &theme).into_any_element(),
            (None, None) => centered(Spinner::new().small(), &theme).into_any_element(),
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h(px(34.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Icon::new(gpui_kit::assets::IconName::GitCompare).small().text_color(theme.muted_foreground))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(path.to_owned()))
                    .when(hunk_count > 0, |this| {
                        this.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(trf(cx, "native.diffHunkCount", &[("count", &hunk_count)])),
                        )
                    })
                    .child(
                        Button::new("diff-mode-hunks")
                            .ghost()
                            .xsmall()
                            .selected(hunk_mode)
                            .label(tr(cx, "native.diffHunks"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.diff_hunk_mode = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("diff-mode-side")
                            .ghost()
                            .xsmall()
                            .selected(!hunk_mode)
                            .label(tr(cx, "native.diffSideBySide"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.diff_hunk_mode = false;
                                cx.notify();
                            })),
                    )
                    .child(Button::new("diff-refresh").ghost().xsmall().icon(IconName::RefreshCw).tooltip(tr(cx, "ide.ideRefresh")).on_click(move |_, window, cx| {
                        let key = reloaded.clone();
                        reload.update(cx, |ide, cx| ide.load_diff(key, window, cx));
                    }))
                    .child(Button::new("diff-open").outline().xsmall().label(tr(cx, "git.gitOpenFile")).on_click(move |_, window, cx| {
                        let key = shown.clone();
                        show.update(cx, |ide, cx| ide.show_file(key, window, cx));
                    })),
            )
            .child(body)
            .into_any_element()
    }

    fn render_file(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if self.ide.read(cx).diff_tabs.contains(&key) {
            return self.render_diff(key, cx);
        }
        let ide = self.ide.read(cx);
        let Some(file) = ide.files.get(&key) else {
            return centered(h_flex().gap_2().child(Spinner::new().small()).child(tr(cx, "ide.ideEditorLoading")), &theme).into_any_element();
        };
        let view = ide.views.get(&key).cloned();
        // Markdown and SVG text have views of their own: a page, the source, or both.
        let text_views: Option<&'static [(&'static str, Option<&'static str>)]> = match viewers::preview_kind(&file.document.path) {
            _ if file.editor.is_none() => None,
            Some("markdown") => Some(&viewers::MARKDOWN_VIEWS),
            Some("svg") => Some(&viewers::SVG_VIEWS),
            _ => None,
        };
        // Edited text whose file changed on disk: reload it or keep the edits, over every view of the file.
        let conflicted = file.editor.is_some() && file.conflict;
        if let Some(views) = text_views {
            let editor = file.editor.clone();
            let shown = viewers::page_view(&file.document.path, None, view.as_deref());
            let conflict = conflicted.then(|| self.render_conflict(&key, cx));
            let toolbar = self.render_views(&key, view.as_deref(), views, cx);
            let body = match shown {
                Some((kind, split)) => self.render_page(&key, kind, split.then_some(editor).flatten(), window, cx),
                None => editor.map(|editor| Editor::new(&editor).h_full().bordered(false).into_any_element()).unwrap_or_else(|| div().into_any_element()),
            };
            return v_flex()
                .size_full()
                .key_context(crate::ui::ide::MARKDOWN_TAB)
                .child(toolbar)
                .children(conflict)
                .child(div().flex_1().min_h_0().child(body))
                .into_any_element();
        }
        if view.as_deref() == Some("hex") {
            return self.render_page(&key, "hex", None, window, cx);
        }
        if let Some(editor) = file.editor.clone() {
            let conflict = conflicted.then(|| self.render_conflict(&key, cx));
            return v_flex()
                .size_full()
                .children(conflict)
                .child(div().flex_1().min_h_0().child(Editor::new(&editor).h_full().bordered(false).context_menu(editor_commands::context_menu)))
                .children(self.ai_edit.clone().filter(|edit| edit.read(cx).key == key))
                .into_any_element();
        }
        let reason = file.document.read_only_reason.clone().unwrap_or_default();
        let preview = file.document.preview.clone();
        let path = file.document.path.clone();
        if let Some((kind, _)) = viewers::page_view(&path, Some(reason.as_str()), view.as_deref()) {
            // Without WebView2 an image is still shown, as GPUI decodes it, only without the viewer's tools.
            let failed = self.viewers.get(&key).filter(|viewer| viewer.kind == kind).is_some_and(|viewer| viewer.pane.read(cx).error().is_some());
            if !(kind == "image" && failed && image_format(&path).is_some()) {
                return self.render_page(&key, kind, None, window, cx);
            }
            return match self.picture(&key, cx) {
                Some(Picture::Ready(image)) => div()
                    .size_full()
                    .p_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.muted.opacity(0.3))
                    .child(img(image.clone()).max_w_full().max_h_full().object_fit(ObjectFit::ScaleDown))
                    .into_any_element(),
                Some(Picture::Failed(error, detail)) => centered(failure(error.clone(), detail.clone(), &theme), &theme).into_any_element(),
                _ => centered(Spinner::new().small(), &theme).into_any_element(),
            };
        }
        let notice = match reason.as_str() {
            "too-large" => tr(cx, "viewers.readOnlyLarge"),
            "unsupported-encoding" => tr(cx, "viewers.notUtf8"),
            _ => tr(cx, "ide.ideReadOnly"),
        };
        let (handle, shown) = (self.ide.clone(), key.clone());
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .flex_shrink_0()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Icon::new(IconName::Info).small())
                    .child(div().flex_1().min_w_0().child(notice))
                    // The hex page reads the whole file in ranges; the text below is only its beginning.
                    .child(Button::new("view-hex").outline().xsmall().label(tr(cx, "viewers.viewAsHex")).on_click(move |_, _, cx| {
                        handle.update(cx, |ide, cx| ide.set_view(&shown, Some("hex"), cx));
                    })),
            )
            .when_some(preview, |this, preview| {
                this.child(
                    div()
                        .id("preview")
                        .flex_1()
                        .min_h_0()
                        .overflow_scrollbar()
                        .p_3()
                        .font_family(theme.mono_font_family.clone())
                        .text_size(theme.mono_font_size)
                        .whitespace_nowrap()
                        .children(preview.text.lines().map(|line| div().child(line.to_owned())).collect::<Vec<_>>()),
                )
            })
            .into_any_element()
    }

    /// The bar over a file whose edits conflict with a change on disk: read the file again, or save the edits over it.
    fn render_conflict(&self, key: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let (reload, keep) = (self.ide.clone(), self.ide.clone());
        let (reloaded, kept) = (key.to_owned(), key.to_owned());
        h_flex()
            .flex_shrink_0()
            .px_3()
            .py_1p5()
            .gap_2()
            .items_center()
            .text_sm()
            .bg(theme.warning.opacity(0.12))
            .border_b_1()
            .border_color(theme.warning.opacity(0.4))
            .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning))
            .child(div().flex_1().min_w_0().child(tr(cx, "ide.ideExternal")))
            .child(Button::new("conflict-reload").outline().xsmall().label(tr(cx, "ide.ideReload")).on_click(move |_, window, cx| {
                reload.update(cx, |ide, cx| ide.reload(&reloaded, window, cx));
            }))
            .child(Button::new("conflict-keep").primary().xsmall().label(tr(cx, "native.keepMine")).on_click(move |_, window, cx| {
                let key = kept.clone();
                keep.update(cx, |ide, cx| ide.save(Some(key), true, window, cx));
            }))
            .into_any_element()
    }

    /// The views of a Markdown or SVG tab, the default first; nothing is marked while the tab shows hex.
    fn render_views(&self, key: &str, current: Option<&str>, views: &'static [(&'static str, Option<&'static str>)], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let hex = current == Some("hex");
        // A stored view this file type does not list (`rich` of Markdown, `preview` of SVG) is its default.
        let current = current.filter(|current| views.iter().any(|(_, view)| *view == Some(*current)));
        h_flex()
            .flex_shrink_0()
            .h(px(32.))
            .px_2()
            .gap_1()
            .items_center()
            .justify_end()
            .border_b_1()
            .border_color(theme.border)
            .children(views.iter().enumerate().map(|(index, (label, view))| {
                let selected = !hex && current == *view;
                let (ide, key, view) = (self.ide.clone(), key.to_owned(), view.map(str::to_owned));
                Button::new(("md-view", index)).ghost().xsmall().label(tr(cx, label)).selected(selected).on_click(move |_, _, cx| {
                    ide.update(cx, |ide, cx| ide.set_view(&key, view.as_deref(), cx));
                })
            }))
            .into_any_element()
    }

    /// A file in a page, with the text editor on the left when `editor` is given.
    fn render_page(&mut self, key: &str, kind: &'static str, editor: Option<Entity<gpui_kit::component::input::EditorState>>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let page = match self.viewer(key, kind, window, cx) {
            Some(pane) => {
                let (creating, error) = {
                    let pane = pane.read(cx);
                    (pane.is_creating(), pane.error())
                };
                match error {
                    Some(error) => centered(failure(tr(cx, "viewers.pageFailed"), Some(error), &theme), &theme).into_any_element(),
                    None => div()
                        .size_full()
                        .relative()
                        .when(creating, |this| this.child(centered(Spinner::new().small(), &theme)))
                        .child(div().absolute().inset_0().child(WebPane::area(&pane)))
                        .into_any_element(),
                }
            }
            None => centered(Spinner::new().small(), &theme).into_any_element(),
        };
        match editor {
            Some(editor) => h_flex()
                .size_full()
                .child(div().flex_1().min_w_0().h_full().border_r_1().border_color(theme.border).child(Editor::new(&editor).h_full().bordered(false)))
                .child(div().flex_1().min_w_0().h_full().child(page))
                .into_any_element(),
            None => page,
        }
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let ide = self.ide.read(cx);
        let recent: Vec<_> = ide.workspaces.iter().rev().take(8).cloned().collect();
        let error = ide.error.clone();
        let handle = self.ide.clone();
        // The content keeps to the editor area: as wide as it allows up to its own width, and scrolled when the area is
        // shorter than it.
        let content = v_flex()
            .w_full()
            .min_h_full()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .gap_4()
            .p_8()
            .child(img("brand/rainy-mark.svg").size(px(48.)).flex_shrink_0())
            .child(div().text_xl().font_semibold().text_center().child(tr(cx, "ide.ideWelcomeTitle")))
            .child(div().w_full().max_w(px(460.)).text_sm().text_center().text_color(theme.muted_foreground).child(tr(cx, "ide.ideWelcomeDescription")))
            .child(Button::new("welcome-open").primary().icon(IconName::FolderOpen).label(tr(cx, "ide.ideOpenFolder")).on_click(|_, window, cx| {
                choose_folder(window, cx);
            }))
            .when_some(error, |this, error| this.child(div().w_full().max_w(px(520.)).text_sm().text_color(theme.danger).child(error)))
            .when(!recent.is_empty(), |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .max_w(px(420.))
                        .gap_0p5()
                        .pt_2()
                        .child(div().px_2().pb_1().text_xs().text_color(theme.muted_foreground).child(tr(cx, "ide.ideRecentProjects")))
                        .children(recent.into_iter().enumerate().map(|(index, workspace)| {
                            let handle = handle.clone();
                            let path = workspace.path.clone();
                            h_flex()
                                .id(("recent", index))
                                .h(px(32.))
                                .px_2()
                                .gap_2()
                                .items_center()
                                .rounded(theme.radius)
                                .cursor_pointer()
                                .hover(|style| style.bg(theme.list_hover))
                                .child(Icon::new(IconName::Folder).small().flex_shrink_0().text_color(theme.muted_foreground))
                                .child(div().max_w(px(220.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().child(workspace.title.clone()))
                                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(theme.muted_foreground).child(path))
                                .on_click(move |_, window, cx| {
                                    let workspace = workspace.clone();
                                    handle.update(cx, |ide, cx| ide.select(workspace, window, cx));
                                })
                        })),
                )
            });
        div().id("welcome").size_full().overflow_y_scroll().child(content).into_any_element()
    }
}

fn centered(content: impl IntoElement, theme: &gpui_kit::component::Theme) -> Div {
    div().size_full().flex().items_center().justify_center().text_sm().text_color(theme.muted_foreground).child(content)
}

/// Why a file is not shown, in the interface language, with the underlying error as a smaller detail line.
fn failure(reason: SharedString, detail: Option<SharedString>, theme: &gpui_kit::component::Theme) -> Div {
    v_flex()
        .max_w(px(560.))
        .gap_1()
        .items_center()
        .text_center()
        .child(div().text_sm().text_color(theme.danger).child(reason))
        .when_some(detail.filter(|detail| !detail.is_empty()), |this, detail| this.child(div().text_xs().text_color(theme.muted_foreground).child(detail)))
}

/// Why a Markdown export has no PDF, in the interface language; the page puts it after "导出失败：".
fn pdf_failure(failure: &crate::web::export::PdfFailure, cx: &App) -> String {
    use crate::web::export::PdfFailure;
    match failure {
        PdfFailure::Page(detail) => trf(cx, "viewers.exportPdfPage", &[("error", detail)]),
        PdfFailure::Control(detail) => trf(cx, "viewers.exportPdfControl", &[("error", detail)]),
        PdfFailure::Print(detail) => trf(cx, "viewers.exportPdfPrint", &[("error", detail)]),
        PdfFailure::Empty => tr(cx, "viewers.exportPdfEmpty"),
    }
    .to_string()
}

/// The source line at the top of an editor, 1-based with the fraction scrolled past; `None` before its first layout.
fn top_line(state: &EditorState) -> Option<f64> {
    let height = state.line_height().map(f32::from).filter(|height| *height > 0.)?;
    let map = state.display_map();
    // Display rows are all one line high; the offset is negative once scrolled down.
    let row = f64::from((-f32::from(state.scroll_offset().y) / height).max(0.));
    line_at_row(row, map.display_row_count(), |row| map.display_row_to_buffer_line(row), |line| map.buffer_line_to_display_row_range(line))
}

/// The 1-based source line, with its fraction, at display row `row` of `rows` (wrapped lines take several rows, folded
/// ones none): `line_of` gives a row's 0-based buffer line, `rows_of` a buffer line's rows.
fn line_at_row(row: f64, rows: usize, line_of: impl Fn(usize) -> usize, rows_of: impl Fn(usize) -> Option<Range<usize>>) -> Option<f64> {
    if rows == 0 || !row.is_finite() {
        return None;
    }
    let row = row.clamp(0., (rows - 1) as f64 + 0.999);
    let line = line_of(row.floor() as usize);
    let fraction = match rows_of(line) {
        Some(range) if range.end > range.start => ((row - range.start as f64) / range.len() as f64).clamp(0., 0.999),
        _ => 0.,
    };
    // Three decimals are finer than a pixel and keep the script short.
    Some(((line as f64 + 1. + fraction) * 1000.).round() / 1000.)
}

/// The display row that puts 1-based source line `line` (with its fraction) at the top of an editor of `lines` buffer
/// lines; the inverse of `line_at_row`. A folded line has no rows of its own and goes to the row `first_row` gives.
fn row_at_line(line: f64, lines: usize, rows_of: impl Fn(usize) -> Option<Range<usize>>, first_row: impl Fn(usize) -> usize) -> f64 {
    if lines == 0 || !line.is_finite() {
        return 0.;
    }
    let line = line.max(1.);
    // Past the last line is the last line's top; the editor clamps its offset to what it can show.
    let (index, fraction) = match line.floor() as usize - 1 {
        index if index < lines => (index, line.fract()),
        _ => (lines - 1, 0.),
    };
    match rows_of(index) {
        Some(range) => range.start as f64 + fraction * range.len() as f64,
        None => first_row(index) as f64,
    }
}

/// The source line at the top of the preview, from the page's `{"type":"scroll","line":12.5}`.
fn scroll_message(text: &str) -> Option<f64> {
    // Every message from the page passes here; only one naming the type is parsed twice.
    if !text.contains("\"scroll\"") {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    if value["type"] != "scroll" {
        return None;
    }
    value["line"].as_f64().filter(|line| line.is_finite())
}

/// The script that scrolls the preview so 1-based source line `line` is at its top; only numbers make one.
fn scroll_script(line: f64) -> Option<String> {
    line.is_finite().then(|| format!("window.__rainyViewer&&window.__rainyViewer.scrollToLine&&window.__rainyViewer.scrollToLine({})", line.max(1.)))
}

/// The scroll offset reported by a viewer before LRU hibernation: `{"type":"scroll-top","top":120}`.
fn scroll_top_message(text: &str) -> Option<f64> {
    if !text.contains("\"scroll-top\"") {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    if value["type"] != "scroll-top" {
        return None;
    }
    value["top"].as_f64().filter(|top| top.is_finite() && *top >= 0.)
}

/// Restore the vertical scroll offset of a viewer page after LRU hibernation.
fn restore_scroll_top_script(top: f64) -> Option<String> {
    (top.is_finite() && top >= 0.).then(|| {
        format!(
            "requestAnimationFrame(() => {{ const el = document.scrollingElement || document.documentElement; if (el) el.scrollTop = {top}; else window.scrollTo(0, {top}); }})"
        )
    })
}

/// Safe document and media extensions allowed for `open-external`.
/// Strictly excludes executables, scripts, shortcuts, installers, HTML/SVG and macro-enabled Office formats.
const SAFE_EXTERNAL_EXTENSIONS: &[&str] = &[
    "pdf", "doc", "docx", "dot", "dotx", "wps", "odt", "rtf",
    "xls", "xlsx", "xlt", "xltx", "ods", "csv", "tsv",
    "ppt", "pptx", "pps", "ppsx", "pot", "potx", "odp", "dps",
    "txt", "md", "markdown", "epub",
    "png", "apng", "jpg", "jpeg", "jfif", "gif", "webp", "bmp", "tif", "tiff", "ico", "avif",
    "mp3", "wav", "ogg", "oga", "opus", "flac", "m4a", "aac", "weba",
    "mp4", "m4v", "webm", "ogv", "mov", "mkv",
];

/// Strip the Windows verbatim prefix (`\\?\`) for component comparisons; returns `None` for UNC or device paths.
fn strip_local_verbatim(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let raw = path.to_str()?;
    let upper = raw.to_ascii_uppercase();
    if upper.starts_with(r"\\?\UNC\") || upper.starts_with(r"\\.\") || raw.starts_with(r"\\") && !raw.starts_with(r"\\?\") {
        return None;
    }
    let cleaned = raw.strip_prefix(r"\\?\").unwrap_or(raw);
    Some(std::path::PathBuf::from(cleaned))
}

/// Component-wise case-insensitive containment check on Windows paths (`C:\proj` never matches `C:\proj-evil`).
fn path_within_root(candidate: &std::path::Path, root: &std::path::Path) -> bool {
    let (Some(cand), Some(base)) = (strip_local_verbatim(candidate), strip_local_verbatim(root)) else {
        return false;
    };
    let mut cand_comps = cand.components();
    for base_comp in base.components() {
        let Some(cand_comp) = cand_comps.next() else {
            return false;
        };
        if !cand_comp.as_os_str().to_string_lossy().eq_ignore_ascii_case(&base_comp.as_os_str().to_string_lossy()) {
            return false;
        }
    }
    true
}

/// Reject UNC paths, device paths, control characters and NTFS Alternate Data Streams (`file.pdf:evil.exe`).
fn has_disallowed_path_syntax(raw: &str) -> bool {
    if raw.trim().is_empty() || raw.chars().any(|c| c.is_control()) {
        return true;
    }
    let trimmed = raw.trim();
    if trimmed.starts_with(r"\\") || trimmed.starts_with("//") {
        return true;
    }
    // Only a Windows drive prefix `X:` at byte index 1 may contain ':'.
    for (idx, ch) in trimmed.char_indices() {
        if ch == ':' {
            let valid_drive = idx == 1 && trimmed.as_bytes()[0].is_ascii_alphabetic();
            if !valid_drive {
                return true;
            }
        }
    }
    false
}

/// Validate that `raw` canonicalizes to an existing regular file inside one of the active workspace roots.
/// Returns a normalized local path without the Windows `\\?\` verbatim prefix so Shell APIs accept it.
fn validate_workspace_file(raw: &str, workspace_roots: &[String]) -> Option<std::path::PathBuf> {
    if has_disallowed_path_syntax(raw) || workspace_roots.is_empty() {
        return None;
    }
    let canonical = std::fs::canonicalize(raw).ok()?;
    if !canonical.is_file() {
        return None;
    }
    // Reject symlinks or reparse points that resolve to a UNC share, and strip `\\?\` for Windows Shell APIs.
    let clean = strip_local_verbatim(&canonical)?;
    let inside = workspace_roots.iter().filter_map(|root| std::fs::canonicalize(root).ok()).any(|root| path_within_root(&canonical, &root));
    inside.then_some(clean)
}

/// Validate `open-external`: path must canonicalize inside an active workspace root and carry an allowlisted document/media extension.
fn validate_open_external_path(raw: &str, workspace_roots: &[String]) -> Option<std::path::PathBuf> {
    let canonical = validate_workspace_file(raw, workspace_roots)?;
    let ext = canonical.extension()?.to_str()?.to_ascii_lowercase();
    SAFE_EXTERNAL_EXTENSIONS.contains(&ext.as_str()).then_some(canonical)
}

/// Close a tab, asking first when it has unsaved edits.
pub fn close_tab(ide: Entity<Ide>, key: String, window: &mut Window, cx: &mut App) {
    if !ide.read(cx).is_dirty(&key) {
        ide.update(cx, |ide, cx| ide.close(&key, cx));
        return;
    }
    window.open_dialog(cx, move |dialog, _, cx| {
        let (saving, discarding) = (ide.clone(), ide.clone());
        let (saved, discarded) = (key.clone(), key.clone());
        dialog
            .title(tr(cx, "ide.ideDirtyTitle"))
            .w(px(440.))
            .child(div().text_sm().child(tr(cx, "ide.ideDirtyDescription")))
            .footer(
                DialogFooter::new()
                    .child(Button::new("dirty-cancel").outline().label(tr(cx, "ide.ideCancel")).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("dirty-discard").danger().label(tr(cx, "ide.ideDiscard")).on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        discarding.update(cx, |ide, cx| ide.close(&discarded, cx));
                    }))
                    .child(Button::new("dirty-save").primary().label(tr(cx, "ide.ideSave")).on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        saving.update(cx, |ide, cx| ide.save_and_close(saved.clone(), window, cx));
                    })),
            )
    });
}

impl EditorArea {
    /// Show or hide the project map; it is created for the project shown now.
    pub fn toggle_map(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.ide.read(cx).workspace.clone() else { return };
        self.map_shown = !self.map_shown || self.map.as_ref().is_none_or(|(id, _)| *id != workspace.workspace_id);
        if self.map_shown && self.map.as_ref().is_none_or(|(id, _)| *id != workspace.workspace_id) {
            let Some(launch) = Store::global(cx).read(cx).launch_url() else { return };
            let pane = viewers::create_pane(viewers::map_url(&launch, &workspace.workspace_id), "map", window, cx);
            cx.subscribe_in(&pane, window, |this, _, event: &PaneEvent, window, cx| this.on_map(event, window, cx)).detach();
            self.map = Some((workspace.workspace_id.clone(), pane));
            self.map_session = None;
        }
        cx.notify();
    }

    /// What the map page asked for.
    fn on_map(&mut self, event: &PaneEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PaneEvent::Message(text) => match viewers::parse_message(text) {
                Some(PageRequest::Ready) => self.send_session(true, cx),
                Some(PageRequest::OpenKey { key, line }) => {
                    self.map_shown = false;
                    self.ide.update(cx, |ide, cx| match line {
                        Some(line) => ide.reveal(key, (line as u32).saturating_sub(1), 0, window, cx),
                        None => ide.open(key, true, window, cx),
                    });
                    cx.notify();
                }
                Some(PageRequest::Ask(text)) => crate::ui::ask_ai(text, window, cx),
                Some(PageRequest::OpenUrl(url)) => viewers::open_page_link(&url, window, cx),
                _ => {}
            },
            PaneEvent::Shortcut(shortcut) => {
                if let Some(action) = viewers::shortcut_action(*shortcut, "map") {
                    crate::web::reclaim_focus(window);
                    window.dispatch_action(action, cx);
                }
            }
            PaneEvent::NewWindow(url) => viewers::open_page_link(url, window, cx),
            PaneEvent::Ready | PaneEvent::Failed(_) => cx.notify(),
        }
    }

    /// Tell the map which chat's footprint to list and changes to mark: when the page is ready (`always`), and then
    /// whenever another chat is selected.
    fn send_session(&mut self, always: bool, cx: &App) {
        let Some((_, pane)) = &self.map else { return };
        let session = Store::global(cx).read(cx).selected.clone();
        if !always && self.map_session.as_ref() == Some(&session) {
            return;
        }
        if let Some(view) = pane.read(cx).view() {
            view.execute(&viewers::set_session_script(session.as_deref()), |_| {});
            self.map_session = Some(session);
        }
    }

    fn render_map(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (workspace, pane) = self.map.as_ref()?;
        if !self.map_shown || self.ide.read(cx).workspace.as_ref().map(|shown| &shown.workspace_id) != Some(workspace) {
            return None;
        }
        let theme = cx.theme().clone();
        Some(
            v_flex()
                .size_full()
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .h(px(34.))
                        .px_3()
                        .gap_2()
                        .items_center()
                        .text_sm()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(Icon::new(gpui_kit::assets::IconName::Map).small())
                        .child(div().flex_1().font_semibold().child(tr(cx, "native.projectMap")))
                        .child(
                            Button::new("map-close")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip(tr(cx, "native.closeMap"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.map_shown = false;
                                    cx.notify();
                                })),
                        ),
                )
                .child(div().flex_1().min_h_0().relative().child(div().absolute().inset_0().child(WebPane::area(pane))))
                .into_any_element(),
        )
    }

    /// Tab while the active editor lists completions takes the highlighted one, as Enter does; the editor alone would
    /// indent under the open list. A suggestion shown at the cursor goes first, as Tab always took it: the list can
    /// open after it, when the language server answers late.
    fn tab_into_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ide = self.ide.read(cx);
        let editor = ide.active.as_ref().and_then(|key| ide.files.get(key)).and_then(|file| file.editor.as_ref());
        let listing = editor.is_some_and(|editor| {
            let editor = editor.read(cx);
            editor.completion_menu_state().open && !EditorMode::has_inline_completion(editor) && editor.focus_handle(cx).is_focused(window)
        });
        if listing {
            cx.stop_propagation();
            window.dispatch_action(Box::new(Enter { secondary: false, shift: false }), cx);
        }
    }

    /// Open Ctrl+K on the active file's editor.
    fn open_ai_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ide = self.ide.read(cx);
        let Some(key) = ide.active.clone() else { return };
        let Some(file) = ide.files.get(&key) else { return };
        let Some(editor) = file.editor.clone() else { return };
        let path = split_key(&key).1.to_owned();
        let language = crate::store::ide::language_of(&file.document.path).to_owned();
        if let Some(open) = self.ai_edit.take() {
            open.update(cx, |edit, cx| edit.dismiss(window, cx));
        }
        let store = Store::global(cx);
        let target = ai_edit::Target { key, path, language };
        let edit = cx.new(|cx| AiEdit::new(store, target, editor, window, cx));
        cx.subscribe(&edit, |this, _, event: &AiEditEvent, cx| match event {
            AiEditEvent::Closed => {
                this.ai_edit = None;
                cx.notify();
            }
        })
        .detach();
        self.ai_edit = Some(edit);
        cx.notify();
    }
}

impl EditorArea {
    /// Ctrl+Shift+V and Ctrl+K V as in VS Code: show the Markdown tab's preview, or its preview beside the source, and
    /// its default view again when it already shows that.
    fn toggle_markdown_view(&mut self, view: &'static str, cx: &mut Context<Self>) {
        let Some(key) = self.markdown_active(cx) else { return };
        let shown = self.ide.read(cx).views.get(&key).map(String::as_str) == Some(view);
        self.ide.update(cx, |ide, cx| ide.set_view(&key, (!shown).then_some(view), cx));
    }

    /// Whether the tab at `index` lies wholly inside the visible part of the tab strip.
    fn tab_shown(&self, index: usize) -> bool {
        let view = self.tabs_scroll.bounds();
        let Some(item) = self.tabs_scroll.bounds_for_item(index) else { return false };
        let offset = self.tabs_scroll.offset().x;
        view.size.width > px(0.) && item.left() + offset >= view.left() - px(1.) && item.right() + offset <= view.right() + px(1.)
    }
}

impl Render for EditorArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Pages of closed tabs go away with their tabs; stale image versions are evicted immediately.
        let (open, versions): (std::collections::HashSet<String>, HashMap<String, String>) = {
            let ide = self.ide.read(cx);
            let open = ide.tabs.iter().cloned().collect();
            let versions = ide.files.iter().map(|(k, f)| (k.clone(), f.document.version.clone())).collect();
            (open, versions)
        };
        self.viewers.retain(|key, _| open.contains(key));
        self.saved_scroll.retain(|key, _| open.contains(key));
        self.pictures.retain(|(key, version), _| open.contains(key) && versions.get(key).is_some_and(|cur| cur == version));
        self.outline.retain(|key, _| open.contains(key));
        // A tab that becomes active is brought into view, also when it was opened past the end of the strip or restored
        // before the strip was first laid out (a scroll asked for then has no sizes to work with, so it is asked again).
        let active = self.ide.read(cx).active.clone();
        let index = active.as_ref().and_then(|key| self.ide.read(cx).tabs.iter().position(|tab| tab == key));
        if active != self.scrolled_to {
            self.scrolled_to = active;
            self.scroll_retries = 12;
        }
        if let Some(index) = index
            && self.scroll_retries > 0
        {
            if self.tab_shown(index) {
                self.scroll_retries = 0;
            } else {
                self.tabs_scroll.scroll_to_item(index);
                self.scroll_retries -= 1;
                window.request_animation_frame();
            }
        }
        let map = self.render_map(cx);
        let ide = self.ide.read(cx);
        let body = if let Some(map) = map {
            map
        } else if ide.workspace.is_none() {
            self.render_welcome(cx)
        } else if ide.tabs.is_empty() {
            let loading = ide.loading;
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(theme.muted_foreground)
                .when(loading, |this| this.child(h_flex().gap_2().text_sm().child(Spinner::new().small()).child(tr(cx, "ide.ideLoading"))))
                .when(!loading, |this| {
                    this.child(div().text_lg().font_semibold().text_color(theme.foreground).child(tr(cx, "ide.ideNoFileTitle")))
                        .child(div().text_sm().child(tr(cx, "ide.ideNoFile")))
                })
                .into_any_element()
        } else {
            let active = ide.active.clone();
            let tabs = self.render_tabs(cx);
            let content = match active {
                Some(key) => self.render_file(key, window, cx),
                None => div().into_any_element(),
            };
            v_flex().size_full().child(tabs).child(div().flex_1().min_h_0().child(content)).into_any_element()
        };
        div()
            .size_full()
            .bg(theme.background)
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| this.tab_into_completion(window, cx)))
            .on_action(cx.listener(|this, _: &AiEditSelection, window, cx| this.open_ai_edit(window, cx)))
            .on_action(cx.listener(|this, _: &MarkdownPreview, _, cx| this.toggle_markdown_view("preview", cx)))
            .on_action(cx.listener(|this, _: &MarkdownSplit, _, cx| this.toggle_markdown_view("split", cx)))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    /// Lines 0, 1 and 2 of a buffer whose line 1 wraps onto three rows: rows 0 | 1 2 3 | 4.
    fn line_of(row: usize) -> usize {
        match row {
            0 => 0,
            1..=3 => 1,
            _ => 2,
        }
    }

    fn rows_of(line: usize) -> Option<Range<usize>> {
        [0..1, 1..4, 4..5].get(line).cloned()
    }

    #[test]
    fn reads_the_top_line_from_the_top_row() {
        assert_eq!(line_at_row(0., 5, line_of, rows_of), Some(1.));
        assert_eq!(line_at_row(0.5, 5, line_of, rows_of), Some(1.5));
        // Two of the wrapped line's three rows scrolled past.
        assert_eq!(line_at_row(3., 5, line_of, rows_of), Some(2.667));
        assert_eq!(line_at_row(4., 5, line_of, rows_of), Some(3.));
        // Beyond the end stays on the last row; before the start on the first.
        assert_eq!(line_at_row(40., 5, line_of, rows_of), Some(3.999));
        assert_eq!(line_at_row(-2., 5, line_of, rows_of), Some(1.));
        assert_eq!(line_at_row(0., 0, line_of, rows_of), None);
        assert_eq!(line_at_row(f64::NAN, 5, line_of, rows_of), None);
    }

    #[test]
    fn finds_the_row_of_a_line() {
        let first_row = |line: usize| [0, 1, 4][line];
        assert_eq!(row_at_line(1., 3, rows_of, first_row), 0.);
        assert_eq!(row_at_line(2.5, 3, rows_of, first_row), 2.5);
        assert_eq!(row_at_line(3., 3, rows_of, first_row), 4.);
        // Past the last line is the last line's top, before the first the first.
        assert_eq!(row_at_line(9.75, 3, rows_of, first_row), 4.);
        assert_eq!(row_at_line(0.2, 3, rows_of, first_row), 0.);
        assert_eq!(row_at_line(f64::INFINITY, 3, rows_of, first_row), 0.);
        assert_eq!(row_at_line(2., 0, rows_of, first_row), 0.);
        // A folded line has no rows; it goes to the row shown in its place.
        assert_eq!(row_at_line(2., 3, |line| (line != 1).then(|| rows_of(line)).flatten(), |_| 1), 1.);
    }

    #[test]
    fn rows_and_lines_round_trip() {
        for row in [0., 0.25, 1., 2.5, 3.9, 4.5] {
            let line = line_at_row(row, 5, line_of, rows_of).expect("a line");
            assert!((row_at_line(line, 3, rows_of, |line| rows_of(line).map_or(0, |rows| rows.start)) - row).abs() < 0.01, "row {row}");
        }
    }

    #[test]
    fn reads_scroll_messages_only() {
        assert_eq!(scroll_message(r#"{"type":"scroll","line":12.5}"#), Some(12.5));
        assert_eq!(scroll_message(r#"{"type":"scroll","line":3}"#), Some(3.));
        assert_eq!(scroll_message(r#"{"type":"scroll","line":"3"}"#), None);
        assert_eq!(scroll_message(r#"{"type":"scroll"}"#), None);
        assert_eq!(scroll_message(r#"{"type":"change","text":"\"scroll\""}"#), None);
        assert_eq!(scroll_message(r#"{"type":"change","text":"scroll"}"#), None);
        assert_eq!(scroll_message("\"scroll\" but not JSON"), None);
    }

    #[test]
    fn scrolls_the_preview_to_numbers_only() {
        assert_eq!(
            scroll_script(12.5).as_deref(),
            Some("window.__rainyViewer&&window.__rainyViewer.scrollToLine&&window.__rainyViewer.scrollToLine(12.5)")
        );
        assert!(scroll_script(40.).is_some_and(|script| script.ends_with("scrollToLine(40)")));
        assert!(scroll_script(-3.).is_some_and(|script| script.ends_with("scrollToLine(1)")));
        assert_eq!(scroll_script(f64::NAN), None);
        assert_eq!(scroll_script(f64::INFINITY), None);
    }

    #[test]
    fn open_external_enforces_workspace_boundary_and_safe_extensions() {
        let base = tempfile::tempdir().expect("tempdir");
        let root = base.path().join("proj");
        let sibling = base.path().join("proj-evil");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        let safe_pdf = root.join("report.pdf");
        let safe_docx = root.join("notes.docx");
        let safe_png = root.join("chart.PNG");
        std::fs::write(&safe_pdf, b"%PDF-1.4").unwrap();
        std::fs::write(&safe_docx, b"PK\x03\x04").unwrap();
        std::fs::write(&safe_png, b"\x89PNG").unwrap();

        let outside_pdf = sibling.join("leak.pdf");
        std::fs::write(&outside_pdf, b"%PDF-1.4").unwrap();

        let roots = vec![root.to_string_lossy().to_string()];

        let resolved_pdf = validate_open_external_path(&safe_pdf.to_string_lossy(), &roots).expect("safe pdf allowed");
        assert!(!resolved_pdf.to_string_lossy().starts_with(r"\\?\"), "Shell APIs reject verbatim prefix: {}", resolved_pdf.display());
        let resolved_docx = validate_open_external_path(&safe_docx.to_string_lossy(), &roots).expect("safe docx allowed");
        assert!(!resolved_docx.to_string_lossy().starts_with(r"\\?\"));
        let resolved_png = validate_open_external_path(&safe_png.to_string_lossy(), &roots).expect("safe png allowed");
        assert!(!resolved_png.to_string_lossy().starts_with(r"\\?\"));

        // Traversal & sibling prefix attacks must be rejected.
        let traversal = root.join("..").join("proj-evil").join("leak.pdf");
        assert!(validate_open_external_path(&traversal.to_string_lossy(), &roots).is_none());
        assert!(validate_open_external_path(&outside_pdf.to_string_lossy(), &roots).is_none());

        // UNC, device, and ADS paths must be rejected.
        assert!(validate_open_external_path(r"\\attacker\share\doc.pdf", &roots).is_none());
        assert!(validate_open_external_path("//attacker/share/doc.pdf", &roots).is_none());
        assert!(validate_open_external_path(&format!("{}:evil.exe", safe_pdf.display()), &roots).is_none());

        // Executable, script, shortcut, and macro-enabled extensions must all be rejected.
        for bad in ["a.exe", "a.bat", "a.cmd", "a.ps1", "a.vbs", "a.msi", "a.lnk", "a.js", "a.hta", "a.docm", "a.xlsm", "a.html"] {
            let file = root.join(bad);
            std::fs::write(&file, b"test").unwrap();
            assert!(validate_open_external_path(&file.to_string_lossy(), &roots).is_none(), "expected rejection for {bad}");
        }
    }
}
