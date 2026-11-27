//! WebView2 document viewers, LRU hibernation with scroll preservation, split Markdown scroll sync,
//! and fallback native image decoding.

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::{ActiveTheme as _, Size, WindowExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::ExecutionTarget;
use rainy_carrier::text::percent_encode as encode;
use serde_json::Value;

use crate::store::Store;
use crate::store::ide::split_key;
use crate::ui::ide::viewers::{self, PageRequest};
use crate::ui::ide::{MarkdownPreview, MarkdownSplit, editor_commands};
use crate::ui::tr;
use crate::web::{PaneEvent, WebPane};

use super::{EditorArea, EditorAreaEvent, centered, failure};

/// Maximum number of active WebView2 viewer panes kept alive simultaneously (LRU eviction above this).
pub(super) const MAX_ACTIVE_VIEWERS: usize = 4;
/// Maximum number of decoded fallback image entries retained in memory.
const MAX_PICTURES: usize = 16;
/// How long after the preview scrolled the editor the editor's own scrolling is taken as that one.
const PAGE_SCROLL_QUIET: Duration = Duration::from_millis(250);
/// Brief grace period before dropping an LRU-evicted `WebPane` so its async `scroll-top` probe can arrive
/// and controller teardown never happens synchronously inside a WebView2 callback.
const EVICT_GRACE: Duration = Duration::from_millis(80);

/// A file shown in a page: which viewer, for which version of the file, and the text the page sent last.
pub(super) struct Viewer {
    pub kind: &'static str,
    pub version: String,
    pub pane: Entity<WebPane>,
    /// Monotonic counter of when this viewer was last accessed, for LRU hibernation.
    pub last_used: u64,
    /// The editor whose text a Markdown page shows. Reading the file again from disk gives the tab a new editor; a page
    /// left on the old one would keep the old text, and its next edit would write that over the new file.
    pub editor: Option<EntityId>,
    /// The page's own edit, so it is not sent back to it.
    pub from_page: Option<String>,
    /// The split view's scrolling, for the preview and its source editor to follow each other.
    pub scroll: ScrollSync,
    pub _subscriptions: Vec<Subscription>,
    /// Changes and scrolling of `editor`, replaced together with it.
    pub _editor_subscriptions: Vec<Subscription>,
}

/// Last known scroll position of a viewer pane so it can be restored after LRU hibernation.
#[derive(Clone, Copy, Debug)]
pub(super) enum SavedScroll {
    Line(f64),
    Top(f64),
}

/// Where the split view's source editor was last seen, and when the preview last moved it.
#[derive(Default)]
pub(super) struct ScrollSync {
    /// The editor's top line, 1-based with the fraction of the line scrolled past.
    pub line: Option<f64>,
    /// The editor scrolls to the preview's line a frame or two after the preview reports it; that scroll is not sent
    /// back, or the preview would be pulled to where the editor stops, such as above its own end.
    pub page_at: Option<Instant>,
}

/// An image file's decoded bytes, fetched from the Host's `/rainy/ide/raw` route, for when its page cannot be shown.
pub(super) enum Picture {
    Loading,
    Ready(Arc<Image>),
    /// What went wrong, and the Host's own answer as a detail.
    Failed(SharedString, Option<SharedString>),
}

pub(super) fn image_format(path: &str) -> Option<ImageFormat> {
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

const SCROLL_TOP_PROBE: &str = "(() => { const el = document.scrollingElement || document.documentElement; const top = el ? el.scrollTop : window.scrollY; if (Number.isFinite(top) && top > 0 && window.ipc) { window.ipc.postMessage(JSON.stringify({ type: 'scroll-top', top })); } })()";

impl EditorArea {
    fn next_viewer_tick(&mut self) -> u64 {
        self.viewer_tick = self.viewer_tick.wrapping_add(1);
        self.viewer_tick
    }

    /// Evict least-recently-used WebPane instances when more than `MAX_ACTIVE_VIEWERS` are alive.
    /// Keeps evicted panes and their message subscriptions alive briefly so `scroll-top` probes arrive
    /// before tearing down WebView2 controllers on a clean UI tick.
    fn evict_lru_viewers(&mut self, keep_key: &str, cx: &mut Context<Self>) {
        let mut retiring: Vec<(Entity<WebPane>, Vec<Subscription>)> = Vec::new();
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
                } else if let Some(view) = removed.pane.read(cx).view() {
                    view.execute(SCROLL_TOP_PROBE, |_| {});
                }
                retiring.push((removed.pane, removed._subscriptions));
            }
        }
        if !retiring.is_empty() {
            cx.spawn(async move |_, cx: &mut AsyncApp| {
                cx.background_executor().timer(EVICT_GRACE).await;
                cx.update(|_| drop(retiring));
            })
            .detach();
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
        // Snapshot scroll positions of other open viewers before switching away from them.
        for (other_key, other) in &self.viewers {
            if other_key != key
                && let Some(view) = other.pane.read(cx).view()
            {
                view.execute(SCROLL_TOP_PROBE, |_| {});
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
        self.evict_lru_viewers(key, cx);
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
    pub(super) fn follow_editors(&mut self, cx: &mut Context<Self>) {
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

    /// Run a workbench key pressed in a page.
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

    /// The picture of an open image file, fetched on first use.
    pub(super) fn picture(&mut self, key: &str, cx: &mut Context<Self>) -> Option<&Picture> {
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

    /// A file in a page, with the text editor (and context menu) on the left when `editor` is given.
    pub(super) fn render_page(&mut self, key: &str, kind: &'static str, editor: Option<Entity<EditorState>>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
                        .when(creating, |this| this.child(centered(crate::ui::motion::spinner(Size::Small, cx), &theme)))
                        .child(div().absolute().inset_0().child(WebPane::area(&pane)))
                        .into_any_element(),
                }
            }
            None => centered(crate::ui::motion::spinner(Size::Small, cx), &theme).into_any_element(),
        };
        match editor {
            Some(editor) => h_flex()
                .size_full()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .border_r_1()
                        .border_color(theme.border)
                        .child(Editor::new(&editor).h_full().bordered(false).context_menu(editor_commands::context_menu)),
                )
                .child(div().flex_1().min_w_0().h_full().child(page))
                .into_any_element(),
            None => page,
        }
    }
}

/// The source line at the top of an editor, 1-based with the fraction scrolled past; `None` before its first layout.
fn top_line(state: &EditorState) -> Option<f64> {
    let height = state.line_height().map(f32::from).filter(|height| *height > 0.)?;
    let map = state.display_map();
    // Display rows are all one line high; the offset is negative once scrolled down.
    let row = f64::from((-f32::from(state.scroll_offset().y) / height).max(0.));
    line_at_row(row, map.display_row_count(), |row| map.display_row_to_buffer_line(row), |line| map.buffer_line_to_display_row_range(line))
}

/// The 1-based source line, with its fraction, at display row `row` of `rows`.
pub(super) fn line_at_row(row: f64, rows: usize, line_of: impl Fn(usize) -> usize, rows_of: impl Fn(usize) -> Option<Range<usize>>) -> Option<f64> {
    if rows == 0 || !row.is_finite() {
        return None;
    }
    let row = row.clamp(0., (rows - 1) as f64 + 0.999);
    let line = line_of(row.floor() as usize);
    let fraction = match rows_of(line) {
        Some(range) if range.end > range.start => ((row - range.start as f64) / range.len() as f64).clamp(0., 0.999),
        _ => 0.,
    };
    Some(((line as f64 + 1. + fraction) * 1000.).round() / 1000.)
}

/// The display row that puts 1-based source line `line` (with its fraction) at the top of an editor of `lines` buffer lines.
pub(super) fn row_at_line(line: f64, lines: usize, rows_of: impl Fn(usize) -> Option<Range<usize>>, first_row: impl Fn(usize) -> usize) -> f64 {
    if lines == 0 || !line.is_finite() {
        return 0.;
    }
    let line = line.max(1.);
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
pub(super) fn scroll_message(text: &str) -> Option<f64> {
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
pub(super) fn scroll_script(line: f64) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

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
        assert_eq!(line_at_row(3., 5, line_of, rows_of), Some(2.667));
        assert_eq!(line_at_row(4., 5, line_of, rows_of), Some(3.));
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
        assert_eq!(row_at_line(9.75, 3, rows_of, first_row), 4.);
        assert_eq!(row_at_line(0.2, 3, rows_of, first_row), 0.);
        assert_eq!(row_at_line(f64::INFINITY, 3, rows_of, first_row), 0.);
        assert_eq!(row_at_line(2., 0, rows_of, first_row), 0.);
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
}
