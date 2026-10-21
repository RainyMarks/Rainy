//! The chat history list (`History.tsx`): pinned chats first, then the most recently updated.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use serde_json::json;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::StyledExt as _;
use rainy_protocol::SessionSummary;

use crate::store::{Store, StoreEvent};
use crate::ui::actions::run;
use crate::ui::prompt::prompt_text;
use crate::ui::{tr, trf};

pub struct HistoryPanel {
    store: Entity<Store>,
    _subscriptions: Vec<Subscription>,
}

impl HistoryPanel {
    pub fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.subscribe(&store, |_, _, event: &StoreEvent, cx| {
            if matches!(event, StoreEvent::SummariesChanged | StoreEvent::SelectionChanged | StoreEvent::PrefsChanged) {
                cx.notify();
            }
        })];
        Self { store, _subscriptions: subscriptions }
    }
}

/// Chats as the list orders them: pinned first, archived left out.
pub fn ordered(summaries: &[SessionSummary]) -> Vec<&SessionSummary> {
    let mut list: Vec<&SessionSummary> = summaries.iter().filter(|summary| !summary.archived).collect();
    list.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.updated_at.total_cmp(&a.updated_at)));
    list
}

impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let selected = store.selected.clone();
        let untitled = tr(cx, "chat.newChat");
        let theme = cx.theme();
        let rows: Vec<AnyElement> = ordered(&store.summaries)
            .into_iter()
            .map(|summary| {
                let id = summary.id.clone();
                let active = selected.as_deref() == Some(summary.id.as_str());
                let busy = matches!(summary.status.as_str(), "running" | "compacting");
                let waiting = summary.awaiting_approval == Some(true);
                let title: SharedString = if summary.title.is_empty() { untitled.clone() } else { summary.title.clone().into() };
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
                        this.child(Icon::new(gpui_kit::assets::IconName::Pin).xsmall().text_color(theme.muted_foreground))
                    })
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
                    .when(busy, |this| this.child(Spinner::new().xsmall()))
                    .when(waiting, |this| this.child(div().size(px(7.)).rounded_full().bg(theme.warning)))
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
            })
            .collect();
        let empty = rows.is_empty();
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .h(px(40.))
                    .flex_shrink_0()
                    .px_3()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.sidebar_border)
                    .child(div().text_sm().font_semibold().child(tr(cx, "chat.history")))
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
                    .p_1()
                    .gap_px()
                    .overflow_y_scroll()
                    .children(rows)
                    .when(empty, |this| {
                        this.child(div().p_3().text_sm().text_color(theme.muted_foreground).child(tr(cx, "chat.noChats")))
                    }),
            )
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
