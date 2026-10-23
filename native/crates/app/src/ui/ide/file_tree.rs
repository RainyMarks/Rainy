//! The project's file tree (`FileTree.tsx`): every mounted folder, its expanded directories, click to open.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::FileEntry;

use crate::store::ide::{Ide, IdeEvent, file_key, file_name, parent_key, split_key};
use crate::ui::ide::{choose_folder, file_icon};
use crate::ui::prompt::prompt_text;
use crate::ui::{tr, trf};

/// One visible line of the tree.
#[derive(Clone)]
enum Row {
    Root { key: String, title: SharedString },
    Entry { key: String, entry: FileEntry, depth: usize, open: bool },
    Message { text: SharedString, depth: usize },
}

pub struct FileTree {
    ide: Entity<Ide>,
    rows: Vec<Row>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl FileTree {
    pub fn new(ide: Entity<Ide>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe(&ide, |this, _, cx| {
                this.rebuild(cx);
            }),
            cx.subscribe(&ide, |this, _, _: &IdeEvent, cx| this.rebuild(cx)),
        ];
        let mut tree = Self { ide, rows: Vec::new(), scroll: UniformListScrollHandle::new(), _subscriptions: subscriptions };
        tree.rebuild(cx);
        tree
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let ide = self.ide.read(cx);
        let mut rows = Vec::new();
        if let Some(workspace) = &ide.workspace {
            let roots = workspace.roots();
            let several = roots.len() > 1;
            for root in &roots {
                let key = file_key("", Some(root));
                if several {
                    rows.push(Row::Root { key: key.clone(), title: root.title.clone().into() });
                }
                Self::push_children(ide, &key, usize::from(several), &mut rows);
            }
        }
        self.rows = rows;
        cx.notify();
    }

    fn push_children(ide: &Ide, directory: &str, depth: usize, rows: &mut Vec<Row>) {
        let Some(listing) = ide.listings.get(directory) else {
            rows.push(Row::Message { text: "…".into(), depth });
            return;
        };
        if let Some(error) = &listing.error {
            rows.push(Row::Message { text: error.clone(), depth });
            return;
        }
        let (root, _) = split_key(directory);
        for entry in &listing.entries {
            let key = match root {
                Some(root) => format!("\0{root}/{}", entry.path),
                None => entry.path.clone(),
            };
            let open = entry.is_directory() && ide.expanded.contains(&key);
            rows.push(Row::Entry { key: key.clone(), entry: entry.clone(), depth, open });
            if open {
                Self::push_children(ide, &key, depth + 1, rows);
            }
        }
    }
}

/// Ask for a name, then create a file or folder in `directory`.
fn ask_create(ide: Entity<Ide>, directory: String, folder: bool, window: &mut Window, cx: &mut App) {
    let title = tr(cx, if folder { "ide.ideNewFolder" } else { "ide.ideNewFile" });
    prompt_text(title, String::new(), tr(cx, "ide.ideConfirm"), move |name, window, cx| {
        let directory = directory.clone();
        ide.update(cx, |ide, cx| ide.create(directory, name, folder, window, cx));
    }, window, cx);
}

/// Ask for the new name of a file or folder.
fn ask_rename(ide: Entity<Ide>, key: String, window: &mut Window, cx: &mut App) {
    let current = file_name(&key).to_owned();
    prompt_text(tr(cx, "ide.ideRename"), current, tr(cx, "ide.ideRename"), move |name, window, cx| {
        let key = key.clone();
        ide.update(cx, |ide, cx| ide.rename(key, name, window, cx));
    }, window, cx);
}

/// Show what deleting removes, then delete with the preview's token.
fn ask_delete(ide: Entity<Ide>, key: String, window: &mut Window, cx: &mut App) {
    let preview = ide.read(cx).delete_preview(&key, cx);
    window
        .spawn(cx, async move |cx| {
            let result = preview.await;
            let _ = cx.update(|window, cx| match result {
                Ok(preview) => {
                    let token = preview["token"].as_str().unwrap_or_default().to_owned();
                    let entries = preview["entries"].as_u64().unwrap_or(1);
                    let bytes = preview["bytes"].as_u64().unwrap_or(0);
                    let (_, path) = split_key(&key);
                    let description = trf(cx, "ide.ideDeleteDescription", &[("path", &path), ("entries", &entries), ("bytes", &bytes)]);
                    let (ide, key) = (ide.clone(), key.clone());
                    window.open_alert_dialog(cx, move |alert, _, cx| {
                        let (ide, key, token) = (ide.clone(), key.clone(), token.clone());
                        alert
                            .title(tr(cx, "ide.ideDelete"))
                            .description(description.clone())
                            .ok_text(tr(cx, "ide.ideDelete"))
                            .cancel_text(tr(cx, "ide.ideCancel"))
                            .show_cancel(true)
                            .on_ok(move |_, window, cx| {
                                let (key, token) = (key.clone(), token.clone());
                                ide.update(cx, |ide, cx| ide.delete(key, token, window, cx));
                                true
                            })
                    });
                }
                Err(error) => window.push_notification(Notification::error(error.message), cx),
            });
        })
        .detach();
}

/// The menu of a tree row: create inside a folder (or beside a file), rename, delete, refresh.
fn row_menu(menu: PopupMenu, ide: Entity<Ide>, key: String, directory: bool, cx: &App) -> PopupMenu {
    let target = if directory { key.clone() } else { parent_key(&key) };
    let (new_file, new_folder, rename, delete, refresh) = (ide.clone(), ide.clone(), ide.clone(), ide.clone(), ide);
    let (file_dir, folder_dir, renamed, deleted) = (target.clone(), target, key.clone(), key);
    menu.item(PopupMenuItem::new(tr(cx, "ide.ideNewFile")).on_click(move |_, window, cx| ask_create(new_file.clone(), file_dir.clone(), false, window, cx)))
        .item(PopupMenuItem::new(tr(cx, "ide.ideNewFolder")).on_click(move |_, window, cx| ask_create(new_folder.clone(), folder_dir.clone(), true, window, cx)))
        .separator()
        .item(PopupMenuItem::new(tr(cx, "ide.ideRename")).on_click(move |_, window, cx| ask_rename(rename.clone(), renamed.clone(), window, cx)))
        .item(PopupMenuItem::new(tr(cx, "ide.ideDelete")).on_click(move |_, window, cx| ask_delete(delete.clone(), deleted.clone(), window, cx)))
        .separator()
        .item(PopupMenuItem::new(tr(cx, "ide.ideRefresh")).on_click(move |_, window, cx| refresh.update(cx, |ide, cx| ide.refresh(window, cx))))
}

impl Render for FileTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ide = self.ide.read(cx);
        let active = ide.active.clone();
        let title = ide.workspace.as_ref().map(|workspace| SharedString::from(workspace.title.clone()));
        let rows = self.rows.clone();
        let theme = cx.theme();
        let ide_handle = self.ide.clone();
        let list = uniform_list("file-tree", rows.len(), move |range, _window, cx| {
            let theme = cx.theme().clone();
            range
                .map(|index| {
                    let row = rows[index].clone();
                    let ide = ide_handle.clone();
                    match row {
                        Row::Root { key, title } => {
                            let root_id = split_key(&key).0.map(str::to_owned);
                            let menu_ide = ide.clone();
                            h_flex()
                            .id(index)
                            .h(px(26.))
                            .px_2()
                            .gap_1()
                            .items_center()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::FolderOpen).xsmall())
                            .child(title)
                            .on_click(move |_, window, cx| {
                                let key = key.clone();
                                ide.update(cx, |ide, cx| ide.list(key, window, cx));
                            })
                            // Folders added to the project can be taken out of it; the main folder stays.
                            .map(|this| match root_id {
                                None => this.into_any_element(),
                                Some(root_id) => this.context_menu(move |menu, _, cx| {
                                    let (ide, root_id) = (menu_ide.clone(), root_id.clone());
                                    menu.item(PopupMenuItem::new(tr(cx, "ide.ideRemoveRoot")).on_click(move |_, window, cx| {
                                        let removed = ide.update(cx, |ide, cx| ide.remove_root(root_id.clone(), window, cx));
                                        if !removed {
                                            use gpui_kit::component::WindowExt as _;
                                            window.push_notification(
                                                gpui_kit::component::notification::Notification::warning(tr(cx, "native.unsavedRoot")),
                                                cx,
                                            );
                                        }
                                    }))
                                })
                                .into_any_element(),
                            })
                        }
                        Row::Message { text, depth } => div()
                            .id(index)
                            .h(px(26.))
                            .pl(px(12. + 14. * depth as f32))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(text)
                            .into_any_element(),
                        Row::Entry { key, entry, depth, open } => {
                            let directory = entry.is_directory();
                            let selected = active.as_deref() == Some(key.as_str());
                            let icon = if directory {
                                if open { IconName::FolderOpen } else { IconName::Folder }
                            } else {
                                file_icon(&entry.name)
                            };
                            let (menu_ide, menu_key) = (ide.clone(), key.clone());
                            h_flex()
                                .id(index)
                                .h(px(26.))
                                .pl(px(8. + 14. * depth as f32))
                                .pr_2()
                                .gap_1p5()
                                .items_center()
                                .text_sm()
                                .cursor_pointer()
                                .when(selected, |this| this.bg(theme.list_active))
                                .when(!selected, |this| this.hover(|style| style.bg(theme.list_hover)))
                                .child(
                                    div().w(px(12.)).flex_shrink_0().when(directory, |this| {
                                        this.child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(theme.muted_foreground))
                                    }),
                                )
                                .child(Icon::new(icon).small().text_color(if directory { theme.muted_foreground } else { theme.foreground }))
                                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(entry.name.clone()))
                                .on_click(move |_, window, cx| {
                                    let key = key.clone();
                                    ide.update(cx, |ide, cx| {
                                        if directory {
                                            ide.toggle_directory(key, window, cx);
                                        } else {
                                            ide.open(key, true, window, cx);
                                        }
                                    });
                                })
                                .context_menu(move |menu, _, cx| row_menu(menu, menu_ide.clone(), menu_key.clone(), directory, cx))
                                .into_any_element()
                        }
                    }
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll)
        .flex_1();
        let has_workspace = title.is_some();
        let (new_file, new_folder, refresh) = (self.ide.clone(), self.ide.clone(), self.ide.clone());
        // New entries go to the top of the primary folder; the row menus create elsewhere.
        let root = String::new();
        let (file_root, folder_root) = (root.clone(), root);
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .h(px(40.))
                    .flex_shrink_0()
                    .pl_3()
                    .pr_1()
                    .gap_0p5()
                    .items_center()
                    .text_sm()
                    .font_semibold()
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title.unwrap_or_else(|| tr(cx, "ide.ideExplorer"))))
                    .when(has_workspace, |this| {
                        this.child(Button::new("tree-new-file").ghost().xsmall().icon(Icon::new(gpui_kit::assets::IconName::FilePlus)).tooltip(tr(cx, "ide.ideNewFile")).on_click(
                            move |_, window, cx| ask_create(new_file.clone(), file_root.clone(), false, window, cx),
                        ))
                        .child(Button::new("tree-new-folder").ghost().xsmall().icon(Icon::new(gpui_kit::assets::IconName::FolderPlus)).tooltip(tr(cx, "ide.ideNewFolder")).on_click(
                            move |_, window, cx| ask_create(new_folder.clone(), folder_root.clone(), true, window, cx),
                        ))
                        .child(Button::new("tree-refresh").ghost().xsmall().icon(IconName::RefreshCw).tooltip(tr(cx, "ide.ideRefresh")).on_click(
                            move |_, window, cx| refresh.update(cx, |ide, cx| ide.refresh(window, cx)),
                        ))
                    })
                    .when(!has_workspace, |this| {
                        this.child(Button::new("tree-open").ghost().xsmall().icon(IconName::FolderOpen).tooltip(tr(cx, "ide.ideOpenFolder")).on_click(
                            |_, window, cx| choose_folder(window, cx),
                        ))
                    }),
            )
            .child(list)
    }
}
