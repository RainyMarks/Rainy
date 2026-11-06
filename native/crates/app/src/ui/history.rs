//! The chat list (`History.tsx`). In IDE mode a tab of the left pane: a search field, then the chats grouped as
//! pinned, the current project, other projects and no project. In Agent mode the sidebar, organized as in the Codex
//! app: new chat and search, then the pinned chats, every project as a row that folds over its chats (a new chat and
//! the project's menu on the row itself, pinned projects first, then the most recently active), and the chats outside
//! any project. A chat shows how long ago it was active, a dot for a reply not seen yet and a branch icon when it works
//! in its own worktree. Chats are listed most recently updated first; archived chats are in their own window; a search
//! looks through titles, folders and, through the Host, the messages themselves.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use serde_json::json;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{IdeWorkspace, SessionSummary};

use crate::store::ide::{Ide, IdeEvent};
use gpui_kit::component::notification::Notification;

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
    /// The query the Host last searched the messages for, and the chats it found with a line of each.
    content_query: String,
    content_hits: Vec<(String, String)>,
    _content_search: Option<Task<()>>,
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
                InputEvent::Change => {
                    let query = search.read(cx).value().trim().to_owned();
                    this.search_messages(query, cx);
                    cx.notify();
                }
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
            // Projects renamed, pinned, opened or removed.
            cx.observe(&Ide::global(cx), |_, _, cx| cx.notify()),
        ];
        Self {
            store,
            search,
            sidebar: false,
            searching: false,
            folded: HashSet::new(),
            unlimited: HashSet::new(),
            hovered: None,
            menu_open: None,
            content_query: String::new(),
            content_hits: Vec::new(),
            _content_search: None,
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

    /// A chat in the sidebar: a dot for a reply not seen yet, its title, a branch icon when it works in its own
    /// worktree, then how long ago it was active (a spinner while it works, a dot while it waits for an approval), and
    /// its menu button under the pointer; a search adds the line of a message that matched. Choosing a chat of another
    /// known project shows that project, so its files, the new chats and file links all belong to the chat.
    fn render_sidebar_row(&self, summary: &SessionSummary, folder: Option<SharedString>, snippet: Option<String>, nested: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let id = summary.id.clone();
        let active = self.store.read(cx).selected.as_deref() == Some(summary.id.as_str());
        let busy = matches!(summary.status.as_str(), "running" | "compacting");
        let waiting = summary.awaiting_approval == Some(true);
        let unread = !active && summary.unread();
        let title = title_of(summary, cx);
        let actions = ChatActions::of(summary, title.clone());
        let menu_shown = self.hovered.as_deref() == Some(id.as_str()) || self.menu_open.as_deref() == Some(id.as_str());
        let workspace = summary.workspace_id.clone();
        let (hover_id, open_id, menu_actions) = (id.clone(), id.clone(), actions.clone());
        let when = ago(summary.updated_at, crate::ui::locale(cx) == crate::i18n::Locale::Zh);
        let right: AnyElement = if menu_shown {
            Button::new(ElementId::Name(format!("chat-menu-{open_id}").into()))
                .ghost()
                .xsmall()
                .icon(IconName::Ellipsis)
                .tooltip(tr(cx, "agent.chatActions"))
                .dropdown_menu(move |menu, _, cx| menu_actions.menu(menu, cx))
                .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
                    this.menu_open = open.then(|| open_id.clone());
                    cx.notify();
                }))
                .into_any_element()
        } else if busy {
            Spinner::new().xsmall().into_any_element()
        } else if waiting {
            div().size(px(7.)).flex_shrink_0().rounded_full().bg(theme.warning).into_any_element()
        } else {
            div().flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(when).into_any_element()
        };
        let line = h_flex()
            .w_full()
            .h(px(30.))
            .gap_1p5()
            .items_center()
            // The unread dot keeps its place, so titles line up whether or not it shows.
            .child(div().w(px(6.)).h(px(6.)).flex_shrink_0().rounded_full().when(unread, |dot| dot.bg(theme.primary)))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().when(unread, |title| title.font_weight(FontWeight::MEDIUM)).child(title))
            .when(summary.worktree.is_some(), |this| {
                this.child(Icon::new(gpui_kit::assets::IconName::GitBranch).xsmall().flex_shrink_0().text_color(theme.muted_foreground))
            })
            .when_some(folder, |this, folder| {
                this.child(
                    div().max_w(px(88.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(theme.muted_foreground).child(folder),
                )
            })
            .when(summary.archived, |this| {
                this.child(Icon::new(gpui_kit::assets::IconName::Archive).xsmall().flex_shrink_0().text_color(theme.muted_foreground))
            })
            .child(right);
        v_flex()
            .id(ElementId::Name(format!("chat-{id}").into()))
            .w_full()
            .flex_shrink_0()
            .when(nested, |this| this.pl_4())
            .when(!nested, |this| this.pl_1())
            .pr_1()
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
            .child(line)
            .when_some(snippet.filter(|snippet| !snippet.is_empty()), |this, snippet| {
                this.child(div().pl(px(12.)).pb_1().text_xs().text_color(theme.muted_foreground).overflow_hidden().text_ellipsis().whitespace_nowrap().child(snippet))
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

    /// The Agent sidebar above the list: new chat and search (or its field).
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
        v_flex().flex_shrink_0().px_2().pt_2().pb_1().gap_0p5().child(new_chat).child(search)
    }

    /// Ask the Host for chats whose messages hold the query, a moment after the typing stops.
    fn search_messages(&mut self, query: String, cx: &mut Context<Self>) {
        if query == self.content_query {
            return;
        }
        self.content_query = query.clone();
        self.content_hits.clear();
        if query.chars().count() < 2 {
            self._content_search = None;
            return;
        }
        self._content_search = Some(cx.spawn(async move |this: WeakEntity<HistoryPanel>, cx: &mut AsyncApp| {
            cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
            let Ok(task) = this.update(cx, |this, cx| this.store.update(cx, |store, cx| store.search_chats(&query, cx))) else { return };
            let hits = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.content_query == query {
                    this.content_hits = hits;
                    cx.notify();
                }
            });
        }));
    }

    /// The "Projects" header, with the button that adds a folder as a project.
    fn render_projects_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .flex_shrink_0()
            .pl_2()
            .pr_1()
            .pt_3()
            .pb_0p5()
            .items_center()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().min_w_0().child(tr(cx, "agent.projects")))
            .child(
                Button::new("sidebar-add-project")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(gpui_kit::assets::IconName::FolderPlus))
                    .tooltip(tr(cx, "agent.addProject"))
                    .dropdown_menu(|menu, _, cx| {
                        menu.item(PopupMenuItem::new(tr(cx, "ide.ideOpenFolder")).on_click(|_, window, cx| crate::ui::ide::choose_folder(window, cx)))
                            .item(PopupMenuItem::new(tr(cx, "ide.ideWslFolder")).on_click(|_, window, cx| crate::ui::ide::browse_folder(window, cx)))
                    }),
            )
            .into_any_element()
    }

    /// A project: its folder and name, a pin when pinned, and under the pointer a new chat in it and its menu. A click
    /// folds or unfolds its chats.
    fn render_project_row(&self, group: &ProjectGroup<'_>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let key = format!("project:{}", group.id);
        let folded = self.folded.contains(&group.id);
        let current = Ide::global(cx).read(cx).workspace.as_ref().is_some_and(|shown| shown.workspace_id == group.id);
        let menu_shown = self.hovered.as_deref() == Some(key.as_str()) || self.menu_open.as_deref() == Some(key.as_str());
        let (fold_id, hover_key, open_key) = (group.id.clone(), key.clone(), key.clone());
        let chats: Vec<String> = group.chats.iter().map(|chat| chat.id.clone()).collect();
        let workspace = group.workspace.clone();
        h_flex()
            .id(ElementId::Name(format!("project-{}", group.id).into()))
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
            .hover(|style| style.bg(theme.list_hover))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered || this.hovered.as_deref() == Some(hover_key.as_str()) {
                    this.hovered = hovered.then(|| hover_key.clone());
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.folded.remove(&fold_id) {
                    this.folded.insert(fold_id.clone());
                }
                cx.notify();
            }))
            .child(Icon::new(if folded { IconName::Folder } else { IconName::FolderOpen }).small().flex_shrink_0().text_color(theme.muted_foreground))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().when(current, |title| title.font_weight(FontWeight::MEDIUM)).child(group.title.clone()))
            .when(group.pinned && !menu_shown, |this| this.child(Icon::new(gpui_kit::assets::IconName::Pin).xsmall().flex_shrink_0().text_color(theme.muted_foreground)))
            .when(menu_shown, |this| {
                let new_in = workspace.clone();
                this.when_some(new_in, |this, new_in| {
                    this.child(
                        Button::new(ElementId::Name(format!("project-new-{key}").into()))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(gpui_kit::assets::IconName::SquarePen))
                            .tooltip(tr(cx, "agent.newChatInProject"))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                new_chat_in(new_in.clone(), window, cx);
                            }),
                    )
                })
                .child(
                    Button::new(ElementId::Name(format!("project-menu-{key}").into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Ellipsis)
                        .tooltip(tr(cx, "agent.projectActions"))
                        .dropdown_menu(move |menu, _, cx| project_menu(menu, workspace.clone(), chats.clone(), cx))
                        .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
                            this.menu_open = open.then(|| open_key.clone());
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    /// The Agent sidebar's list: search results (titles and folders, then messages the Host found), or the pinned
    /// chats, each project with its chats, and the chats outside any project.
    fn render_sidebar_list(&self, summaries: &[SessionSummary], words: &[String], cx: &mut Context<Self>) -> Vec<AnyElement> {
        let ide = Ide::global(cx).read(cx);
        let workspaces = ide.workspaces.clone();
        let titles: HashMap<String, String> = workspaces.iter().map(|workspace| (workspace.workspace_id.clone(), workspace.title.clone())).collect();
        let untitled = tr(cx, "chat.newChat");
        let mut rows = Vec::new();
        if !words.is_empty() {
            let mut shown: HashSet<String> = HashSet::new();
            for summary in sorted(summaries.iter().filter(|summary| found(summary, &untitled, words))) {
                shown.insert(summary.id.clone());
                rows.push(self.render_sidebar_row(summary, Some(folder_name(summary, &titles)), None, false, cx));
            }
            let hits: Vec<(&SessionSummary, String)> = self
                .content_hits
                .iter()
                .filter(|(id, _)| !shown.contains(id))
                .filter_map(|(id, snippet)| summaries.iter().find(|summary| &summary.id == id).map(|summary| (summary, snippet.clone())))
                .collect();
            if !hits.is_empty() {
                rows.push(self.render_label(tr(cx, "agent.foundInMessages"), cx));
                for (summary, snippet) in hits {
                    rows.push(self.render_sidebar_row(summary, Some(folder_name(summary, &titles)), Some(snippet), false, cx));
                }
            }
            return rows;
        }
        let visible = newest_first(summaries.iter().filter(|summary| !summary.archived));
        let tree = sidebar_tree(&visible, &workspaces);
        if !tree.pinned.is_empty() {
            rows.push(self.render_group_header("pinned", tr(cx, "agent.pinned"), cx));
            if !self.folded.contains("pinned") {
                for summary in &tree.pinned {
                    let folder = summary.workspace_id.is_some().then(|| folder_name(summary, &titles));
                    rows.push(self.render_sidebar_row(summary, folder, None, false, cx));
                }
            }
        }
        rows.push(self.render_projects_header(cx));
        for group in &tree.projects {
            rows.push(self.render_project_row(group, cx));
            if self.folded.contains(&group.id) {
                continue;
            }
            rows.extend(self.render_limited(&group.id, &group.chats, true, cx));
        }
        if !tree.loose.is_empty() {
            rows.push(self.render_group_header("none", tr(cx, "agent.chats"), cx));
            if !self.folded.contains("none") {
                rows.extend(self.render_limited("none", &tree.loose, false, cx));
            }
        }
        rows
    }

    /// A small heading inside the list.
    fn render_label(&self, text: SharedString, cx: &mut Context<Self>) -> AnyElement {
        div().w_full().flex_shrink_0().px_2().pt_3().pb_1().text_xs().text_color(cx.theme().muted_foreground).child(text).into_any_element()
    }

    /// A group's chats up to [`GROUP_LIMIT`], then the entry that shows all of them or fewer again.
    fn render_limited(&self, key: &str, chats: &[&SessionSummary], nested: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let all = self.unlimited.contains(key);
        let shown = if all { chats.len() } else { chats.len().min(GROUP_LIMIT) };
        let mut rows: Vec<AnyElement> = chats[..shown].iter().map(|summary| self.render_sidebar_row(summary, None, None, nested, cx)).collect();
        if chats.len() > GROUP_LIMIT {
            let key = key.to_owned();
            let label = if all { tr(cx, "agent.showFewer") } else { trf(cx, "agent.showAll", &[("count", &chats.len())]) };
            let theme = cx.theme();
            rows.push(
                div()
                    .id(ElementId::Name(format!("group-more-{key}").into()))
                    .w_full()
                    .when(nested, |this| this.pl(px(30.)))
                    .when(!nested, |this| this.pl(px(16.)))
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
        rows
    }
}

/// Start a new chat in a project: show it, without bringing back its last chat, and empty the conversation.
fn new_chat_in(workspace: IdeWorkspace, window: &mut Window, cx: &mut App) {
    Ide::global(cx).update(cx, |ide, cx| ide.select_for_new_chat(workspace, window, cx));
    window.dispatch_action(Box::new(NewChat), cx);
}

/// A project's menu: a new chat, rename, pin, add a folder, archive its chats, remove it from the list. A project that
/// is no longer in the list only offers to archive its chats.
fn project_menu(menu: PopupMenu, workspace: Option<IdeWorkspace>, chats: Vec<String>, cx: &App) -> PopupMenu {
    let archive = PopupMenuItem::new(tr(cx, "agent.archiveProjectChats")).disabled(chats.is_empty()).on_click(move |_, window, cx| {
        for id in &chats {
            run("sessions.archive", json!({ "sessionId": id, "archived": true }), window.window_handle(), cx);
        }
    });
    let Some(workspace) = workspace else { return menu.item(archive) };
    let (new_in, rename, pin, attach, remove) = (workspace.clone(), workspace.clone(), workspace.clone(), workspace.clone(), workspace.clone());
    menu.item(PopupMenuItem::new(tr(cx, "agent.newChatInProject")).on_click(move |_, window, cx| new_chat_in(new_in.clone(), window, cx)))
        .item(PopupMenuItem::new(tr(cx, "agent.renameProject")).on_click(move |_, window, cx| {
            let id = rename.workspace_id.clone();
            prompt_text(
                tr(cx, "agent.renameProject"),
                rename.title.clone(),
                tr(cx, "chat.save"),
                move |title, window, cx| {
                    let request = json!({ "op": "workspaces.rename", "workspaceId": id, "title": title });
                    Ide::global(cx).update(cx, |ide, cx| ide.change_workspace(request, window, cx));
                },
                window,
                cx,
            );
        }))
        .item(PopupMenuItem::new(tr(cx, if workspace.pinned { "agent.unpinProject" } else { "agent.pinProject" })).on_click(move |_, window, cx| {
            let request = json!({ "op": "workspaces.pin", "workspaceId": pin.workspace_id, "pinned": !pin.pinned });
            Ide::global(cx).update(cx, |ide, cx| ide.change_workspace(request, window, cx));
        }))
        .item(PopupMenuItem::new(tr(cx, "agent.addFolder")).on_click(move |_, window, cx| {
            // A folder is added to the project shown.
            let workspace = attach.clone();
            Ide::global(cx).update(cx, |ide, cx| ide.select(workspace, window, cx));
            crate::ui::ide::choose_folder_to_attach(window, cx);
        }))
        .item(archive)
        .separator()
        .item(PopupMenuItem::new(tr(cx, "agent.removeProject")).on_click(move |_, window, cx| {
            crate::ui::agent::confirm_remove(Ide::global(cx), remove.workspace_id.clone(), remove.title.clone(), window, cx);
        }))
}

/// How long ago, as briefly as the sidebar has room for: `刚刚`, `5分钟`, `3小时`, `2天`; `now`, `5m`, `3h`, `2d`.
fn ago(updated_ms: f64, zh: bool) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_millis() as f64).unwrap_or(updated_ms);
    ago_from(now, updated_ms, zh)
}

fn ago_from(now_ms: f64, updated_ms: f64, zh: bool) -> String {
    let minutes = ((now_ms - updated_ms) / 60_000.0).max(0.0) as u64;
    let (hours, days) = (minutes / 60, minutes / (60 * 24));
    let (number, zh_unit, en_unit) = match minutes {
        0 => return if zh { "刚刚".into() } else { "now".into() },
        1..60 => (minutes, "分钟", "m"),
        _ if hours < 24 => (hours, "小时", "h"),
        _ if days < 7 => (days, "天", "d"),
        _ if days < 30 => (days / 7, "周", "w"),
        _ if days < 365 => (days / 30, "个月", "mo"),
        _ => (days / 365, "年", "y"),
    };
    if zh { format!("{number}{zh_unit}") } else { format!("{number}{en_unit}") }
}

/// A project in the sidebar and its chats (newest first, pinned ones aside); `workspace` is none for a project no
/// longer in the list, named after its chats' folder.
struct ProjectGroup<'a> {
    id: String,
    title: String,
    pinned: bool,
    workspace: Option<IdeWorkspace>,
    chats: Vec<&'a SessionSummary>,
}

struct SidebarTree<'a> {
    pinned: Vec<&'a SessionSummary>,
    projects: Vec<ProjectGroup<'a>>,
    loose: Vec<&'a SessionSummary>,
}

/// The sidebar for chats given newest first: the pinned chats; every project, also one without chats, pinned ones
/// first and then by their latest chat (so choosing a chat never moves them), projects without chats in the list's
/// order (most recently opened first); and the chats outside any project.
fn sidebar_tree<'a>(chats: &[&'a SessionSummary], workspaces: &[IdeWorkspace]) -> SidebarTree<'a> {
    let mut projects: Vec<ProjectGroup<'a>> = workspaces
        .iter()
        .map(|workspace| ProjectGroup { id: workspace.workspace_id.clone(), title: workspace.title.clone(), pinned: workspace.pinned, workspace: Some(workspace.clone()), chats: Vec::new() })
        .collect();
    let (mut pinned, mut loose) = (Vec::new(), Vec::new());
    let mut latest: HashMap<String, f64> = HashMap::new();
    for &chat in chats {
        if let Some(id) = &chat.workspace_id {
            let newest = latest.entry(id.clone()).or_insert(chat.updated_at);
            *newest = newest.max(chat.updated_at);
        }
        if chat.pinned {
            pinned.push(chat);
            continue;
        }
        match chat.workspace_id.as_deref() {
            None => loose.push(chat),
            Some(id) => match projects.iter_mut().find(|group| group.id == id) {
                Some(group) => group.chats.push(chat),
                None => projects.push(ProjectGroup {
                    id: id.to_owned(),
                    title: chat.cwd.split(['/', '\\']).rfind(|part| !part.is_empty()).unwrap_or(&chat.cwd).to_owned(),
                    pinned: false,
                    workspace: None,
                    chats: vec![chat],
                }),
            },
        }
    }
    projects.sort_by(|a, b| {
        let newest = |group: &ProjectGroup<'_>| latest.get(&group.id).copied().unwrap_or(0.0);
        b.pinned.cmp(&a.pinned).then(newest(b).total_cmp(&newest(a)))
    });
    SidebarTree { pinned, projects, loose }
}

/// The archived chats, in their own window: open, restore or delete each.
pub fn open_archived(store: Entity<Store>, window: &mut Window, cx: &mut App) {
    let list = cx.new(|cx| ArchivedList::new(store, cx));
    window.open_dialog(cx, move |dialog, _, cx| dialog.title(tr(cx, "agent.archivedChats")).w(px(600.)).child(list.clone()));
}

struct ArchivedList {
    store: Entity<Store>,
    _subscriptions: Vec<Subscription>,
}

impl ArchivedList {
    fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.observe(&store, |_, _, cx| cx.notify())];
        Self { store, _subscriptions: subscriptions }
    }
}

impl Render for ArchivedList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let zh = crate::ui::locale(cx) == crate::i18n::Locale::Zh;
        let titles: HashMap<String, String> = Ide::global(cx).read(cx).workspaces.iter().map(|workspace| (workspace.workspace_id.clone(), workspace.title.clone())).collect();
        let summaries = self.store.read(cx).summaries.clone();
        let archived = newest_first(summaries.iter().filter(|summary| summary.archived));
        let rows: Vec<AnyElement> = archived
            .iter()
            .map(|summary| {
                let title = title_of(summary, cx);
                let (open, restore, delete) = (summary.id.clone(), summary.id.clone(), (summary.id.clone(), title.clone()));
                let store = self.store.clone();
                h_flex()
                    .w_full()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(format!("{} · {}", folder_name(summary, &titles), ago(summary.updated_at, zh)))),
                    )
                    .child(Button::new(ElementId::Name(format!("archived-open-{open}").into())).ghost().xsmall().label(tr(cx, "agent.open")).on_click(move |_, window, cx| {
                        store.update(cx, |store, cx| store.select(Some(open.clone()), cx));
                        window.close_dialog(cx);
                    }))
                    .child(Button::new(ElementId::Name(format!("archived-restore-{restore}").into())).outline().xsmall().label(tr(cx, "chat.unarchive")).on_click(move |_, window, cx| {
                        run("sessions.archive", json!({ "sessionId": restore, "archived": false }), window.window_handle(), cx);
                    }))
                    .child(Button::new(ElementId::Name(format!("archived-delete-{}", delete.0).into())).ghost().xsmall().label(tr(cx, "chat.delete")).on_click(move |_, window, cx| {
                        confirm_delete(delete.0.clone(), delete.1.clone(), window, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty();
        v_flex()
            .id("archived-list")
            .w_full()
            .max_h(px(440.))
            .overflow_y_scroll()
            .children(rows)
            .when(empty, |this| this.child(div().py_4().text_sm().text_color(theme.muted_foreground).child(tr(cx, "agent.noArchived"))))
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
    worktree: bool,
}

impl ChatActions {
    pub fn of(summary: &SessionSummary, title: SharedString) -> Self {
        Self { id: summary.id.clone(), title, pinned: summary.pinned, archived: summary.archived, worktree: summary.worktree.is_some() }
    }

    /// Rename, pin or unpin, archive or restore, and delete the chat; a chat in its own worktree can also make a branch
    /// of it or move its work into the project.
    pub fn menu(&self, menu: PopupMenu, cx: &App) -> PopupMenu {
        let menu = if self.worktree {
            let (branch, handed) = (self.id.clone(), self.id.clone());
            menu.item(PopupMenuItem::new(tr(cx, "agent.worktreeBranch")).on_click(move |_, window, cx| create_branch(branch.clone(), window, cx)))
                .item(PopupMenuItem::new(tr(cx, "agent.handOff")).on_click(move |_, window, cx| hand_off(handed.clone(), window, cx)))
                .separator()
        } else {
            menu
        };
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
    list.sort_by_key(|chat| std::cmp::Reverse(chat.pinned));
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
        let footer = (!searching && archived_count > 0).then(|| {
            Button::new("history-archived")
                .ghost()
                .small()
                .w_full()
                .when(self.sidebar, |this| this.justify_start())
                .icon(Icon::new(gpui_kit::assets::IconName::Archive))
                .label(trf(cx, "agent.archivedCount", &[("count", &archived_count)]))
                .on_click(cx.listener(|this, _, window, cx| open_archived(this.store.clone(), window, cx)))
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
        let visible = sorted(summaries.iter().filter(|summary| !summary.archived));
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

/// Ask for a branch name and turn the chat's worktree into that branch.
pub fn create_branch(id: String, window: &mut Window, cx: &mut App) {
    prompt_text(
        tr(cx, "agent.branchName"),
        "rainy/".to_owned(),
        tr(cx, "agent.createBranch"),
        move |name, window, cx| {
            let request = Store::global(cx).read(cx).call("sessions.branch", json!({ "sessionId": id, "name": name }));
            window
                .spawn(cx, async move |cx| {
                    let result = request.await;
                    let _ = cx.update(|window, cx| match result {
                        Ok(_) => window.push_notification(Notification::success(trf(cx, "agent.branchCreated", &[("name", &name)]).to_string()), cx),
                        Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
                    });
                })
                .detach();
        },
        window,
        cx,
    );
}

/// Move the work of the chat's worktree into the project's checkout; the chat continues there.
pub fn hand_off(id: String, window: &mut Window, cx: &mut App) {
    let request = Store::global(cx).read(cx).call("sessions.handOff", json!({ "sessionId": id }));
    window
        .spawn(cx, async move |cx| {
            let result = request.await;
            let _ = cx.update(|window, cx| match result {
                Ok(answer) => {
                    let files = answer["files"].as_u64().unwrap_or(0);
                    let text = if files == 0 { tr(cx, "agent.handedOffNothing") } else { trf(cx, "agent.handedOff", &[("files", &files)]) };
                    window.push_notification(Notification::success(text.to_string()), cx);
                }
                Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
            });
        })
        .detach();
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

    fn project(id: &str, pinned: bool) -> IdeWorkspace {
        IdeWorkspace { workspace_id: id.into(), path: format!("C:\\p\\{id}"), title: id.to_uppercase(), roots: None, pinned }
    }

    #[test]
    fn the_sidebar_lists_every_project_pinned_first_then_by_latest_chat() {
        let list = [
            chat("a", Some("w1"), "C:\\p\\one", 9., false),
            chat("b", Some("w2"), "/home/me/two", 8., false),
            chat("c", None, "C:\\home", 7., false),
            chat("d", Some("w3"), "C:\\p\\three", 6., true),
            chat("e", Some("w2"), "/home/me/two", 5., false),
            chat("f", Some("gone"), "/srv/old-project/", 4., false),
        ];
        let chats = newest_first(list.iter());
        // w4 has no chat yet; w3 is pinned and has only a pinned chat.
        let workspaces = [project("w4", false), project("w2", false), project("w3", true), project("w1", false)];
        let tree = sidebar_tree(&chats, &workspaces);
        let ids = |chats: &[&SessionSummary]| chats.iter().map(|chat| chat.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&tree.pinned), ["d"]);
        let order: Vec<&str> = tree.projects.iter().map(|group| group.id.as_str()).collect();
        assert_eq!(order, ["w3", "w1", "w2", "gone", "w4"]);
        assert_eq!(ids(&tree.projects[2].chats), ["b", "e"]);
        assert!(tree.projects[1].workspace.is_some() && tree.projects[3].workspace.is_none());
        assert_eq!(tree.projects[3].title, "old-project");
        assert!(tree.projects[0].chats.is_empty());
        assert_eq!(ids(&tree.loose), ["c"]);
        // Choosing a chat never reorders projects: the order follows the chats, not the project shown.
        assert_eq!(sidebar_tree(&chats, &workspaces).projects.iter().map(|group| group.id.clone()).collect::<Vec<_>>(), order);
    }

    #[test]
    fn says_how_long_ago_in_a_few_characters() {
        let minute = 60_000.0;
        assert_eq!(ago_from(10.0 * minute, 10.0 * minute, true), "刚刚");
        assert_eq!(ago_from(15.0 * minute, 10.0 * minute, true), "5分钟");
        assert_eq!(ago_from(200.0 * minute, 20.0 * minute, false), "3h");
        assert_eq!(ago_from(3.0 * 1440.0 * minute, 0.0, true), "3天");
        assert_eq!(ago_from(15.0 * 1440.0 * minute, 0.0, false), "2w");
        assert_eq!(ago_from(70.0 * 1440.0 * minute, 0.0, true), "2个月");
        assert_eq!(ago_from(800.0 * 1440.0 * minute, 0.0, false), "2y");
        // A clock that went back reads as now.
        assert_eq!(ago_from(0.0, minute, false), "now");
    }

    #[test]
    fn a_reply_after_the_chat_was_seen_is_unread_once_it_stops() {
        let mut seen = chat("a", None, "C:\\", 5000., false);
        assert!(!seen.unread(), "a chat of 4.8 has no read mark and is never unread");
        seen.read_at = Some(1000.);
        assert!(seen.unread());
        seen.status = "running".into();
        assert!(!seen.unread());
        seen.status = "idle".into();
        seen.read_at = Some(4500.);
        assert!(!seen.unread(), "within a second of the mark");
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
