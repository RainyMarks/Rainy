//! Tool, sub-agent, diff, and plan card rendering with synced low-fps status animations and zero-reclip output.

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{RevertResult, RpcError};
use serde_json::{Value, json};

use crate::i18n::{self, Locale};
use crate::store::Store;
use crate::store::ide::Ide;
use crate::ui::{tr, trf};

use super::model::{Exit, LineKind, Refusal, SubagentView, Toggle, ToolRow, ToolState, TurnStyle};
use super::{animated_chevron, apply_reveal, limited_spinner, readable, reveal_progress};

/// Lines of a working sub-agent's text its card shows.
const SUBAGENT_LIVE_LINES: usize = 6;

/// The key a tool card's open state goes by. A working sub-agent's card has its own: it starts open, and folding it
/// while it works does not leave the finished card open.
pub(super) fn tool_key(tool: &ToolRow) -> SharedString {
    if tool.subagent.is_some() && tool.state == ToolState::Running {
        format!("tool-live:{}", tool.call_id)
    } else {
        format!("tool:{}", tool.call_id)
    }
    .into()
}

/// What a tool line shows before its title: a frame-capped spinner while it runs, a softly breathing dot when waiting,
/// a steady dot when failed, nothing once done.
fn state_mark(call_id: &SharedString, state: ToolState, cx: &App) -> Option<AnyElement> {
    let theme = cx.theme();
    match state {
        ToolState::Running => Some(limited_spinner(ElementId::Name(format!("tool-spin-{call_id}").into()), theme.muted_foreground, cx)),
        ToolState::Waiting => {
            let dot = div().size(px(6.)).flex_shrink_0().rounded_full().bg(theme.warning);
            if cx.reduce_motion() {
                Some(dot.into_any_element())
            } else {
                Some(
                    dot.with_animation(
                        ElementId::Name(format!("tool-wait-{call_id}").into()),
                        Animation::new(Duration::from_millis(1600))
                            .repeat_synced()
                            .with_max_fps(24.0)
                            .with_easing(pulsating_between(0.45, 1.0)),
                        |dot, delta| dot.opacity(delta),
                    )
                    .into_any_element(),
                )
            }
        }
        ToolState::Failed => Some(div().size(px(6.)).flex_shrink_0().rounded_full().bg(theme.danger).into_any_element()),
        ToolState::Done => None,
    }
}

/// How a command ended, and whether that is worth showing on the closed card.
fn exit_label(exit: &Exit, locale: Locale) -> (String, bool) {
    match exit {
        Exit::Code(code) => (i18n::format(locale, "chat.exitCode", &[("code", code)]), *code != 0),
        Exit::Signal(signal) => (i18n::format(locale, "chat.signal", &[("signal", signal)]), true),
        Exit::Background => (i18n::text(locale, "chat.runningInBackground").to_owned(), false),
        Exit::Unknown => (i18n::text(locale, "chat.noExitCode").to_owned(), false),
    }
}

fn open_file(target: &str, window: &mut Window, cx: &mut App) {
    let ide = Ide::global(cx);
    let key = ide.read(cx).key_from_absolute(target);
    match key {
        Some(key) => {
            ide.update(cx, |ide, cx| ide.open(key, true, window, cx));
            crate::ui::agent::reveal_editor(cx);
        }
        None => window.push_notification(tr(cx, "chat.fileOutsideProject").to_string(), cx),
    }
}

/// Undo one change (`chat.revert`) and tell the user what happened, as the changes bar does.
fn undo_change(session: SharedString, changes: Vec<SharedString>, window: &mut Window, cx: &mut App) {
    let ids: Vec<&str> = changes.iter().map(SharedString::as_ref).collect();
    let request = Store::global(cx).read(cx).call("chat.revert", json!({ "sessionId": session.as_ref(), "toolCallIds": ids }));
    window
        .spawn(cx, async move |cx| {
            let result = request.await;
            let _ = cx.update(|window, cx| report_revert(result, window, cx));
        })
        .detach();
}

fn report_revert(result: Result<Value, RpcError>, window: &mut Window, cx: &mut App) {
    let value = match result {
        Ok(value) => value,
        Err(error) => {
            window.push_notification(crate::ui::error_notification(cx, &error), cx);
            return;
        }
    };
    let Ok(result) = serde_json::from_value::<RevertResult>(value) else { return };
    if !result.reverted.is_empty() {
        window.push_notification(trf(cx, "chat.undoDone", &[("count", &result.reverted.len())]).to_string(), cx);
    }
    for failure in &result.failed {
        let reason = tr(cx, match failure.reason.as_str() {
            "changed" => "chat.undoChanged",
            "unavailable" => "chat.undoUnavailable",
            _ => "chat.undoError",
        });
        let path = if failure.path.is_empty() { "…" } else { failure.path.as_str() };
        window.push_notification(Notification::error(trf(cx, "chat.undoFailed", &[("path", &path), ("reason", &reason)])), cx);
    }
    // Comparisons of undone files show text that is gone; open editors follow the files put back.
    let ide = Ide::global(cx);
    let keys: Vec<String> = result.reverted.iter().filter_map(|item| item["path"].as_str()).filter_map(|path| ide.read(cx).key_from_absolute(path)).collect();
    ide.update(cx, |ide, cx| {
        for key in &keys {
            ide.close_review(key, cx);
        }
        ide.check_files(cx);
    });
}

/// A sub-agent's report as its card shows it: without the note to the model that it stopped at a limit (the card says
/// so in the interface language) or the placeholder of an empty report.
pub(super) fn report_text(text: &str, stopped: bool) -> &str {
    let text = if stopped { text.rfind("\n\n[the sub-agent reached").map_or(text, |at| &text[..at]) } else { text };
    if text.trim() == "(the sub-agent reported nothing)" { "" } else { text }
}

/// The end of a working sub-agent's text: its last lines, with the fence of a code block the cut falls inside put back
/// in front, so the Markdown comes out as it will in the report.
pub(super) fn live_tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let (mut start, mut shown) = (lines.len(), 0);
    while start > 0 && shown < SUBAGENT_LIVE_LINES {
        start -= 1;
        if !lines[start].trim().is_empty() {
            shown += 1;
        }
    }
    let open = lines[..start].iter().map(|line| line.trim()).filter(|line| line.starts_with("```")).fold(None, |open: Option<&str>, fence| match open {
        Some(_) => None,
        None => Some(fence),
    });
    let tail = lines[start..].join("\n");
    match open {
        Some(fence) => format!("{fence}\n{tail}"),
        None => tail,
    }
}

/// A `task` card's body: the sub-agent's steps, then what it is writing, or the report it handed back.
fn subagent_body(tool: &ToolRow, agent: &SubagentView, cx: &App) -> AnyElement {
    let (danger, foreground, muted_foreground) = {
        let theme = cx.theme();
        (theme.danger, theme.foreground, theme.muted_foreground)
    };
    let text = tool.output.as_deref().unwrap_or_default();
    let mut body = v_flex().w_full().gap_1();
    if let Some(model) = &agent.model {
        body = body.child(div().child(trf(cx, "chat.subagentModel", &[("model", model)])));
    }
    if agent.earlier > 0 {
        body = body.child(div().child(trf(cx, "chat.subagentEarlier", &[("count", &agent.earlier)])));
    }
    body = body.children(agent.steps.iter().enumerate().map(|(ix, (title, state))| {
        let mark = match state {
            ToolState::Running | ToolState::Waiting => {
                limited_spinner(ElementId::Name(format!("sub-spin-{}-{ix}", tool.call_id).into()), muted_foreground, cx)
            }
            ToolState::Failed => div().size(px(6.)).rounded_full().bg(danger).into_any_element(),
            ToolState::Done => div().child("✓").into_any_element(),
        };
        h_flex()
            .w_full()
            .gap_1p5()
            .items_center()
            .child(h_flex().w_3().flex_shrink_0().justify_center().child(mark))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title.clone()))
    }));
    match tool.state {
        ToolState::Running | ToolState::Waiting => {
            let tail = live_tail(text);
            if !tail.trim().is_empty() {
                let id = ElementId::Name(format!("task-live-{}", tool.call_id).into());
                body = body.child(div().w_full().pt_1().text_sm().child(crate::ui::markdown::markdown_live(id, &SharedString::from(tail), true)));
            } else if agent.steps.is_empty() {
                body = body.child(div().child(tr(cx, "chat.subagentStarting")));
            }
        }
        ToolState::Failed => {
            body = body.child(div().whitespace_normal().text_color(readable(danger, cx)).child(text.to_owned()));
        }
        ToolState::Done => {
            let report = report_text(text, agent.stopped.is_some());
            if !agent.steps.is_empty() {
                body = body.child(div().pt_1().font_medium().text_color(foreground).child(tr(cx, "chat.subagentReport")));
            }
            body = if report.trim().is_empty() {
                body.child(div().child(tr(cx, "chat.subagentNoReport")))
            } else {
                let id = ElementId::Name(format!("task-md-{}", tool.call_id).into());
                body.child(div().w_full().text_sm().child(crate::ui::markdown::markdown(id, &SharedString::from(report.to_owned()))))
            };
            if let Some(stopped) = &agent.stopped {
                let key = match stopped.as_ref() {
                    "steps" => "chat.subagentStoppedSteps",
                    "budget" => "chat.subagentStoppedBudget",
                    _ => "chat.subagentStoppedTime",
                };
                body = body.child(div().text_color(crate::theme::warning_text(cx)).child(tr(cx, key)));
            }
        }
    }
    body.into_any_element()
}

pub(super) fn render_tool(
    tool: &ToolRow,
    open: bool,
    row: usize,
    toggle: &Toggle,
    style: &TurnStyle,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let locale = style.locale;
    let (mono, muted, foreground, border, link, success, danger) = {
        let theme = cx.theme();
        (theme.mono_font_family.clone(), theme.muted_foreground, theme.foreground, theme.border, theme.link, theme.success, theme.danger)
    };
    let key = tool_key(tool);
    let toggle = toggle.clone();
    let (good, bad) = (readable(success, cx), readable(danger, cx));
    let title = tool.command.clone().filter(|command| !command.is_empty() && tool.title == *command).unwrap_or(tool.title.clone());
    let result = tool.result.as_deref();
    let refused = result.and_then(|result| result.refused);
    let diff = result.and_then(|result| result.diff.as_ref());
    let exit = result.and_then(|result| result.exit.as_ref()).map(|exit| exit_label(exit, locale));
    let badge = |text: SharedString, color: Hsla| div().flex_shrink_0().text_xs().text_color(color).child(text);
    let chevron = animated_chevron(SharedString::from(format!("tool-chev-{}", tool.call_id)), open, window, cx);
    let reveal = reveal_progress(SharedString::from(format!("tool-reveal-{}", tool.call_id)), open, window, cx);
    let body = open.then(|| {
        let mut body = v_flex().pl_5().border_l_1().border_color(border).gap_1().text_xs().text_color(muted);
        let undoable = result.is_some_and(|result| result.undoable && !result.failed) && !tool.reverted;
        if tool.file.is_some() || undoable {
            let mut line = h_flex().w_full().gap_2().items_center();
            if let Some((shown, target)) = tool.file.clone() {
                let hint = tr(cx, "chat.openInEditor");
                line = line.child(
                    div()
                        .id(ElementId::Name(format!("tool-path-{}", tool.call_id).into()))
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(link)
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx))
                        .on_click(move |_, window, cx| open_file(&target, window, cx))
                        .child(shown),
                );
            } else {
                line = line.child(div().flex_1());
            }
            if undoable {
                let session = style.session.clone();
                let busy = style.chat_busy || session.is_none();
                let changes = result.map(|result| result.undo.clone()).unwrap_or_default();
                line = line.child(
                    Button::new(ElementId::Name(format!("tool-undo-{}", tool.call_id).into()))
                        .ghost()
                        .xsmall()
                        .label(tr(cx, "chat.undoChange"))
                        .disabled(busy)
                        .when(busy, |this| this.tooltip(tr(cx, "review.changesBusy")))
                        .on_click(move |_, window, cx| {
                            if let Some(session) = session.clone() {
                                undo_change(session, changes.clone(), window, cx);
                            }
                        }),
                );
            }
            body = body.child(line);
        }
        if let Some(command) = &tool.command {
            body = body.child(div().font_family(mono.clone()).text_color(muted).child(format!("$ {command}")));
        }
        let plan = result.and_then(|result| result.plan.as_ref());
        if let Some(refused) = refused {
            body = body.child(div().text_color(muted).child(tr(cx, if refused == Refusal::User { "chat.toolRefusedUser" } else { "chat.toolRefusedMode" })));
        } else if let Some(plan) = plan {
            body = body.child(v_flex().w_full().gap_0p5().text_sm().children(plan.iter().map(|(text, status)| {
                let (mark, color) = match status.as_ref() {
                    "completed" => ("✓", good),
                    "in_progress" => ("▸", foreground),
                    "cancelled" => ("✕", muted),
                    _ => ("○", muted),
                };
                let finished = matches!(status.as_ref(), "completed" | "cancelled");
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_start()
                    .child(div().flex_shrink_0().w_4().text_color(color).child(mark))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(finished, |this| this.text_color(muted).line_through())
                            .when(status.as_ref() == "in_progress", |this| this.font_semibold())
                            .child(text.clone()),
                    )
            })));
        } else if let Some(agent) = tool.subagent.as_deref() {
            body = body.child(subagent_body(tool, agent, cx));
        } else if let Some(diff) = diff {
            body = body.child(v_flex().w_full().font_family(mono.clone()).children(diff.lines.iter().map(|(kind, text)| {
                let tint = match kind {
                    LineKind::Added => Some(success.opacity(0.16)),
                    LineKind::Removed => Some(danger.opacity(0.16)),
                    LineKind::Context | LineKind::Gap | LineKind::File => None,
                };
                div()
                    .px_1()
                    .when_some(tint, |this, tint| this.bg(tint))
                    .when(*kind == LineKind::Gap, |this| this.text_color(muted))
                    .when(*kind == LineKind::File, |this| this.mt_1().font_semibold().text_color(foreground))
                    .child(text.clone())
            })));
            if diff.hidden > 0 {
                body = body.child(div().text_color(muted).child(trf(cx, "chat.linesHidden", &[("count", &diff.hidden)])));
            }
        } else {
            let (output, hidden) = match &tool.clipped_output {
                Some((clipped, hidden)) => (clipped.clone(), *hidden),
                None => (SharedString::default(), 0),
            };
            let output: SharedString = if output.trim().is_empty() && tool.state != ToolState::Running {
                i18n::text(locale, "chat.noOutput").into()
            } else {
                output
            };
            let failed = tool.state == ToolState::Failed && tool.command.is_none();
            body = body.child(div().font_family(mono.clone()).whitespace_normal().when(failed, |this| this.text_color(bad)).child(output));
            if hidden > 0 {
                body = body.child(div().text_color(muted).child(trf(cx, "chat.linesHidden", &[("count", &hidden)])));
            }
        }
        if let Some(result) = result {
            body = body.children(result.images.iter().map(|image| img(image.clone()).max_w_full().max_h(px(240.)).object_fit(ObjectFit::ScaleDown)));
        }
        if let Some((label, notable)) = exit.clone() {
            body = body.child(div().text_color(if notable { bad } else { muted }).child(label));
        }
        apply_reveal(reveal, body, cx)
    });
    // One quiet line, as thinking is: muted text on the chat's own background, details behind the chevron.
    v_flex()
        .w_full()
        .gap_1()
        .child(
            h_flex()
                .id(ElementId::Name(format!("tool-head-{}", tool.call_id).into()))
                .max_w_full()
                .gap_1p5()
                .items_center()
                .text_sm()
                .text_color(muted)
                .cursor_pointer()
                .hover(move |style| style.text_color(foreground))
                .on_click(move |_, window, cx| toggle(key.clone(), row, window, cx))
                .children(state_mark(&tool.call_id, tool.state, cx))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .when(tool.command.is_some(), |this| this.font_family(mono.clone()))
                        .child(title),
                )
                .when_some(tool.subagent.as_deref().filter(|agent| agent.calls > 0), |this, agent| {
                    this.child(badge(trf(cx, "chat.subagentCalls", &[("count", &agent.calls)]), muted))
                })
                .when(tool.subagent.as_deref().is_some_and(|agent| agent.stopped.is_some()), |this| {
                    this.child(badge(tr(cx, "chat.subagentLimited"), crate::theme::warning_text(cx)))
                })
                .when(tool.state == ToolState::Waiting, |this| this.child(badge(tr(cx, "chat.approvalWaiting"), crate::theme::warning_text(cx))))
                .when(refused.is_some(), |this| this.child(badge(tr(cx, "chat.toolDenied"), muted)))
                .when(tool.reverted, |this| this.child(badge(tr(cx, "chat.undone"), muted)))
                .when_some(diff, |this, diff| {
                    this.child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_1()
                            .text_xs()
                            .child(div().text_color(good).child(format!("+{}", diff.added)))
                            .child(div().text_color(bad).child(format!("−{}", diff.removed))),
                    )
                })
                .when_some(exit.filter(|(_, notable)| *notable), |this, (label, _)| this.child(badge(label.into(), bad)))
                .when(matches!(result.and_then(|result| result.exit.as_ref()), Some(Exit::Background)), |this| {
                    this.child(badge(tr(cx, "chat.runningInBackground"), muted))
                })
                .child(chevron),
        )
        .children(body)
        .into_any_element()
}
