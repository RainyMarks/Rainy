//! Approval bar, queued/steered messages, retry countdown, run errors and background commands above the composer.

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{BackgroundCommand, PendingApproval, QueuedMessage, RetryWait};
use serde_json::json;

use super::{ChatPane, now_ms};
use crate::i18n::{self, Locale};
use crate::ui::{tr, trf};

/// Heights of the lists above the composer, which scroll beyond them so the composer always keeps its place.
const MAX_QUEUE_HEIGHT: f32 = 100.;
const MAX_BACKGROUND_HEIGHT: f32 = 96.;
const MAX_ERROR_HEIGHT: f32 = 160.;
/// A run error longer than this may not fit three lines; it gets a details toggle.
const LONG_ERROR_CHARS: usize = 48;
/// Safety window after an accepted `chat.approve` RPC before re-enabling buttons if the Host missed an update.
const APPROVAL_SAFETY_RESET: Duration = Duration::from_millis(1500);

impl ChatPane {
    /// Answer the approval the bar shows; one answer at a time per `tool_call_id`.
    pub(super) fn answer(&mut self, tool_call_id: String, decision: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session_id(cx) else { return };
        if self.answering_for.as_deref() == Some(tool_call_id.as_str()) {
            return;
        }
        self.answering_for = Some(tool_call_id.clone());
        cx.notify();
        let request = self.store.read(cx).call("chat.approve", json!({ "sessionId": session, "toolCallId": tool_call_id, "decision": decision }));
        cx.spawn_in(window, async move |this, cx| {
            match request.await {
                Ok(_) => {
                    // Normally `session.state` clears the approval within milliseconds. If a reconnect or race left the
                    // same prompt visible, release the lock after a brief debounce so the buttons can never stick forever.
                    cx.background_executor().timer(APPROVAL_SAFETY_RESET).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.answering_for.as_deref() == Some(tool_call_id.as_str()) {
                            this.answering_for = None;
                            cx.notify();
                        }
                    });
                }
                Err(error) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        if this.answering_for.as_deref() == Some(tool_call_id.as_str()) {
                            this.answering_for = None;
                        }
                        let text = trf(cx, "chat.approvalFailed", &[("message", &crate::ui::error_text(cx, &error))]);
                        window.push_notification(Notification::error(text), cx);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// The approval bar's keys (`ApprovalBar.tsx`): Enter allows, Esc denies.
    pub(super) fn approval_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tool_call_id) = self.approval_shown.clone() else { return };
        if event.keystroke.modifiers.modified() {
            return;
        }
        let decision = match event.keystroke.key.as_str() {
            "enter" => "allow",
            "escape" => "deny",
            _ => return,
        };
        cx.stop_propagation();
        self.answer(tool_call_id, decision, window, cx);
    }

    pub(super) fn render_approval(&self, approval: &PendingApproval, focused: bool, locale: Locale, cx: &mut Context<Self>) -> AnyElement {
        let (radius_lg, radius, ring, danger, warning, secondary, muted, foreground, muted_foreground, mono_font) = {
            let theme = cx.theme();
            (
                theme.radius_lg,
                theme.radius,
                theme.ring,
                theme.danger,
                theme.warning,
                theme.secondary,
                theme.muted,
                theme.foreground,
                theme.muted_foreground,
                theme.mono_font_family.clone(),
            )
        };
        let path = approval.path.clone().unwrap_or_default();
        let workdir = approval.workdir.clone().filter(|w| !w.trim().is_empty());
        let outside = matches!(approval.reason.as_str(), "outside-write" | "outside-read" | "outside-workdir");
        let text = match approval.reason.as_str() {
            "outside-write" => i18n::format(locale, "chat.approvalOutsideWrite", &[("path", &path)]),
            "outside-read" => i18n::text(locale, "native.approvalOutsideRead").to_owned(),
            "outside-workdir" => i18n::text(locale, "native.approvalOutsideWorkdir").to_owned(),
            "mcp" => i18n::text(locale, "chat.approvalMcp").to_owned(),
            "web" => i18n::format(locale, "chat.approvalWeb", &[("url", &path)]),
            "file-change" => i18n::text(locale, "external.approvalFileChange").to_owned(),
            "permission" => i18n::text(locale, "external.approvalPermission").to_owned(),
            _ => i18n::text(locale, "chat.approvalCommand").to_owned(),
        };
        // A member of the external agent's team asks, not the agent itself.
        let text = match approval.agent.as_deref().filter(|agent| !agent.is_empty()) {
            Some(agent) => format!("{agent} · {text}"),
            None => text,
        };
        let detail_path = (!path.trim().is_empty()
            && matches!(approval.reason.as_str(), "outside-read" | "outside-workdir" | "outside-write" | "file-change"))
        .then(|| path.clone());
        let decide = |decision: &'static str| {
            let tool_call_id = approval.tool_call_id.clone();
            cx.listener(move |this, _, window, cx| this.answer(tool_call_id.clone(), decision, window, cx))
        };
        let answering = self.answering_for.as_deref() == Some(approval.tool_call_id.as_str());
        let anim_id = ElementId::Name(format!("approval-enter-{}", approval.tool_call_id).into());
        let panel = v_flex()
            .id("approval")
            .flex_shrink_0()
            .track_focus(&self.approval_focus)
            .on_key_down(cx.listener(Self::approval_key))
            .mx_3()
            .mb_2()
            .p_3()
            .gap_2()
            .rounded(radius_lg)
            .border_1()
            .border_color(if focused { ring } else if outside { danger } else { warning })
            .bg(secondary)
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .when(outside, |this| this.child(Icon::new(IconName::TriangleAlert).small().text_color(warning)))
                    .child(div().flex_1().min_w_0().text_sm().font_medium().child(text)),
            )
            .when_some(detail_path, |this, target| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(radius)
                        .bg(muted.opacity(0.6))
                        .font_family(mono_font.clone())
                        .text_xs()
                        .text_color(foreground)
                        .child(i18n::format(locale, "native.approvalTargetPath", &[("path", &target)])),
                )
            })
            .when_some(workdir, |this, dir| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(radius)
                        .bg(muted.opacity(0.6))
                        .font_family(mono_font)
                        .text_xs()
                        .text_color(foreground)
                        .child(i18n::format(locale, "native.approvalWorkdir", &[("path", &dir)])),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(Button::new("approve").primary().small().label(i18n::text(locale, "chat.approvalAllow")).disabled(answering).on_click(decide("allow")))
                    .when_some(approval.rule.clone(), |this, rule| {
                        this.child(
                            Button::new("approve-always")
                                .outline()
                                .small()
                                .label(i18n::format(locale, "chat.approvalAlways", &[("rule", &rule)]))
                                .disabled(answering)
                                .on_click(decide("always")),
                        )
                    })
                    .child(Button::new("deny").ghost().small().label(i18n::text(locale, "chat.approvalDeny")).disabled(answering).on_click(decide("deny"))),
            )
            .when(focused, |this| this.child(div().text_xs().text_color(muted_foreground).child(i18n::text(locale, "chat.approvalKeys"))));
        if cx.reduce_motion() {
            panel.into_any_element()
        } else {
            div()
                .child(panel)
                .with_animation(
                    anim_id,
                    Animation::new(Duration::from_millis(130)).with_easing(gpui_kit::base::animation::ease_out_cubic),
                    |el, delta| el.opacity(delta).relative().top(px(5.0 * (1.0 - delta))),
                )
                .into_any_element()
        }
    }

    pub(super) fn render_queue(&self, session: &str, queue: &[QueuedMessage], locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (radius, muted, muted_foreground) = {
            let theme = cx.theme();
            (theme.radius, theme.muted, theme.muted_foreground)
        };
        let items: Vec<AnyElement> = queue
            .iter()
            .map(|item| {
                let label = if item.mode == "steer" { "chat.steering" } else { "chat.queued" };
                let params = json!({ "sessionId": session, "id": item.id });
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(radius)
                    .bg(muted)
                    .text_xs()
                    .items_center()
                    .child(div().flex_shrink_0().text_color(muted_foreground).child(i18n::text(locale, label)))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(item.text.clone()))
                    .child(
                        Button::new(ElementId::Name(format!("unqueue-{}", item.id).into()))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip(i18n::text(locale, "chat.removeQueued"))
                            .on_click(cx.listener(move |this, _, window, cx| this.call("chat.unqueue", params.clone(), window, cx))),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .relative()
            .flex_shrink_0()
            .mx_3()
            .mb_2()
            .child(v_flex().id("queue").max_h(px(MAX_QUEUE_HEIGHT)).overflow_y_scroll().track_scroll(&self.queue_scroll).gap_1().children(items))
            .vertical_scrollbar(&self.queue_scroll)
    }

    pub(super) fn render_retry(&self, session: &str, retry: &RetryWait, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let seconds = ((retry.at - now_ms()) / 1000.0).ceil().max(0.0);
        let retried = session.to_owned();
        h_flex()
            .flex_shrink_0()
            .mx_3()
            .mb_2()
            .gap_2()
            .items_center()
            .child(div().flex_1().min_w_0().text_xs().text_color(crate::theme::warning_text(cx)).child(trf(cx, "chat.retryWaiting", &[
                ("seconds", &seconds),
                ("attempt", &retry.attempt),
                ("max", &retry.max),
            ])))
            .child(Button::new("retry-now").outline().xsmall().label(tr(cx, "chat.retryNow")).on_click(cx.listener(move |this, _, window, cx| {
                this.call("chat.retry", json!({ "sessionId": retried }), window, cx);
            })))
    }

    /// The error a run ended in, three lines at most until its details are opened, and a retry.
    pub(super) fn render_run_error(&self, session: &str, error: String, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let long = error.chars().count() > LONG_ERROR_CHARS || error.lines().count() > 3;
        let open = self.error_open && long;
        let text: SharedString = if error.is_empty() { tr(cx, "chat.errorTitle") } else { error.into() };
        let retried = session.to_owned();
        let (radius, danger) = {
            let theme = cx.theme();
            (theme.radius, theme.danger)
        };
        h_flex()
            .flex_shrink_0()
            .mx_3()
            .mb_2()
            .px_3()
            .py_2()
            .gap_2()
            .items_start()
            .rounded(radius)
            .border_1()
            .border_color(danger.opacity(0.4))
            .bg(danger.opacity(0.08))
            .child(Icon::new(IconName::TriangleAlert).small().text_color(danger))
            .child(
                div()
                    .id("run-error")
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .when(open, |this| this.max_h(px(MAX_ERROR_HEIGHT)).overflow_y_scroll())
                    .when(!open, |this| this.line_clamp(3).text_ellipsis())
                    .child(text),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .items_end()
                    .child(Button::new("retry").outline().xsmall().icon(IconName::RefreshCw).label(tr(cx, "chat.retry")).on_click(cx.listener(move |this, _, window, cx| {
                        this.call("chat.retry", json!({ "sessionId": retried }), window, cx);
                    })))
                    .when(long, |this| {
                        this.child(Button::new("error-details").ghost().xsmall().label(tr(cx, if open { "chat.collapse" } else { "chat.showDetails" })).on_click(cx.listener(|this, _, _, cx| {
                            this.error_open = !this.error_open;
                            cx.notify();
                        })))
                    }),
            )
    }

    /// The chat's background commands (`BackgroundBar.tsx`): stop a running one, clear a finished one.
    pub(super) fn render_background(&self, session: &str, commands: &[BackgroundCommand], cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (radius_lg, border, secondary, muted_foreground, warning, success, danger, mono_font) = {
            let theme = cx.theme();
            (
                theme.radius_lg,
                theme.border,
                theme.secondary,
                theme.muted_foreground,
                theme.warning,
                theme.success,
                theme.danger,
                theme.mono_font_family.clone(),
            )
        };
        let rows: Vec<AnyElement> = commands
            .iter()
            .enumerate()
            .map(|(index, command)| {
                let running = command.state == "running";
                let dot = if running { warning } else if command.exit_code == Some(0) { success } else { danger };
                let state = if running {
                    if command.pending > 0.0 {
                        format!("{} · {}", tr(cx, "chat.backgroundRunning"), trf(cx, "chat.backgroundUnread", &[("bytes", &command.pending)]))
                    } else {
                        tr(cx, "chat.backgroundRunning").to_string()
                    }
                } else if command.signal.is_some() {
                    tr(cx, "chat.backgroundStopped").to_string()
                } else {
                    let code = command.exit_code.map_or("?".to_owned(), |code| code.to_string());
                    trf(cx, "chat.backgroundExited", &[("code", &code)]).to_string()
                };
                let what = if command.description.is_empty() { command.command.clone() } else { command.description.clone() };
                let (method, label) = if running { ("chat.stopCommand", "chat.backgroundStop") } else { ("chat.dismissCommand", "chat.backgroundDismiss") };
                let params = json!({ "sessionId": session, "id": command.id });
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(dot))
                    .child(div().flex_shrink_0().font_family(mono_font.clone()).child(command.id.clone()))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(what))
                    .child(div().flex_shrink_0().text_color(muted_foreground).child(state))
                    .child(Button::new(("background", index)).outline().xsmall().label(trf(cx, label, &[("id", &command.id)])).on_click(cx.listener(
                        move |this, _, window, cx| this.call(method, params.clone(), window, cx),
                    )))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .flex_shrink_0()
            .mx_3()
            .mb_2()
            .px_3()
            .py_2()
            .gap_1()
            .rounded(radius_lg)
            .border_1()
            .border_color(border)
            .bg(secondary)
            .child(div().text_xs().font_semibold().text_color(muted_foreground).child(trf(cx, "chat.backgroundTitle", &[("count", &commands.len())])))
            .child(
                div()
                    .relative()
                    .child(v_flex().id("background-list").max_h(px(MAX_BACKGROUND_HEIGHT)).overflow_y_scroll().track_scroll(&self.background_scroll).gap_1().children(rows))
                    .vertical_scrollbar(&self.background_scroll),
            )
    }
}
