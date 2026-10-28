//! The chat list (`History.tsx`). In IDE mode a tab of the left pane: a search field, then the chats grouped as
//! pinned, the current project, other projects and no project. In Agent mode the sidebar: new chat, search and the
//! project, then the pinned chats and the chats of each project under headers that fold. Chats are listed most recently
//! updated first; archived chats are listed on their own, behind the button at the bottom; a search looks through all
//! of them.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use serde_json::json;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::SessionSummary;

use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};
use crate::ui::actions::run;
use crate::ui::ide::{NewChat, SearchHistory};
use crate::ui::prompt::prompt_text;
use crate::ui::{tr, trf};

/// Chats a sidebar group shows until it is asked for all of them.
const GROUP_LIMIT: usize = 8;

pub struct HistoryPanel {
    store: Entity<Store>,
    search: Entity<InputState>,
    /// Whether the list shows the archived chats instead of the others.
    show_archived: bool,
    /// Drawn as the sidebar of Agent mode.
    sidebar: bool,
    /// The sidebar shows its search field in place of the search entry: it was asked for, or it holds a query.
    searching: bool,
    /// Sidebar groups the user folded, and those showing all their chats, by group key.
    folded: HashSet<String>,
    unlimited: HashSet<String>,
    /// The sidebar row under the pointer and the one whose menu is open; both show their menu button.
    hovered: Option<String>,
    menu_open: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl HistoryPanel {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "chat.searchHistory")).clean_on_escape());
        let subscriptions = vec![
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| match event {
                // The interface language may have changed.
                StoreEvent::PrefsChanged => {
                    let placeholder = tr(cx, "chat.searchHistory");
                    this.search.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
                    cx.notify();
                }
                StoreEvent::SummariesChanged | StoreEvent::SelectionChanged => cx.notify(),
                _ => {}
            }),
            cx.subscribe(&search, |this, search, event: &InputEvent, cx| match event {
                InputEvent::Change => cx.notify(),
                // An empty field gives its place back to the search entry once the user leaves it.
                InputEvent::Blur if search.read(cx).value().trim().is_empty() => {
                    this.searching = false;
                    cx.notify();
                }
                _ => {}
            }),
            // The chats of the project shown come first.
            cx.subscribe(&Ide::global(cx), |_, _, event: &IdeEvent, cx| {
                if matches!(event, IdeEvent::WorkspaceChanged) {
                    cx.notify();
                }
            }),
        ];
        Self {
            store,
            search,
            show_archived: false,
            sidebar: false,
            searching: false,
            folded: HashSet::new(),
            unlimited: HashSet::new(),
            hovered: None,
            menu_open: None,
            _subscriptions: subscriptions,
        }
    }

    /// Draw the list as the sidebar of Agent mode, or as the tab of the IDE's left pane.
    pub fn set_sidebar(&mut self, sidebar: bool, cx: &mut Context<Self>) {
        if self.sidebar != sidebar {
            self.sidebar = sidebar;
            self.hovered = None;
            cx.notify();
        }
    }

    /// Put the keyboard in the search field (Ctrl+Alt+K).
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.searching = true;
        self.search.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    fn render_row(&self, summary: &SessionSummary, folder: Option<SharedString>, searching: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let id = summary.id.clone();
        let active = self.store.read(cx).selected.as_deref() == Some(summary.id.as_str());
        let busy = matches!(summary.status.as_str(), "running" | "compacting");
        let waiting = summary.awaiting_approval == Some(true);
        let title = title_of(summary, cx);
        let actions = ChatActions::of(summary, title.clone());
        h_flex()
            .id(ElementId::Name(format!("chat-{id}").into()))
            .w_full()
            .h(px(30.))
            .px_2()
            .gap_2()
            .items_center()
            .rounded(theme.radius)
            .text_sm()
            .cursor_pointer()
            .when(active, |this| this.bg(theme.list_active))
            .when(!active, |this| this.hover(|style| style.bg(theme.list_hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                let id = id.clone();
                this.store.update(cx, |store, cx| store.select(Some(id), cx));
            }))
            .when(summary.pinned, |this| {
                this.child(Icon::new(gpui_kit::assets::IconName::Pin).xsmall().flex_shrink_0().text_color(theme.muted_foreground))
            })
            // Found by a search among the archived ones.
            .when(searching && summary.archived, |this| {
                this.child(Icon::new(gpui_kit::assets::IconName::Archive).xsmall().flex_shrink_0().text_color(theme.muted_foreground))
            })
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
            .when_some(folder, |this, folder| {
                this.child(
                    div().max_w(px(96.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(theme.muted_foreground).child(folder),
                )
            })
            .when(busy, |this| this.child(Spinner::new().xsmall()))
            .when(waiting, |this| this.child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(theme.warning)))
            .context_menu(move |menu, _, cx| actions.menu(menu, cx))
            .into_any_element()
    }

    /// A chat in the sidebar: its title, a spinner while it works, a dot while it waits for an approval, and its menu
    /// button under the pointer. Choosing a chat of another known project shows that project, so its files, the new
    /// chats and file links all belong to the chat.
    fn render_sidebar_row(&self, summary: &SessionSummary, folder: Option<SharedString>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let id = summary.id.clone();
        let active = self.store.read(cx).selected.as_deref() == Some(summary.id.as_str());
        let busy = matches!(summary.status.as_str(), "running" | "compacting");
        let waiting = summary.awaiting_approval == Some(true);
        let title = title_of(summary, cx);
        let actions = ChatActions::of(summary, title.clone());
        let menu_shown = self.hovered.as_deref() == Some(id.as_str()) || self.menu_open.as_deref() == Some(id.as_str());
        let workspace = summary.workspace_id.clone();
        let (hover_id, open_id, menu_actions) = (id.clone(), id.clone(), actions.clone());
        h_flex()
            .id(ElementId::Name(format!("chat-{id}").into()))
            .w_full()
            .h(px(30.))
            .flex_shrink_0()
            .pl_2()
            .pr_1()
            .gap_1p5()
            .items_center()
            .rounded(theme.radius)
            .text_sm()
            .cursor_pointer()
            .when(active, |this| this.bg(theme.list_active))
            .when(!active, |this| this.hover(|style| style.bg(theme.list_hover)))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let now = hovered.then(|| hover_id.clone());
                if *hovered || this.hovered.as_deref() == Some(hover_id.as_str()) {
                    this.hovered = now;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                let id = id.clone();
                this.store.update(cx, |store, cx| store.select(Some(id), cx));
                // After the selection, so the project's own last chat does not replace it (`Ide::restore_session`).
                let ide = Ide::global(cx);
                let other = ide.read(cx).workspaces.iter().find(|candidate| {
                    workspace.as_deref() == Some(candidate.workspace_id.as_str())
                        && ide.read(cx).workspace.as_ref().is_none_or(|shown| shown.workspace_id != candidate.workspace_id)
                });
                if let Some(other) = other.cloned() {
                    ide.update(cx, |ide, cx| ide.select(other, window, cx));
                }
            }))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
            .when_some(folder, |this, folder| {
                this.child(
                    div().max_w(px(88.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(theme.muted_foreground).child(folder),
                )
            })
            .when(summary.archived, |this| {
                this.child(Icon::new(gpui_kit::assets::IconName::Archive).xsmall().flex_shrink_0().text_color(theme.muted_foreground))
            })
            .when(busy, |this| this.child(Spinner::new().xsmall()))
            .when(waiting, |this| this.child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(theme.warning)))
            .when(menu_shown, |this| {
                this.child(
                    Button::new(ElementId::Name(format!("chat-menu-{open_id}").into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Ellipsis)
                        .tooltip(tr(cx, "agent.chatActions"))
                        .dropdown_menu(move |menu, _, cx| menu_actions.menu(menu, cx))
                        .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
                            this.menu_open = open.then(|| open_id.clone());
                            cx.notify();
                        })),
                )
            })
            .context_menu(move |menu, _, cx| actions.menu(menu, cx))
            .into_any_element()
    }

    /// The header of a sidebar group; a click folds or unfolds it.
    fn render_group_header(&self, key: &str, title: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let folded = self.folded.contains(key);
        let key = key.to_owned();
        h_flex()
            .id(ElementId::Name(format!("group-{key}").into()))
            .w_full()
            .flex_shrink_0()
            .px_2()
            .pt_3()
            .pb_1()
            .gap_1()
            .items_center()
            .text_xs()
            .text_color(theme.muted_foreground)
            .cursor_pointer()
            .hover(|style| style.text_color(theme.foreground))
            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
            .child(Icon::new(if folded { IconName::ChevronRight } else { IconName::ChevronDown }).xsmall().flex_shrink_0())
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.folded.remove(&key) {
                    this.folded.insert(key.clone());
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// An entry at the top of the sidebar: an icon, its name and its key.
    fn nav_row(&self, id: &'static str, icon: impl Into<Icon>, label: SharedString, keys: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        h_flex()
            .id(id)
            .w_full()
            .h(px(30.))
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .items_center()
            .rounded(theme.radius)
            .text_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme.list_hover))
            .child(icon.into().small().text_color(theme.muted_foreground))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
            .child(div().flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(keys))
    }

    /// The Agent sidebar above the list: new chat, search (or its field) and the project new chats start in.
    fn render_sidebar_top(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let new_chat = self.nav_row("sidebar-new", Icon::new(gpui_kit::assets::IconName::SquarePen), tr(cx, "agent.newChat"), "Ctrl+N", cx)
            .on_click(|_, window, cx| window.dispatch_action(Box::new(NewChat), cx));
        let search: AnyElement = if self.searching {
            div()
                .w_full()
                .py_0p5()
                .child(Input::new(&self.search).small().cleanable(true).prefix(Icon::new(IconName::Search).small().text_color(theme.muted_foreground)))
                .into_any_element()
        } else {
            self.nav_row("sidebar-search", Icon::new(IconName::Search), tr(cx, "agent.search"), "Ctrl+Alt+K", cx)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(SearchHistory), cx))
                .into_any_element()
        };
        let ide = Ide::global(cx).read(cx);
        let (project, path): (SharedString, SharedString) = match &ide.workspace {
            Some(workspace) => (workspace.title.clone().into(), workspace.path.clone().into()),
            None => (tr(cx, "agent.chooseProject"), tr(cx, "native.projects")),
        };
        // The name takes the room between the icon and the chevrons, so it starts at the left like the rows above.
        let project = crate::ui::agent::with_project_menu(
            Button::new("sidebar-project")
                .ghost()
                .small()
                .w_full()
                .h(px(30.))
                .icon(Icon::new(IconName::FolderOpen))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(project))
                .child(Icon::new(IconName::ChevronsUpDown).xsmall().text_color(theme.muted_foreground))
                .tooltip(path),
            cx,
        );
        v_flex().flex_shrink_0().px_2().pt_2().pb_1().gap_0p5().child(new_chat).child(search).child(project)
    }

    /// The Agent sidebar's list: search results, or the pinned chats and each project's chats under their headers.
    fn render_sidebar_list(&self, summaries: &[SessionSummary], words: &[String], cx: &mut Context<Self>) -> Vec<AnyElement> {
        let ide = Ide::global(cx).read(cx);
        let current = ide.workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        let titles: HashMap<String, String> = ide.workspaces.iter().map(|workspace| (workspace.workspace_id.clone(), workspace.title.clone())).collect();
        let untitled = tr(cx, "chat.newChat");
        let mut rows = Vec::new();
        if !words.is_empty() {
            for summary in sorted(summaries.iter().filter(|summary| found(summary, &untitled, words))) {
                rows.push(self.render_sidebar_row(summary, Some(folder_name(summary, &titles)), cx));
            }
            return rows;
        }
        let visible = newest_first(summaries.iter().filter(|summary| summary.archived == self.show_archived));
        for section in sections(&visible, &titles, current.as_deref()) {
            let title: SharedString = match &section.kind {
                SectionKind::Pinned => tr(cx, "agent.pinned"),
                SectionKind::Project(title) => title.clone().into(),
                SectionKind::NoProject => tr(cx, "agent.noProject"),
            };
            rows.push(self.render_group_header(&section.key, title, cx));
            if self.folded.contains(&section.key) {
                continue;
            }
            let all = self.unlimited.contains(&section.key) || section.kind == SectionKind::Pinned;
            let shown = if all { section.chats.len() } else { section.chats.len().min(GROUP_LIMIT) };
            for summary in &section.chats[..shown] {
                rows.push(self.render_sidebar_row(summary, None, cx));
            }
            if section.chats.len() > GROUP_LIMIT && section.kind != SectionKind::Pinned {
                let key = section.key.clone();
                let label = if all { tr(cx, "agent.showFewer") } else { trf(cx, "agent.showAll", &[("count", &section.chats.len())]) };
                let theme = cx.theme();
                rows.push(
                    div()
                        .id(ElementId::Name(format!("group-more-{key}").into()))
                        .w_full()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .cursor_pointer()
                        .hover(|style| style.text_color(theme.foreground))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.unlimited.remove(&key) {
                                this.unlimited.insert(key.clone());
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
        }
        rows
    }
}

/// A chat's title, or what an untitled one is called.
fn title_of(summary: &SessionSummary, cx: &App) -> SharedString {
    if summary.title.is_empty() { tr(cx, "chat.newChat") } else { summary.title.clone().into() }
}

/// What a chat's menu acts on.
#[derive(Clone)]
pub struct ChatActions {
    id: String,
    title: SharedString,
    pinned: bool,
    archived: bool,
}

impl ChatActions {
    pub fn of(summary: &SessionSummary, title: SharedString) -> Self {
        Self { id: summary.id.clone(), title, pinned: summary.pinned, archived: summary.archived }
    }

    /// Rename, pin or unpin, archive or restore, and delete the chat.
    pub fn menu(&self, menu: PopupMenu, cx: &App) -> PopupMenu {
        let (pinned, archived) = (self.pinned, self.archived);
        let rename = (self.id.clone(), self.title.clone());
        let delete = (self.id.clone(), self.title.clone());
        let (pin, archive) = (self.id.clone(), self.id.clone());
        menu.item(PopupMenuItem::new(tr(cx, "chat.rename")).on_click(move |_, window, cx| {
            open_rename(rename.0.clone(), rename.1.clone(), window, cx);
        }))
        .item(PopupMenuItem::new(tr(cx, if pinned { "chat.unpin" } else { "chat.pin" })).on_click(move |_, window, cx| {
            run("sessions.pin", json!({ "sessionId": pin, "pinned": !pinned }), window.window_handle(), cx);
        }))
        .item(PopupMenuItem::new(tr(cx, if archived { "chat.unarchive" } else { "chat.archive" })).on_click(move |_, window, cx| {
            run("sessions.archive", json!({ "sessionId": archive, "archived": !archived }), window.window_handle(), cx);
        }))
        .separator()
        .item(PopupMenuItem::new(tr(cx, "chat.delete")).on_click(move |_, window, cx| {
            confirm_delete(delete.0.clone(), delete.1.clone(), window, cx);
        }))
    }
}

/// Chats newest first, pinned ones before the others.
fn sorted<'a>(chats: impl Iterator<Item = &'a SessionSummary>) -> Vec<&'a SessionSummary> {
    let mut list = newest_first(chats);
    list.sort_by(|a, b| b.pinned.cmp(&a.pinned));
    list
}

fn newest_first<'a>(chats: impl Iterator<Item = &'a SessionSummary>) -> Vec<&'a SessionSummary> {
    let mut list: Vec<&SessionSummary> = chats.collect();
    list.sort_by(|a, b| b.updated_at.total_cmp(&a.updated_at));
    list
}

/// Which group of the IDE list a chat is in (3.x `groupChats`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Pinned,
    Current,
    Other,
    NoProject,
}

const GROUPS: [(Group, &str); 4] =
    [(Group::Pinned, "chat.pinned"), (Group::Current, "chat.currentProject"), (Group::Other, "chat.otherProjects"), (Group::NoProject, "chat.noProject")];

fn group_of(chat: &SessionSummary, workspace_id: Option<&str>) -> Group {
    match chat.workspace_id.as_deref() {
        _ if chat.pinned => Group::Pinned,
        None => Group::NoProject,
        Some(id) if Some(id) == workspace_id => Group::Current,
        Some(_) => Group::Other,
    }
}

/// What a sidebar group holds.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SectionKind {
    Pinned,
    /// The chats of one project, under its title.
    Project(String),
    NoProject,
}

/// A group of the sidebar and its chats, newest first; `key` names it for folding.
#[derive(Debug)]
struct Section<'a> {
    key: String,
    kind: SectionKind,
    chats: Vec<&'a SessionSummary>,
}

/// The sidebar's groups for chats given newest first: the pinned ones, then one group per project with the project
/// shown first and the others by their latest chat, then the chats outside any project. A project no longer in the
/// list is named after its folder.
fn sections<'a>(chats: &[&'a SessionSummary], titles: &HashMap<String, String>, current: Option<&str>) -> Vec<Section<'a>> {
    let mut pinned = Vec::new();
    let mut projects: Vec<Section<'a>> = Vec::new();
    let mut none = Vec::new();
    for &chat in chats {
        match chat.workspace_id.as_deref() {
            _ if chat.pinned => pinned.push(chat),
            None => none.push(chat),
            Some(id) => match projects.iter_mut().find(|section| section.key == id) {
                Some(section) => section.chats.push(chat),
                None => projects.push(Section { key: id.to_owned(), kind: SectionKind::Project(folder_name(chat, titles).to_string()), chats: vec![chat] }),
            },
        }
    }
    // The order of first appearance is the order of the newest chat; the shown project moves to the front.
    if let Some(at) = projects.iter().position(|section| Some(section.key.as_str()) == current) {
        let shown = projects.remove(at);
        projects.insert(0, shown);
    }
    let mut out = Vec::new();
    if !pinned.is_empty() {
        out.push(Section { key: "pinned".into(), kind: SectionKind::Pinned, chats: pinned });
    }
    out.extend(projects);
    if !none.is_empty() {
        out.push(Section { key: "none".into(), kind: SectionKind::NoProject, chats: none });
    }
    out
}

/// The name of a chat's project, else of the folder it ran in.
fn folder_name(chat: &SessionSummary, titles: &HashMap<String, String>) -> SharedString {
    if let Some(title) = chat.workspace_id.as_ref().and_then(|id| titles.get(id)) {
        return title.clone().into();
    }
    chat.cwd.split(['/', '\\']).rfind(|part| !part.is_empty()).unwrap_or(&chat.cwd).to_owned().into()
}

/// The folder a chat of another project ran in, named beside its title.
fn folder_of(chat: &SessionSummary, workspace_id: Option<&str>) -> Option<SharedString> {
    let other = chat.workspace_id.is_some() && chat.workspace_id.as_deref() != workspace_id;
    other.then(|| chat.cwd.split(['/', '\\']).rfind(|part| !part.is_empty()).unwrap_or(&chat.cwd).to_owned().into())
}

/// Whether a search finds a chat: every word of the query in its title or its folder, ignoring case.
fn found(chat: &SessionSummary, untitled: &str, words: &[String]) -> bool {
    let title = if chat.title.is_empty() { untitled.to_lowercase() } else { chat.title.to_lowercase() };
    let folder = chat.cwd.to_lowercase();
    words.iter().all(|word| title.contains(word.as_str()) || folder.contains(word.as_str()))
}

impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let words: Vec<String> = query.split_whitespace().map(str::to_owned).collect();
        let summaries = self.store.read(cx).summaries.clone();
        let archived_count = summaries.iter().filter(|summary| summary.archived).count();
        let searching = !words.is_empty();
        if searching {
            self.searching = true;
        }
        let rows = if self.sidebar { self.render_sidebar_list(&summaries, &words, cx) } else { self.render_side_list(&summaries, &words, cx) };
        let empty = rows.is_empty();
        let footer = (!searching && (archived_count > 0 || self.show_archived)).then(|| {
            let label: SharedString =
                if self.show_archived { tr(cx, "chat.hideArchived") } else { format!("{} ({archived_count})", tr(cx, "chat.showArchived")).into() };
            Button::new("history-archived")
                .ghost()
                .small()
                .w_full()
                .when(self.sidebar, |this| this.justify_start())
                .icon(Icon::new(gpui_kit::assets::IconName::Archive))
                .label(label)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_archived = !this.show_archived;
                    cx.notify();
                }))
        });
        let theme = cx.theme().clone();
        let top = if self.sidebar { self.render_sidebar_top(cx).into_any_element() } else { self.render_side_header(cx).into_any_element() };
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(top)
            .child(
                v_flex()
                    .id("history-list")
                    .flex_1()
                    .min_h_0()
                    .p_1()
                    .when(self.sidebar, |this| this.px_2())
                    .gap_px()
                    .overflow_y_scroll()
                    .children(rows)
                    .when(empty, |this| {
                        this.child(div().p_3().text_sm().text_color(theme.muted_foreground).child(tr(cx, if searching { "chat.noResults" } else { "chat.noChats" })))
                    }),
            )
            .when_some(footer, |this, footer| {
                this.child(div().flex_shrink_0().p_1().when(self.sidebar, |this| this.px_2()).border_t_1().border_color(theme.sidebar_border).child(footer))
            })
    }
}

impl HistoryPanel {
    /// The IDE tab's header: the search field and the new chat button.
    fn render_side_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        h_flex()
            .h(px(40.))
            .flex_shrink_0()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(theme.sidebar_border)
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.search)
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small().text_color(theme.muted_foreground)),
                ),
            )
            .child(
                Button::new("history-new")
                    .ghost()
                    .small()
                    .icon(Icon::new(gpui_kit::assets::IconName::SquarePen))
                    .tooltip(tr(cx, "chat.newChat"))
                    .on_click(cx.listener(|this, _, _, cx| this.store.update(cx, |store, cx| store.select(None, cx)))),
            )
    }

    /// The IDE tab's list: search results, or the chats grouped as pinned, this project, other projects and none.
    fn render_side_list(&self, summaries: &[SessionSummary], words: &[String], cx: &mut Context<Self>) -> Vec<AnyElement> {
        let workspace_id = Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        let workspace_id = workspace_id.as_deref();
        let untitled = tr(cx, "chat.newChat");
        let mut rows: Vec<AnyElement> = Vec::new();
        if !words.is_empty() {
            for summary in sorted(summaries.iter().filter(|summary| found(summary, &untitled, words))) {
                rows.push(self.render_row(summary, folder_of(summary, workspace_id), true, cx));
            }
            return rows;
        }
        let visible = sorted(summaries.iter().filter(|summary| summary.archived == self.show_archived));
        for (group, title) in GROUPS {
            let chats: Vec<&SessionSummary> = visible.iter().copied().filter(|summary| group_of(summary, workspace_id) == group).collect();
            if chats.is_empty() {
                continue;
            }
            let theme = cx.theme();
            rows.push(div().px_2().pt_2().pb_0p5().text_xs().text_color(theme.muted_foreground).child(tr(cx, title)).into_any_element());
            for summary in chats {
                rows.push(self.render_row(summary, folder_of(summary, workspace_id), false, cx));
            }
        }
        rows
    }
}

/// Ask for a new title and rename the chat.
pub fn open_rename(id: String, title: SharedString, window: &mut Window, cx: &mut App) {
    prompt_text(tr(cx, "chat.rename"), title.to_string(), tr(cx, "chat.save"), move |title, window, cx| {
        run("sessions.rename", json!({ "sessionId": id, "title": title }), window.window_handle(), cx);
    }, window, cx);
}

/// Confirm, then delete the chat.
pub fn confirm_delete(id: String, title: SharedString, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let id = id.clone();
        alert
            .title(tr(cx, "chat.delete"))
            .description(trf(cx, "chat.deleteConfirm", &[("title", &title)]))
            .ok_text(tr(cx, "chat.delete"))
            .cancel_text(tr(cx, "chat.cancel"))
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                run("sessions.delete", json!({ "sessionId": id }), window.window_handle(), cx);
                true
            })
    });
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    fn chat(id: &str, workspace: Option<&str>, cwd: &str, updated: f64, pinned: bool) -> SessionSummary {
        serde_json::from_value(json!({
            "id": id, "workspaceId": workspace, "cwd": cwd, "title": id, "createdAt": 1, "updatedAt": updated,
            "archived": false, "pinned": pinned, "status": "idle"
        }))
        .unwrap()
    }

    #[test]
    fn groups_the_sidebar_by_project_with_the_shown_one_first() {
        let list = [
            chat("a", Some("w1"), "C:\\p\\one", 9., false),
            chat("b", Some("w2"), "/home/me/two", 8., false),
            chat("c", None, "C:\\home", 7., false),
            chat("d", Some("w1"), "C:\\p\\one", 6., true),
            chat("e", Some("w2"), "/home/me/two", 5., false),
            chat("f", Some("gone"), "/srv/old-project/", 4., false),
        ];
        let chats = newest_first(list.iter());
        let titles = HashMap::from([("w1".to_owned(), "One".to_owned()), ("w2".to_owned(), "Two".to_owned())]);
        let ids = |section: &Section<'_>| section.chats.iter().map(|chat| chat.id.clone()).collect::<Vec<_>>();
        let shown = sections(&chats, &titles, Some("w2"));
        let kinds: Vec<&SectionKind> = shown.iter().map(|section| &section.kind).collect();
        assert_eq!(kinds, [
            &SectionKind::Pinned,
            &SectionKind::Project("Two".into()),
            &SectionKind::Project("One".into()),
            &SectionKind::Project("old-project".into()),
            &SectionKind::NoProject
        ]);
        assert_eq!(ids(&shown[0]), ["d"]);
        assert_eq!(ids(&shown[1]), ["b", "e"]);
        assert_eq!(ids(&shown[2]), ["a"]);
        assert_eq!(ids(&shown[4]), ["c"]);
        // With no project shown, the projects follow their latest chat.
        let keys: Vec<String> = sections(&chats, &titles, None).into_iter().map(|section| section.key).collect();
        assert_eq!(keys, ["pinned", "w1", "w2", "gone", "none"]);
        assert!(sections(&[], &titles, None).is_empty());
    }

    #[test]
    fn finds_chats_by_title_and_folder() {
        let one = chat("修复登录", Some("w1"), "C:\\work\\Rainy", 1., false);
        let words = |query: &str| query.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>();
        assert!(found(&one, "新对话", &words("登录")));
        assert!(found(&one, "新对话", &words("rainy 修复")));
        assert!(!found(&one, "新对话", &words("rainy 注册")));
    }
}
