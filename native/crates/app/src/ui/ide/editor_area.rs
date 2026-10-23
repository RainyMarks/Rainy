//! The editor area (`IdeEditor.tsx` and the tab strip of `Workbench.tsx`): open files as tabs, the active one in an
//! editor or a viewer, and what a window without a project or without open files shows.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::diff::Diff;
use gpui_kit::component::input::{Editor, InputEvent};
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
use crate::ui::ide::{AiEditSelection, choose_folder, editor_commands, file_icon};
use crate::ui::tr;
use crate::web::{PaneEvent, WebPane};

/// A file shown in a page: which viewer, for which version of the file, and the text the page sent last.
struct Viewer {
    kind: &'static str,
    version: String,
    pane: Entity<WebPane>,
    /// The page's own edit, so it is not sent back to it.
    from_page: Option<String>,
    _subscriptions: Vec<Subscription>,
}

/// An image file's decoded bytes, fetched from the Host's `/rainy/ide/raw` route.
enum Picture {
    Loading,
    Ready(Arc<Image>),
    Failed(SharedString),
}

pub struct EditorArea {
    ide: Entity<Ide>,
    /// Pictures by file key and version, so a changed file is fetched again.
    pictures: HashMap<(String, String), Picture>,
    /// Pages by file key.
    viewers: HashMap<String, Viewer>,
    /// The tab strip, scrolled so the active tab is in view when it changes.
    tabs_scroll: ScrollHandle,
    scrolled_to: Option<String>,
    /// Ctrl+K on a file's editor.
    ai_edit: Option<Entity<AiEdit>>,
    /// The project map, shown in place of the open files, for the project it was opened for.
    map: Option<(String, Entity<WebPane>)>,
    map_shown: bool,
    /// The heading at the top of each rich editor, by file key.
    outline: HashMap<String, i64>,
    /// An export to start once the file's page is ready.
    pending_export: Option<(String, &'static str)>,
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
            cx.observe(&ide, |_, _, cx| cx.notify()),
            // Choosing a tab or opening a file brings the files back in place of the map.
            cx.subscribe(&ide, |this, _, event: &crate::store::ide::IdeEvent, cx| {
                if *event == crate::store::ide::IdeEvent::ActiveChanged && this.map_shown {
                    this.map_shown = false;
                    cx.notify();
                }
            }),
        ];
        Self { ide, pictures: HashMap::new(), viewers: HashMap::new(), tabs_scroll: ScrollHandle::new(), scrolled_to: None, ai_edit: None, map: None, map_shown: false, outline: HashMap::new(), pending_export: None, _subscriptions: subscriptions }
    }

    /// The page of a file view, created on first use and again when the view or the file's version changes.
    fn viewer(&mut self, key: &str, kind: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<WebPane>> {
        let ide = self.ide.read(cx);
        let file = ide.files.get(key)?;
        let markdown = kind.starts_with("markdown");
        let version = file.document.version.clone();
        if let Some(viewer) = self.viewers.get(key)
            && viewer.kind == kind
            && (markdown || viewer.version == version)
        {
            return Some(viewer.pane.clone());
        }
        let launch = Store::global(cx).read(cx).launch_url()?;
        let workspace = ide.workspace.clone()?;
        // The desktop opens Office files in their own application only where it can reach them.
        let windows = matches!(Store::global(cx).read(cx).target, None | Some(ExecutionTarget::Windows { .. }));
        let host_path = windows.then(|| {
            let (root, path) = split_key(key);
            let base = workspace.roots().iter().find(|known| Some(known.root_id.as_str()) == root || (root.is_none() && known.primary)).map_or(workspace.path.clone(), |known| known.path.clone());
            format!("{}\\{}", base.trim_end_matches(['\\', '/']), path.replace('/', "\\"))
        });
        let url = viewers::viewer_url(&launch, kind, key, &workspace.workspace_id, file.document.bytes as u64, &version, host_path.as_deref());
        let editor = file.editor.clone();
        let pane = viewers::create_pane(url, window, cx);
        let owner = key.to_owned();
        let mut subscriptions = vec![cx.subscribe_in(&pane, window, move |this, _, event: &PaneEvent, window, cx| this.on_page(&owner, event, window, cx))];
        if let (true, Some(editor)) = (markdown, editor) {
            let owner = key.to_owned();
            subscriptions.push(cx.subscribe(&editor, move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.send_text(&owner, cx);
                }
            }));
        }
        self.viewers.insert(key.to_owned(), Viewer { kind, version, pane: pane.clone(), from_page: None, _subscriptions: subscriptions });
        Some(pane)
    }

    /// Hand a Markdown page the file's current text, unless the page wrote it itself.
    fn send_text(&mut self, key: &str, cx: &mut Context<Self>) {
        let ide = self.ide.read(cx);
        let Some(file) = ide.files.get(key) else { return };
        let Some(editor) = file.editor.as_ref() else { return };
        let text = editor.read(cx).value().to_string();
        let version = file.document.version.clone();
        let Some(viewer) = self.viewers.get_mut(key) else { return };
        if !viewer.kind.starts_with("markdown") || viewer.from_page.as_deref() == Some(text.as_str()) {
            return;
        }
        viewer.from_page = None;
        if let Some(view) = viewer.pane.read(cx).view() {
            view.execute(&viewers::set_text_script(&text, &version), |_| {});
        }
    }

    /// What a page asked for.
    fn on_page(&mut self, key: &str, event: &PaneEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PaneEvent::Message(text) => match viewers::parse_message(text) {
                Some(PageRequest::Ready) => {
                    self.send_text(key, cx);
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
                Some(PageRequest::OpenUrl(url)) => crate::ui::open_link(&url, window, cx),
                Some(PageRequest::OpenFile { path, root, line }) => {
                    let key = match root {
                        Some(root) => format!("\0{root}/{path}"),
                        None => path,
                    };
                    self.ide.update(cx, |ide, cx| match line {
                        Some(line) => ide.reveal(key, (line as u32).saturating_sub(1), 0, window, cx),
                        None => ide.open(key, true, window, cx),
                    });
                }
                Some(PageRequest::Call { id, name, params }) => self.answer(key, id, &name, &params, window, cx),
                Some(PageRequest::OpenKey(_) | PageRequest::Ask(_)) | None => {}
            },
            PaneEvent::Shortcut(shortcut) => {
                if let Some(action) = viewers::shortcut_action(*shortcut) {
                    crate::web::reclaim_focus(window);
                    window.dispatch_action(action, cx);
                }
            }
            PaneEvent::NewWindow(url) => crate::ui::open_link(url, window, cx),
            PaneEvent::Ready | PaneEvent::Failed(_) => cx.notify(),
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
                let path = params["path"].as_str().unwrap_or_default().to_owned();
                if params["reveal"] == true {
                    cx.reveal_path(std::path::Path::new(&path));
                } else {
                    cx.open_with_system(std::path::Path::new(&path));
                }
                respond(json!({}), cx);
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
                        Err(error) => {
                            let _ = cx.update(|_, cx| respond(json!({ "error": error.to_string() }), cx));
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
                            let _ = cx.update(|_, cx| respond(json!({ "error": "No window to print in." }), cx));
                            return;
                        }
                        let made = receiver.recv().await.unwrap_or_else(|_| Err("The PDF was not made.".to_owned()));
                        let _ = cx.update(|_, _| crate::web::export::close_finished());
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
            // Slides and old Word files are drawn as the PDF the installed Office makes; the bytes came through the
            // Host, so files in WSL and on SSH servers convert too.
            "office-pdf" => {
                let engine = base64::engine::general_purpose::STANDARD;
                let bytes = engine.decode(params["base64"].as_str().unwrap_or_default()).unwrap_or_default();
                let extension = params["extension"].as_str().unwrap_or_default().to_ascii_lowercase();
                let converting = cx.background_spawn(async move { crate::web::office::to_pdf(&bytes, &extension) });
                cx.spawn(async move |_, cx: &mut AsyncApp| {
                    let result = match converting.await {
                        Ok(converted) => json!({ "pdf": engine.encode(&converted.pdf), "application": converted.application }),
                        Err(failure) => json!({ "error": failure.error, "missing": failure.missing, "encrypted": failure.encrypted }),
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
            self.pictures.insert(entry.clone(), Picture::Loading);
            let fetched = entry.clone();
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let result = match rpc {
                    Some(rpc) => cx.background_spawn(async move { rpc.get(&query).await }).await,
                    None => Err(anyhow::anyhow!("Not connected to the Host.")),
                };
                let picture = match result {
                    Ok((200..=299, bytes)) => Picture::Ready(Arc::new(Image::from_bytes(format, bytes))),
                    Ok((status, body)) => Picture::Failed(format!("HTTP {status}: {}", String::from_utf8_lossy(&body)).into()),
                    Err(error) => Picture::Failed(error.to_string().into()),
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
                    .when(active, |this| this.bg(theme.tab_active).text_color(theme.tab_active_foreground))
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
        let theme = cx.theme().clone();
        let ide = self.ide.read(cx);
        let tab = ide.diffs.get(&key);
        let state = tab.and_then(|tab| tab.state.clone());
        let error = tab.and_then(|tab| tab.error.clone());
        let (show, reload) = (self.ide.clone(), self.ide.clone());
        let (shown, reloaded) = (key.clone(), key.clone());
        let (_, path) = split_key(&key);
        let body = match (state, error) {
            (Some(state), _) => div().flex_1().min_h_0().child(Diff::new(&state)).into_any_element(),
            (None, Some(error)) => {
                centered(div().child(format!("{} {}", tr(cx, "ide.ideDiffUnavailable"), error)), &theme).into_any_element()
            }
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
        let markdown = file.editor.is_some() && viewers::preview_kind(&file.document.path) == Some("markdown");
        if markdown {
            let view = ide.views.get(&key).cloned();
            let editor = file.editor.clone();
            let shown = viewers::page_view(&file.document.path, None, view.as_deref());
            let toolbar = self.render_views(&key, view.as_deref(), cx);
            let body = match shown {
                Some((kind, split)) => self.render_page(&key, kind, split.then_some(editor).flatten(), window, cx),
                None => editor.map(|editor| Editor::new(&editor).h_full().bordered(false).into_any_element()).unwrap_or_else(|| div().into_any_element()),
            };
            return v_flex().size_full().child(toolbar).child(div().flex_1().min_h_0().child(body)).into_any_element();
        }
        if let Some(editor) = file.editor.clone() {
            let conflict = file.conflict;
            let handle = self.ide.clone();
            let (reloaded, kept) = (key.clone(), key.clone());
            let (reload, keep) = (handle.clone(), handle);
            return v_flex()
                .size_full()
                .when(conflict, |this| {
                    this.child(
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
                            .child(div().flex_1().child(tr(cx, "ide.ideExternal")))
                            .child(Button::new("conflict-reload").outline().xsmall().label(tr(cx, "ide.ideReload")).on_click(move |_, window, cx| {
                                let key = reloaded.clone();
                                reload.update(cx, |ide, cx| ide.reload(&key, window, cx));
                            }))
                            .child(Button::new("conflict-keep").primary().xsmall().label(tr(cx, "native.keepMine")).on_click(move |_, window, cx| {
                                let key = kept.clone();
                                keep.update(cx, |ide, cx| ide.save(Some(key), true, window, cx));
                            })),
                    )
                })
                .child(div().flex_1().min_h_0().child(Editor::new(&editor).h_full().bordered(false).context_menu(editor_commands::context_menu)))
                .children(self.ai_edit.clone().filter(|edit| edit.read(cx).key == key))
                .into_any_element();
        }
        let reason = file.document.read_only_reason.clone().unwrap_or_default();
        let preview = file.document.preview.clone();
        let path = file.document.path.clone();
        if let Some((kind, _)) = viewers::page_view(&path, Some(reason.as_str()), None) {
            return self.render_page(&key, kind, None, window, cx);
        }
        if reason == "media" && image_format(&path).is_some() {
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
                Some(Picture::Failed(error)) => centered(div().text_color(theme.danger).child(error.clone()), &theme).into_any_element(),
                _ => centered(Spinner::new().small(), &theme).into_any_element(),
            };
        }
        let notice = match reason.as_str() {
            "too-large" => tr(cx, "ide.ideTooLarge"),
            _ => tr(cx, "ide.ideReadOnly"),
        };
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
                    .child(notice),
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

    /// The Markdown views of a tab: rich, source, preview and split.
    fn render_views(&self, key: &str, current: Option<&str>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        h_flex()
            .flex_shrink_0()
            .h(px(32.))
            .px_2()
            .gap_1()
            .items_center()
            .justify_end()
            .border_b_1()
            .border_color(theme.border)
            .children(viewers::MARKDOWN_VIEWS.iter().enumerate().map(|(index, (label, view))| {
                let selected = current.filter(|current| *current != "rich") == *view;
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
                    Some(error) => centered(div().max_w(px(560.)).text_sm().text_color(theme.danger).child(error), &theme).into_any_element(),
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
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .p_8()
            .child(img("brand/rainy-mark.svg").size(px(48.)))
            .child(div().text_xl().font_semibold().child(tr(cx, "ide.ideWelcomeTitle")))
            .child(div().max_w(px(460.)).text_sm().text_center().text_color(theme.muted_foreground).child(tr(cx, "ide.ideWelcomeDescription")))
            .child(Button::new("welcome-open").primary().icon(IconName::FolderOpen).label(tr(cx, "ide.ideOpenFolder")).on_click(|_, window, cx| {
                choose_folder(window, cx);
            }))
            .when_some(error, |this, error| this.child(div().max_w(px(520.)).text_sm().text_color(theme.danger).child(error)))
            .when(!recent.is_empty(), |this| {
                this.child(
                    v_flex()
                        .w(px(420.))
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
                                .child(Icon::new(IconName::Folder).small().text_color(theme.muted_foreground))
                                .child(div().text_sm().child(workspace.title.clone()))
                                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(theme.muted_foreground).child(path))
                                .on_click(move |_, window, cx| {
                                    let workspace = workspace.clone();
                                    handle.update(cx, |ide, cx| ide.select(workspace, window, cx));
                                })
                        })),
                )
            })
            .into_any_element()
    }
}

fn centered(content: impl IntoElement, theme: &gpui_kit::component::Theme) -> Div {
    div().size_full().flex().items_center().justify_center().text_sm().text_color(theme.muted_foreground).child(content)
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
            let pane = viewers::create_pane(viewers::map_url(&launch, &workspace.workspace_id), window, cx);
            cx.subscribe_in(&pane, window, |this, _, event: &PaneEvent, window, cx| this.on_map(event, window, cx)).detach();
            self.map = Some((workspace.workspace_id.clone(), pane));
        }
        cx.notify();
    }

    /// What the map page asked for.
    fn on_map(&mut self, event: &PaneEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PaneEvent::Message(text) => match viewers::parse_message(text) {
                Some(PageRequest::Ready) => self.send_session(cx),
                Some(PageRequest::OpenKey(key)) => {
                    self.map_shown = false;
                    self.ide.update(cx, |ide, cx| ide.open(key, true, window, cx));
                    cx.notify();
                }
                Some(PageRequest::Ask(text)) => crate::ui::ask_ai(text, window, cx),
                Some(PageRequest::OpenUrl(url)) => crate::ui::open_link(&url, window, cx),
                _ => {}
            },
            PaneEvent::Shortcut(shortcut) => {
                if let Some(action) = viewers::shortcut_action(*shortcut) {
                    crate::web::reclaim_focus(window);
                    window.dispatch_action(action, cx);
                }
            }
            PaneEvent::NewWindow(url) => crate::ui::open_link(url, window, cx),
            PaneEvent::Ready | PaneEvent::Failed(_) => cx.notify(),
        }
    }

    /// Tell the map which chat's changes to mark.
    fn send_session(&self, cx: &App) {
        let Some((_, pane)) = &self.map else { return };
        let session = Store::global(cx).read(cx).selected.clone();
        if let Some(view) = pane.read(cx).view() {
            view.execute(&viewers::set_session_script(session.as_deref()), |_| {});
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

impl Render for EditorArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Pages of closed tabs go away with their tabs.
        let open: std::collections::HashSet<String> = self.ide.read(cx).tabs.iter().cloned().collect();
        self.viewers.retain(|key, _| open.contains(key));
        self.pictures.retain(|(key, _), _| open.contains(key));
        self.outline.retain(|key, _| open.contains(key));
        // A tab that becomes active is brought into view, also when it was opened past the end of the strip.
        let active = self.ide.read(cx).active.clone();
        if active != self.scrolled_to {
            if let Some(index) = active.as_ref().and_then(|key| self.ide.read(cx).tabs.iter().position(|tab| tab == key)) {
                self.tabs_scroll.scroll_to_item(index);
            }
            self.scrolled_to = active;
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
            .on_action(cx.listener(|this, _: &AiEditSelection, window, cx| this.open_ai_edit(window, cx)))
            .child(body)
    }
}
