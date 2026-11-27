//! The transcript of one chat: turns built from the chat state, and how each turn is drawn
//! (`Transcript.tsx` and `ToolCard.tsx`).

mod builder;
mod model;
mod tool_card;

use std::time::{Duration, Instant};

use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, Size, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::i18n::{self, Locale};
use crate::ui::{format_duration, format_tokens, motion};

#[allow(unused_imports)]
pub use builder::{build_rows, rebuild_rows};
#[allow(unused_imports)]
pub use model::{
    Block, Diff, Exit, Expanded, Footer, Fork, LineKind, Refusal, ResultView, RowSource, SubagentView, Toggle, ToolRow, ToolState, TurnRow, TurnStyle,
};
use tool_card::{render_tool, tool_key};

/// Frame-capped synced spinner (30 fps) delegated to [`motion::spinner_with_id`].
pub(super) fn limited_spinner(id: impl Into<ElementId>, color: Hsla, cx: &App) -> AnyElement {
    motion::spinner_with_id(id, Size::XSmall, Some(color), cx)
}

/// Smooth 90-degree chevron rotation (140 ms, 0 fps when settled, obeys `cx.reduce_motion()`).
pub(super) fn animated_chevron(id: SharedString, open: bool, window: &mut Window, cx: &mut App) -> Icon {
    let target = if open { std::f32::consts::FRAC_PI_2 } else { 0.0 };
    let angle = transition(id, target, Transition::new(Duration::from_millis(140)), window, cx);
    Icon::new(IconName::ChevronRight).xsmall().rotate(radians(angle))
}

/// Track a collapsible block's open reveal progress (130 ms, 0 fps at rest) on the always-mounted header so
/// repeatedly toggling the same block replays its gentle fade + 4 px slide every time without animating height.
pub(super) fn reveal_progress(id: SharedString, open: bool, window: &mut Window, cx: &mut App) -> f32 {
    transition(id, if open { 1.0 } else { 0.0 }, Transition::new(Duration::from_millis(130)), window, cx)
}

/// Wrap an expanded body with the sampled reveal progress (no dynamic height animation inside virtualized `ListState`).
pub(super) fn apply_reveal(progress: f32, element: impl IntoElement + 'static, cx: &App) -> AnyElement {
    if cx.reduce_motion() || progress >= 0.999 {
        return element.into_any_element();
    }
    div().relative().top(px(4.0 * (1.0 - progress))).opacity(progress).child(element).into_any_element()
}

/// A status color as text: the theme's green and red are fill colors, too light on a light surface.
pub(super) fn readable(color: Hsla, cx: &App) -> Hsla {
    if cx.theme().mode.is_dark() { color } else { Hsla { l: color.l.min(0.32), ..color } }
}

/// Rainy's mark beside a thinking block; it breathes at a capped synced 24 fps while thinking. Reduced motion holds it still.
fn thinking_mark(key: &SharedString, live: bool, cx: &App) -> AnyElement {
    let mark = img("brand/rainy-mark.svg").size(px(14.)).flex_shrink_0();
    if !live || cx.reduce_motion() {
        return mark.into_any_element();
    }
    mark.with_animation(
        ElementId::Name(format!("think-mark-{key}").into()),
        Animation::new(Duration::from_millis(1600))
            .repeat_synced()
            .with_max_fps(24.0)
            .with_easing(pulsating_between(0.45, 1.0)),
        |mark, delta| mark.opacity(delta),
    )
    .into_any_element()
}

struct ThinkingRender<'a> {
    key: &'a SharedString,
    prepared: &'a SharedString,
    redacted: bool,
    live: bool,
    duration_ms: Option<f64>,
    started_at: Option<Instant>,
    open: bool,
    row: usize,
    toggle: &'a Toggle,
    locale: Locale,
}

fn render_thinking(input: ThinkingRender<'_>, window: &mut Window, cx: &mut App) -> AnyElement {
    let ThinkingRender { key, prepared, redacted, live, duration_ms, started_at, open, row, toggle, locale } = input;
    let live_ms = if live {
        started_at.map(|since| since.elapsed().as_millis() as f64).or(duration_ms)
    } else {
        duration_ms
    };
    let label = if live {
        match live_ms {
            Some(ms) => i18n::format(locale, "chat.thinkingRunningFor", &[("duration", &format_duration(ms))]),
            None => i18n::text(locale, "chat.thinkingRunning").to_owned(),
        }
    } else if let Some(ms) = live_ms {
        i18n::format(locale, "chat.thinkingDoneFor", &[("duration", &format_duration(ms))])
    } else {
        i18n::text(locale, "chat.thinkingDone").to_owned()
    };
    let (muted_foreground, foreground, border) = {
        let theme = cx.theme();
        (theme.muted_foreground, theme.foreground, theme.border)
    };
    let chevron = animated_chevron(SharedString::from(format!("think-chev-{key}")), open, window, cx);
    let reveal = reveal_progress(SharedString::from(format!("think-reveal-{key}")), open, window, cx);
    let body = open.then(|| {
        let content = div()
            .pl_5()
            .border_l_1()
            .border_color(border)
            .text_sm()
            .text_color(muted_foreground)
            .child(if redacted {
                div().child(i18n::text(locale, "chat.thinkingRedacted")).into_any_element()
            } else {
                crate::ui::markdown::markdown_prepared(ElementId::Name(format!("think-md-{key}").into()), prepared, live).into_any_element()
            });
        apply_reveal(reveal, content, cx)
    });
    let toggle = toggle.clone();
    let key_for_click = key.clone();
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .id(ElementId::Name(format!("think-{key}").into()))
                .gap_1p5()
                .items_center()
                .text_sm()
                .text_color(muted_foreground)
                .cursor_pointer()
                .hover(move |style| style.text_color(foreground))
                .on_click(move |_, window, cx| toggle(key_for_click.clone(), row, window, cx))
                .child(thinking_mark(key, live, cx))
                .child(label)
                .child(chevron),
        )
        .children(body)
        .into_any_element()
}

/// Borrowed callbacks and styling passed into [`render_turn`].
pub struct TurnRender<'a> {
    pub expanded: &'a Expanded,
    pub toggle: &'a Toggle,
    pub fork: &'a Fork,
    pub style: &'a TurnStyle,
}

/// Draw one turn.
pub fn render_turn(
    row: &TurnRow,
    index: usize,
    ctx: &TurnRender<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let TurnRender { expanded, toggle, fork, style } = *ctx;
    let (locale, show_usage) = (style.locale, style.show_usage);
    let (radius_lg, radius, muted, muted_foreground, foreground, danger) = {
        let theme = cx.theme();
        (theme.radius_lg, theme.radius, theme.muted, theme.muted_foreground, theme.foreground, theme.danger)
    };
    let mut column = v_flex().w_full().gap_3().px_4().py_2();
    if let Some((text, images)) = &row.user {
        let message_view = match &row.user_html {
            Some(html) => crate::ui::markdown::typed_html(ElementId::Name(format!("user-{}", row.key).into()), html),
            None => crate::ui::markdown::typed_text(ElementId::Name(format!("user-{}", row.key).into()), text),
        };
        column = column.child(
            h_flex().w_full().justify_end().child(
                v_flex()
                    .max_w(relative(0.85))
                    .px_3()
                    .py_2()
                    .rounded(radius_lg)
                    .bg(muted)
                    .child(message_view)
                    .when(*images > 0, |this| {
                        this.child(div().text_xs().text_color(muted_foreground).child(format!("🖼 × {images}")))
                    }),
            ),
        );
    }
    // A finished turn's steps fold behind one line in the compact style.
    let foldable = style.step_detail == "compact" && !row.live && row.work_steps > 0;
    let fold_key: SharedString = format!("steps:{}", row.key).into();
    let steps_open = !foldable || expanded.contains(&fold_key);
    if foldable {
        let label = match &row.footer {
            Some(footer) => i18n::format(locale, "chat.workedFor", &[("duration", &format_duration(footer.duration_ms))]),
            None => i18n::format(locale, "chat.stepsSummary", &[("steps", &row.work_steps)]),
        };
        let toggle = toggle.clone();
        let key = fold_key.clone();
        let chevron = animated_chevron(SharedString::from(format!("fold-chev-{}", row.key)), steps_open, window, cx);
        column = column.child(
            h_flex()
                .id(ElementId::Name(format!("fold-{}", row.key).into()))
                .gap_2()
                .items_center()
                .text_sm()
                .text_color(muted_foreground)
                .cursor_pointer()
                .hover(move |style| style.text_color(foreground))
                .on_click(move |_, window, cx| toggle(key.clone(), index, window, cx))
                .child(label)
                .when(!row.summary.is_empty(), |this| this.child(div().text_xs().child(row.summary.clone())))
                .child(chevron),
        );
    }
    let detailed = style.step_detail == "detailed";
    let last_text_pos = row.live.then(|| {
        row.blocks.iter().rposition(|block| matches!(block, Block::Text { .. }))
    }).flatten();
    for (position, block) in row.blocks.iter().enumerate() {
        if position < row.answer_from && !steps_open {
            continue;
        }
        let element = match block {
            Block::Thinking { key, prepared, redacted, live, duration_ms, started_at, .. } => {
                let open = *live || expanded.contains(key);
                render_thinking(
                    ThinkingRender {
                        key,
                        prepared,
                        redacted: *redacted,
                        live: *live,
                        duration_ms: *duration_ms,
                        started_at: *started_at,
                        open,
                        row: index,
                        toggle,
                        locale,
                    },
                    window,
                    cx,
                )
            }
            Block::Text { key, prepared, .. } => {
                let live_fade = last_text_pos == Some(position);
                crate::ui::markdown::markdown_prepared(ElementId::Name(format!("md-{key}").into()), prepared, live_fade).into_any_element()
            }
            Block::Tool(tool) => {
                let key = tool_key(tool);
                // The detailed style opens cards; a click toggles away from the default either way.
                // Plan cards, working sub-agents, and streaming tool output open by default, yet remain foldable on click.
                let plan = tool.result.as_ref().is_some_and(|result| result.plan.is_some());
                let working_agent = tool.subagent.is_some() && tool.state == ToolState::Running;
                let streams = tool.subagent.is_none() && tool.state == ToolState::Running && tool.output.is_some();
                let open = expanded.contains(&key) != (detailed || plan || working_agent || streams);
                render_tool(tool, open, index, toggle, style, window, cx)
            }
            Block::Notice { level, text } => {
                let color = match level.as_ref() {
                    "error" => danger,
                    "warning" => crate::theme::warning_text(cx),
                    _ => muted_foreground,
                };
                div().text_sm().text_color(color).child(text.clone()).into_any_element()
            }
            Block::Muted { text } => div().text_xs().text_color(muted_foreground).child(text.clone()).into_any_element(),
            Block::Error { text } => v_flex()
                .gap_1()
                .p_3()
                .rounded(radius)
                .border_1()
                .border_color(danger)
                .text_sm()
                .child(div().font_semibold().text_color(danger).child(i18n::text(locale, "chat.errorTitle")))
                .child(div().child(text.clone()))
                .into_any_element(),
        };
        column = column.child(element);
    }
    if let Some(ms) = row.waiting_since.map(|since| since.elapsed().as_secs_f64() * 1000.).or(row.waiting) {
        // Shown from the start, with the time once it is worth counting.
        let name = if style.waiting_for.is_empty() { i18n::text(locale, "chat.waitingAgent").to_owned() } else { style.waiting_for.to_string() };
        let label = if ms >= 2000. {
            i18n::format(locale, "chat.waitingReplyFor", &[("name", &name), ("duration", &format_duration(ms))])
        } else {
            i18n::format(locale, "chat.waitingReply", &[("name", &name)])
        };
        column = column.child(
            h_flex()
                .gap_1p5()
                .items_center()
                .text_sm()
                .text_color(muted_foreground)
                .child(thinking_mark(&format!("wait-{}", row.key).into(), true, cx))
                .child(label),
        );
    }
    if row.compacting {
        column = column.child(
            h_flex()
                .gap_1p5()
                .items_center()
                .text_sm()
                .text_color(muted_foreground)
                .child(thinking_mark(&format!("compact-{}", row.key).into(), true, cx))
                .child(i18n::text(locale, "chat.compactingNow")),
        );
    }
    if row.footer.is_some() || row.copy.is_some() {
        let mut parts = Vec::new();
        if let Some(footer) = &row.footer {
            parts.push(i18n::format(locale, "chat.workedFor", &[("duration", &format_duration(footer.duration_ms))]));
            if show_usage {
                parts.push(i18n::format(locale, "chat.turnUsage", &[
                    ("input", &format_tokens(footer.usage.input)),
                    ("cache", &format_tokens(footer.usage.cache_read)),
                    ("output", &format_tokens(footer.usage.output)),
                ]));
            }
        }
        let copy = row.copy.clone();
        let copy_view = copy.is_some().then(|| window.current_view());
        let fork_from = row.footer.as_ref().map(|footer| footer.id.clone());
        let fork = fork.clone();
        column = column.child(
            h_flex()
                .gap_3()
                .items_center()
                .text_xs()
                .text_color(muted_foreground)
                .when_some(copy.zip(copy_view), |this, (copy, view)| {
                    let key = crate::ui::copy::key_of(&format!("reply:{}:{}", row.source.chat, row.key));
                    this.child(
                        div()
                            .id(ElementId::Name(format!("copy-{}", row.key).into()))
                            .cursor_pointer()
                            .hover(move |style| style.text_color(foreground))
                            .child(crate::ui::copy::indicator(key, cx))
                            .on_click(move |_, window, cx| crate::ui::copy::copy_in(key, &copy, view, window, cx)),
                    )
                })
                .when_some(fork_from, |this, entry| {
                    this.child(
                        div()
                            .id(ElementId::Name(format!("fork-{}", row.key).into()))
                            .cursor_pointer()
                            .hover(move |style| style.text_color(foreground))
                            .child(Icon::new(gpui_kit::assets::IconName::GitBranch).xsmall())
                            .on_click(move |_, window, cx| fork(entry.clone(), window, cx)),
                    )
                })
                .child(parts.join(" · ")),
        );
    }
    column.into_any_element()
}
