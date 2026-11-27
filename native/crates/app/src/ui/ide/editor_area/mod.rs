//! The editor area (`IdeEditor.tsx` and the tab strip of `Workbench.tsx`): open files as tabs, the active one in an
//! editor or a viewer, project map, diff review, oversized file live-tail, and the empty/welcome states.

mod diff_view;
mod export_office;
mod file_tail;
mod map;
mod viewer_host;

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::base::motion::{Spring, spring};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Editor, EditorMode, Enter, IndentInline, InputModeKind as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, Size, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::store::ide::{Ide, IdeEvent, file_name, split_key};
use crate::store::{Store, StoreEvent};
use crate::ui::ide::ai_edit::{self, AiEdit, AiEditEvent};
use crate::ui::ide::viewers;
use crate::ui::ide::{AiEditSelection, MarkdownPreview, MarkdownSplit, choose_folder, editor_commands, file_icon};
use crate::ui::{motion, tr};
use crate::web::WebPane;

use file_tail::FileTail;
use viewer_host::{Picture, SavedScroll, Viewer, image_format};

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
    /// Live incremental tail for an oversized text/log file.
    file_tail: Option<FileTail>,
    _subscriptions: Vec<Subscription>,
}

impl EditorArea {
    pub fn new(ide: Entity<Ide>, cx: &mut Context<Self>) -> Self {
        let store = Store::global(cx);
        let subscriptions = vec![
            cx.observe(&ide, |this, _, cx| {
                this.prune_closed(cx);
                this.follow_editors(cx);
                cx.notify();
            }),
            // The map lists the footprint and marks the changes of the chat selected now.
            cx.observe(&store, |this, _, cx| {
                if this.map_session.is_some() {
                    this.send_session(false, cx);
                }
            }),
            // Recreate all WebView2 panes and images against the new Host origin & session cookie after reconnect/swap.
            cx.subscribe(&store, |this, _, event: &StoreEvent, cx| {
                if matches!(event, StoreEvent::HostReconnected(_)) {
                    this.viewers.clear();
                    this.pictures.clear();
                    this.map = None;
                    this.map_shown = false;
                    this.map_session = None;
                    this.file_tail = None;
                    cx.notify();
                }
            }),
            // Choosing a tab or opening a file brings the files back in place of the map.
            cx.subscribe(&ide, |this, ide, event: &IdeEvent, cx| {
                if *event == IdeEvent::ActiveChanged {
                    if this.map_shown {
                        this.map_shown = false;
                    }
                    let active = ide.read(cx).active.as_deref();
                    if this.file_tail.as_ref().is_some_and(|tail| Some(tail.key.as_str()) != active) {
                        this.file_tail = None;
                    }
                    cx.notify();
                }
                // Pages address their project; another project's file under the same key gets a page of its own.
                if *event == IdeEvent::WorkspaceChanged {
                    this.viewers.clear();
                    this.saved_scroll.clear();
                    this.pictures.clear();
                    this.outline.clear();
                    this.map = None;
                    this.map_shown = false;
                    this.map_session = None;
                    this.file_tail = None;
                    cx.notify();
                }
            }),
        ];
        Self {
            ide,
            pictures: HashMap::new(),
            viewers: HashMap::new(),
            viewer_tick: 0,
            saved_scroll: HashMap::new(),
            tabs_scroll: ScrollHandle::new(),
            scrolled_to: None,
            scroll_retries: 0,
            ai_edit: None,
            map: None,
            map_shown: false,
            map_session: None,
            outline: HashMap::new(),
            pending_export: None,
            diff_hunk_mode: true,
            file_tail: None,
            _subscriptions: subscriptions,
        }
    }

    /// Evict viewers, cached images, and outline entries for tabs that closed or changed version,
    /// without rebuilding `HashSet`/`HashMap` allocations on every frame.
    fn prune_closed(&mut self, cx: &App) {
        if self.viewers.is_empty() && self.saved_scroll.is_empty() && self.pictures.is_empty() && self.outline.is_empty() {
            return;
        }
        let ide = self.ide.read(cx);
        let is_open = |key: &str| ide.tabs.iter().any(|tab| tab == key);
        self.viewers.retain(|key, _| is_open(key));
        self.saved_scroll.retain(|key, _| is_open(key));
        self.outline.retain(|key, _| is_open(key));
        self.pictures.retain(|(key, version), _| {
            is_open(key) && ide.files.get(key).is_some_and(|file| file.document.version == *version)
        });
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

    /// Ctrl+/ as in Typora: switch a Markdown tab between the rich view and its source.
    pub fn toggle_markdown_source(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.markdown_active(cx) else { return };
        let source = self.ide.read(cx).views.get(&key).map(String::as_str) == Some("source");
        self.ide.update(cx, |ide, cx| ide.set_view(&key, if source { None } else { Some("source") }, cx));
    }

    /// Ctrl+Shift+V and Ctrl+K V as in VS Code: show the Markdown tab's preview, or its preview beside the source, and
    /// its default view again when it already shows that.
    pub(super) fn toggle_markdown_view(&mut self, view: &'static str, cx: &mut Context<Self>) {
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

    fn render_tabs(&self, active_index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ide = self.ide.read(cx);
        let theme = cx.theme().clone();
        let indicator_color = theme.primary;
        let tabs: Vec<(String, bool, bool)> =
            ide.tabs.iter().map(|key| (key.clone(), ide.active.as_deref() == Some(key.as_str()), ide.is_dirty(key))).collect();
        // Smooth spring-driven 2px top accent bar tracked from the measured active tab geometry.
        let view_bounds = self.tabs_scroll.bounds();
        let measured_indicator = active_index.and_then(|idx| {
            let item = self.tabs_scroll.bounds_for_item(idx)?;
            if view_bounds.size.width <= px(0.) || item.size.width <= px(0.) {
                return None;
            }
            let target_x = f32::from(item.left() - view_bounds.left() + self.tabs_scroll.offset().x);
            let target_w = f32::from(item.size.width);
            let policy = Spring::new(Duration::from_millis(200)).with_epsilon(0.5);
            let anim_x: f32 = spring(("editor-tab-indicator", "x"), target_x, policy, window, cx);
            let anim_w: f32 = spring(("editor-tab-indicator", "w"), target_w, policy, window, cx);
            Some((anim_x, anim_w.max(0.)))
        });
        let draw_inline_bar = measured_indicator.is_none();
        let handle = self.ide.clone();
        let strip = h_flex()
            .id("editor-tabs")
            .size_full()
            .min_w_0()
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
                        this.bg(theme.tab_active)
                            .text_color(theme.tab_active_foreground)
                            .when(draw_inline_bar, |this| this.child(div().absolute().top_0().left_0().right_0().h(px(2.)).bg(theme.primary)))
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
            }));

        div()
            .relative()
            .w_full()
            .min_w_0()
            .h(px(36.))
            .flex_shrink_0()
            .overflow_hidden()
            .child(strip)
            .when_some(measured_indicator, |this, (x, w)| {
                this.child(div().absolute().top_0().left(px(x)).w(px(w)).h(px(2.)).bg(indicator_color))
            })
    }

    fn render_file(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if self.ide.read(cx).diff_tabs.contains(&key) {
            return self.render_diff(key, cx);
        }
        let ide = self.ide.read(cx);
        let Some(file) = ide.files.get(&key) else {
            return centered(h_flex().gap_2().child(motion::spinner(Size::Small, cx)).child(tr(cx, "ide.ideEditorLoading")), &theme).into_any_element();
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
                None => editor
                    .map(|editor| Editor::new(&editor).h_full().bordered(false).context_menu(editor_commands::context_menu).into_any_element())
                    .unwrap_or_else(|| div().into_any_element()),
            };
            return v_flex()
                .size_full()
                .key_context(crate::ui::ide::MARKDOWN_TAB)
                .child(toolbar)
                .children(conflict)
                .child(div().flex_1().min_h_0().child(body))
                .children(self.ai_edit.clone().filter(|edit| edit.read(cx).key == key))
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
                _ => centered(motion::spinner(Size::Small, cx), &theme).into_any_element(),
            };
        }
        if reason == "too-large" && self.file_tail.as_ref().is_some_and(|tail| tail.key == key) {
            return self.render_file_tail(cx);
        }
        let notice = match reason.as_str() {
            "too-large" => tr(cx, "viewers.readOnlyLarge"),
            "unsupported-encoding" => tr(cx, "viewers.notUtf8"),
            _ => tr(cx, "ide.ideReadOnly"),
        };
        let is_too_large = reason == "too-large";
        let (handle, shown, tail_key) = (self.ide.clone(), key.clone(), key.clone());
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
                    .when(is_too_large, |this| {
                        this.child(
                            Button::new("live-tail")
                                .primary()
                                .xsmall()
                                .label(tr(cx, "experiments.fileTailButton"))
                                .on_click(cx.listener(move |this, _, _, cx| this.start_file_tail(tail_key.clone(), cx))),
                        )
                    })
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

    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let ide = self.ide.read(cx);
        let recent: Vec<_> = ide.workspaces.iter().rev().take(8).cloned().collect();
        let error = ide.error.clone();
        let handle = self.ide.clone();
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

    /// Tab while the active editor lists completions takes the highlighted one, as Enter does.
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

pub(super) fn centered(content: impl IntoElement, theme: &gpui_kit::component::Theme) -> Div {
    div().size_full().flex().items_center().justify_center().text_sm().text_color(theme.muted_foreground).child(content)
}

/// Why a file is not shown, in the interface language, with the underlying error as a smaller detail line.
pub(super) fn failure(reason: SharedString, detail: Option<SharedString>, theme: &gpui_kit::component::Theme) -> Div {
    v_flex()
        .max_w(px(560.))
        .gap_1()
        .items_center()
        .text_center()
        .child(div().text_sm().text_color(theme.danger).child(reason))
        .when_some(detail.filter(|detail| !detail.is_empty()), |this, detail| this.child(div().text_xs().text_color(theme.muted_foreground).child(detail)))
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

impl Render for EditorArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // A tab that becomes active is brought into view, also when it was opened past the end of the strip or restored
        // before the strip was first laid out.
        let (active, index) = {
            let ide = self.ide.read(cx);
            let active = ide.active.clone();
            let index = active.as_ref().and_then(|key| ide.tabs.iter().position(|tab| tab == key));
            (active, index)
        };
        if active != self.scrolled_to {
            self.scrolled_to = active.clone();
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
        let body = if let Some(map) = map {
            map
        } else if self.ide.read(cx).workspace.is_none() {
            self.render_welcome(cx)
        } else if self.ide.read(cx).tabs.is_empty() {
            let loading = self.ide.read(cx).loading;
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(theme.muted_foreground)
                .when(loading, |this| this.child(h_flex().gap_2().text_sm().child(motion::spinner(Size::Small, cx)).child(tr(cx, "ide.ideLoading"))))
                .when(!loading, |this| {
                    this.child(div().text_lg().font_semibold().text_color(theme.foreground).child(tr(cx, "ide.ideNoFileTitle")))
                        .child(div().text_sm().child(tr(cx, "ide.ideNoFile")))
                })
                .into_any_element()
        } else {
            let tabs = self.render_tabs(index, window, cx);
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
