//! The files the agent changed in the current chat, above the composer (`ChangesBar.tsx`): review each in the
//! editor, keep it, or undo it.

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{ChatChange, RevertResult};
use serde_json::{Value, json};

use crate::store::ide::Ide;
use crate::store::{Store, StoreEvent};
use crate::ui::ide::file_icon;
use crate::ui::{tr, trf};

pub struct ChangesBar {
    store: Entity<Store>,
    session: Option<String>,
    changes: Vec<ChatChange>,
    open: bool,
    pending: bool,
    refresh: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ChangesBar {
    pub fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.subscribe(&store, |this, store, event: &StoreEvent, cx| match event {
            StoreEvent::SelectionChanged => {
                this.session = store.read(cx).selected.clone();
                this.changes.clear();
                this.load(cx);
            }
            StoreEvent::Host { name, data }
                if name == "session.changes" && data.get("sessionId").and_then(Value::as_str) == this.session.as_deref() =>
            {
                this.schedule(cx);
            }
            _ => {}
        })];
        let session = store.read(cx).selected.clone();
        let mut bar = Self { store, session, changes: Vec::new(), open: false, pending: false, refresh: None, _subscriptions: subscriptions };
        bar.load(cx);
        bar
    }

    /// Read the list a moment after the last change event; edits come in bursts.
    fn schedule(&mut self, cx: &mut Context<Self>) {
        self.refresh = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let _ = this.update(cx, |this, cx| this.load(cx));
        }));
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            cx.notify();
            return;
        };
        let call = self.store.read(cx).call("chat.changes", json!({ "sessionId": session }));
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                if this.session.as_deref() != Some(session.as_str()) {
                    return;
                }
                if let Ok(Ok(changes)) = result.map(serde_json::from_value::<Vec<ChatChange>>) {
                    this.changes = changes;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn busy(&self, cx: &App) -> bool {
        let Some(session) = &self.session else { return false };
        self.store.read(cx).summaries.iter().any(|summary| summary.id == *session && matches!(summary.status.as_str(), "running" | "compacting"))
    }

    fn keep(&mut self, list: Vec<ChatChange>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else { return };
        let ids: Vec<String> = list.iter().flat_map(|change| change.tool_call_ids.clone()).collect();
        let call = self.store.read(cx).call("chat.keep", json!({ "sessionId": session, "toolCallIds": ids }));
        self.run(call, false, window, cx);
    }

    fn undo(&mut self, list: Vec<ChatChange>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else { return };
        let ids: Vec<String> = list.iter().filter(|change| change.undoable).flat_map(|change| change.tool_call_ids.clone()).collect();
        let call = self.store.read(cx).call("chat.revert", json!({ "sessionId": session, "toolCallIds": ids }));
        // Comparisons of undone files show text that is gone.
        let ide = Ide::global(cx);
        let keys: Vec<String> = list.iter().filter_map(|change| ide.read(cx).key_from_absolute(&change.path)).collect();
        ide.update(cx, |ide, cx| {
            for key in &keys {
                ide.close_review(key, cx);
            }
        });
        self.run(call, true, window, cx);
    }

    /// Run a keep or undo, report an undo's outcome, and read the list again.
    fn run(
        &mut self,
        call: impl Future<Output = Result<Value, rainy_protocol::RpcError>> + 'static,
        revert: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending = false;
                match result {
                    Ok(value) if revert => {
                        if let Ok(result) = serde_json::from_value::<RevertResult>(value) {
                            if !result.reverted.is_empty() {
                                window.push_notification(trf(cx, "chat.undoDone", &[("count", &result.reverted.len())]).to_string(), cx);
                            }
                            for failure in result.failed {
                                let reason = tr(cx, match failure.reason.as_str() {
                                    "changed" => "chat.undoChanged",
                                    "unavailable" => "chat.undoUnavailable",
                                    _ => "chat.undoError",
                                });
                                let path = if failure.path.is_empty() { "…".to_owned() } else { failure.path };
                                window.push_notification(Notification::error(trf(cx, "chat.undoFailed", &[("path", &path), ("reason", &reason)])), cx);
                            }
                        }
                        // Open editors follow the files put back.
                        Ide::global(cx).update(cx, |ide, cx| ide.check_files(cx));
                    }
                    Ok(_) => {}
                    Err(error) => window.push_notification(Notification::error(error.message), cx),
                }
                this.load(cx);
            });
        })
        .detach();
    }

    /// Show a file's changes as a comparison in the editor (`chat.changeBase`).
    fn review(&mut self, change: ChatChange, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else { return };
        let ide = Ide::global(cx);
        let Some(key) = ide.read(cx).key_from_absolute(&change.path) else {
            window.push_notification(tr(cx, "review.changesOutside").to_string(), cx);
            return;
        };
        let call = self.store.read(cx).call("chat.changeBase", json!({ "sessionId": session, "path": change.path }));
        cx.spawn_in(window, async move |_, cx| {
            let result = call.await;
            let _ = cx.update(|window, cx| match result {
                Ok(value) => {
                    let base = value["base"].as_str().unwrap_or_default().to_owned();
                    let current = value["current"].as_str().unwrap_or_default().to_owned();
                    ide.update(cx, |ide, cx| ide.show_review(key, base, current, window, cx));
                }
                Err(error) => window.push_notification(Notification::error(error.message), cx),
            });
        })
        .detach();
    }

    fn confirm_undo_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changes = self.changes.clone();
        let count = changes.iter().filter(|change| change.undoable).count();
        let bar = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (bar, changes) = (bar.clone(), changes.clone());
            alert
                .title(tr(cx, "review.changesUndoAllTitle"))
                .description(trf(cx, "review.changesUndoAllBody", &[("count", &count)]))
                .ok_text(tr(cx, "review.changesUndoAll"))
                .cancel_text(tr(cx, "review.cancel"))
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let changes = changes.clone();
                    bar.update(cx, |bar, cx| bar.undo(changes, window, cx));
                    true
                })
        });
    }
}

impl Render for ChangesBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.changes.is_empty() {
            return div().into_any_element();
        }
        let theme = cx.theme().clone();
        let busy = self.busy(cx);
        let added: u64 = self.changes.iter().map(|change| change.added).sum();
        let removed: u64 = self.changes.iter().map(|change| change.removed).sum();
        let undoable = self.changes.iter().any(|change| change.undoable);
        let auto_keep = self.store.read(cx).prefs.auto_keep_changes;
        let all = self.changes.clone();
        let header = h_flex()
            .gap_2()
            .items_center()
            .child(
                h_flex()
                    .id("changes-toggle")
                    .flex_1()
                    .min_w_0()
                    .gap_1p5()
                    .items_center()
                    .cursor_pointer()
                    .child(Icon::new(if self.open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(theme.muted_foreground))
                    .child(div().text_sm().font_semibold().child(trf(cx, "review.changesTitle", &[("count", &self.changes.len())])))
                    .child(div().text_xs().text_color(theme.success).child(format!("+{added}")))
                    .child(div().text_xs().text_color(theme.danger).child(format!("−{removed}")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open = !this.open;
                        if this.open {
                            this.load(cx);
                        }
                        cx.notify();
                    })),
            )
            .child(Button::new("changes-keep-all").outline().xsmall().label(tr(cx, "review.changesKeepAll")).disabled(self.pending).on_click(cx.listener(
                move |this, _, window, cx| this.keep(all.clone(), window, cx),
            )))
            .child(
                Button::new("changes-undo-all")
                    .outline()
                    .xsmall()
                    .label(tr(cx, "review.changesUndoAll"))
                    .disabled(self.pending || busy || !undoable)
                    .when(busy, |this| this.tooltip(tr(cx, "review.changesBusy")))
                    .on_click(cx.listener(|this, _, window, cx| this.confirm_undo_all(window, cx))),
            );
        let rows: Vec<AnyElement> = if self.open {
            self.changes
                .iter()
                .enumerate()
                .map(|(index, change)| {
                    let display = change.display.replace('\\', "/");
                    let (directory, name) = display.rsplit_once('/').map_or((String::new(), display.clone()), |(d, n)| (d.to_owned(), n.to_owned()));
                    let reviewable = change.comparable && !change.deleted;
                    let (reviewed, kept, undone) = (change.clone(), change.clone(), change.clone());
                    h_flex()
                        .id(("change", index))
                        .h(px(28.))
                        .gap_1p5()
                        .items_center()
                        .text_sm()
                        .child(Icon::new(file_icon(&name)).small())
                        .child(div().flex_shrink_0().child(name))
                        .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(theme.muted_foreground).child(directory))
                        .when(change.created, |this| this.child(div().text_xs().text_color(theme.success).child(tr(cx, "review.changesCreated"))))
                        .when(change.deleted, |this| this.child(div().text_xs().text_color(theme.danger).child(tr(cx, "review.changesDeleted"))))
                        .child(if change.comparable {
                            h_flex()
                                .gap_1()
                                .text_xs()
                                .child(div().text_color(theme.success).child(format!("+{}", change.added)))
                                .child(div().text_color(theme.danger).child(format!("−{}", change.removed)))
                                .into_any_element()
                        } else {
                            div().text_xs().text_color(theme.muted_foreground).child("…").into_any_element()
                        })
                        .child(Button::new(("change-review", index)).ghost().xsmall().label(tr(cx, "review.changesReview")).disabled(!reviewable).on_click(cx.listener(
                            move |this, _, window, cx| this.review(reviewed.clone(), window, cx),
                        )))
                        .child(Button::new(("change-keep", index)).ghost().xsmall().label(tr(cx, "review.changesKeep")).disabled(self.pending).on_click(cx.listener(
                            move |this, _, window, cx| this.keep(vec![kept.clone()], window, cx),
                        )))
                        .child(
                            Button::new(("change-undo", index))
                                .ghost()
                                .xsmall()
                                .label(tr(cx, "review.changesUndo"))
                                .disabled(self.pending || busy || !change.undoable)
                                .on_click(cx.listener(move |this, _, window, cx| this.undo(vec![undone.clone()], window, cx))),
                        )
                        .into_any_element()
                })
                .collect()
        } else {
            Vec::new()
        };
        v_flex()
            .mx_3()
            .mb_2()
            .px_3()
            .py_2()
            .gap_1()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(header)
            .children(rows)
            .when(self.open && auto_keep, |this| this.child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "review.changesAutoKeep"))))
            .into_any_element()
    }
}
