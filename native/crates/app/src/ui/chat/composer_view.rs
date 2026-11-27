//! Composer chrome, header bar, attachment strip, permission/worktree/model pickers and context breakdown popover.

use std::time::Duration;

use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{ContextBreakdown, RpcError};
use serde_json::json;

use super::ChatPane;
use super::drafts::report_failure;
use super::model_card::{model_control, model_state};
use crate::i18n::{self, Locale};
use crate::ui::{tr, trf};

const PERMISSIONS: [(&str, &str); 3] = [("read-only", "chat.permissionReadOnly"), ("ask", "chat.permissionAsk"), ("auto", "chat.permissionAuto")];
/// Height of one line of Agent mode's composer bar: its text and its buttons are centred in it.
const COMPOSER_LINE: f32 = 32.;

impl ChatPane {
    pub(super) fn render_attachments(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (radius, border) = {
            let theme = cx.theme();
            (theme.radius, theme.border)
        };
        h_flex().gap_2().flex_wrap().children(self.attachments.iter().enumerate().map(|(index, image)| {
            div()
                .relative()
                .size(px(56.))
                .rounded(radius)
                .border_1()
                .border_color(border)
                .overflow_hidden()
                .child(img(image.preview.clone()).size_full().object_fit(ObjectFit::Cover))
                .child(div().absolute().top_0().right_0().child(Button::new(("remove-image", index)).ghost().xsmall().icon(IconName::Close).tooltip(tr(cx, "chat.removeImage")).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if index < this.attachments.len() {
                            this.attachments.remove(index);
                        }
                        cx.notify();
                    },
                ))))
        }))
    }

    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let title = self
            .chat
            .as_ref()
            .and_then(|chat| chat.read(cx).view.as_ref().map(|view| view.snapshot.summary.title.clone()))
            .filter(|title| !title.is_empty())
            .map(SharedString::from)
            .unwrap_or_else(|| tr(cx, "chat.newChat"));
        let border = cx.theme().border;
        h_flex()
            .h(px(40.))
            .flex_shrink_0()
            .px_3()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_semibold().text_sm().child(title))
            .child(
                Button::new("new-chat")
                    .ghost()
                    .small()
                    .icon(Icon::new(gpui_kit::assets::IconName::CirclePlus))
                    .tooltip(tr(cx, "chat.newChat"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.store.update(cx, |store, cx| store.select(None, cx));
                    })),
            )
    }

    /// The model and reasoning control under the composer ([`model_control`]).
    fn model_picker(&self, _locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // A chat with Codex or Antigravity shows how much of the account is left: showing it reads that again
        // (the Host keeps a reading under a minute old), so the button can warn before a turn fails.
        if let Some(backend) = model_state(&self.store, self.chat.as_ref(), cx).and_then(|state| state.external) {
            let key = format!("{}:{}", self.session_id(cx).unwrap_or_default(), backend.kind);
            if self.quota_asked.borrow().as_deref() != Some(key.as_str()) {
                *self.quota_asked.borrow_mut() = Some(key);
                crate::ui::quota::refresh(&self.store, &backend.kind, false, cx);
            }
        }
        model_control(&self.store, self.chat.as_ref(), cx)
    }

    /// The chat's mode; before a new chat's first message, the mode it will start in (3.x `nextMode`, else the
    /// default from Settings).
    fn permission_picker(&self, locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let session = self.session_id(cx);
        let current = self
            .chat
            .as_ref()
            .and_then(|chat| chat.read(cx).view.as_ref().map(|view| view.snapshot.permission.clone()))
            .or_else(|| session.is_none().then(|| self.store.read(cx).pending_permission.clone()).flatten())
            .unwrap_or_else(|| self.default_permission.clone());
        let label = PERMISSIONS.iter().find(|(id, _)| *id == current).map_or("chat.permissionAsk", |(_, key)| key);
        let store = self.store.clone();
        Button::new("permission-picker")
            .ghost()
            .small()
            .label(i18n::text(locale, label))
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for (mode, key) in PERMISSIONS {
                    let (store, session) = (store.clone(), session.clone());
                    menu = menu.item(PopupMenuItem::new(i18n::text(locale, key)).checked(current == mode).on_click(move |_, window, cx| match session.clone() {
                        Some(session) => report_failure(store.read(cx).call("chat.setPermission", json!({ "sessionId": session, "mode": mode })), window, cx),
                        None => store.update(cx, |store, cx| {
                            store.pending_permission = Some(mode.to_owned());
                            store.notify_composer(cx);
                            cx.notify();
                        }),
                    }));
                }
                menu
            })
    }

    /// Where a new chat of a project runs: in the project's checkout, or in its own Git worktree (the Codex app's Local
    /// and Worktree). A chat that runs in a worktree says so; its menu in the sidebar makes a branch or hands it off.
    fn where_picker(&self, locale: Locale, cx: &mut Context<Self>) -> Option<AnyElement> {
        let worktree = self.session_id(cx).and_then(|id| self.store.read(cx).summaries.iter().find(|summary| summary.id == id).map(|summary| summary.worktree.clone()));
        match worktree {
            Some(Some(worktree)) => {
                let label = worktree.branch.unwrap_or_else(|| i18n::text(locale, "agent.worktree").to_string());
                Some(
                    h_flex()
                        .flex_none()
                        .gap_1()
                        .px_1()
                        .items_center()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(Icon::new(gpui_kit::assets::IconName::GitBranch).xsmall())
                        .child(label)
                        .into_any_element(),
                )
            }
            Some(None) => None,
            None => {
                // A new chat: only a project can have a worktree.
                crate::store::ide::Ide::global(cx).read(cx).workspace.as_ref()?;
                let worktree = self.store.read(cx).pending_worktree;
                let store = self.store.clone();
                Some(
                    Button::new("where-picker")
                        .ghost()
                        .small()
                        .when(worktree, |button| button.icon(Icon::new(gpui_kit::assets::IconName::GitBranch)))
                        .label(i18n::text(locale, if worktree { "agent.worktree" } else { "agent.whereLocal" }))
                        .tooltip(i18n::text(locale, "agent.whereWorktreeHint"))
                        .dropdown_caret(true)
                        .dropdown_menu(move |menu, _, _| {
                            let (local, own) = (store.clone(), store.clone());
                            menu.item(PopupMenuItem::new(i18n::text(locale, "agent.whereLocal")).checked(!worktree).on_click(move |_, _, cx| {
                                local.update(cx, |store, cx| {
                                    store.pending_worktree = false;
                                    store.notify_composer(cx);
                                    cx.notify();
                                })
                            }))
                            .item(PopupMenuItem::new(i18n::text(locale, "agent.whereWorktree")).checked(worktree).on_click(move |_, _, cx| {
                                own.update(cx, |store, cx| {
                                    store.pending_worktree = true;
                                    store.notify_composer(cx);
                                    cx.notify();
                                })
                            }))
                        })
                        .into_any_element(),
                )
            }
        }
    }

    /// Send or Stop action button with smooth 140 ms state transition and 110 ms ready-opacity feedback (0 fps at rest).
    fn render_action_button(&self, busy: bool, creating: bool, session: Option<String>, locale: Locale, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let wide = self.wide;
        let ready = !creating && (!self.composer.read(cx).value().trim().is_empty() || !self.attachments.is_empty());
        let target_opacity = if busy || ready || creating { 1.0 } else { 0.62 };
        let readiness_opacity = transition("chat-action-ready", target_opacity, Transition::new(Duration::from_millis(110)), window, cx);
        let button: AnyElement = if busy {
            let icon = if wide { Icon::new(IconName::Square) } else { Icon::new(gpui_kit::assets::IconName::CircleStop) };
            Button::new("stop")
                .small()
                .icon(icon)
                .tooltip(i18n::text(locale, "chat.stop"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(session) = session.clone() {
                        this.call("chat.abort", json!({ "sessionId": session }), window, cx);
                    }
                }))
                .into_any_element()
        } else {
            let icon = if wide { Icon::new(IconName::ArrowUp) } else { Icon::new(gpui_kit::assets::IconName::Send) };
            Button::new("send")
                .primary()
                .small()
                .icon(icon)
                .loading(creating)
                .tooltip(i18n::text(locale, if creating { "chat.creating" } else { "chat.send" }))
                .on_click(cx.listener(|this, _, window, cx| this.send(false, window, cx)))
                .into_any_element()
        };
        let wrap = div().opacity(readiness_opacity).child(button);
        if cx.reduce_motion() {
            wrap.into_any_element()
        } else {
            let anim_id = ElementId::Name(format!("chat-action-enter-{}", self.action_gen).into());
            wrap.with_animation(
                anim_id,
                Animation::new(Duration::from_millis(140)).with_easing(gpui_kit::base::animation::ease_out_cubic),
                move |el, delta| el.opacity(readiness_opacity * delta),
            )
            .into_any_element()
        }
    }

    pub(super) fn render_composer(&self, locale: Locale, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let busy = self.busy(cx);
        let creating = self.creating_here();
        let is_external = self.is_external_chat(cx);
        let context = (!is_external)
            .then(|| self.chat.as_ref().and_then(|chat| chat.read(cx).view.as_ref().and_then(|view| view.snapshot.context.clone())))
            .flatten();
        let model_picker = self.model_picker(locale, cx);
        let permission_picker = self.permission_picker(locale, cx);
        let where_picker = self.where_picker(locale, cx);
        let session = self.session_id(cx);
        let attachments = (!self.attachments.is_empty()).then(|| self.render_attachments(cx));
        let action = self.render_action_button(busy, creating, session, locale, window, cx);
        let (radius_lg, input, background, muted_foreground) = {
            let theme = cx.theme();
            (theme.radius_lg, theme.input, theme.background, theme.muted_foreground)
        };
        v_flex()
            .id("composer")
            .flex_shrink_0()
            .mx_3()
            .mb_3()
            .p_2()
            .gap_1()
            .rounded(radius_lg)
            .border_1()
            .border_color(input)
            .bg(background)
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.border_color(cx.theme().ring))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.attach_paths(paths.paths().to_vec(), window, cx)))
            .children(attachments)
            .child(Textarea::new(&self.composer).appearance(false))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("attach-image")
                            .ghost()
                            .small()
                            .icon(Icon::new(gpui_kit::assets::IconName::Paperclip))
                            .tooltip(tr(cx, "chat.attachImage"))
                            .on_click(cx.listener(|this, _, window, cx| this.choose_images(window, cx))),
                    )
                    // The pickers give way in a narrow panel; the context share and the send button never leave it.
                    .child(h_flex().flex_1().min_w_0().overflow_hidden().gap_1().child(model_picker).child(permission_picker).children(where_picker))
                    .when_some(context, |this, context| {
                        let percent = if context.input_limit > 0.0 { (context.tokens / context.input_limit * 100.0).round() } else { 0.0 };
                        this.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(muted_foreground)
                                .child(i18n::format(locale, "chat.contextUsage", &[("percent", &percent)])),
                        )
                    })
                    .child(div().flex_none().child(action)),
            )
    }

    /// Agent mode's composer, one slim bar as Claude's: attach on the left, the text in the middle, send or stop on the
    /// right, and images above them; the bar grows with the text and the buttons stay on its last line. The pickers
    /// are under it (`render_composer_footer`).
    pub(super) fn render_composer_wide(&self, locale: Locale, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let busy = self.busy(cx);
        let creating = self.creating_here();
        let session = self.session_id(cx);
        let attachments = (!self.attachments.is_empty()).then(|| div().px_1().pt_1().child(self.render_attachments(cx)));
        // The optical look puts the pickers in the glass, under the text.
        let inside = crate::ui::optics::enabled(cx).then(|| self.render_composer_footer(locale, true, cx));
        let action = self.render_action_button(busy, creating, session, locale, window, cx);
        let composer = v_flex().id("composer").flex_shrink_0().mx_3().px_2().py_2().gap_1();
        let composer = {
            let theme = cx.theme();
            if inside.is_some() {
                crate::ui::optics::glaze(composer, &crate::ui::optics::look::Look::of(theme), theme.radius_lg * 1.5)
            } else {
                composer.rounded(theme.radius_lg * 1.5).border_1().border_color(theme.input).bg(theme.popover).shadow_sm()
            }
        };
        composer
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.border_color(cx.theme().ring))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.attach_paths(paths.paths().to_vec(), window, cx)))
            .children(attachments)
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(
                        h_flex().h(px(COMPOSER_LINE)).items_center().child(
                            Button::new("attach-image")
                                .ghost()
                                .small()
                                .icon(IconName::Plus)
                                .tooltip(tr(cx, "agent.attach"))
                                .on_click(cx.listener(|this, _, window, cx| this.choose_images(window, cx))),
                        ),
                    )
                    .child(h_flex().flex_1().min_w_0().min_h(px(COMPOSER_LINE)).items_center().child(Textarea::new(&self.composer).appearance(false).py_0().w_full()))
                    .child(h_flex().h(px(COMPOSER_LINE)).items_center().child(action)),
            )
            .children(inside)
    }

    /// Fetch the context breakdown for `(session_id, revision)` if not already cached or in flight.
    fn request_breakdown(&mut self, session_id: String, revision: u64, cx: &mut Context<Self>) {
        if self.breakdown.as_ref().is_some_and(|(id, rev, _)| *id == session_id && *rev == revision)
            || self.breakdown_loading.as_ref().is_some_and(|(id, rev)| *id == session_id && *rev == revision)
        {
            return;
        }
        self.breakdown_loading = Some((session_id.clone(), revision));
        let call = self.store.read(cx).call("session.contextBreakdown", json!({ "sessionId": session_id }));
        let locale = self.store.read(cx).locale();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = call.await.and_then(|value| {
                serde_json::from_value::<ContextBreakdown>(value).map_err(|error| RpcError { code: "invalid".into(), message: error.to_string(), data: None })
            });
            let _ = this.update(cx, |this, cx| {
                if this.breakdown_loading.as_ref().is_some_and(|(id, rev)| *id == session_id && *rev == revision) {
                    this.breakdown_loading = None;
                }
                let mapped = result.map_err(|error| SharedString::from(i18n::error_text(locale, &error)));
                this.breakdown = Some((session_id, revision, mapped));
                cx.notify();
            });
        })
        .detach();
    }

    /// Under Agent mode's composer, on the right as in Claude: the model and its reasoning, the permission mode and how
    /// full the context is. The model gives way in a narrow window; the mode and the ring never leave it.
    pub(super) fn render_composer_footer(&self, locale: Locale, inside: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let is_external = self.is_external_chat(cx);
        let chat_meta = (!is_external)
            .then(|| {
                self.chat.as_ref().and_then(|chat| {
                    let model = chat.read(cx);
                    let view = model.view.as_ref()?;
                    Some((model.id.clone(), view.revision, view.snapshot.context.clone()?))
                })
            })
            .flatten();
        let model_picker = self.model_picker(locale, cx);
        let permission_picker = self.permission_picker(locale, cx);
        // On the left, as in the Codex app: where the chat runs.
        let where_picker = self.where_picker(locale, cx);
        let pane_entity = cx.entity();
        let ring = chat_meta.map(|(session_id, revision, context)| {
            let fraction = if context.input_limit > 0.0 { (context.tokens / context.input_limit) as f32 } else { 0. };
            let tip = trf(cx, "agent.contextUsage", &[
                ("percent", &(fraction * 100.).round()),
                ("used", &crate::ui::format_tokens(context.tokens)),
                ("limit", &crate::ui::format_tokens(context.input_limit)),
            ]);
            let (danger, muted_foreground, radius_lg) = {
                let theme = cx.theme();
                (theme.danger, theme.muted_foreground, theme.radius_lg)
            };
            let color = if fraction >= 0.9 {
                danger
            } else if fraction >= 0.75 {
                crate::theme::warning_text(cx)
            } else {
                muted_foreground
            };
            let trigger = Button::new("context-usage")
                .ghost()
                .xsmall()
                .px_1()
                .tooltip(tip)
                .child(crate::ui::agent::usage_ring(fraction, color, muted_foreground.opacity(0.3)));
            let pane_for_content = pane_entity.clone();
            Popover::new("context-breakdown-card")
                .anchor(gpui_kit::Anchor::BottomRight)
                .w(px(284.))
                .p_3()
                .rounded(radius_lg)
                .trigger(trigger)
                .content(move |_, window, cx| {
                    crate::web::mark_popover_open(cx);
                    let open_token = window.use_state(cx, |_, _| ()).entity_id();
                    let pane_req = pane_for_content.clone();
                    let sid = session_id.clone();
                    cx.defer(move |cx| {
                        pane_req.update(cx, |pane, cx| pane.request_breakdown(sid, revision, cx));
                    });
                    let snapshot = pane_for_content.read(cx).breakdown.clone();
                    render_context_breakdown_card(&session_id, revision, snapshot.as_ref(), locale, open_token, cx)
                })
        });
        h_flex()
            .flex_shrink_0()
            .when(!inside, |this| this.mx_3().mt_1().mb_2())
            .gap_1()
            .items_center()
            .children(where_picker)
            .child(div().flex_1())
            .child(h_flex().min_w_0().overflow_hidden().gap_1().child(model_picker))
            .child(div().flex_shrink_0().child(permission_picker))
            .children(ring)
    }
}

fn breakdown_spinner(color: Hsla, cx: &App) -> AnyElement {
    let icon = Icon::new(IconName::Loader).xsmall().text_color(color);
    if cx.reduce_motion() {
        return icon.into_any_element();
    }
    icon.with_animation(
        "context-breakdown-spin",
        Animation::new(Duration::from_millis(900)).repeat_synced().with_max_fps(30.0),
        |icon, delta| icon.rotate(radians(delta * std::f32::consts::TAU)),
    )
    .into_any_element()
}

/// Lightweight stacked bar + legend card rendered when clicking `usage_ring`.
fn render_context_breakdown_card(
    session_id: &str,
    revision: u64,
    cached: Option<&(String, u64, Result<ContextBreakdown, SharedString>)>,
    locale: Locale,
    open_token: EntityId,
    cx: &App,
) -> AnyElement {
    let (muted_foreground, danger, muted, border, mono_font) = {
        let theme = cx.theme();
        (theme.muted_foreground, theme.danger, theme.muted, theme.border, theme.mono_font_family.clone())
    };
    let fresh = cached.filter(|(id, rev, _)| id == session_id && *rev == revision);
    let Some((_, _, result)) = fresh.or_else(|| cached.filter(|(id, _, _)| id == session_id)) else {
        return h_flex()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(muted_foreground)
            .child(breakdown_spinner(muted_foreground, cx))
            .child(i18n::text(locale, "native.contextBreakdownLoading"))
            .into_any_element();
    };
    let breakdown = match result {
        Ok(b) => b,
        Err(err) => {
            return v_flex()
                .gap_1()
                .child(div().text_xs().font_semibold().child(i18n::text(locale, "native.contextBreakdownTitle")))
                .child(div().text_xs().text_color(muted_foreground).child(i18n::text(locale, "native.contextBreakdownUnavailable")))
                .child(div().text_xs().text_color(danger).child(err.clone()))
                .into_any_element();
        }
    };
    let s = &breakdown.segments;
    let items: [(&'static str, u64, Hsla); 6] = [
        ("native.contextSegSystem", s.system, rgb(0x5b8def).into()),
        ("native.contextSegInstructions", s.instructions, rgb(0x8b6ce8).into()),
        ("native.contextSegMcpTools", s.mcp_tools, rgb(0xd97745).into()),
        ("native.contextSegCoreTools", s.core_tools, rgb(0x3ba68c).into()),
        ("native.contextSegHistory", s.history, rgb(0x64748b).into()),
        ("native.contextSegRecall", s.recall, rgb(0xc49a3a).into()),
    ];
    let denom = breakdown.input_limit.max(breakdown.total).max(1) as f64;
    let bar_width: f32 = 256.0;
    let bar_segments: Vec<AnyElement> = items
        .iter()
        .filter(|(_, tokens, _)| *tokens > 0)
        .map(|(_, tokens, color)| {
            let w = ((*tokens as f64 / denom) as f32 * bar_width).max(3.0);
            div().h_full().w(px(w)).bg(*color).into_any_element()
        })
        .collect();
    let header_usage = format!(
        "{} / {}",
        crate::ui::format_tokens(breakdown.calibrated_total as f64),
        crate::ui::format_tokens(breakdown.input_limit as f64)
    );
    let show_factor = (breakdown.calibration_factor - 1.0).abs() >= 0.02;
    let factor_str = format!("{:.2}", breakdown.calibration_factor);
    let compact_str = crate::ui::format_tokens(breakdown.compact_at as f64);
    let card = v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(div().text_xs().font_semibold().child(i18n::text(locale, "native.contextBreakdownTitle")))
                .child(div().text_xs().font_family(mono_font.clone()).text_color(muted_foreground).child(header_usage)),
        )
        .child(
            h_flex()
                .w(px(bar_width))
                .h(px(8.))
                .rounded(px(4.))
                .overflow_hidden()
                .bg(muted)
                .children(bar_segments),
        )
        .child(v_flex().gap_1().children(items.into_iter().map(|(label_key, tokens, color)| {
            let pct = ((tokens as f64 / denom) * 100.0).round() as u64;
            h_flex()
                .items_center()
                .gap_2()
                .text_xs()
                .child(div().size(px(7.)).rounded_full().flex_shrink_0().bg(color))
                .child(div().flex_1().min_w_0().truncate().child(i18n::text(locale, label_key)))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(mono_font.clone())
                        .text_color(muted_foreground)
                        .child(format!("{} ({pct}%)", crate::ui::format_tokens(tokens as f64))),
                )
        })))
        .child(
            h_flex()
                .pt_1()
                .border_t_1()
                .border_color(border)
                .justify_between()
                .text_xs()
                .text_color(muted_foreground)
                .child(i18n::format(locale, "native.contextCompactThreshold", &[("tokens", &compact_str)]))
                .when(show_factor, |this| {
                    this.child(i18n::format(locale, "native.contextCalibrated", &[("factor", &factor_str)]))
                }),
        );
    if cx.reduce_motion() {
        card.into_any_element()
    } else {
        div()
            .child(card)
            .with_animation(
                ElementId::Name(format!("context-breakdown-enter-{open_token:?}").into()),
                Animation::new(Duration::from_millis(130)).with_easing(gpui_kit::base::animation::ease_out_cubic),
                |el, delta| el.opacity(delta).relative().top(px(5.0 * (1.0 - delta))),
            )
            .into_any_element()
    }
}
