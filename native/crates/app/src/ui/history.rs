//! The chat history list (`History.tsx`): a search field, then the chats grouped as pinned, the current project, other
//! projects and no project, most recently updated first. Archived chats are listed on their own, behind the button at
//! the bottom; a search looks through all of them.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use serde_json::json;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::SessionSummary;

use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};
use crate::ui::actions::run;
use crate::ui::prompt::prompt_text;
use crate::ui::{tr, trf};

pub struct HistoryPanel {
    store: Entity<Store>,
    search: Entity<InputState>,
    /// Whether the list shows the archived chats instead of the others.
    show_archived: bool,
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
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            // The chats of the project shown come first.
            cx.subscribe(&Ide::global(cx), |_, _, event: &IdeEvent, cx| {
                if matches!(event, IdeEvent::WorkspaceChanged) {
                    cx.notify();
                }
            }),
        ];
        Self { store, search, show_archived: false, _subscriptions: subscriptions }
    }

    /// Put the keyboard in the search field (Ctrl+Alt+K).
    pub fn focus_search(&self, window: &mut Window, cx: &mut App) {
        self.search.update(cx, |state, cx| state.focus(window, cx));
    }

    fn render_row(&self, summary: &SessionSummary, folder: Option<SharedString>, searching: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let id = summary.id.clone();
        let active = self.store.read(cx).selected.as_deref() == Some(summary.id.as_str());
        let busy = matches!(summary.status.as_str(), "running" | "compacting");
        let waiting = summary.awaiting_approval == Some(true);
        let title: SharedString = if summary.title.is_empty() { tr(cx, "chat.newChat") } else { summary.title.clone().into() };
        let (pinned, archived) = (summary.pinned, summary.archived);
        let menu_id = summary.id.clone();
        let menu_title = title.clone();
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
            .when(searching && archived, |this| {
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
            .context_menu(move |menu, _, cx| {
                let (id, title) = (menu_id.clone(), menu_title.clone());
                let rename = (id.clone(), title.clone());
                let delete = (id.clone(), title.clone());
                let pin = id.clone();
                let archive = id.clone();
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
            })
            .into_any_element()
    }
}

/// Chats newest first, pinned ones before the others.
fn sorted<'a>(chats: impl Iterator<Item = &'a SessionSummary>) -> Vec<&'a SessionSummary> {
    let mut list: Vec<&SessionSummary> = chats.collect();
    list.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.updated_at.total_cmp(&a.updated_at)));
    list
}

/// Which group of the list a chat is in (3.x `groupChats`).
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
        let workspace_id = Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        let workspace_id = workspace_id.as_deref();
        let summaries = self.store.read(cx).summaries.clone();
        let archived_count = summaries.iter().filter(|summary| summary.archived).count();
        let untitled = tr(cx, "chat.newChat");
        let searching = !words.is_empty();
        let mut rows: Vec<AnyElement> = Vec::new();
        if searching {
            for summary in sorted(summaries.iter().filter(|summary| found(summary, &untitled, &words))) {
                rows.push(self.render_row(summary, folder_of(summary, workspace_id), true, cx));
            }
        } else {
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
        }
        let empty = rows.is_empty();
        let footer = (!searching && (archived_count > 0 || self.show_archived)).then(|| {
            let label: SharedString =
                if self.show_archived { tr(cx, "chat.hideArchived") } else { format!("{} ({archived_count})", tr(cx, "chat.showArchived")).into() };
            Button::new("history-archived")
                .ghost()
                .small()
                .w_full()
                .icon(Icon::new(gpui_kit::assets::IconName::Archive))
                .label(label)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_archived = !this.show_archived;
                    cx.notify();
                }))
        });
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(
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
                                .prefix(Icon::new(gpui_kit::component::IconName::Search).small().text_color(theme.muted_foreground)),
                        ),
                    )
                    .child(
                        Button::new("history-new")
                            .ghost()
                            .small()
                            .icon(Icon::new(gpui_kit::assets::IconName::SquarePen))
                            .tooltip(tr(cx, "chat.newChat"))
                            .on_click(cx.listener(|this, _, _, cx| this.store.update(cx, |store, cx| store.select(None, cx)))),
                    ),
            )
            .child(
                v_flex()
                    .id("history-list")
                    .flex_1()
                    .min_h_0()
                    .p_1()
                    .gap_px()
                    .overflow_y_scroll()
                    .children(rows)
                    .when(empty, |this| {
                        this.child(div().p_3().text_sm().text_color(theme.muted_foreground).child(tr(cx, if searching { "chat.noResults" } else { "chat.noChats" })))
                    }),
            )
            .when_some(footer, |this, footer| {
                this.child(div().flex_shrink_0().p_1().border_t_1().border_color(theme.sidebar_border).child(footer))
            })
    }
}

/// Ask for a new title and rename the chat.
fn open_rename(id: String, title: SharedString, window: &mut Window, cx: &mut App) {
    prompt_text(tr(cx, "chat.rename"), title.to_string(), tr(cx, "chat.save"), move |title, window, cx| {
        run("sessions.rename", json!({ "sessionId": id, "title": title }), window.window_handle(), cx);
    }, window, cx);
}

/// Confirm, then delete the chat.
fn confirm_delete(id: String, title: SharedString, window: &mut Window, cx: &mut App) {
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
