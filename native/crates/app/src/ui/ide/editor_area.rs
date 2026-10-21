//! The editor area (`IdeEditor.tsx` and the tab strip of `Workbench.tsx`): open files as tabs, the active one in an
//! editor or a viewer, and what a window without a project or without open files shows.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::diff::Diff;
use gpui_kit::component::input::Editor;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::store::Store;
use crate::store::ide::{Ide, file_name, split_key};
use crate::ui::ide::{choose_folder, file_icon};
use crate::ui::tr;

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

/// Percent-encode a query value: everything but unreserved characters.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

impl EditorArea {
    pub fn new(ide: Entity<Ide>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.observe(&ide, |_, _, cx| cx.notify())];
        Self { ide, pictures: HashMap::new(), _subscriptions: subscriptions }
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
            .h(px(36.))
            .flex_shrink_0()
            .overflow_x_scroll()
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
                .child(div().flex_1().min_h_0().child(Editor::new(&editor).h_full().bordered(false)))
                .into_any_element();
        }
        let reason = file.document.read_only_reason.clone().unwrap_or_default();
        let preview = file.document.preview.clone();
        let path = file.document.path.clone();
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
        let _ = window;
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

impl Render for EditorArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let ide = self.ide.read(cx);
        let body = if ide.workspace.is_none() {
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
        div().size_full().bg(theme.background).child(body)
    }
}
