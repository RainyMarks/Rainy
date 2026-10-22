//! The transcript of one chat: turns built from the chat state, and how each turn is drawn
//! (`Transcript.tsx` and `ToolCard.tsx`).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, spinner::Spinner, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::StyledExt as _;
use rainy_protocol::{AssistantBlock, AssistantMessage, ResultContent, TranscriptEntry, UsageSummary};
use serde_json::{Map, Value};

use crate::i18n::{self, Locale};
use crate::store::chat::{ChatView, RunningTool, assistant_text, group_turns};
use crate::ui::tool_title::{is_command, tool_title};
use crate::ui::{format_duration, format_tokens};

/// What a tool call is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Waiting,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolRow {
    pub call_id: SharedString,
    pub name: SharedString,
    pub title: SharedString,
    pub command: Option<SharedString>,
    pub state: ToolState,
    pub output: Option<SharedString>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Thinking { key: SharedString, text: SharedString, redacted: bool, live: bool, duration_ms: Option<f64> },
    Text { key: SharedString, text: SharedString },
    Tool(ToolRow),
    Notice { level: SharedString, text: SharedString },
    Muted { text: SharedString },
    Error { text: SharedString },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Footer {
    /// The `turn` entry, where a fork starts.
    pub id: SharedString,
    pub duration_ms: f64,
    pub usage: UsageSummary,
    pub requests: f64,
}

/// One user message and everything the agent did for it.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnRow {
    pub key: SharedString,
    pub user: Option<(SharedString, usize)>,
    pub blocks: Vec<Block>,
    pub live: bool,
    pub footer: Option<Footer>,
    pub copy: Option<SharedString>,
    /// Blocks before this index are the steps the reply took; the rest is its answer.
    pub answer_from: usize,
    /// Model requests and tool results among the steps.
    pub work_steps: usize,
    /// What the steps did: files read, searches, changes, commands.
    pub summary: String,
}

fn result_text(content: &[ResultContent]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ResultContent::Text { text } => Some(text.as_str()),
            ResultContent::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

struct Context<'a> {
    locale: Locale,
    results: HashMap<&'a str, (String, bool)>,
    running: &'a std::collections::BTreeMap<String, RunningTool>,
    waiting: HashSet<&'a str>,
    thinking_since: Option<&'a (String, std::time::Instant)>,
}

fn assistant_blocks(ctx: &Context<'_>, key: &str, message: &AssistantMessage, live: bool, thinking_ms: Option<&Map<String, Value>>, out: &mut Vec<Block>) {
    let last = message.content.len().saturating_sub(1);
    for (index, block) in message.content.iter().enumerate() {
        let block_key: SharedString = format!("{key}:{index}").into();
        match block {
            AssistantBlock::Thinking { thinking, redacted, .. } => {
                let redacted = redacted.unwrap_or(false);
                if thinking.trim().is_empty() && !redacted {
                    continue;
                }
                let duration_ms = thinking_ms.and_then(|map| map.get(&index.to_string())).and_then(Value::as_f64).or_else(|| {
                    // A block still thinking counts from when it began.
                    let live_key = format!("{}:{index}", message.timestamp);
                    ctx.thinking_since.filter(|(key, _)| live && *key == live_key).map(|(_, since)| since.elapsed().as_millis() as f64)
                });
                out.push(Block::Thinking { key: block_key, text: thinking.clone().into(), redacted, live: live && index == last, duration_ms });
            }
            AssistantBlock::Text { text, .. } => {
                if !text.trim().is_empty() {
                    out.push(Block::Text { key: block_key, text: text.clone().into() });
                }
            }
            AssistantBlock::ToolCall { id, name, arguments, .. } => {
                let result = ctx.results.get(id.as_str());
                let running = ctx.running.get(id);
                let state = if ctx.waiting.contains(id.as_str()) {
                    ToolState::Waiting
                } else if let Some((_, is_error)) = result {
                    if *is_error { ToolState::Failed } else { ToolState::Done }
                } else if running.is_some() || live {
                    ToolState::Running
                } else {
                    ToolState::Failed
                };
                let output = result.map(|(text, _)| text.clone()).or_else(|| running.and_then(|tool| tool.partial.clone()));
                let command = is_command(name).then(|| arguments.get("command").and_then(Value::as_str).unwrap_or_default().to_owned().into());
                out.push(Block::Tool(ToolRow {
                    call_id: id.clone().into(),
                    name: name.clone().into(),
                    title: tool_title(ctx.locale, name, arguments).into(),
                    command,
                    state,
                    output: output.map(Into::into),
                }));
            }
        }
    }
    match message.stop_reason.as_str() {
        "error" => out.push(Block::Error { text: message.error_message.clone().unwrap_or_default().into() }),
        "aborted" => out.push(Block::Muted { text: i18n::text(ctx.locale, "chat.stopped").into() }),
        _ => {}
    }
}

/// Turns of a chat as the transcript shows them.
pub fn build_rows(view: &ChatView, locale: Locale) -> Vec<TurnRow> {
    let snapshot = &view.snapshot;
    let ctx = Context {
        locale,
        results: snapshot
            .entries
            .iter()
            .filter_map(|entry| match entry {
                TranscriptEntry::ToolResult { tool_call_id, content, is_error, .. } => {
                    Some((tool_call_id.as_str(), (result_text(content), *is_error)))
                }
                _ => None,
            })
            .collect(),
        running: &view.tools,
        waiting: snapshot.approvals.iter().map(|approval| approval.tool_call_id.as_str()).collect(),
        thinking_since: view.thinking_since.as_ref(),
    };
    let busy = view.busy();
    let turns = group_turns(&snapshot.entries);
    let count = turns.len();
    let mut rows: Vec<TurnRow> = turns
        .into_iter()
        .enumerate()
        .map(|(index, turn)| {
            let live = busy && index + 1 == count;
            let mut blocks = Vec::new();
            let mut footer = None;
            let mut copy = None;
            // The answer is the last reply when it called no tools, or when it failed or stopped.
            let answer = (!live)
                .then(|| {
                    turn.items.iter().rev().find_map(|item| match item {
                        TranscriptEntry::Assistant { id, message, .. } => Some((id.as_str(), message)),
                        _ => None,
                    })
                })
                .flatten()
                .filter(|(_, message)| {
                    matches!(message.stop_reason.as_str(), "error" | "aborted")
                        || !message.content.iter().any(|block| matches!(block, AssistantBlock::ToolCall { .. }))
                })
                .map(|(id, _)| id);
            let mut answer_from = None;
            let (mut work_steps, mut reads, mut searches, mut changes, mut commands, mut others) = (0, 0, 0, 0, 0, 0);
            for item in &turn.items {
                match item {
                    TranscriptEntry::Assistant { id, .. } if Some(id.as_str()) == answer => {}
                    TranscriptEntry::Assistant { .. } => work_steps += 1,
                    TranscriptEntry::ToolResult { tool_name, .. } => {
                        work_steps += 1;
                        match tool_name.as_str() {
                            "read" => reads += 1,
                            "grep" | "glob" => searches += 1,
                            "write" | "edit" => changes += 1,
                            "bash" | "pwsh" => commands += 1,
                            _ => others += 1,
                        }
                    }
                    _ => {}
                }
            }
            let summary = [
                (reads, "chat.readFiles"),
                (searches, "chat.searched"),
                (changes, "chat.changedFiles"),
                (commands, "chat.ranCommands"),
                (others, "chat.calledTools"),
            ]
            .into_iter()
            .filter(|(count, _)| *count > 0)
            .map(|(count, key)| i18n::format(locale, key, &[("count", &count)]))
            .collect::<Vec<_>>()
            .join(" · ");
            for item in &turn.items {
                match item {
                    TranscriptEntry::Assistant { id, message, thinking_ms, .. } => {
                        if Some(id.as_str()) == answer {
                            answer_from = Some(blocks.len());
                        }
                        assistant_blocks(&ctx, id, message, false, thinking_ms.as_ref(), &mut blocks);
                        let text = assistant_text(message);
                        if !text.is_empty() {
                            copy = Some(text.into());
                        }
                    }
                    TranscriptEntry::Notice { level, text, .. } => {
                        blocks.push(Block::Notice { level: level.clone().into(), text: text.clone().into() });
                    }
                    TranscriptEntry::Compaction { tokens_before, trigger, .. } => {
                        let key = if trigger == "manual" { "chat.compactionManual" } else { "chat.compaction" };
                        let text = i18n::format(locale, key, &[("tokens", &format_tokens(*tokens_before))]);
                        blocks.push(Block::Muted { text: text.into() });
                    }
                    TranscriptEntry::Context { label, .. } => {
                        let key = match label.as_str() {
                            "instructions" => "chat.contextInstructions",
                            "memory" => "chat.contextMemory",
                            "reference" => "chat.contextReference",
                            _ => "chat.contextNotice",
                        };
                        blocks.push(Block::Muted { text: i18n::text(locale, key).into() });
                    }
                    TranscriptEntry::Turn { id, duration_ms, usage, requests, .. } => {
                        footer = Some(Footer { id: id.clone().into(), duration_ms: *duration_ms, usage: usage.clone(), requests: *requests });
                    }
                    TranscriptEntry::User { .. } | TranscriptEntry::ToolResult { .. } => {}
                }
            }
            let (key, user) = match turn.user {
                Some(TranscriptEntry::User { id, text, images, .. }) => {
                    (id.clone().into(), Some((text.clone().into(), images.as_ref().map_or(0, Vec::len))))
                }
                _ => ("lead".into(), None),
            };
            let answer_from = answer_from.unwrap_or(blocks.len());
            TurnRow { key, user, blocks, live, footer: footer.filter(|_| !live), copy: copy.filter(|_| !live), answer_from, work_steps, summary }
        })
        .collect();
    if let Some(streaming) = &snapshot.streaming {
        if rows.is_empty() {
            rows.push(TurnRow {
                key: "lead".into(),
                user: None,
                blocks: Vec::new(),
                live: true,
                footer: None,
                copy: None,
                answer_from: 0,
                work_steps: 0,
                summary: String::new(),
            });
        }
        let last = rows.last_mut().expect("a row");
        assistant_blocks(&ctx, "stream", streaming, true, None, &mut last.blocks);
        last.answer_from = last.blocks.len();
    }
    rows
}

/// Expanded tool cards and thinking blocks, by key.
pub type Expanded = Rc<HashSet<SharedString>>;

/// Called with a key when a fold is clicked, and the row index to measure again.
pub type Toggle = Rc<dyn Fn(SharedString, usize, &mut Window, &mut App)>;

fn chevron(open: bool) -> Icon {
    Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall()
}

fn render_thinking(
    key: &SharedString, text: &SharedString, redacted: bool, live: bool, duration_ms: Option<f64>,
    open: bool, row: usize, toggle: &Toggle, locale: Locale, cx: &App,
) -> AnyElement {
    let label = if live {
        match duration_ms {
            Some(ms) => i18n::format(locale, "chat.thinkingRunningFor", &[("duration", &format_duration(ms))]),
            None => i18n::text(locale, "chat.thinkingRunning").to_owned(),
        }
    } else if let Some(ms) = duration_ms {
        i18n::format(locale, "chat.thinkingDoneFor", &[("duration", &format_duration(ms))])
    } else {
        i18n::text(locale, "chat.thinkingDone").to_owned()
    };
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
                .text_color(cx.theme().muted_foreground)
                .cursor_pointer()
                .hover(|style| style.text_color(cx.theme().foreground))
                .on_click(move |_, window, cx| toggle(key_for_click.clone(), row, window, cx))
                .child(Icon::new(IconName::Bot).xsmall())
                .child(label)
                .when(live, |this| this.child(Spinner::new().xsmall()))
                .child(chevron(open)),
        )
        .when(open, |this| {
            this.child(
                div()
                    .pl_5()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if redacted {
                        div().child(i18n::text(locale, "chat.thinkingRedacted")).into_any_element()
                    } else {
                        crate::ui::markdown::markdown(ElementId::Name(format!("think-md-{key}").into()), text).into_any_element()
                    }),
            )
        })
        .into_any_element()
}

fn state_dot(state: ToolState, cx: &App) -> AnyElement {
    match state {
        ToolState::Running => Spinner::new().xsmall().into_any_element(),
        _ => {
            let color = match state {
                ToolState::Done => cx.theme().success,
                ToolState::Failed => cx.theme().danger,
                _ => cx.theme().warning,
            };
            div().size(px(7.)).rounded_full().bg(color).into_any_element()
        }
    }
}

/// At most this many lines of a tool's output are shown.
const OUTPUT_LINES: usize = 60;

fn clip_output(text: &str) -> (String, usize) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= OUTPUT_LINES {
        return (text.to_owned(), 0);
    }
    let head = lines[..OUTPUT_LINES / 2].join("\n");
    let tail = lines[lines.len() - OUTPUT_LINES / 2..].join("\n");
    (format!("{head}\n…\n{tail}"), lines.len() - OUTPUT_LINES)
}

fn render_tool(tool: &ToolRow, open: bool, row: usize, toggle: &Toggle, locale: Locale, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let key: SharedString = format!("tool:{}", tool.call_id).into();
    let toggle = toggle.clone();
    let mono = theme.mono_font_family.clone();
    let title = tool.command.clone().filter(|command| !command.is_empty() && tool.title == *command).unwrap_or(tool.title.clone());
    v_flex()
        .w_full()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .overflow_hidden()
        .child(
            h_flex()
                .id(ElementId::Name(format!("tool-head-{}", tool.call_id).into()))
                .w_full()
                .gap_2()
                .px_3()
                .py_1p5()
                .items_center()
                .text_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme.accent))
                .on_click(move |_, window, cx| toggle(key.clone(), row, window, cx))
                .child(state_dot(tool.state, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .when(tool.command.is_some(), |this| this.font_family(mono.clone()))
                        .child(title),
                )
                .when(tool.state == ToolState::Waiting, |this| {
                    this.child(div().text_xs().text_color(theme.warning).child(i18n::text(locale, "chat.approvalWaiting")))
                })
                .child(chevron(open)),
        )
        .when(open, |this| {
            let mut body = v_flex().px_3().pb_2().gap_1().text_xs().font_family(mono.clone());
            if let Some(command) = &tool.command {
                body = body.child(div().text_color(theme.muted_foreground).child(format!("$ {command}")));
            }
            let (output, hidden) = clip_output(tool.output.as_deref().unwrap_or_default());
            let output = if output.trim().is_empty() && tool.state != ToolState::Running {
                i18n::text(locale, "chat.noOutput").to_owned()
            } else {
                output
            };
            body = body.child(div().whitespace_normal().child(output));
            if hidden > 0 {
                body = body.child(div().text_color(theme.muted_foreground).child(format!("(+{hidden} lines)")));
            }
            this.child(body)
        })
        .into_any_element()
}

/// Start a new chat from a turn's `turn` entry.
pub type Fork = Rc<dyn Fn(SharedString, &mut Window, &mut App)>;

/// How a turn is drawn: the interface language and the chat preferences that shape it.
pub struct TurnStyle {
    pub locale: Locale,
    pub show_usage: bool,
    /// `compact` folds a finished turn's steps; `detailed` opens tool cards.
    pub step_detail: SharedString,
}

/// Draw one turn.
pub fn render_turn(row: &TurnRow, index: usize, expanded: &Expanded, toggle: &Toggle, fork: &Fork, style: &TurnStyle, cx: &App) -> AnyElement {
    let (locale, show_usage) = (style.locale, style.show_usage);
    let theme = cx.theme();
    let mut column = v_flex().w_full().gap_3().px_4().py_2();
    if let Some((text, images)) = &row.user {
        column = column.child(
            h_flex().w_full().justify_end().child(
                v_flex()
                    .max_w(relative(0.85))
                    .px_3()
                    .py_2()
                    .rounded(theme.radius_lg)
                    .bg(theme.muted)
                    .child(crate::ui::markdown::markdown(ElementId::Name(format!("user-{}", row.key).into()), text))
                    .when(*images > 0, |this| {
                        this.child(div().text_xs().text_color(theme.muted_foreground).child(format!("🖼 × {images}")))
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
        column = column.child(
            h_flex()
                .id(ElementId::Name(format!("fold-{}", row.key).into()))
                .gap_2()
                .items_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .hover(|style| style.text_color(theme.foreground))
                .on_click(move |_, window, cx| toggle(key.clone(), index, window, cx))
                .child(label)
                .when(!row.summary.is_empty(), |this| this.child(div().text_xs().child(row.summary.clone())))
                .child(chevron(steps_open)),
        );
    }
    let detailed = style.step_detail == "detailed";
    for (position, block) in row.blocks.iter().enumerate() {
        if position < row.answer_from && !steps_open {
            continue;
        }
        let element = match block {
            Block::Thinking { key, text, redacted, live, duration_ms } => {
                let open = *live || expanded.contains(key);
                render_thinking(key, text, *redacted, *live, *duration_ms, open, index, toggle, locale, cx)
            }
            Block::Text { key, text } => {
                crate::ui::markdown::markdown(ElementId::Name(format!("md-{key}").into()), text).into_any_element()
            }
            Block::Tool(tool) => {
                let key: SharedString = format!("tool:{}", tool.call_id).into();
                // The detailed style opens cards; a click toggles away from the default either way.
                let open = expanded.contains(&key) != detailed || (tool.state == ToolState::Running && tool.output.is_some());
                render_tool(tool, open, index, toggle, locale, cx)
            }
            Block::Notice { level, text } => {
                let color = match level.as_ref() {
                    "error" => theme.danger,
                    "warning" => theme.warning,
                    _ => theme.muted_foreground,
                };
                div().text_sm().text_color(color).child(text.clone()).into_any_element()
            }
            Block::Muted { text } => div().text_xs().text_color(theme.muted_foreground).child(text.clone()).into_any_element(),
            Block::Error { text } => v_flex()
                .gap_1()
                .p_3()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.danger)
                .text_sm()
                .child(div().font_semibold().text_color(theme.danger).child(i18n::text(locale, "chat.errorTitle")))
                .child(div().child(text.clone()))
                .into_any_element(),
        };
        column = column.child(element);
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
        let fork_from = row.footer.as_ref().map(|footer| footer.id.clone());
        let fork = fork.clone();
        column = column.child(
            h_flex()
                .gap_3()
                .items_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .when_some(copy, |this, copy| {
                    this.child(
                        div()
                            .id(ElementId::Name(format!("copy-{}", row.key).into()))
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme.foreground))
                            .child(Icon::new(IconName::Copy).xsmall())
                            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.to_string()))),
                    )
                })
                .when_some(fork_from, |this, entry| {
                    this.child(
                        div()
                            .id(ElementId::Name(format!("fork-{}", row.key).into()))
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme.foreground))
                            .child(Icon::new(gpui_kit::assets::IconName::GitBranch).xsmall())
                            .on_click(move |_, window, cx| fork(entry.clone(), window, cx)),
                    )
                })
                .child(parts.join(" · ")),
        );
    }
    column.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui_kit::*` brings GPUI's own `test` macro; use Rust's.
    use ::core::prelude::v1::test;
    use serde_json::json;

    fn view(entries: Value, streaming: Value, status: &str) -> ChatView {
        ChatView::new(
            serde_json::from_value(json!({
                "summary": {"id": "s", "workspaceId": null, "cwd": "/p", "title": "t", "createdAt": 1, "updatedAt": 2,
                            "archived": false, "pinned": false, "status": status},
                "entries": entries, "model": null, "queue": [], "context": null, "streaming": streaming,
                "runningTools": [], "permission": "ask", "approvals": [], "background": []
            }))
            .unwrap(),
            true,
        )
    }

    fn assistant(id: &str, content: Value, stop: &str) -> Value {
        json!({"id": id, "kind": "assistant", "ts": 2, "message": {"role": "assistant", "content": content, "api": "x",
            "provider": "p", "model": "m", "stopReason": stop, "timestamp": 2,
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                      "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}}}})
    }

    #[test]
    fn builds_turns_with_tools_and_footer() {
        let entries = json!([
            {"id": "u1", "kind": "user", "ts": 1, "text": "读 a.txt"},
            assistant("a1", json!([{"type": "thinking", "thinking": "想想"}, {"type": "toolCall", "id": "c1", "name": "read", "arguments": {"file_path": "a.txt"}}]), "toolUse"),
            {"id": "r1", "kind": "toolResult", "ts": 3, "toolCallId": "c1", "toolName": "read", "content": [{"type": "text", "text": "hello"}], "isError": false},
            assistant("a2", json!([{"type": "text", "text": "内容是 hello"}]), "stop"),
            {"id": "t1", "kind": "turn", "ts": 4, "durationMs": 3200, "usage": {"input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0}, "provider": "p", "model": "m", "requests": 2}
        ]);
        let rows = build_rows(&view(entries, Value::Null, "idle"), Locale::Zh);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.user.as_ref().unwrap().0, "读 a.txt");
        assert!(matches!(&row.blocks[0], Block::Thinking { .. }));
        let Block::Tool(tool) = &row.blocks[1] else { panic!("tool") };
        assert_eq!((tool.title.as_ref(), tool.state, tool.output.as_deref()), ("读取 a.txt", ToolState::Done, Some("hello")));
        assert!(matches!(&row.blocks[2], Block::Text { text, .. } if text == "内容是 hello"));
        assert_eq!(row.footer.as_ref().unwrap().duration_ms, 3200.0);
        assert_eq!(row.copy.as_deref(), Some("内容是 hello"));
    }

    #[test]
    fn appends_the_streaming_reply_to_the_last_turn() {
        let entries = json!([{"id": "u1", "kind": "user", "ts": 1, "text": "hi"}]);
        let streaming = assistant("x", json!([{"type": "text", "text": "正在"}]), "pending")["message"].clone();
        let rows = build_rows(&view(entries, streaming, "running"), Locale::Zh);
        assert!(rows[0].live);
        assert!(matches!(&rows[0].blocks[0], Block::Text { key, .. } if key == "stream:0"));
    }

    #[test]
    fn clips_long_output() {
        let text = (0..100).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let (clipped, hidden) = clip_output(&text);
        assert_eq!(hidden, 40);
        assert!(clipped.starts_with("0\n") && clipped.ends_with("\n99"));
    }
}
