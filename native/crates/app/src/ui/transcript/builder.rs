//! Incremental turn-row builder: reuses unchanged turns, finished tool cards, decoded results, and pre-normalized text.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use gpui_kit::*;
use rainy_protocol::{AssistantBlock, AssistantMessage, ResultContent, TranscriptEntry};
use serde_json::{Map, Value};

use crate::i18n::{self, Locale};
use crate::store::chat::{ChatView, RunningTool, TurnRange, assistant_text};
use crate::ui::format_tokens;
use crate::ui::markdown::{prepare_markdown, prepare_typed_html};
use crate::ui::tool_title::{is_command, patch_files, relative_to, tool_title_in};

use super::model::{
    Block, DIFF_CONTEXT, DIFF_TIMEOUT, Diff, Exit, Footer, LINE_CHARS, LineKind, OUTPUT_LINES, Refusal, ResultView, RowSource, SubagentView, ToolRow,
    ToolState, TurnRow,
};

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

fn image_format(mime: &str) -> ImageFormat {
    match mime {
        "image/png" => ImageFormat::Png,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        "image/bmp" => ImageFormat::Bmp,
        "image/svg+xml" => ImageFormat::Svg,
        _ => ImageFormat::Jpeg,
    }
}

/// Commands, whose cards show how they ended.
fn is_terminal(name: &str) -> bool {
    is_command(name) || matches!(name, "command_output" | "stop_command")
}

fn clip_line(text: &str) -> String {
    match text.char_indices().nth(LINE_CHARS) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

pub(super) fn clip_output(text: &str) -> (String, usize) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= OUTPUT_LINES {
        return (text.to_owned(), 0);
    }
    let head = lines[..OUTPUT_LINES / 2].join("\n");
    let tail = lines[lines.len() - OUTPUT_LINES / 2..].join("\n");
    (format!("{head}\n…\n{tail}"), lines.len() - OUTPUT_LINES)
}

fn diff_of(before: &str, after: &str) -> Diff {
    let diff = similar::TextDiff::configure().timeout(DIFF_TIMEOUT).diff_lines(before, after);
    let (mut added, mut removed) = (0, 0);
    for op in diff.ops() {
        let (_, old, new) = op.as_tag_tuple();
        match op.tag() {
            similar::DiffTag::Delete => removed += old.len(),
            similar::DiffTag::Insert => added += new.len(),
            similar::DiffTag::Replace => {
                removed += old.len();
                added += new.len();
            }
            similar::DiffTag::Equal => {}
        }
    }
    let (mut lines, mut hidden) = (Vec::new(), 0);
    for (index, group) in diff.grouped_ops(DIFF_CONTEXT).iter().enumerate() {
        if index > 0 && lines.len() < OUTPUT_LINES {
            lines.push((LineKind::Gap, SharedString::new_static("⋯")));
        }
        for op in group {
            for change in diff.iter_changes(op) {
                if lines.len() >= OUTPUT_LINES {
                    hidden += 1;
                    continue;
                }
                let (kind, mark) = match change.tag() {
                    similar::ChangeTag::Delete => (LineKind::Removed, '-'),
                    similar::ChangeTag::Insert => (LineKind::Added, '+'),
                    similar::ChangeTag::Equal => (LineKind::Context, ' '),
                };
                lines.push((kind, format!("{mark} {}", clip_line(change.value().trim_end_matches(['\n', '\r']))).into()));
            }
        }
    }
    Diff { added, removed, lines, hidden }
}

/// A `toolResult` entry of the turn.
struct ResultEntry<'a> {
    id: &'a str,
    call_id: &'a str,
    tool_name: &'a str,
    content: &'a [ResultContent],
    is_error: bool,
    details: Option<&'a Value>,
}

/// The diff of each file an `apply_patch` call changed, under a line naming the file.
fn patch_diff(files: &[Value], locale: Locale) -> Option<Diff> {
    let mut combined = Diff { added: 0, removed: 0, lines: Vec::new(), hidden: 0 };
    for file in files {
        let path = file["path"].as_str().unwrap_or_default();
        let Some(after) = file["after"].as_str() else { continue };
        let note = match (file["operation"].as_str(), file["movedFrom"].as_str()) {
            (_, Some(from)) => format!(" · {}", i18n::format(locale, "chat.patchMovedFrom", &[("from", &from)])),
            (Some("create"), _) => format!(" · {}", i18n::text(locale, "chat.patchCreated")),
            (Some("delete"), _) => format!(" · {}", i18n::text(locale, "chat.patchDeleted")),
            _ => String::new(),
        };
        let diff = diff_of(file["before"].as_str().unwrap_or_default(), after);
        combined.added += diff.added;
        combined.removed += diff.removed;
        if files.len() > 1 {
            if combined.lines.len() < OUTPUT_LINES {
                combined.lines.push((LineKind::File, format!("{path}{note}").into()));
            } else {
                combined.hidden += 1;
            }
        }
        for line in diff.lines {
            if combined.lines.len() < OUTPUT_LINES {
                combined.lines.push(line);
            } else {
                combined.hidden += 1;
            }
        }
        combined.hidden += diff.hidden;
    }
    (!combined.lines.is_empty() || combined.added + combined.removed > 0).then_some(combined)
}

fn result_view(result: &ResultEntry<'_>, locale: Locale) -> ResultView {
    let details = result.details.and_then(Value::as_object);
    let field = |key: &str| details.and_then(|details| details.get(key));
    let string = |key: &str| field(key).and_then(Value::as_str);
    let refused = match string("refused") {
        Some("user") => Some(Refusal::User),
        Some("mode") => Some(Refusal::Mode),
        _ => None,
    };
    let files: &[Value] = field("files").and_then(Value::as_array).map_or(&[], Vec::as_slice);
    let diff = match (result.tool_name, string("after")) {
        _ if result.is_error || refused.is_some() => None,
        ("write" | "edit", Some(after)) => Some(diff_of(string("before").unwrap_or_default(), after)),
        ("apply_patch", _) => patch_diff(files, locale),
        _ => None,
    };
    let undoable_field = field("undoable").and_then(Value::as_bool) == Some(true);
    let (changes, undo): (Vec<SharedString>, Vec<SharedString>) = match result.tool_name {
        _ if result.is_error || refused.is_some() => (Vec::new(), Vec::new()),
        "write" | "edit" => {
            let id: SharedString = result.call_id.to_owned().into();
            (vec![id.clone()], if undoable_field { vec![id] } else { Vec::new() })
        }
        "apply_patch" => {
            let id = |file: &Value| file["changeId"].as_str().map(|id| SharedString::from(id.to_owned()));
            (files.iter().filter_map(id).collect(), files.iter().filter(|file| file["undoable"].as_bool() == Some(true)).filter_map(id).collect())
        }
        _ => (Vec::new(), Vec::new()),
    };
    let plan = (matches!(result.tool_name, "todo_write" | "update_plan" | "write_todos") && !result.is_error).then(|| {
        field("plan")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|item| (SharedString::from(item["text"].as_str().unwrap_or_default().to_owned()), SharedString::from(item["status"].as_str().unwrap_or("pending").to_owned())))
            .collect()
    });
    let exit = (is_terminal(result.tool_name) && details.is_some() && refused.is_none()).then(|| {
        match (field("exitCode").and_then(Value::as_i64), string("signal")) {
            (Some(code), _) => Exit::Code(code),
            (None, Some(signal)) => Exit::Signal(signal.to_owned().into()),
            // A command left running in the background has no exit code yet, which is not a failure.
            _ if string("state") == Some("running") || (field("jobId").is_some() && string("state") != Some("exited")) => Exit::Background,
            _ => Exit::Unknown,
        }
    });
    let engine = base64::engine::general_purpose::STANDARD;
    let images = result
        .content
        .iter()
        .filter_map(|block| match block {
            ResultContent::Image { data, mime_type } => Some(Arc::new(Image::from_bytes(image_format(mime_type), engine.decode(data).ok()?))),
            ResultContent::Text { .. } => None,
        })
        .collect();
    ResultView {
        entry: result.id.to_owned().into(),
        text: result_text(result.content).into(),
        diff,
        images,
        exit,
        refused,
        undoable: !undo.is_empty(),
        changes,
        undo,
        plan,
        failed: result.is_error,
    }
}

/// A path inside the chat's folder as the line shows it, relative to the folder; others as they are.
fn shown_path(cwd: &str, path: &str) -> Option<String> {
    relative_to(path, cwd)
}

/// A sub-agent's steps from a `task` call's `details`.
fn subagent_view(locale: Locale, cwd: &str, details: &Value) -> SubagentView {
    let empty = Map::new();
    let steps: Vec<(SharedString, ToolState)> = details["activity"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|call| match call {
            // Chats from before 4.13 kept only each step's tool.
            Value::String(name) => (SharedString::from(name.clone()), ToolState::Done),
            _ => {
                let name = call["name"].as_str().unwrap_or_default();
                let arguments = call["args"].as_object().unwrap_or(&empty);
                let state = match call["state"].as_str() {
                    Some("running") => ToolState::Running,
                    Some("failed") => ToolState::Failed,
                    _ => ToolState::Done,
                };
                (tool_title_in(locale, name, arguments, Some(cwd)).into(), state)
            }
        })
        .collect();
    let calls = details["calls"].as_u64().map_or(0, |calls| calls as usize).max(steps.len());
    SubagentView {
        earlier: calls - steps.len(),
        calls,
        steps,
        model: details["model"].as_str().map(|model| model.to_owned().into()),
        stopped: details["stopped"].as_str().map(|stopped| stopped.to_owned().into()),
    }
}

/// A path as the Host reaches it: relative ones are in the chat's folder.
fn host_path(cwd: &str, path: &str) -> String {
    let absolute = path.starts_with('/') || path.starts_with('\\') || path.as_bytes().get(1) == Some(&b':');
    if absolute || cwd.is_empty() { path.to_owned() } else { format!("{}/{}", cwd.trim_end_matches(['/', '\\']), path.trim_start_matches("./")) }
}

struct BuildContext<'a> {
    locale: Locale,
    cwd: &'a str,
    running: &'a BTreeMap<String, RunningTool>,
    waiting: HashSet<&'a str>,
    /// Calls the user undid, from the `context` entries that record it.
    reverted: &'a HashSet<String>,
    thinking_since: Option<&'a (String, Instant)>,
    /// The chat runs a turn (not a compaction), and when it last changed.
    turn_running: bool,
    changed_at: Instant,
    compacting: bool,
}

/// Parts of an earlier build of a turn that come out the same: finished text/thinking blocks,
/// finished `ToolRow`s, decoded `ResultView`s, and already-settled preceding blocks inside a live stream.
#[derive(Default)]
struct Reuse<'a> {
    blocks: HashMap<&'a str, &'a Block>,
    stream_blocks: HashMap<&'a str, &'a Block>,
    tools: HashMap<&'a str, &'a ToolRow>,
    results: HashMap<&'a str, &'a Arc<ResultView>>,
    user: Option<(&'a SharedString, &'a SharedString)>,
}

impl<'a> Reuse<'a> {
    fn of(row: &'a TurnRow) -> Self {
        let mut reuse = Self::default();
        if let (Some((text, _)), Some(html)) = (&row.user, &row.user_html) {
            reuse.user = Some((text, html));
        }
        for block in row.blocks.iter() {
            match block {
                Block::Thinking { key, live: false, .. } | Block::Text { key, .. } => {
                    if key.starts_with("stream:") {
                        reuse.stream_blocks.insert(key.as_ref(), block);
                    } else {
                        reuse.blocks.insert(key.as_ref(), block);
                    }
                }
                Block::Tool(tool) => {
                    if let Some(result) = &tool.result {
                        reuse.results.insert(result.entry.as_ref(), result);
                    }
                    if matches!(tool.state, ToolState::Done | ToolState::Failed) {
                        reuse.tools.insert(tool.call_id.as_ref(), tool);
                    }
                }
                _ => {}
            }
        }
        reuse
    }
}

fn prepare_block_markdown(raw: &SharedString) -> SharedString {
    if !raw.contains("\\(") && !raw.contains("\\[") {
        raw.clone()
    } else {
        prepare_markdown(raw)
    }
}

/// Whether the user undid a call: its own id for `write` and `edit`, every file's change for `apply_patch`.
fn reverted(ctx: &BuildContext<'_>, id: &str, result: Option<&ResultView>) -> bool {
    ctx.reverted.contains(id) || result.is_some_and(|result| !result.changes.is_empty() && result.changes.iter().all(|change| ctx.reverted.contains(change.as_ref())))
}

/// `unfinished`: a call without a result is still to run.
fn tool_row(
    ctx: &BuildContext<'_>,
    results: &HashMap<&str, ResultEntry<'_>>,
    reuse: Option<&Reuse<'_>>,
    id: &str,
    name: &str,
    arguments: &Map<String, Value>,
    unfinished: bool,
) -> ToolRow {
    let result = results.get(id);
    let running = ctx.running.get(id);
    let state = if ctx.waiting.contains(id) {
        ToolState::Waiting
    } else if let Some(result) = result {
        if result.is_error { ToolState::Failed } else { ToolState::Done }
    } else if running.is_some() || unfinished {
        ToolState::Running
    } else {
        ToolState::Failed
    };

    // Fast path: reuse an already-finished ToolRow whole (zero title/subagent/clip_output work during streaming).
    if matches!(state, ToolState::Done | ToolState::Failed)
        && let Some(prev) = reuse.and_then(|r| r.tools.get(id))
        && prev.state == state
        && prev.name.as_ref() == name
        && prev.result.as_ref().map(|r| r.entry.as_ref()) == result.map(|r| r.id)
        && prev.reverted == reverted(ctx, id, prev.result.as_deref())
    {
        return (*prev).clone();
    }

    let subagent = (name == "task").then(|| {
        let details = result.and_then(|result| result.details).or_else(|| running.and_then(|tool| tool.details.as_ref()));
        Arc::new(details.map_or_else(SubagentView::default, |details| subagent_view(ctx.locale, ctx.cwd, details)))
    });
    let result = result.map(|result| match reuse.and_then(|reuse| reuse.results.get(result.id)) {
        Some(view) => Arc::clone(view),
        None => Arc::new(result_view(result, ctx.locale)),
    });
    let output: Option<SharedString> = match &result {
        Some(result) => Some(result.text.clone()),
        None => running.and_then(|tool| tool.partial.clone()).map(Into::into),
    };
    let clipped_output = output.as_deref().map(|text| {
        let (clipped, hidden) = clip_output(text);
        (SharedString::from(clipped), hidden)
    });
    let command = is_command(name).then(|| arguments.get("command").and_then(Value::as_str).unwrap_or_default().to_owned().into());
    let patched = (name == "apply_patch").then(|| patch_files(arguments.get("input").and_then(Value::as_str).unwrap_or_default())).filter(|files| files.len() == 1);
    let file = arguments
        .get("file_path")
        .and_then(Value::as_str)
        .or_else(|| patched.as_ref().map(|files| files[0]))
        .filter(|path| !path.trim().is_empty())
        .map(|path| (SharedString::from(shown_path(ctx.cwd, path).unwrap_or_else(|| path.to_owned())), SharedString::from(host_path(ctx.cwd, path))));
    let is_reverted = reverted(ctx, id, result.as_deref());
    let title = match arguments.get("teamMember").and_then(Value::as_str).filter(|member| !member.is_empty()) {
        // A call of an external agent's team member (Antigravity), not of the chat's own agent.
        Some(member) => format!("{member} · {}", tool_title_in(ctx.locale, name, arguments, Some(ctx.cwd))),
        None => tool_title_in(ctx.locale, name, arguments, Some(ctx.cwd)),
    };
    ToolRow {
        call_id: id.to_owned().into(),
        name: name.to_owned().into(),
        title: title.into(),
        command,
        state,
        output,
        clipped_output,
        file,
        result,
        reverted: is_reverted,
        subagent,
    }
}

struct AssistantBlocksInput<'a> {
    ctx: &'a BuildContext<'a>,
    results: &'a HashMap<&'a str, ResultEntry<'a>>,
    reuse: Option<&'a Reuse<'a>>,
    key: &'a str,
    message: &'a AssistantMessage,
    /// `live`: the message is the stream; `pending`: its turn is still running.
    live: bool,
    pending: bool,
    thinking_ms: Option<&'a Map<String, Value>>,
}

fn assistant_blocks(input: AssistantBlocksInput<'_>, out: &mut Vec<Block>) {
    let AssistantBlocksInput { ctx, results, reuse, key, message, live, pending, thinking_ms } = input;
    let last = message.content.len().saturating_sub(1);
    for (index, block) in message.content.iter().enumerate() {
        let block_key = format!("{key}:{index}");
        if !live
            && let Some(earlier) = reuse.and_then(|reuse| reuse.blocks.get(block_key.as_str()))
        {
            out.push((*earlier).clone());
            continue;
        }
        // Inside an active stream, preceding blocks (< last) are already settled and can be reused if unchanged.
        if live && index < last {
            let lookup_key = if matches!(block, AssistantBlock::Thinking { .. }) && message.timestamp > 0.0 {
                format!("think:{}:{index}", message.timestamp)
            } else {
                block_key.clone()
            };
            let candidate = reuse.and_then(|r| r.stream_blocks.get(lookup_key.as_str()).or_else(|| r.blocks.get(lookup_key.as_str())));
            if let Some(earlier) = candidate {
                let matches_content = match (block, *earlier) {
                    (AssistantBlock::Text { text, .. }, Block::Text { text: prev, .. }) => prev.as_ref() == text.as_str(),
                    (AssistantBlock::Thinking { thinking, redacted, .. }, Block::Thinking { text: prev, redacted: prev_red, live: false, .. }) => {
                        prev.as_ref() == thinking.as_str() && *prev_red == redacted.unwrap_or(false)
                    }
                    _ => false,
                };
                if matches_content {
                    out.push((*earlier).clone());
                    continue;
                }
            }
        }
        match block {
            AssistantBlock::Thinking { thinking, redacted, .. } => {
                let redacted = redacted.unwrap_or(false);
                if thinking.trim().is_empty() && !redacted {
                    continue;
                }
                let is_live_block = live && index == last;
                let live_started = is_live_block.then(|| {
                    let live_key = format!("{}:{index}", message.timestamp);
                    ctx.thinking_since.filter(|(k, _)| *k == live_key).map(|(_, since)| *since)
                }).flatten();
                let duration_ms = thinking_ms
                    .and_then(|map| map.get(&index.to_string()))
                    .and_then(Value::as_f64)
                    .or_else(|| live_started.map(|since| since.elapsed().as_millis() as f64));
                // The same key while the reply streams and once it is saved, so a block the user opened stays open when
                // the reply ends (the streaming message and the saved one share their start time).
                let key = if message.timestamp > 0.0 { format!("think:{}:{index}", message.timestamp) } else { block_key };
                let text: SharedString = thinking.clone().into();
                let prepared = prepare_block_markdown(&text);
                out.push(Block::Thinking {
                    key: key.into(),
                    text,
                    prepared,
                    redacted,
                    live: is_live_block,
                    duration_ms,
                    started_at: live_started,
                });
            }
            AssistantBlock::Text { text, .. } => {
                if !text.trim().is_empty() {
                    let raw: SharedString = text.clone().into();
                    let prepared = prepare_block_markdown(&raw);
                    out.push(Block::Text { key: block_key.into(), text: raw, prepared });
                }
            }
            AssistantBlock::ToolCall { id, name, arguments, .. } => {
                out.push(Block::Tool(tool_row(ctx, results, reuse, id, name, arguments, live || pending)));
            }
        }
    }
    match message.stop_reason.as_str() {
        "error" => out.push(Block::Error { text: message.error_message.clone().unwrap_or_default().into() }),
        "aborted" => out.push(Block::Muted { text: i18n::text(ctx.locale, "chat.stopped").into() }),
        _ => {}
    }
}

fn turn_key<'a>(entries: &'a [TranscriptEntry], range: &TurnRange) -> &'a str {
    match range.user.and_then(|idx| entries.get(idx)) {
        Some(TranscriptEntry::User { id, .. }) => id,
        _ => "lead",
    }
}

/// Whether an earlier row of the turn comes out the same: the same entries, and none of its calls is running,
/// waiting, or undone since.
fn unchanged(row: &TurnRow, entries: &[TranscriptEntry], range: &TurnRange, ctx: &BuildContext<'_>) -> bool {
    let ids = &row.source.entries;
    let user = range.user.and_then(|idx| entries.get(idx));
    let items = &entries[range.items.clone()];
    !row.source.live
        && ids.len() == usize::from(user.is_some()) + items.len()
        && user.into_iter().chain(items.iter()).zip(ids.iter()).all(|(entry, id)| entry.id() == id.as_ref())
        && row.blocks.iter().all(|block| match block {
            Block::Tool(tool) => {
                let id = tool.call_id.as_ref();
                !matches!(tool.state, ToolState::Running | ToolState::Waiting)
                    && !ctx.running.contains_key(id)
                    && !ctx.waiting.contains(id)
                    && tool.reverted == reverted(ctx, id, tool.result.as_deref())
            }
            _ => true,
        })
}

struct TurnBuildInput<'a> {
    ctx: &'a BuildContext<'a>,
    chat: &'a SharedString,
    user_entry: Option<&'a TranscriptEntry>,
    items: &'a [TranscriptEntry],
    key: &'a str,
    live: bool,
    stream: Option<&'a AssistantMessage>,
    reuse: Option<&'a Reuse<'a>>,
}

/// One turn; `live` while the chat works on it, `stream` the reply streaming into it.
fn build_turn(input: TurnBuildInput<'_>) -> TurnRow {
    let TurnBuildInput { ctx, chat, user_entry, items, key, live, stream, reuse } = input;
    let locale = ctx.locale;
    let mut results: HashMap<&str, ResultEntry<'_>> = HashMap::new();
    for item in items {
        if let TranscriptEntry::ToolResult { id, tool_call_id, tool_name, content, is_error, details, .. } = item {
            results.entry(tool_call_id.as_str()).or_insert(ResultEntry { id, call_id: tool_call_id, tool_name, content, is_error: *is_error, details: details.as_ref() });
        }
    }
    let mut blocks = Vec::new();
    let mut footer = None;
    let mut copy = None;
    // The answer is the last reply when it called no tools, or when it failed or stopped.
    let answer = (!live)
        .then(|| {
            items.iter().rev().find_map(|item| match item {
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
    for item in items {
        match item {
            TranscriptEntry::Assistant { id, .. } if Some(id.as_str()) == answer => {}
            TranscriptEntry::Assistant { .. } => work_steps += 1,
            TranscriptEntry::ToolResult { tool_name, details, .. } => {
                work_steps += 1;
                match tool_name.as_str() {
                    "read" => reads += 1,
                    "grep" | "glob" | "recall" => searches += 1,
                    "write" | "edit" => changes += 1,
                    "apply_patch" => changes += details.as_ref().and_then(|details| details["files"].as_array()).map_or(1, |files| files.len().max(1)),
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
    for item in items {
        match item {
            TranscriptEntry::Assistant { id, message, thinking_ms, .. } => {
                if Some(id.as_str()) == answer {
                    answer_from = Some(blocks.len());
                }
                assistant_blocks(
                    AssistantBlocksInput { ctx, results: &results, reuse, key: id, message, live: false, pending: live, thinking_ms: thinking_ms.as_ref() },
                    &mut blocks,
                );
                if !live {
                    let text = assistant_text(message);
                    if !text.is_empty() {
                        copy = Some(text.into());
                    }
                }
            }
            TranscriptEntry::Notice { level, text, code, .. } => {
                // A stopped reply already says so right above.
                let stopped = i18n::text(locale, "chat.stopped");
                let said = code.as_deref() == Some("stopped") && matches!(blocks.last(), Some(Block::Muted { text }) if text.as_ref() == stopped);
                if !said {
                    blocks.push(Block::Notice { level: level.clone().into(), text: text.clone().into() });
                }
            }
            TranscriptEntry::Compaction { tokens_before, trigger, request, .. } => {
                let recall = request.as_ref().and_then(|request| request["mode"].as_str()) == Some("recall");
                let key = match (trigger == "manual", recall) {
                    (true, true) => "chat.compactionRecallManual",
                    (false, true) => "chat.compactionRecall",
                    (true, false) => "chat.compactionManual",
                    (false, false) => "chat.compaction",
                };
                let text = i18n::format(locale, key, &[("tokens", &format_tokens(*tokens_before))]);
                blocks.push(Block::Muted { text: text.into() });
            }
            TranscriptEntry::Cleared { entry_ids, tokens_before, tokens_after, .. } => {
                let freed = format_tokens((tokens_before - tokens_after).max(0.0));
                let text = i18n::format(locale, "chat.clearedOutputs", &[("count", &entry_ids.len()), ("tokens", &freed)]);
                blocks.push(Block::Muted { text: text.into() });
            }
            TranscriptEntry::Context { label, reverted, .. } => {
                let text = match reverted {
                    Some(ids) => i18n::format(locale, "chat.revertedNotice", &[("count", &ids.len())]),
                    None => i18n::text(locale, match label.as_str() {
                        "instructions" => "chat.contextInstructions",
                        "memory" => "chat.contextMemory",
                        "reference" => "chat.contextReference",
                        _ => "chat.contextNotice",
                    })
                    .to_owned(),
                };
                blocks.push(Block::Muted { text: text.into() });
            }
            TranscriptEntry::Turn { id, duration_ms, usage, requests, .. } => {
                footer = Some(Footer { id: id.clone().into(), duration_ms: *duration_ms, usage: usage.clone(), requests: *requests });
            }
            TranscriptEntry::User { .. } | TranscriptEntry::ToolResult { .. } => {}
        }
    }
    let (user, user_html) = match user_entry {
        Some(TranscriptEntry::User { text, images, .. }) => {
            let (raw, html) = match reuse.and_then(|r| r.user) {
                Some((prev_text, prev_html)) if prev_text.as_ref() == text.as_str() => (prev_text.clone(), prev_html.clone()),
                _ => (SharedString::from(text.clone()), prepare_typed_html(text)),
            };
            (Some((raw, images.as_ref().map_or(0, Vec::len))), Some(html))
        }
        _ => (None, None),
    };
    let mut answer_from = answer_from.unwrap_or(blocks.len());
    if let Some(stream) = stream {
        assistant_blocks(
            AssistantBlocksInput { ctx, results: &results, reuse, key: "stream", message: stream, live: true, pending: true, thinking_ms: None },
            &mut blocks,
        );
        answer_from = blocks.len();
    }
    let entries = user_entry.into_iter().chain(items.iter()).map(|entry| SharedString::from(entry.id().to_owned())).collect();
    // Nothing streams and no tool runs or waits for approval: the agent is starting, or working out its next step.
    let busy_block = blocks.last().is_some_and(|block| matches!(block, Block::Tool(tool) if matches!(tool.state, ToolState::Running | ToolState::Waiting)));
    let streaming = stream.is_some_and(|stream| !stream.content.is_empty());
    let is_waiting = live && ctx.turn_running && !streaming && !busy_block;
    let waiting = is_waiting.then(|| ctx.changed_at.elapsed().as_secs_f64() * 1000.);
    let waiting_since = is_waiting.then_some(ctx.changed_at);
    let compacting = live && ctx.compacting;
    TurnRow {
        key: key.to_owned().into(),
        user,
        user_html,
        blocks: blocks.into(),
        live,
        footer: footer.filter(|_| !live),
        copy,
        answer_from,
        work_steps,
        summary: summary.into(),
        waiting,
        waiting_since,
        compacting,
        source: RowSource { chat: chat.clone(), locale, entries, live: live || stream.is_some() },
    }
}

/// The rows of a chat; turns of `previous` that come out the same are taken over instead of built again.
fn rows_from(view: &ChatView, locale: Locale, previous: &[TurnRow]) -> Vec<TurnRow> {
    let snapshot = &view.snapshot;
    let chat: SharedString = snapshot.summary.id.clone().into();
    let ctx = BuildContext {
        locale,
        cwd: &snapshot.summary.cwd,
        running: &view.tools,
        waiting: snapshot.approvals.iter().map(|approval| approval.tool_call_id.as_str()).collect(),
        reverted: view.reverted_ids(),
        thinking_since: view.thinking_since.as_ref(),
        turn_running: snapshot.summary.status == "running",
        changed_at: view.changed_at,
        compacting: snapshot.summary.status == "compacting",
    };
    let busy = view.busy();
    let ranges = view.turn_ranges();
    let count = ranges.len();
    let mut by_key: Option<HashMap<&str, &TurnRow>> = None;
    let mut rows = Vec::with_capacity(count.max(1));
    for (index, range) in ranges.iter().enumerate() {
        let last = index + 1 == count;
        let live = busy && last;
        let stream = snapshot.streaming.as_ref().filter(|_| last);
        let key = turn_key(&snapshot.entries, range);
        // Rows stay at their index while a chat grows; loading older messages shifts them.
        let prior = previous
            .get(index)
            .filter(|row| row.key.as_ref() == key)
            .or_else(|| by_key.get_or_insert_with(|| previous.iter().map(|row| (row.key.as_ref(), row)).collect()).get(key).copied())
            .filter(|row| row.source.chat == chat && row.source.locale == locale);
        if let Some(prior) = prior.filter(|prior| !live && stream.is_none() && unchanged(prior, &snapshot.entries, range, &ctx)) {
            rows.push(prior.clone());
            continue;
        }
        let reuse = prior.map(Reuse::of);
        let user_entry = range.user.and_then(|idx| snapshot.entries.get(idx));
        let items = &snapshot.entries[range.items.clone()];
        rows.push(build_turn(TurnBuildInput { ctx: &ctx, chat: &chat, user_entry, items, key, live, stream, reuse: reuse.as_ref() }));
    }
    if rows.is_empty()
        && let Some(stream) = &snapshot.streaming
    {
        rows.push(build_turn(TurnBuildInput {
            ctx: &ctx,
            chat: &chat,
            user_entry: None,
            items: &[],
            key: "lead",
            live: true,
            stream: Some(stream),
            reuse: None,
        }));
    }
    rows
}

/// Turns of a chat as the transcript shows them.
pub fn build_rows(view: &ChatView, locale: Locale) -> Vec<TurnRow> {
    rows_from(view, locale, &[])
}

/// The rows `build_rows` makes, reusing the finished turns of `previous` (rows of an earlier state of the same chat)
/// while only the live turn changed.
pub fn rebuild_rows(view: &ChatView, locale: Locale, previous: &[TurnRow]) -> Vec<TurnRow> {
    if previous.is_empty() { build_rows(view, locale) } else { rows_from(view, locale, previous) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tool_card::{live_tail, report_text};
    // `gpui_kit::*` brings GPUI's own `test` macro; use Rust's.
    use ::core::prelude::v1::test;
    use serde_json::json;

    fn chat(id: &str, entries: Value, streaming: Value, status: &str) -> ChatView {
        ChatView::new(
            serde_json::from_value(json!({
                "summary": {"id": id, "workspaceId": null, "cwd": "/p", "title": "t", "createdAt": 1, "updatedAt": 2,
                            "archived": false, "pinned": false, "status": status},
                "entries": entries, "model": null, "queue": [], "context": null, "streaming": streaming,
                "runningTools": [], "permission": "ask", "approvals": [], "background": []
            }))
            .unwrap(),
            true,
        )
    }

    fn view(entries: Value, streaming: Value, status: &str) -> ChatView {
        chat("s", entries, streaming, status)
    }

    fn assistant(id: &str, content: Value, stop: &str) -> Value {
        json!({"id": id, "kind": "assistant", "ts": 2, "message": {"role": "assistant", "content": content, "api": "x",
            "provider": "p", "model": "m", "stopReason": stop, "timestamp": 2,
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                      "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}}}})
    }

    fn call(id: &str, name: &str, arguments: Value) -> Value {
        json!({"type": "toolCall", "id": id, "name": name, "arguments": arguments})
    }

    fn result(id: &str, call: &str, name: &str, content: Value, error: bool, details: Value) -> Value {
        json!({"id": id, "kind": "toolResult", "ts": 3, "toolCallId": call, "toolName": name, "content": content, "isError": error, "details": details})
    }

    fn tools(row: &TurnRow) -> Vec<&ToolRow> {
        row.blocks.iter().filter_map(|block| if let Block::Tool(tool) = block { Some(tool) } else { None }).collect()
    }

    #[test]
    fn a_sub_agents_card_lists_its_steps_and_its_report() {
        let user = json!({"id": "u1", "kind": "user", "ts": 1, "text": "找登录"});
        let task = call("t1", "task", json!({"description": "Find the login flow", "prompt": "Where is the login?"}));
        // While it works: its latest steps, the last one running, and how many it made in all.
        let mut running = view(json!([user.clone(), assistant("a1", json!([task.clone()]), "toolUse")]), Value::Null, "running");
        running.tools.insert("t1".into(), RunningTool {
            tool_name: "task".into(),
            args: None,
            partial: Some("Looking at the auth module".into()),
            details: Some(json!({"steps": 2, "calls": 14, "activity": [
                {"id": "s1", "name": "grep", "args": {"pattern": "login"}, "state": "done"},
                {"id": "s2", "name": "read", "args": {"file_path": "/p/src/auth.rs"}, "state": "running"},
            ]})),
        });
        let rows = build_rows(&running, Locale::Zh);
        let tool = tools(&rows[0])[0];
        let agent = tool.subagent.as_deref().expect("a sub-agent card");
        assert_eq!(tool.state, ToolState::Running);
        let steps: Vec<(&str, ToolState)> = agent.steps.iter().map(|(title, state)| (title.as_ref(), *state)).collect();
        assert_eq!(steps, [("搜索内容 login", ToolState::Done), ("读取 src/auth.rs", ToolState::Running)]);
        assert_eq!((agent.calls, agent.earlier), (14, 12));
        assert_eq!(tool.output.as_deref(), Some("Looking at the auth module"));
        // Just started: no steps yet, still a sub-agent card.
        let mut starting = view(json!([user.clone(), assistant("a1", json!([task.clone()]), "toolUse")]), Value::Null, "running");
        starting.tools.insert("t1".into(), RunningTool::default());
        assert_eq!(tools(&build_rows(&starting, Locale::Zh)[0])[0].subagent.as_deref(), Some(&SubagentView::default()));
        // Its result: the report and the limit it hit. Chats from before 4.13 kept only each step's tool.
        let report = "Found it in `src/auth.rs`.\n\n[the sub-agent reached its step limit; its report may be incomplete]";
        let done = json!([user, assistant("a1", json!([task]), "toolUse"),
            result("r1", "t1", "task", json!([{"type": "text", "text": report}]), false, json!({"steps": 24, "activity": ["grep", "read"], "stopped": "steps"}))]);
        let rows = build_rows(&view(done, Value::Null, "idle"), Locale::Zh);
        let tool = tools(&rows[0])[0];
        let agent = tool.subagent.as_deref().expect("a sub-agent card");
        assert_eq!(agent.steps.iter().map(|(title, _)| title.as_ref()).collect::<Vec<_>>(), ["grep", "read"]);
        assert_eq!((agent.calls, agent.earlier, agent.stopped.as_deref()), (2, 0, Some("steps")));
        assert_eq!(report_text(tool.output.as_deref().unwrap(), true), "Found it in `src/auth.rs`.");
        assert_eq!(report_text("(the sub-agent reported nothing)", false), "");
        // What it is writing: the last lines, inside the code block they belong to.
        let writing = "## Found\n\n```rust\nfn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\nfn e() {}\nfn f() {}";
        assert_eq!(live_tail(writing), "```rust\nfn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\nfn e() {}\nfn f() {}");
        assert_eq!(live_tail("one\n\ntwo"), "one\n\ntwo");
        assert_eq!(live_tail("```\nx\n```\n1\n2\n3\n4\n5\n6"), "1\n2\n3\n4\n5\n6");
        // Other tools have no sub-agent card.
        assert!(tools(&build_rows(&view(json!([assistant("a2", json!([call("c1", "read", json!({"file_path": "a.txt"}))]), "toolUse")]), Value::Null, "idle"), Locale::Zh)[0])[0].subagent.is_none());
    }

    #[test]
    fn a_running_turn_waits_while_nothing_streams_or_runs() {
        let user = json!({"id": "u1", "kind": "user", "ts": 1, "text": "你好"});
        // Just sent: the agent has said nothing yet.
        let rows = build_rows(&view(json!([user.clone()]), Value::Null, "running"), Locale::Zh);
        assert!(rows[0].waiting.is_some());
        // The reply streams.
        let streaming = assistant("x", json!([{"type": "text", "text": "正在"}]), "pending")["message"].clone();
        assert!(build_rows(&view(json!([user.clone()]), streaming, "running"), Locale::Zh)[0].waiting.is_none());
        // A tool runs.
        let calling = json!([user.clone(), assistant("a1", json!([call("c1", "read", json!({"file_path": "a.txt"}))]), "toolUse")]);
        let mut running = view(calling, Value::Null, "running");
        running.tools.insert("c1".into(), RunningTool::default());
        assert!(build_rows(&running, Locale::Zh)[0].waiting.is_none());
        // Between steps: the tool is done and the next reply has not begun.
        let between = json!([user.clone(), assistant("a1", json!([call("c1", "read", json!({"file_path": "a.txt"}))]), "toolUse"),
            result("r1", "c1", "read", json!([{"type": "text", "text": "hello"}]), false, Value::Null)]);
        assert!(build_rows(&view(between, Value::Null, "running"), Locale::Zh)[0].waiting.is_some());
        // Idle, or compacting.
        assert!(build_rows(&view(json!([user.clone()]), Value::Null, "idle"), Locale::Zh)[0].waiting.is_none());
        // Compacting says so instead.
        let compacting = &build_rows(&view(json!([user.clone()]), Value::Null, "compacting"), Locale::Zh)[0];
        assert!(compacting.waiting.is_none() && compacting.compacting);
        assert!(!build_rows(&view(json!([user]), Value::Null, "running"), Locale::Zh)[0].compacting);
    }

    #[test]
    fn builds_turns_with_tools_and_footer() {
        let entries = json!([
            {"id": "u1", "kind": "user", "ts": 1, "text": "读 a.txt"},
            assistant("a1", json!([{"type": "thinking", "thinking": "想想"}, call("c1", "read", json!({"file_path": "a.txt"}))]), "toolUse"),
            result("r1", "c1", "read", json!([{"type": "text", "text": "hello"}]), false, Value::Null),
            assistant("a2", json!([{"type": "text", "text": "内容是 hello"}]), "stop"),
            {"id": "t1", "kind": "turn", "ts": 4, "durationMs": 3200, "usage": {"input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0}, "provider": "p", "model": "m", "requests": 2}
        ]);
        let rows = build_rows(&view(entries, Value::Null, "idle"), Locale::Zh);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.user.as_ref().unwrap().0, "读 a.txt");
        assert!(row.user_html.as_deref().is_some_and(|h| h.contains("读 a.txt")));
        assert!(matches!(&row.blocks[0], Block::Thinking { .. }));
        let Block::Tool(tool) = &row.blocks[1] else { panic!("tool") };
        assert_eq!((tool.title.as_ref(), tool.state, tool.output.as_deref()), ("读取 a.txt", ToolState::Done, Some("hello")));
        assert_eq!(tool.file.as_ref().map(|(shown, target)| (shown.as_ref(), target.as_ref())), Some(("a.txt", "/p/a.txt")));
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
    fn a_thinking_block_keeps_its_key_when_the_reply_is_saved() {
        let user = json!({"id": "u1", "kind": "user", "ts": 1, "text": "hi"});
        let content = json!([{"type": "thinking", "thinking": "想一想"}, {"type": "text", "text": "好"}]);
        let streaming = assistant("x", content.clone(), "pending")["message"].clone();
        let live = build_rows(&view(json!([user.clone()]), streaming, "running"), Locale::Zh);
        let saved = build_rows(&view(json!([user, assistant("a1", content, "stop")]), Value::Null, "idle"), Locale::Zh);
        let key = |rows: &[TurnRow]| match &rows[0].blocks[0] {
            Block::Thinking { key, .. } => key.clone(),
            other => panic!("{other:?}"),
        };
        // A block the user opened while it streamed is open by the same key once saved.
        assert_eq!(key(&live), key(&saved));
    }

    #[test]
    fn shows_what_tool_results_hold() {
        let entries = json!([
            {"id": "u1", "kind": "user", "ts": 1, "text": "改"},
            assistant("a1", json!([
                call("w", "write", json!({"file_path": "C:\\p\\b.txt"})),
                call("b", "bash", json!({"command": "false", "description": "fail"})),
                call("d", "edit", json!({"file_path": "c.txt"})),
                call("s", "browser_screenshot", json!({})),
                call("q", "bash", json!({"command": "sleep 9", "description": "wait"})),
            ]), "toolUse"),
            result("r1", "w", "write", json!([{"type": "text", "text": "Updated"}]), false, json!({"path": "b.txt", "before": "a\nb\nc\n", "after": "a\nB\nc\nd\n", "undoable": true})),
            result("r2", "b", "bash", json!([{"type": "text", "text": ""}]), false, json!({"command": "false", "exitCode": 1, "signal": null})),
            result("r3", "d", "edit", json!([{"type": "text", "text": "denied"}]), true, json!({"refused": "user"})),
            result("r4", "s", "browser_screenshot", json!([{"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"}]), false, Value::Null),
            result("r5", "q", "bash", json!([{"type": "text", "text": "[j1 is running]"}]), false, json!({"exitCode": null, "signal": null, "jobId": "j1"})),
            {"id": "x1", "kind": "context", "ts": 4, "label": "notice", "text": "undone", "reverted": ["w"]}
        ]);
        let rows = build_rows(&view(entries, Value::Null, "idle"), Locale::Zh);
        let tools = tools(&rows[0]);
        let write = tools[0].result.as_ref().unwrap();
        let diff = write.diff.as_ref().unwrap();
        assert_eq!((diff.added, diff.removed, diff.hidden), (2, 1, 0));
        assert!(diff.lines.iter().any(|(kind, text)| *kind == LineKind::Removed && text == "- b"));
        assert!(write.undoable && tools[0].reverted);
        assert_eq!(tools[0].file.as_ref().unwrap().1, "C:\\p\\b.txt");
        assert_eq!(tools[1].result.as_ref().unwrap().exit, Some(Exit::Code(1)));
        assert_eq!((tools[2].state, tools[2].result.as_ref().unwrap().refused), (ToolState::Failed, Some(Refusal::User)));
        assert_eq!(tools[3].result.as_ref().unwrap().images[0].bytes, b"hello");
        assert_eq!(tools[4].result.as_ref().unwrap().exit, Some(Exit::Background));
        assert!(matches!(rows[0].blocks.last(), Some(Block::Muted { text }) if text == "已撤销 1 处文件修改"));
    }

    #[test]
    fn shows_patches_and_plans() {
        let patch = "*** Begin Patch\n*** Update File: a.txt\n-x\n+y\n*** Add File: n.txt\n+z\n*** End Patch";
        let entries = json!([
            {"id": "u1", "kind": "user", "ts": 1, "text": "改"},
            assistant("a1", json!([
                call("p", "apply_patch", json!({"input": patch})),
                call("t", "update_plan", json!({"plan": [{"step": "Read", "status": "completed"}, {"step": "Fix", "status": "in_progress"}]})),
            ]), "toolUse"),
            result("r1", "p", "apply_patch", json!([{"type": "text", "text": "Success."}]), false, json!({"files": [
                {"changeId": "p#0", "path": "a.txt", "operation": "update", "before": "x\n", "after": "y\n", "undoable": true},
                {"changeId": "p#1", "path": "n.txt", "operation": "create", "after": "z\n"},
            ]})),
            result("r2", "t", "update_plan", json!([{"type": "text", "text": "Plan updated"}]), false, json!({"plan": [{"text": "Read", "status": "completed"}, {"text": "Fix", "status": "in_progress"}]})),
            {"id": "x1", "kind": "context", "ts": 4, "label": "notice", "text": "undone", "reverted": ["p#0"]}
        ]);
        let rows = build_rows(&view(entries.clone(), Value::Null, "idle"), Locale::Zh);
        let tools = tools(&rows[0]);
        let patched = tools[0].result.as_ref().unwrap();
        let diff = patched.diff.as_ref().unwrap();
        assert_eq!((diff.added, diff.removed), (2, 1));
        assert_eq!(diff.lines[0], (LineKind::File, SharedString::from("a.txt")));
        assert!(diff.lines.iter().any(|(kind, text)| *kind == LineKind::File && text == "n.txt · 新建"));
        assert_eq!((patched.changes.len(), patched.undo.len()), (2, 1));
        // One of the two files undone leaves the call standing.
        assert!(patched.undoable && !tools[0].reverted);
        assert_eq!(tools[0].title, "修改 2 个文件");
        let plan = tools[1].result.as_ref().unwrap().plan.as_ref().unwrap();
        assert_eq!(plan[1], (SharedString::from("Fix"), SharedString::from("in_progress")));
        assert_eq!(tools[1].title, "更新计划（1/2）");
        // Both files undone: the call is undone.
        let mut entries = entries;
        entries.as_array_mut().unwrap().push(json!({"id": "x2", "kind": "context", "ts": 5, "label": "notice", "text": "undone", "reverted": ["p#1"]}));
        assert!(tools_reverted(&build_rows(&view(entries, Value::Null, "idle"), Locale::Zh)[0]));
    }

    fn tools_reverted(row: &TurnRow) -> bool {
        tools(row)[0].reverted
    }

    #[test]
    fn calls_of_a_running_turn_without_results_are_running() {
        let entries = json!([
            {"id": "u1", "kind": "user", "ts": 1, "text": "跑"},
            assistant("a1", json!([call("c1", "read", json!({"file_path": "a"})), call("c2", "read", json!({"file_path": "b"}))]), "toolUse"),
        ]);
        let states = |status: &str| build_rows(&view(entries.clone(), Value::Null, status), Locale::Zh)[0].blocks.iter().filter_map(|block| match block {
            Block::Tool(tool) => Some(tool.state),
            _ => None,
        }).collect::<Vec<_>>();
        assert_eq!(states("running"), [ToolState::Running, ToolState::Running]);
        assert_eq!(states("idle"), [ToolState::Failed, ToolState::Failed]);
    }

    #[test]
    fn rebuilds_only_what_changed() {
        let first = vec![
            json!({"id": "u1", "kind": "user", "ts": 1, "text": "一"}),
            assistant("a1", json!([{"type": "thinking", "thinking": "想"}, call("c1", "write", json!({"file_path": "a"}))]), "toolUse"),
            result("r1", "c1", "write", json!([{"type": "text", "text": "ok"}]), false, json!({"path": "a", "after": "x\n", "undoable": true})),
            assistant("a2", json!([{"type": "text", "text": "答一"}]), "stop"),
            json!({"id": "u2", "kind": "user", "ts": 5, "text": "这是一个用于测试指针复用的较长用户输入"}),
            assistant("a3", json!([{"type": "text", "text": "先看看这个文件里现在写了什么"}, call("c2", "edit", json!({"file_path": "b"}))]), "toolUse"),
            result("r2", "c2", "edit", json!([{"type": "text", "text": "ok output line for pointer reuse"}]), false, json!({"path": "b", "before": "1\n", "after": "2\n"})),
        ];
        let stream = |thinking: &str, text: &str| {
            assistant("x", json!([{"type": "thinking", "thinking": thinking}, {"type": "text", "text": text}]), "pending")["message"].clone()
        };
        let previous = build_rows(&view(Value::from(first.clone()), stream("已完成思考内容并且足够长以分配在堆上", "正"), "running"), Locale::Zh);
        // A streamed token: finished turns, finished ToolRows, user HTML, and settled preceding stream blocks are reused.
        let token = view(Value::from(first.clone()), stream("已完成思考内容并且足够长以分配在堆上", "正在"), "running");
        let rows = rebuild_rows(&token, Locale::Zh, &previous);
        assert_eq!(rows, build_rows(&token, Locale::Zh));
        assert!(Arc::ptr_eq(&rows[0].blocks, &previous[0].blocks));
        assert!(!Arc::ptr_eq(&rows[1].blocks, &previous[1].blocks));
        assert_eq!(rows[1].user_html.as_ref().unwrap().as_ptr(), previous[1].user_html.as_ref().unwrap().as_ptr());
        let (Block::Text { text: now, .. }, Block::Text { text: before, .. }) = (&rows[1].blocks[0], &previous[1].blocks[0]) else { panic!("text") };
        assert_eq!(now.as_ptr(), before.as_ptr());
        // Finished ToolRow and its pre-clipped output are reused wholesale across stream tokens.
        assert!(Arc::ptr_eq(tools(&rows[1])[0].result.as_ref().unwrap(), tools(&previous[1])[0].result.as_ref().unwrap()));
        assert_eq!(
            tools(&rows[1])[0].clipped_output.as_ref().unwrap().0.as_ptr(),
            tools(&previous[1])[0].clipped_output.as_ref().unwrap().0.as_ptr()
        );
        // Preceding settled thinking block inside the live stream is reused without re-cloning.
        let (Block::Thinking { text: now_th, .. }, Block::Thinking { text: prev_th, .. }) = (&rows[1].blocks[2], &previous[1].blocks[2]) else {
            panic!("stream thinking");
        };
        assert_eq!(now_th.as_ptr(), prev_th.as_ptr());

        // The reply settles and a new turn starts; undoing a change of the first turn builds that turn again.
        let mut next = first.clone();
        next.push(assistant("a4", json!([{"type": "text", "text": "答二"}]), "stop"));
        next.push(json!({"id": "u3", "kind": "user", "ts": 7, "text": "撤销"}));
        next.push(json!({"id": "x1", "kind": "context", "ts": 8, "label": "notice", "text": "undone", "reverted": ["c1"]}));
        let turn = view(Value::from(next.clone()), Value::Null, "idle");
        let later = rebuild_rows(&turn, Locale::Zh, &rows);
        assert_eq!(later, build_rows(&turn, Locale::Zh));
        assert_eq!(later.len(), 3);
        assert!(!Arc::ptr_eq(&later[0].blocks, &rows[0].blocks) && tools(&later[0])[0].reverted);
        // Nothing changed: every turn is taken over.
        let again = rebuild_rows(&turn, Locale::Zh, &later);
        assert!(again.iter().zip(&later).all(|(row, earlier)| Arc::ptr_eq(&row.blocks, &earlier.blocks)));
        // Another language, another chat, or a window that starts later (a reload) builds again.
        let english = rebuild_rows(&turn, Locale::En, &later);
        assert_eq!(english, build_rows(&turn, Locale::En));
        assert!(!Arc::ptr_eq(&english[0].blocks, &later[0].blocks));
        let other = chat("other", Value::from(next.clone()), Value::Null, "idle");
        assert!(!Arc::ptr_eq(&rebuild_rows(&other, Locale::Zh, &later)[2].blocks, &later[2].blocks));
        let window = view(Value::from(next[2..].to_vec()), Value::Null, "idle");
        let shifted = rebuild_rows(&window, Locale::Zh, &later);
        assert_eq!(shifted, build_rows(&window, Locale::Zh));
        assert!(Arc::ptr_eq(&shifted[1].blocks, &later[1].blocks));
    }

    #[test]
    fn shows_paths_in_the_folder_relative_to_it() {
        assert_eq!(shown_path(r"C:\Work\app", r"C:\Work\app\src\a.py").as_deref(), Some(r"src\a.py"));
        assert_eq!(shown_path(r"C:\Work\app\", r"c:\work\app\a.py").as_deref(), Some("a.py"));
        assert_eq!(shown_path("/home/me/app", "/home/me/app/a.py").as_deref(), Some("a.py"));
        assert_eq!(shown_path(r"C:\Work\app", r"C:\Work\apple\a.py"), None);
        assert_eq!(shown_path(r"C:\Work\app", "src/a.py"), None);
        assert_eq!(shown_path("", "/a.py"), None);
        // The folder's length falls inside `裂` of a path elsewhere; this panicked on every start of 4.10.0.
        assert_eq!(shown_path(r"C:\Users\ROG\Desktop", r"C:\迅雷下载\撕裂者核弹-数据版\说明.txt"), None);
        assert_eq!(shown_path(r"C:\Users\ROG\Desktop", r"C:\Users\ROG\Desktop\撕裂者\a.txt").as_deref(), Some(r"撕裂者\a.txt"));
        let args = json!({ "file_path": r"C:\Work\app\report.py", "offset": 3 }).as_object().unwrap().clone();
        assert_eq!(tool_title_in(Locale::Zh, "read", &args, Some(r"C:\Work\app")), "读取 report.py（从第 3 行起）");
    }

    #[test]
    fn clips_long_output() {
        let text = (0..100).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let (clipped, hidden) = clip_output(&text);
        assert_eq!(hidden, 40);
        assert!(clipped.starts_with("0\n") && clipped.ends_with("\n99"));
    }
}
