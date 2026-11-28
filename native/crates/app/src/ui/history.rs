//! The chat list (`History.tsx`). In IDE mode a tab of the left pane: a search field, then the chats grouped as
//! pinned, the current project, other projects and no project. In Agent mode the sidebar, organized as in the Codex
//! app: new chat and search, then the pinned chats, every project as a row that folds over its chats (a new chat and
//! the project's menu on the row itself, pinned projects first, then the most recently active), and the chats outside
//! any project. A chat shows how long ago it was active, a dot for a reply not seen yet and a branch icon when it works
//! in its own worktree. Chats are listed most recently updated first; archived chats are in their own window; a search
//! looks through titles, folders and, through the Host, the messages themselves. Ctrl-click (or "Select" in a chat's
//! menu) chooses chats, Shift-click every chat listed between; while any is chosen a click chooses, and the top of the
//! list turns into a bar that pins, archives or deletes them all (Esc forgets them, Delete deletes them).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::GlobalState;
use gpui_kit::base::motion::{MotionReveal, Presence, Transition};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, Size, WindowExt as _, h_flex, v_flex};
use serde_json::json;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{IdeWorkspace, SessionSummary};

use crate::store::ide::Ide;
use gpui_kit::component::notification::Notification;

use crate::store::{Store, StoreEvent};
use crate::ui::actions::run;
use crate::ui::ide::{NewChat, SearchHistory};
pub use crate::ui::manage::open_archived;
use crate::ui::manage::{bulk, check_box, confirm_bulk_delete};
use crate::ui::prompt::prompt_text;
use crate::ui::{tr, trf};

/// Chats a sidebar group shows until it is asked for all of them.
const GROUP_LIMIT: usize = 8;

/// Synced subtle pulse (24 fps) for chats awaiting user approval.
fn waiting_dot(id: impl Into<ElementId>, color: Hsla, reduced: bool) -> AnyElement {
    let dot = div().size(px(7.)).flex_shrink_0().rounded_full().bg(color);
    if reduced {
        return dot.into_any_element();
    }
    dot.with_animation(
        id.into(),
        Animation::new(Duration::from_millis(1600))
            .repeat_synced()
            .with_max_fps(24.0)
            .with_easing(pulsating_between(0.45, 1.0)),
        |dot, delta| dot.opacity(delta),
    )
    .into_any_element()
}

struct SidebarRowSpec {
    folder: Option<SharedString>,
    snippet: Option<String>,
    depth: usize,
    workers_progress: Option<f32>,
    now_ms: f64,
}

pub(crate) fn now_millis() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as f64)
        .unwrap_or(0.0)
}

/// Lightweight signature of the project list and active selection shown in HistoryPanel.
#[derive(Clone, PartialEq, Eq, Debug)]
struct WorkspaceSignature {
    selected: Option<String>,
    items: Vec<(String, String, bool)>,
}

fn workspace_signature(selected: Option<&IdeWorkspace>, workspaces: &[IdeWorkspace]) -> WorkspaceSignature {
    WorkspaceSignature {
        selected: selected.map(|workspace| workspace.workspace_id.clone()),
        items: workspaces
            .iter()
            .map(|workspace| (workspace.workspace_id.clone(), workspace.title.clone(), workspace.pinned))
            .collect(),
    }
}

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
    /// Cached project metadata signature to avoid rebuilding on unrelated `Ide` notifications (dirty/tabs/diagnostics).
    workspace_sig: WorkspaceSignature,
    /// The query the Host last searched the messages for, and the chats it found with a line of each.
    content_query: String,
    content_hits: Vec<(String, String)>,
    _content_search: Option<Task<()>>,
    /// Chats chosen for a bulk action, and where a Shift-click range starts (the chat last clicked to choose).
    chosen: HashSet<String>,
    anchor: Option<String>,
    /// The chats in the order the list last drew them, for Shift-click ranges.
    row_order: RefCell<Vec<String>>,
    /// Takes the keys (Esc, Delete, Ctrl+A) while chats are chosen.
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

/// What a bulk action on the chosen chats takes, in the order the list shows them.
struct Chosen {
    ids: Vec<String>,
    titles: Vec<SharedString>,
    /// Every one is pinned (the action unpins), every one is archived (it restores).
    pinned: bool,
    archived: bool,
}

impl HistoryPanel {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "chat.searchHistory")).clean_on_escape());
        let ide = Ide::global(cx);
        let initial_sig = {
            let ide = ide.read(cx);
            workspace_signature(ide.workspace.as_ref(), &ide.workspaces)
        };
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
            // Observe `Ide` for project list mutations (rename, pin, non-active remove, reconnect) while filtering out
            // unrelated editor notifications via a lightweight workspace metadata signature.
            cx.observe(&ide, |this, ide, cx| {
                let ide = ide.read(cx);
                let next = workspace_signature(ide.workspace.as_ref(), &ide.workspaces);
                if this.workspace_sig != next {
                    this.workspace_sig = next;
                    cx.notify();
                }
            }),
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
            workspace_sig: initial_sig,
            content_query: String::new(),
            content_hits: Vec::new(),
            _content_search: None,
            chosen: HashSet::new(),
            anchor: None,
            row_order: RefCell::new(Vec::new()),
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Whether chats are chosen; then a click on a chat chooses it instead of opening it.
    fn choosing(&self) -> bool {
        !self.chosen.is_empty()
    }

    /// Forget the chosen chats.
    fn clear_choice(&mut self, cx: &mut Context<Self>) {
        if !self.chosen.is_empty() || self.anchor.is_some() {
            self.chosen.clear();
            self.anchor = None;
            cx.notify();
        }
    }

    /// Choose these chats in place of the chosen ones.
    fn choose(&mut self, ids: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.chosen = ids.into_iter().collect();
        self.anchor = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// A click on a chat: with Ctrl, or while chats are chosen, choose it or leave it; with Shift, also choose every
    /// chat listed from the one last clicked (else the open one) to it. False for a plain click, which opens the chat.
    fn click_to_choose(&mut self, id: &str, modifiers: Modifiers, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if modifiers.shift {
            let order = self.row_order.borrow().clone();
            let anchor = self.anchor.clone().or_else(|| self.store.read(cx).selected.clone()).filter(|anchor| order.contains(anchor));
            self.chosen.extend(shift_range(&order, anchor.as_deref(), id));
            self.anchor = anchor.or_else(|| Some(id.to_owned()));
        } else if modifiers.control || modifiers.platform || self.choosing() {
            if !self.chosen.remove(id) {
                self.chosen.insert(id.to_owned());
            }
            self.anchor = Some(id.to_owned());
        } else {
            return false;
        }
        self.focus.focus(window, cx);
        cx.notify();
        true
    }

    /// The chosen chats as a bulk action takes them, none when none is chosen.
    fn chosen_now(&self, cx: &App) -> Option<Chosen> {
        if self.chosen.is_empty() {
            return None;
        }
        let order = self.row_order.borrow();
        let place: HashMap<&str, usize> = order.iter().enumerate().map(|(index, id)| (id.as_str(), index)).collect();
        let store = self.store.read(cx);
        let mut chats: Vec<&SessionSummary> = store.summaries.iter().filter(|summary| self.chosen.contains(&summary.id)).collect();
        chats.sort_by_key(|summary| place.get(summary.id.as_str()).copied().unwrap_or(usize::MAX));
        (!chats.is_empty()).then(|| Chosen {
            ids: chats.iter().map(|summary| summary.id.clone()).collect(),
            titles: chats.iter().map(|summary| title_of(summary, cx)).collect(),
            pinned: chats.iter().all(|summary| summary.pinned),
            archived: chats.iter().all(|summary| summary.archived),
        })
    }

    /// Pin, unpin, archive, restore or delete every chosen chat (deleting asks first), then forget them.
    fn act_on_chosen(&mut self, action: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chosen) = self.chosen_now(cx) else { return };
        if action == "delete" {
            let panel = cx.entity().downgrade();
            let after = move |cx: &mut App| {
                let _ = panel.update(cx, |this, cx| this.clear_choice(cx));
            };
            confirm_bulk_delete(chosen.ids, chosen.titles, after, window, cx);
        } else {
            bulk(action, chosen.ids, window, cx);
            self.clear_choice(cx);
        }
    }

    /// Every chat a "Select all" takes: those a search found, else every chat not archived.
    fn all_listed(&self, searching: bool, cx: &App) -> Vec<String> {
        if searching {
            return self.row_order.borrow().clone();
        }
        self.store.read(cx).summaries.iter().filter(|summary| !summary.archived).map(|summary| summary.id.clone()).collect()
    }

    /// Esc forgets the chosen chats, Delete deletes them, Ctrl+A chooses every chat listed.
    fn choice_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.choosing() || !self.focus.is_focused(window) {
            return;
        }
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "escape" => self.clear_choice(cx),
            "delete" | "backspace" => self.act_on_chosen("delete", window, cx),
            "a" if keystroke.modifiers.control || keystroke.modifiers.platform => {
                let searching = !self.search.read(cx).value().trim().is_empty();
                let all = self.all_listed(searching, cx);
                self.choose(all, window, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// In place of the list's top while chats are chosen: how many, forget them, choose all, and pin, archive or
    /// delete them all. The sidebar has room for labels on a second line; the IDE tab has one line of icons.
    fn render_selection_bar(&self, chosen: &Chosen, all: Vec<String>, cx: &mut Context<Self>) -> AnyElement {
        let sidebar_border = cx.theme().sidebar_border;
        let count = chosen.ids.len();
        let cancel = Button::new("selection-cancel")
            .ghost()
            .xsmall()
            .icon(IconName::Close)
            .tooltip(tr(cx, "agent.clearSelection"))
            .on_click(cx.listener(|this, _, _, cx| this.clear_choice(cx)));
        let label = div().flex_1().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(trf(cx, "agent.selectedCount", &[("count", &count)]));
        let everything = all.iter().all(|id| self.chosen.contains(id));
        let select_all = Button::new("selection-all")
            .ghost()
            .xsmall()
            .icon(Icon::new(gpui_kit::assets::IconName::ListChecks))
            .tooltip(tr(cx, "agent.selectAll"))
            .disabled(everything)
            .on_click(cx.listener(move |this, _, window, cx| this.choose(all.clone(), window, cx)));
        let (pin_action, pin_icon, pin_label) =
            if chosen.pinned { ("unpin", gpui_kit::assets::IconName::PinOff, "chat.unpin") } else { ("pin", gpui_kit::assets::IconName::Pin, "chat.pin") };
        let (archive_action, archive_icon, archive_label) = if chosen.archived {
            ("unarchive", gpui_kit::assets::IconName::ArchiveRestore, "chat.unarchive")
        } else {
            ("archive", gpui_kit::assets::IconName::Archive, "chat.archive")
        };
        let actions = [
            ("selection-pin", pin_action, pin_icon, pin_label),
            ("selection-archive", archive_action, archive_icon, archive_label),
            ("selection-delete", "delete", gpui_kit::assets::IconName::Trash, "chat.delete"),
        ];
        if self.sidebar {
            let buttons = actions.map(|(id, action, icon, text)| {
                div().flex_1().min_w_0().child(
                    Button::new(id)
                        .outline()
                        .xsmall()
                        .w_full()
                        .icon(Icon::new(icon))
                        .label(tr(cx, text))
                        .on_click(cx.listener(move |this, _, window, cx| this.act_on_chosen(action, window, cx))),
                )
            });
            v_flex()
                .flex_shrink_0()
                .px_2()
                .pt_2()
                .pb_1()
                .gap_0p5()
                .child(h_flex().w_full().h(px(30.)).gap_1().items_center().child(cancel).child(label).child(select_all))
                .child(h_flex().w_full().h(px(30.)).gap_1().items_center().children(buttons))
                .into_any_element()
        } else {
            let buttons = actions.map(|(id, action, icon, text)| {
                Button::new(id)
                    .ghost()
                    .small()
                    .icon(Icon::new(icon))
                    .tooltip(tr(cx, text))
                    .on_click(cx.listener(move |this, _, window, cx| this.act_on_chosen(action, window, cx)))
            });
            h_flex()
                .h(px(40.))
                .flex_shrink_0()
                .px_2()
                .gap_1()
                .items_center()
                .border_b_1()
                .border_color(sidebar_border)
                .child(cancel)
                .child(label)
                .child(select_all)
                .children(buttons)
                .into_any_element()
        }
    }

    /// A chat's right-click menu: while it is chosen, the bulk actions on every chosen chat; else "Select" and its own
    /// actions.
    fn row_menu(&self, summary: &SessionSummary, actions: ChatActions, cx: &mut Context<Self>) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let panel = cx.entity().downgrade();
        let is_chosen = self.chosen.contains(&summary.id);
        let id = summary.id.clone();
        // What the chosen chats are is read when the menu opens, not for every row drawn.
        move |menu, _, cx| match is_chosen.then(|| panel.upgrade().and_then(|panel| panel.read(cx).chosen_now(cx))).flatten().map(Rc::new) {
            Some(chosen) => chosen_menu(menu, chosen, panel.clone(), cx),
            None => {
                let (panel, id) = (panel.clone(), id.clone());
                let menu = menu
                    .item(PopupMenuItem::new(tr(cx, "agent.select")).on_click(move |_, window, cx| {
                        let id = id.clone();
                        let _ = panel.update(cx, |this, cx| {
                            this.chosen.insert(id.clone());
                            this.anchor = Some(id);
                            this.focus.focus(window, cx);
                            cx.notify();
                        });
                    }))
                    .separator();
                actions.menu(menu, cx)
            }
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
        let (radius, list_active, list_hover, muted_foreground, warning, primary) = {
            let theme = cx.theme();
            (theme.radius, theme.list_active, theme.list_hover, theme.muted_foreground, theme.warning, theme.primary)
        };
        self.row_order.borrow_mut().push(summary.id.clone());
        let id = summary.id.clone();
        let active = self.store.read(cx).selected.as_deref() == Some(summary.id.as_str());
        let (choosing, chosen) = (self.choosing(), self.chosen.contains(&summary.id));
        let busy = matches!(summary.status.as_str(), "running" | "compacting");
        let waiting = summary.awaiting_approval == Some(true);
        let title = title_of(summary, cx);
        let actions = ChatActions::of(summary, title.clone());
        let menu = self.row_menu(summary, actions, cx);
        let spin_id = ElementId::Name(format!("chat-spin-{id}").into());
        let wait_id = ElementId::Name(format!("chat-wait-{id}").into());
        h_flex()
            .id(ElementId::Name(format!("chat-{id}").into()))
            .w_full()
            .h(px(30.))
            .px_2()
            .gap_2()
            .items_center()
            .rounded(radius)
            .text_sm()
            .cursor_pointer()
            .when(chosen, |this| this.bg(primary.opacity(0.14)))
            .when(active && !chosen, |this| this.bg(list_active))
            .when(!active && !chosen, |this| this.hover(move |style| style.bg(list_hover)))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if this.click_to_choose(&id, event.modifiers(), window, cx) {
                    return;
                }
                let id = id.clone();
                this.store.update(cx, |store, cx| store.select(Some(id), cx));
            }))
            .when(choosing, |this| this.child(check_box(chosen, cx)))
            .when(summary.pinned, |this| {
                this.child(Icon::new(gpui_kit::assets::IconName::Pin).xsmall().flex_shrink_0().text_color(muted_foreground))
            })
            // Found by a search among the archived ones.
            .when(searching && summary.archived, |this| {
                this.child(Icon::new(gpui_kit::assets::IconName::Archive).xsmall().flex_shrink_0().text_color(muted_foreground))
            })
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
            .when_some(external_badge(summary, cx), |this, badge| this.child(badge))
            .when_some(folder, |this, folder| {
                this.child(
                    div().max_w(px(96.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(muted_foreground).child(folder),
                )
            })
            .when(busy, |this| this.child(crate::ui::motion::spinner_with_id(spin_id, Size::XSmall, Some(muted_foreground), cx)))
            .when(waiting, |this| this.child(waiting_dot(wait_id, warning, cx.reduce_motion())))
            .context_menu(menu)
            .into_any_element()
    }

    /// A chat in the sidebar: a dot for a reply not seen yet, its title, a branch icon when it works in its own
    /// worktree, then how long ago it was active (a spinner while it works, a dot while it waits for an approval), and
    /// its menu button under the pointer; a search adds the line of a message that matched. Choosing a chat of another
    /// known project shows that project, so its files, the new chats and file links all belong to the chat. `depth`
    /// indents it (1 in a project, one more for a swarm worker under its coordinator); `workers` is whether the
    /// coordinator's workers listed under it are folded (with chevron rotation progress 0..1), and adds the arrow that folds them.
    fn render_sidebar_row(&self, summary: &SessionSummary, spec: SidebarRowSpec, cx: &mut Context<Self>) -> AnyElement {
        let SidebarRowSpec { folder, snippet, depth, workers_progress, now_ms } = spec;
        let (radius, list_active, list_hover, muted_foreground, foreground, warning, primary) = {
            let theme = cx.theme();
            (theme.radius, theme.list_active, theme.list_hover, theme.muted_foreground, theme.foreground, theme.warning, theme.primary)
        };
        self.row_order.borrow_mut().push(summary.id.clone());
        let id = summary.id.clone();
        let active = self.store.read(cx).selected.as_deref() == Some(summary.id.as_str());
        let (choosing, chosen) = (self.choosing(), self.chosen.contains(&summary.id));
        let busy = matches!(summary.status.as_str(), "running" | "compacting");
        let waiting = summary.awaiting_approval == Some(true);
        let unread = !active && summary.unread();
        let title = title_of(summary, cx);
        let actions = ChatActions::of(summary, title.clone());
        let row_menu = self.row_menu(summary, actions.clone(), cx);
        // While chats are chosen the row's menu is the bulk one, so the button that opens a single chat's menu goes.
        let menu_shown = !choosing && (self.hovered.as_deref() == Some(id.as_str()) || self.menu_open.as_deref() == Some(id.as_str()));
        let workspace = summary.workspace_id.clone();
        let (hover_id, open_id, menu_actions) = (id.clone(), id.clone(), actions.clone());
        let when = ago_from(now_ms, summary.updated_at, crate::ui::locale(cx) == crate::i18n::Locale::Zh);
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
            crate::ui::motion::spinner_with_id(ElementId::Name(format!("sidebar-spin-{id}").into()), Size::XSmall, Some(muted_foreground), cx)
        } else if waiting {
            waiting_dot(ElementId::Name(format!("sidebar-wait-{id}").into()), warning, cx.reduce_motion())
        } else {
            div().flex_shrink_0().text_xs().text_color(muted_foreground).child(when).into_any_element()
        };
        let line = h_flex()
            .w_full()
            .h(px(30.))
            .gap_1p5()
            .items_center()
            // The unread dot keeps its place, so titles line up whether or not it shows; while chats are chosen the
            // box that shows whether this one is takes it.
            .child(if choosing {
                check_box(chosen, cx)
            } else {
                div().w(px(6.)).h(px(6.)).flex_shrink_0().rounded_full().when(unread, |dot| dot.bg(primary)).into_any_element()
            })
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().when(unread, |title| title.font_weight(FontWeight::MEDIUM)).child(title))
            .when_some(external_badge(summary, cx), |this, badge| this.child(badge))
            .when_some(workers_progress, |this, progress| {
                let key = swarm_key(&summary.id);
                this.child(
                    div()
                        .id(ElementId::Name(format!("swarm-fold-{}", summary.id).into()))
                        .flex_shrink_0()
                        .rounded(radius)
                        .text_color(muted_foreground)
                        .hover(move |style| style.text_color(foreground))
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .xsmall()
                                .rotate(radians(progress * std::f32::consts::FRAC_PI_2)),
                        )
                        // Stop Left MouseDown here so the parent chat row does not register a click, and suppress
                        // window text selection; never stop MouseUp/on_click so TextSelectionLayer resets cleanly.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            GlobalState::suppress_text_selection(cx);
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.folded.remove(&key) {
                                this.folded.insert(key.clone());
                            }
                            cx.notify();
                        })),
                )
            })
            .when(summary.worktree.is_some(), |this| {
                this.child(Icon::new(gpui_kit::assets::IconName::GitBranch).xsmall().flex_shrink_0().text_color(muted_foreground))
            })
            .when_some(folder, |this, folder| {
                this.child(
                    div().max_w(px(88.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(muted_foreground).child(folder),
                )
            })
            .when(summary.archived, |this| {
                this.child(Icon::new(gpui_kit::assets::IconName::Archive).xsmall().flex_shrink_0().text_color(muted_foreground))
            })
            .child(right);
        v_flex()
            .id(ElementId::Name(format!("chat-{id}").into()))
            .w_full()
            .flex_shrink_0()
            .pl(px(4. + 12. * depth as f32))
            .pr_1()
            .rounded(radius)
            .text_sm()
            .cursor_pointer()
            .when(chosen, |this| this.bg(primary.opacity(0.14)))
            .when(active && !chosen, |this| this.bg(list_active))
            .when(!active && !chosen, |this| this.hover(move |style| style.bg(list_hover)))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let now = hovered.then(|| hover_id.clone());
                if *hovered || this.hovered.as_deref() == Some(hover_id.as_str()) {
                    this.hovered = now;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if this.click_to_choose(&id, event.modifiers(), window, cx) {
                    return;
                }
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
                this.child(div().pl(px(12.)).pb_1().text_xs().text_color(muted_foreground).overflow_hidden().text_ellipsis().whitespace_nowrap().child(snippet))
            })
            .context_menu(row_menu)
            .into_any_element()
    }

    /// The header of a sidebar group; a click folds or unfolds it.
    fn render_group_header(&self, key: &str, title: SharedString, chevron_progress: f32, cx: &mut Context<Self>) -> AnyElement {
        let (muted_foreground, foreground) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.foreground)
        };
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
            .text_color(muted_foreground)
            .cursor_pointer()
            .hover(move |style| style.text_color(foreground))
            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
            .child(
                Icon::new(IconName::ChevronRight)
                    .xsmall()
                    .flex_shrink_0()
                    .rotate(radians(chevron_progress * std::f32::consts::FRAC_PI_2)),
            )
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
        let (radius, list_hover, muted_foreground) = {
            let theme = cx.theme();
            (theme.radius, theme.list_hover, theme.muted_foreground)
        };
        h_flex()
            .id(id)
            .w_full()
            .h(px(30.))
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .items_center()
            .rounded(radius)
            .text_sm()
            .cursor_pointer()
            .hover(move |style| style.bg(list_hover))
            .child(icon.into().small().text_color(muted_foreground))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
            .child(div().flex_shrink_0().text_xs().text_color(muted_foreground).child(keys))
    }

    /// The Agent sidebar above the list: new chat and search (or its field).
    fn render_sidebar_top(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let muted_foreground = cx.theme().muted_foreground;
        let new_chat = self.nav_row("sidebar-new", Icon::new(gpui_kit::assets::IconName::SquarePen), tr(cx, "agent.newChat"), "Ctrl+N", cx)
            .on_click(|_, window, cx| window.dispatch_action(Box::new(NewChat), cx));
        let search: AnyElement = if self.searching {
            div()
                .w_full()
                .py_0p5()
                .child(Input::new(&self.search).small().cleanable(true).prefix(Icon::new(IconName::Search).small().text_color(muted_foreground)))
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
                Button::new("sidebar-manage-projects")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(gpui_kit::assets::IconName::FolderCog))
                    .tooltip(tr(cx, "agent.manageProjects"))
                    .on_click(|_, window, cx| crate::ui::manage::open_projects(window, cx)),
            )
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
        let (radius, list_hover, muted_foreground) = {
            let theme = cx.theme();
            (theme.radius, theme.list_hover, theme.muted_foreground)
        };
        let key = format!("project:{}", group.id);
        let folded = self.folded.contains(&group.id);
        let current = Ide::global(cx).read(cx).workspace.as_ref().is_some_and(|shown| shown.workspace_id == group.id);
        let menu_shown = self.hovered.as_deref() == Some(key.as_str()) || self.menu_open.as_deref() == Some(key.as_str());
        let (fold_id, hover_key, open_key) = (group.id.clone(), key.clone(), key.clone());
        let chats: Vec<String> = group.chats.iter().map(|chat| chat.id.clone()).collect();
        let workspace = group.workspace.clone();
        let (panel, group_key) = (cx.entity().downgrade(), group.id.clone());
        h_flex()
            .id(ElementId::Name(format!("project-{}", group.id).into()))
            .w_full()
            .h(px(30.))
            .flex_shrink_0()
            .pl_2()
            .pr_1()
            .gap_1p5()
            .items_center()
            .rounded(radius)
            .text_sm()
            .cursor_pointer()
            .hover(move |style| style.bg(list_hover))
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
            .child(Icon::new(if folded { IconName::Folder } else { IconName::FolderOpen }).small().flex_shrink_0().text_color(muted_foreground))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().when(current, |title| title.font_weight(FontWeight::MEDIUM)).child(group.title.clone()))
            .when(group.pinned && !menu_shown, |this| this.child(Icon::new(gpui_kit::assets::IconName::Pin).xsmall().flex_shrink_0().text_color(muted_foreground)))
            .when(menu_shown, |this| {
                let new_in = workspace.clone();
                this.when_some(new_in, |this, new_in| {
                    this.child(
                        // Block Left MouseDown from reaching the project row (so clicking New Chat does not fold it)
                        // without stopping MouseUp/on_click so TextSelectionLayer always resets.
                        div()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                GlobalState::suppress_text_selection(cx);
                                cx.stop_propagation();
                            })
                            .child(
                                Button::new(ElementId::Name(format!("project-new-{key}").into()))
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(gpui_kit::assets::IconName::SquarePen))
                                    .tooltip(tr(cx, "agent.newChatInProject"))
                                    .on_click(move |_, window, cx| {
                                        new_chat_in(new_in.clone(), window, cx);
                                    }),
                            ),
                    )
                })
                .child(
                    div()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            GlobalState::suppress_text_selection(cx);
                            cx.stop_propagation();
                        })
                        .child(
                            Button::new(ElementId::Name(format!("project-menu-{key}").into()))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Ellipsis)
                                .tooltip(tr(cx, "agent.projectActions"))
                                .dropdown_menu(move |menu, _, cx| project_menu(menu, workspace.clone(), chats.clone(), (panel.clone(), group_key.clone()), cx))
                                .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
                                    this.menu_open = open.then(|| open_key.clone());
                                    cx.notify();
                                })),
                        ),
                )
            })
            .into_any_element()
    }

    /// The Agent sidebar's list: search results (titles and folders, then messages the Host found), or the pinned
    /// chats, each project with its chats, and the chats outside any project.
    fn render_sidebar_list(&self, summaries: &[SessionSummary], words: &[String], now_ms: f64, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let workspaces = Ide::global(cx).read(cx).workspaces.clone();
        let titles: HashMap<&str, &str> = workspaces.iter().map(|workspace| (workspace.workspace_id.as_str(), workspace.title.as_str())).collect();
        let untitled = tr(cx, "chat.newChat");
        let mut rows = Vec::new();
        if !words.is_empty() {
            let mut shown: HashSet<&str> = HashSet::new();
            let by_id: HashMap<&str, &SessionSummary> = summaries.iter().map(|summary| (summary.id.as_str(), summary)).collect();
            for summary in sorted(summaries.iter().filter(|summary| found(summary, &untitled, words))) {
                shown.insert(summary.id.as_str());
                rows.push(self.render_sidebar_row(
                    summary,
                    SidebarRowSpec { folder: Some(folder_name(summary, &titles)), snippet: None, depth: 0, workers_progress: None, now_ms },
                    cx,
                ));
            }
            let hits: Vec<(&SessionSummary, &str)> = self
                .content_hits
                .iter()
                .filter(|(id, _)| !shown.contains(id.as_str()))
                .filter_map(|(id, snippet)| by_id.get(id.as_str()).map(|summary| (*summary, snippet.as_str())))
                .collect();
            if !hits.is_empty() {
                rows.push(self.render_label(tr(cx, "agent.foundInMessages"), cx));
                for (summary, snippet) in hits {
                    rows.push(self.render_sidebar_row(
                        summary,
                        SidebarRowSpec { folder: Some(folder_name(summary, &titles)), snippet: Some(snippet.to_owned()), depth: 0, workers_progress: None, now_ms },
                        cx,
                    ));
                }
            }
            return rows;
        }
        let visible = newest_first(summaries.iter().filter(|summary| !summary.archived));
        let tree = sidebar_tree(&visible, &workspaces);
        let reveal_policy = Transition::new(Duration::from_millis(160));
        if !tree.pinned.is_empty() {
            let open = !self.folded.contains("pinned");
            let presence = Presence::new("history-group-pinned", open).transition(reveal_policy.clone()).sample(window, cx);
            rows.push(self.render_group_header("pinned", tr(cx, "agent.pinned"), presence.progress, cx));
            if presence.should_render() {
                let items: Vec<AnyElement> = tree
                    .pinned
                    .iter()
                    .map(|summary| {
                        let folder = summary.workspace_id.is_some().then(|| folder_name(summary, &titles));
                        self.render_sidebar_row(summary, SidebarRowSpec { folder, snippet: None, depth: 0, workers_progress: None, now_ms }, cx)
                    })
                    .collect();
                let group_body = v_flex().w_full().gap_px().opacity(presence.progress).children(items).into_any_element();
                rows.push(MotionReveal::new("history-reveal-pinned", presence.progress, group_body).into_any_element());
            }
        }
        rows.push(self.render_projects_header(cx));
        for group in &tree.projects {
            rows.push(self.render_project_row(group, cx));
            let open = !self.folded.contains(&group.id);
            let presence = Presence::new(SharedString::from(format!("history-group-proj-{}", group.id)), open)
                .transition(reveal_policy.clone())
                .sample(window, cx);
            if presence.should_render() && !group.chats.is_empty() {
                let items = self.render_limited(&group.id, &group.chats, true, now_ms, window, cx);
                let group_body = v_flex().w_full().gap_px().opacity(presence.progress).children(items).into_any_element();
                rows.push(
                    MotionReveal::new(ElementId::Name(format!("history-reveal-proj-{}", group.id).into()), presence.progress, group_body).into_any_element(),
                );
            }
        }
        if !tree.loose.is_empty() {
            let open = !self.folded.contains("none");
            let presence = Presence::new("history-group-none", open).transition(reveal_policy).sample(window, cx);
            rows.push(self.render_group_header("none", tr(cx, "agent.chats"), presence.progress, cx));
            if presence.should_render() {
                let items = self.render_limited("none", &tree.loose, false, now_ms, window, cx);
                let group_body = v_flex().w_full().gap_px().opacity(presence.progress).children(items).into_any_element();
                rows.push(MotionReveal::new("history-reveal-none", presence.progress, group_body).into_any_element());
            }
        }
        rows
    }

    /// A small heading inside the list.
    fn render_label(&self, text: SharedString, cx: &mut Context<Self>) -> AnyElement {
        div().w_full().flex_shrink_0().px_2().pt_3().pb_1().text_xs().text_color(cx.theme().muted_foreground).child(text).into_any_element()
    }

    /// A group's chats up to [`GROUP_LIMIT`], each swarm coordinator with its workers under it (they do not count
    /// towards the limit), then the entry that shows all of them or fewer again.
    fn render_limited(&self, key: &str, chats: &[&SessionSummary], nested: bool, now_ms: f64, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let total = chats.len();
        let entries = nest_workers(chats);
        let chats = &entries;
        let depth = usize::from(nested);
        let all = self.unlimited.contains(key);
        let shown = if all { chats.len() } else { chats.len().min(GROUP_LIMIT) };
        let mut rows: Vec<AnyElement> = Vec::new();
        for (summary, workers) in &chats[..shown] {
            if workers.is_empty() {
                rows.push(self.render_sidebar_row(summary, SidebarRowSpec { folder: None, snippet: None, depth, workers_progress: None, now_ms }, cx));
            } else {
                let open = !self.folded.contains(&swarm_key(&summary.id));
                let presence = Presence::new(SharedString::from(format!("history-swarm-{}", summary.id)), open)
                    .transition(Transition::new(Duration::from_millis(160)))
                    .sample(window, cx);
                rows.push(self.render_sidebar_row(summary, SidebarRowSpec { folder: None, snippet: None, depth, workers_progress: Some(presence.progress), now_ms }, cx));
                if presence.should_render() {
                    let worker_rows: Vec<AnyElement> = workers
                        .iter()
                        .map(|worker| {
                            self.render_sidebar_row(worker, SidebarRowSpec { folder: None, snippet: None, depth: depth + 1, workers_progress: None, now_ms }, cx)
                        })
                        .collect();
                    let body = v_flex().w_full().gap_px().opacity(presence.progress).children(worker_rows).into_any_element();
                    rows.push(
                        MotionReveal::new(ElementId::Name(format!("history-swarm-reveal-{}", summary.id).into()), presence.progress, body).into_any_element(),
                    );
                }
            }
        }
        if chats.len() > GROUP_LIMIT {
            let key = key.to_owned();
            let label = if all { tr(cx, "agent.showFewer") } else { trf(cx, "agent.showAll", &[("count", &total)]) };
            let (muted_foreground, foreground) = {
                let theme = cx.theme();
                (theme.muted_foreground, theme.foreground)
            };
            rows.push(
                div()
                    .id(ElementId::Name(format!("group-more-{key}").into()))
                    .w_full()
                    .when(nested, |this| this.pl(px(30.)))
                    .when(!nested, |this| this.pl(px(16.)))
                    .py_1()
                    .text_xs()
                    .text_color(muted_foreground)
                    .cursor_pointer()
                    .hover(move |style| style.text_color(foreground))
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

/// Subtle pill badge identifying chats driven by an external CLI agent (`Codex`, `AGY`, `ACP`).
fn external_badge(summary: &SessionSummary, cx: &App) -> Option<AnyElement> {
    let backend = summary.external_backend.as_ref()?;
    let label: SharedString = match backend.kind.as_str() {
        "codex" => tr(cx, "external.badgeCodex"),
        "antigravity" => tr(cx, "external.badgeAgy"),
        _ => backend
            .label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| SharedString::from(s.chars().take(10).collect::<String>()))
            .unwrap_or_else(|| tr(cx, "external.badgeAcp")),
    };
    let theme = cx.theme();
    Some(
        div()
            .flex_shrink_0()
            .px_1()
            .py(px(1.))
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.65))
            .text_size(px(10.))
            .line_height(px(12.))
            .text_color(theme.muted_foreground)
            .child(label)
            .into_any_element(),
    )
}

/// Show a chat, and its project when that is another known one (as a click on its row does).
pub fn show_chat(id: String, window: &mut Window, cx: &mut App) {
    let store = Store::global(cx);
    let workspace =
        store.read(cx).summaries.iter().find(|summary| summary.id == id).and_then(|summary| summary.workspace_id.clone());
    store.update(cx, |store, cx| store.select(Some(id), cx));
    let ide = Ide::global(cx);
    let other = ide.read(cx).workspaces.iter().find(|candidate| {
        workspace.as_deref() == Some(candidate.workspace_id.as_str())
            && ide.read(cx).workspace.as_ref().is_none_or(|shown| shown.workspace_id != candidate.workspace_id)
    });
    if let Some(other) = other.cloned() {
        ide.update(cx, |ide, cx| ide.select(other, window, cx));
    }
}

/// Start a new chat in a project: show it, without bringing back its last chat, and empty the conversation.
fn new_chat_in(workspace: IdeWorkspace, window: &mut Window, cx: &mut App) {
    Ide::global(cx).update(cx, |ide, cx| ide.select_for_new_chat(workspace, window, cx));
    window.dispatch_action(Box::new(NewChat), cx);
}

/// A project's menu: a new chat, rename, pin, add a folder, choose or archive its chats, remove it from the list. A
/// project that is no longer in the list only offers to choose or archive its chats. `panel` is the sidebar and the
/// project's key there (choosing its chats unfolds it and shows all of them).
fn project_menu(menu: PopupMenu, workspace: Option<IdeWorkspace>, chats: Vec<String>, panel: (WeakEntity<HistoryPanel>, String), cx: &App) -> PopupMenu {
    let chosen = chats.clone();
    let select = PopupMenuItem::new(tr(cx, "agent.selectProjectChats")).disabled(chats.is_empty()).on_click(move |_, window, cx| {
        let (key, ids) = (panel.1.clone(), chosen.clone());
        let _ = panel.0.update(cx, |this, cx| {
            this.folded.remove(&key);
            this.unlimited.insert(key);
            this.choose(ids, window, cx);
        });
    });
    let archive = PopupMenuItem::new(tr(cx, "agent.archiveProjectChats")).disabled(chats.is_empty()).on_click(move |_, window, cx| {
        bulk("archive", chats.clone(), window, cx);
    });
    let Some(workspace) = workspace else { return menu.item(select).item(archive) };
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
        .item(select)
        .item(archive)
        .separator()
        .item(PopupMenuItem::new(tr(cx, "agent.removeProject")).on_click(move |_, window, cx| {
            crate::ui::agent::confirm_remove(Ide::global(cx), remove.workspace_id.clone(), remove.title.clone(), window, cx);
        }))
}

/// How long ago, as briefly as the sidebar has room for: `刚刚`, `5分钟`, `3小时`, `2天`; `now`, `5m`, `3h`, `2d`.
pub(crate) fn ago_from(now_ms: f64, updated_ms: f64, zh: bool) -> String {
    let locale = if zh { crate::i18n::Locale::Zh } else { crate::i18n::Locale::En };
    let minutes = ((now_ms - updated_ms) / 60_000.0).max(0.0) as u64;
    let (hours, days) = (minutes / 60, minutes / (60 * 24));
    let (number, key) = match minutes {
        0 => return crate::i18n::text(locale, "native.timeJustNow").to_owned(),
        1..60 => (minutes, "native.timeMinutesAgo"),
        _ if hours < 24 => (hours, "native.timeHoursAgo"),
        _ if days < 7 => (days, "native.timeDaysAgo"),
        _ if days < 30 => (days / 7, "native.timeWeeksAgo"),
        _ if days < 365 => (days / 30, "native.timeMonthsAgo"),
        _ => (days / 365, "native.timeYearsAgo"),
    };
    crate::i18n::format(locale, key, &[("count", &number)])
}

/// The folding key of a swarm coordinator's workers in the sidebar.
fn swarm_key(coordinator: &str) -> String {
    format!("swarm:{coordinator}")
}

/// A group's chats in their order, each with the swarm workers it coordinates, oldest first, taken from their own
/// places. A worker whose coordinator is not in the group (pinned, archived, deleted or in another project) keeps its
/// place.
fn nest_workers<'a>(chats: &[&'a SessionSummary]) -> Vec<(&'a SessionSummary, Vec<&'a SessionSummary>)> {
    let leads: HashSet<&str> = chats.iter().filter(|chat| chat.swarm.as_ref().is_none_or(|link| link.coordinator == chat.id)).map(|chat| chat.id.as_str()).collect();
    let lead_of = |chat: &'a SessionSummary| chat.swarm.as_ref().map(|link| link.coordinator.as_str()).filter(|lead| *lead != chat.id && leads.contains(*lead));
    let mut entries: Vec<(&'a SessionSummary, Vec<&'a SessionSummary>)> = Vec::with_capacity(chats.len());
    let mut index_by_id: HashMap<&'a str, usize> = HashMap::with_capacity(chats.len());
    for &chat in chats {
        if lead_of(chat).is_none() {
            index_by_id.insert(chat.id.as_str(), entries.len());
            entries.push((chat, Vec::new()));
        }
    }
    for &chat in chats {
        if let Some(lead) = lead_of(chat)
            && let Some(&slot) = index_by_id.get(lead)
        {
            entries[slot].1.push(chat);
        }
    }
    for (_, workers) in &mut entries {
        workers.sort_by(|a, b| a.created_at.total_cmp(&b.created_at));
    }
    entries
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
    let mut projects: Vec<ProjectGroup<'a>> = Vec::with_capacity(workspaces.len());
    let mut project_index: HashMap<String, usize> = HashMap::with_capacity(workspaces.len());
    for workspace in workspaces {
        project_index.insert(workspace.workspace_id.clone(), projects.len());
        projects.push(ProjectGroup {
            id: workspace.workspace_id.clone(),
            title: workspace.title.clone(),
            pinned: workspace.pinned,
            workspace: Some(workspace.clone()),
            chats: Vec::new(),
        });
    }
    let (mut pinned, mut loose) = (Vec::new(), Vec::new());
    let mut latest: HashMap<&str, f64> = HashMap::new();
    for &chat in chats {
        if let Some(id) = chat.workspace_id.as_deref() {
            let newest = latest.entry(id).or_insert(chat.updated_at);
            *newest = newest.max(chat.updated_at);
        }
        if chat.pinned {
            pinned.push(chat);
            continue;
        }
        match chat.workspace_id.as_deref() {
            None => loose.push(chat),
            Some(id) => match project_index.get(id).copied() {
                Some(slot) => projects[slot].chats.push(chat),
                None => {
                    project_index.insert(id.to_owned(), projects.len());
                    projects.push(ProjectGroup {
                        id: id.to_owned(),
                        title: chat.cwd.split(['/', '\\']).rfind(|part| !part.is_empty()).unwrap_or(&chat.cwd).to_owned(),
                        pinned: false,
                        workspace: None,
                        chats: vec![chat],
                    });
                }
            },
        }
    }
    projects.sort_by(|a, b| {
        let newest = |group: &ProjectGroup<'_>| latest.get(group.id.as_str()).copied().unwrap_or(0.0);
        b.pinned.cmp(&a.pinned).then(newest(b).total_cmp(&newest(a)))
    });
    SidebarTree { pinned, projects, loose }
}

/// A chat's title, or what an untitled one is called.
pub(crate) fn title_of(summary: &SessionSummary, cx: &App) -> SharedString {
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

/// The chats a Shift-click on `id` chooses: every one listed from `anchor` to it, either way round, or only it when
/// there is no anchor in the list.
fn shift_range(order: &[String], anchor: Option<&str>, id: &str) -> Vec<String> {
    let from = anchor.and_then(|anchor| order.iter().position(|row| row == anchor));
    match (from, order.iter().position(|row| row == id)) {
        (Some(from), Some(to)) => order[from.min(to)..=from.max(to)].to_vec(),
        _ => vec![id.to_owned()],
    }
}

/// The right-click menu of a chosen chat: pin or unpin, archive or restore, or delete every chosen chat, or forget
/// them.
fn chosen_menu(menu: PopupMenu, chosen: Rc<Chosen>, panel: WeakEntity<HistoryPanel>, cx: &App) -> PopupMenu {
    let count = chosen.ids.len();
    let item = |label: SharedString, action: &'static str| {
        let panel = panel.clone();
        PopupMenuItem::new(label).on_click(move |_, window, cx| {
            let _ = panel.update(cx, |this, cx| this.act_on_chosen(action, window, cx));
        })
    };
    let pin = if chosen.pinned { item(tr(cx, "chat.unpin"), "unpin") } else { item(tr(cx, "chat.pin"), "pin") };
    let archive = if chosen.archived { item(tr(cx, "chat.unarchive"), "unarchive") } else { item(tr(cx, "chat.archive"), "archive") };
    let forget = panel.clone();
    menu.label(trf(cx, "agent.selectedCount", &[("count", &count)]))
        .item(pin)
        .item(archive)
        .separator()
        .item(item(tr(cx, "chat.delete"), "delete"))
        .separator()
        .item(PopupMenuItem::new(tr(cx, "agent.clearSelection")).on_click(move |_, _, cx| {
            let _ = forget.update(cx, |this, cx| this.clear_choice(cx));
        }))
}

/// Chats newest first, pinned ones before the others.
fn sorted<'a>(chats: impl Iterator<Item = &'a SessionSummary>) -> Vec<&'a SessionSummary> {
    let mut list = newest_first(chats);
    list.sort_by_key(|chat| std::cmp::Reverse(chat.pinned));
    list
}

pub(crate) fn newest_first<'a>(chats: impl Iterator<Item = &'a SessionSummary>) -> Vec<&'a SessionSummary> {
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
pub(crate) fn folder_name(chat: &SessionSummary, titles: &HashMap<&str, &str>) -> SharedString {
    if let Some(title) = chat.workspace_id.as_deref().and_then(|id| titles.get(id)) {
        return (*title).to_owned().into();
    }
    chat.cwd.split(['/', '\\']).rfind(|part| !part.is_empty()).unwrap_or(&chat.cwd).to_owned().into()
}

/// The folder a chat of another project ran in, named beside its title.
fn folder_of(chat: &SessionSummary, workspace_id: Option<&str>) -> Option<SharedString> {
    let other = chat.workspace_id.is_some() && chat.workspace_id.as_deref() != workspace_id;
    other.then(|| chat.cwd.split(['/', '\\']).rfind(|part| !part.is_empty()).unwrap_or(&chat.cwd).to_owned().into())
}

/// Whether a search finds a chat: every word of the query in its title or its folder, ignoring case.
pub(crate) fn found(chat: &SessionSummary, untitled: &str, words: &[String]) -> bool {
    let title = if chat.title.is_empty() { untitled.to_lowercase() } else { chat.title.to_lowercase() };
    let folder = chat.cwd.to_lowercase();
    words.iter().all(|word| title.contains(word.as_str()) || folder.contains(word.as_str()))
}

impl Render for HistoryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let words: Vec<String> = query.split_whitespace().map(str::to_owned).collect();
        let summaries = self.store.read(cx).summaries.clone();
        let archived_count = summaries.iter().filter(|summary| summary.archived).count();
        let searching = !words.is_empty();
        if searching {
            self.searching = true;
        }
        // A chosen chat that is gone (deleted, or by another window) is no longer chosen.
        if self.choosing() || self.anchor.is_some() {
            let known: HashSet<&str> = summaries.iter().map(|summary| summary.id.as_str()).collect();
            self.chosen.retain(|id| known.contains(id.as_str()));
            if self.anchor.as_deref().is_some_and(|anchor| !known.contains(anchor)) {
                self.anchor = None;
            }
        }
        self.row_order.borrow_mut().clear();
        let now_ms = now_millis();
        let rows = if self.sidebar {
            self.render_sidebar_list(&summaries, &words, now_ms, window, cx)
        } else {
            self.render_side_list(&summaries, &words, cx)
        };
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
        let (sidebar_bg, muted_foreground, sidebar_border) = {
            let theme = cx.theme();
            (theme.sidebar, theme.muted_foreground, theme.sidebar_border)
        };
        let top = match self.chosen_now(cx) {
            Some(chosen) => {
                let all = self.all_listed(searching, cx);
                self.render_selection_bar(&chosen, all, cx)
            }
            None if self.sidebar => self.render_sidebar_top(cx).into_any_element(),
            None => self.render_side_header(cx).into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(sidebar_bg)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::choice_key))
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
                        this.child(div().p_3().text_sm().text_color(muted_foreground).child(tr(cx, if searching { "chat.noResults" } else { "chat.noChats" })))
                    }),
            )
            .when_some(footer, |this, footer| {
                this.child(div().flex_shrink_0().p_1().when(self.sidebar, |this| this.px_2()).border_t_1().border_color(sidebar_border).child(footer))
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
    fn shift_click_takes_every_chat_listed_between_the_anchor_and_it() {
        let order: Vec<String> = ["a", "b", "c", "d", "e"].into_iter().map(str::to_owned).collect();
        assert_eq!(shift_range(&order, Some("b"), "d"), ["b", "c", "d"]);
        assert_eq!(shift_range(&order, Some("d"), "a"), ["a", "b", "c", "d"]);
        assert_eq!(shift_range(&order, Some("c"), "c"), ["c"]);
        assert_eq!(shift_range(&order, None, "e"), ["e"]);
        assert_eq!(shift_range(&order, Some("gone"), "b"), ["b"]);
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

    #[test]
    fn swarm_workers_follow_their_coordinator() {
        let member = |id: &str, updated: f64, created: f64, lead: &str| {
            let mut summary = chat(id, None, "C:\\home", updated, false);
            summary.created_at = created;
            summary.swarm = Some(rainy_protocol::SwarmLink { coordinator: lead.into(), name: if id == lead { lead.into() } else { id.into() } });
            summary
        };
        // Newest first: worker w2 works, the coordinator waits, w1 is quiet; o's coordinator is not in the list.
        let list = [member("w2", 9., 3., "lead"), chat("x", None, "C:\\home", 8., false), member("lead", 7., 1., "lead"), member("w1", 6., 2., "lead"), member("o", 5., 4., "gone")];
        let chats: Vec<&SessionSummary> = list.iter().collect();
        let nested = nest_workers(&chats);
        let shape: Vec<(&str, Vec<&str>)> = nested.iter().map(|(chat, workers)| (chat.id.as_str(), workers.iter().map(|worker| worker.id.as_str()).collect())).collect();
        assert_eq!(shape, [("x", vec![]), ("lead", vec!["w1", "w2"]), ("o", vec![])]);
    }

    #[test]
    fn workspace_signature_tracks_rename_pin_remove_and_selection() {
        let mut a = project("a", false);
        let b = project("b", false);
        let base = workspace_signature(Some(&a), &[a.clone(), b.clone()]);
        assert_eq!(base, workspace_signature(Some(&a), &[a.clone(), b.clone()]));
        // Renaming a project changes the signature.
        a.title = "Alpha Renamed".into();
        let renamed = workspace_signature(Some(&a), &[a.clone(), b.clone()]);
        assert_ne!(base, renamed);
        // Pinning a project changes the signature.
        let mut b_pinned = b.clone();
        b_pinned.pinned = true;
        assert_ne!(renamed, workspace_signature(Some(&a), &[a.clone(), b_pinned]));
        // Removing a non-active project changes the signature.
        assert_ne!(renamed, workspace_signature(Some(&a), std::slice::from_ref(&a)));
        // Changing the active project changes the signature.
        let both = [a, b];
        assert_ne!(renamed, workspace_signature(Some(&both[1]), &both));
    }
}
