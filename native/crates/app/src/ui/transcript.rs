//! The transcript of one chat: turns built from the chat state, and how each turn is drawn
//! (`Transcript.tsx` and `ToolCard.tsx`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, spinner::Spinner, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::StyledExt as _;
use rainy_protocol::{AssistantBlock, AssistantMessage, ResultContent, RevertResult, RpcError, TranscriptEntry, UsageSummary};
use serde_json::{Map, Value, json};

use crate::i18n::{self, Locale};
use crate::store::Store;
use crate::store::chat::{ChatView, RunningTool, Turn, assistant_text, group_turns};
use crate::store::ide::Ide;
use crate::ui::tool_title::{is_command, tool_title};
use crate::ui::{format_duration, format_tokens, tr, trf};

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
    /// The file the call names, as the model wrote it and as a Host path, to open it in the editor.
    pub file: Option<(SharedString, SharedString)>,
    pub result: Option<Arc<ResultView>>,
    /// The user undid the change.
    pub reverted: bool,
}

/// Why the call did not run (`details.refused`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    User,
    Mode,
}

/// How a command ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Exit {
    Code(i64),
    Signal(SharedString),
    Background,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    /// Unchanged lines left out between two changes.
    Gap,
}

/// A change's lines around what changed, at most `OUTPUT_LINES` of them, each with its `+`/`-` mark.
#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    pub added: usize,
    pub removed: usize,
    pub lines: Vec<(LineKind, SharedString)>,
    pub hidden: usize,
}

/// A tool result as its card shows it. Made once per result entry, which never changes once written; later builds
/// of the transcript take it over.
#[derive(Debug, PartialEq)]
pub struct ResultView {
    /// The `toolResult` entry.
    pub entry: SharedString,
    pub text: SharedString,
    pub diff: Option<Diff>,
    /// Decoded once here, not on every frame.
    pub images: Vec<Arc<Image>>,
    pub exit: Option<Exit>,
    pub refused: Option<Refusal>,
    /// The Host kept what the change replaced (`details.undoable`).
    pub undoable: bool,
    pub failed: bool,
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

/// What a row was built from, so a later build can tell whether it would come out the same.
#[derive(Debug, Clone, PartialEq)]
pub struct RowSource {
    chat: SharedString,
    locale: Locale,
    /// Ids of the turn's entries in order. An entry never changes once written (the Host appends to the chat file,
    /// `ChatView` only appends, a reload brings the same records), so the same ids are the same entries.
    entries: Arc<[SharedString]>,
    /// Built with the live reply or the stream in it, so the next build makes it again.
    live: bool,
}

/// One user message and everything the agent did for it. Cloning one is cheap: its parts are shared.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnRow {
    pub key: SharedString,
    pub user: Option<(SharedString, usize)>,
    pub blocks: Arc<[Block]>,
    pub live: bool,
    pub footer: Option<Footer>,
    pub copy: Option<SharedString>,
    /// Blocks before this index are the steps the reply took; the rest is its answer.
    pub answer_from: usize,
    /// Model requests and tool results among the steps.
    pub work_steps: usize,
    /// What the steps did: files read, searches, changes, commands.
    pub summary: SharedString,
    pub source: RowSource,
}

/// At most this many lines of a tool's output or a change are shown.
const OUTPUT_LINES: usize = 60;
/// A longer line of a change is cut.
const LINE_CHARS: usize = 400;
/// Unchanged lines shown around a change.
const DIFF_CONTEXT: usize = 2;
/// A diff taking longer is approximated; it runs while the transcript is built, once per result.
const DIFF_TIMEOUT: Duration = Duration::from_millis(30);

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
    tool_name: &'a str,
    content: &'a [ResultContent],
    is_error: bool,
    details: Option<&'a Value>,
}

fn result_view(result: &ResultEntry<'_>) -> ResultView {
    let details = result.details.and_then(Value::as_object);
    let field = |key: &str| details.and_then(|details| details.get(key));
    let string = |key: &str| field(key).and_then(Value::as_str);
    let refused = match string("refused") {
        Some("user") => Some(Refusal::User),
        Some("mode") => Some(Refusal::Mode),
        _ => None,
    };
    let diff = match (result.tool_name, string("after")) {
        ("write" | "edit", Some(after)) if !result.is_error && refused.is_none() => Some(diff_of(string("before").unwrap_or_default(), after)),
        _ => None,
    };
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
        undoable: field("undoable").and_then(Value::as_bool) == Some(true),
        failed: result.is_error,
    }
}

/// A path as the Host reaches it: relative ones are in the chat's folder.
fn host_path(cwd: &str, path: &str) -> String {
    let absolute = path.starts_with('/') || path.starts_with('\\') || path.as_bytes().get(1) == Some(&b':');
    if absolute || cwd.is_empty() { path.to_owned() } else { format!("{}/{}", cwd.trim_end_matches(['/', '\\']), path.trim_start_matches("./")) }
}

struct Context<'a> {
    locale: Locale,
    cwd: &'a str,
    running: &'a BTreeMap<String, RunningTool>,
    waiting: HashSet<&'a str>,
    /// Calls the user undid, from the `context` entries that record it.
    reverted: HashSet<&'a str>,
    thinking_since: Option<&'a (String, std::time::Instant)>,
}

/// Parts of an earlier build of a turn that come out the same: the text and thinking of its entries, and the views
/// of its results.
#[derive(Default)]
struct Reuse<'a> {
    blocks: HashMap<&'a str, &'a Block>,
    results: HashMap<&'a str, &'a Arc<ResultView>>,
}

impl<'a> Reuse<'a> {
    fn of(row: &'a TurnRow) -> Self {
        let mut reuse = Self::default();
        for block in row.blocks.iter() {
            match block {
                Block::Thinking { key, live: false, .. } | Block::Text { key, .. } if !key.starts_with("stream:") => {
                    reuse.blocks.insert(key.as_ref(), block);
                }
                Block::Tool(ToolRow { result: Some(result), .. }) => {
                    reuse.results.insert(result.entry.as_ref(), result);
                }
                _ => {}
            }
        }
        reuse
    }
}

/// `unfinished`: a call without a result is still to run.
fn tool_row(
    ctx: &Context<'_>, results: &HashMap<&str, ResultEntry<'_>>, reuse: Option<&Reuse<'_>>, id: &str, name: &str,
    arguments: &Map<String, Value>, unfinished: bool,
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
    let result = result.map(|result| match reuse.and_then(|reuse| reuse.results.get(result.id)) {
        Some(view) => Arc::clone(view),
        None => Arc::new(result_view(result)),
    });
    let output = match &result {
        Some(result) => Some(result.text.clone()),
        None => running.and_then(|tool| tool.partial.clone()).map(Into::into),
    };
    let command = is_command(name).then(|| arguments.get("command").and_then(Value::as_str).unwrap_or_default().to_owned().into());
    let file = arguments
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(|path| (SharedString::from(path.to_owned()), SharedString::from(host_path(ctx.cwd, path))));
    ToolRow {
        call_id: id.to_owned().into(),
        name: name.to_owned().into(),
        title: tool_title(ctx.locale, name, arguments).into(),
        command,
        state,
        output,
        file,
        result,
        reverted: ctx.reverted.contains(id),
    }
}

/// `live`: the message is the stream; `pending`: its turn is still running.
fn assistant_blocks(
    ctx: &Context<'_>, results: &HashMap<&str, ResultEntry<'_>>, reuse: Option<&Reuse<'_>>, key: &str, message: &AssistantMessage,
    live: bool, pending: bool, thinking_ms: Option<&Map<String, Value>>, out: &mut Vec<Block>,
) {
    let last = message.content.len().saturating_sub(1);
    for (index, block) in message.content.iter().enumerate() {
        let block_key = format!("{key}:{index}");
        if let Some(earlier) = reuse.and_then(|reuse| reuse.blocks.get(block_key.as_str())) {
            out.push((*earlier).clone());
            continue;
        }
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
                out.push(Block::Thinking { key: block_key.into(), text: thinking.clone().into(), redacted, live: live && index == last, duration_ms });
            }
            AssistantBlock::Text { text, .. } => {
                if !text.trim().is_empty() {
                    out.push(Block::Text { key: block_key.into(), text: text.clone().into() });
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

fn turn_key<'a>(turn: &Turn<'a>) -> &'a str {
    match turn.user {
        Some(TranscriptEntry::User { id, .. }) => id,
        _ => "lead",
    }
}

/// Whether an earlier row of the turn comes out the same: the same entries, and none of its calls is running,
/// waiting, or undone since.
fn unchanged(row: &TurnRow, turn: &Turn<'_>, ctx: &Context<'_>) -> bool {
    let ids = &row.source.entries;
    !row.source.live
        && ids.len() == usize::from(turn.user.is_some()) + turn.items.len()
        && turn.user.into_iter().chain(turn.items.iter().copied()).zip(ids.iter()).all(|(entry, id)| entry.id() == id.as_ref())
        && row.blocks.iter().all(|block| match block {
            Block::Tool(tool) => {
                let id = tool.call_id.as_ref();
                !matches!(tool.state, ToolState::Running | ToolState::Waiting)
                    && !ctx.running.contains_key(id)
                    && !ctx.waiting.contains(id)
                    && tool.reverted == ctx.reverted.contains(id)
            }
            _ => true,
        })
}

/// One turn; `live` while the chat works on it, `stream` the reply streaming into it.
fn build_turn(ctx: &Context<'_>, chat: &SharedString, turn: &Turn<'_>, live: bool, stream: Option<&AssistantMessage>, reuse: Option<&Reuse<'_>>) -> TurnRow {
    let locale = ctx.locale;
    let mut results: HashMap<&str, ResultEntry<'_>> = HashMap::new();
    for item in &turn.items {
        if let TranscriptEntry::ToolResult { id, tool_call_id, tool_name, content, is_error, details, .. } = item {
            results.entry(tool_call_id.as_str()).or_insert(ResultEntry { id, tool_name, content, is_error: *is_error, details: details.as_ref() });
        }
    }
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
                assistant_blocks(ctx, &results, reuse, id, message, false, live, thinking_ms.as_ref(), &mut blocks);
                if !live {
                    let text = assistant_text(message);
                    if !text.is_empty() {
                        copy = Some(text.into());
                    }
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
    let user = match turn.user {
        Some(TranscriptEntry::User { text, images, .. }) => Some((text.clone().into(), images.as_ref().map_or(0, Vec::len))),
        _ => None,
    };
    let mut answer_from = answer_from.unwrap_or(blocks.len());
    if let Some(stream) = stream {
        assistant_blocks(ctx, &results, None, "stream", stream, true, true, None, &mut blocks);
        answer_from = blocks.len();
    }
    let entries = turn.user.into_iter().chain(turn.items.iter().copied()).map(|entry| SharedString::from(entry.id().to_owned())).collect();
    TurnRow {
        key: turn_key(turn).to_owned().into(),
        user,
        blocks: blocks.into(),
        live,
        footer: footer.filter(|_| !live),
        copy,
        answer_from,
        work_steps,
        summary: summary.into(),
        source: RowSource { chat: chat.clone(), locale, entries, live: live || stream.is_some() },
    }
}

/// The rows of a chat; turns of `previous` that come out the same are taken over instead of built again.
fn rows_from(view: &ChatView, locale: Locale, previous: &[TurnRow]) -> Vec<TurnRow> {
    let snapshot = &view.snapshot;
    let chat: SharedString = snapshot.summary.id.clone().into();
    let ctx = Context {
        locale,
        cwd: &snapshot.summary.cwd,
        running: &view.tools,
        waiting: snapshot.approvals.iter().map(|approval| approval.tool_call_id.as_str()).collect(),
        reverted: snapshot
            .entries
            .iter()
            .filter_map(|entry| match entry {
                TranscriptEntry::Context { reverted: Some(ids), .. } => Some(ids),
                _ => None,
            })
            .flatten()
            .map(String::as_str)
            .collect(),
        thinking_since: view.thinking_since.as_ref(),
    };
    let busy = view.busy();
    let turns = group_turns(&snapshot.entries);
    let count = turns.len();
    let mut by_key: Option<HashMap<&str, &TurnRow>> = None;
    let mut rows = Vec::with_capacity(count.max(1));
    for (index, turn) in turns.iter().enumerate() {
        let last = index + 1 == count;
        let live = busy && last;
        let stream = snapshot.streaming.as_ref().filter(|_| last);
        let key = turn_key(turn);
        // Rows stay at their index while a chat grows; loading older messages shifts them.
        let prior = previous
            .get(index)
            .filter(|row| row.key.as_ref() == key)
            .or_else(|| by_key.get_or_insert_with(|| previous.iter().map(|row| (row.key.as_ref(), row)).collect()).get(key).copied())
            .filter(|row| row.source.chat == chat && row.source.locale == locale);
        if let Some(prior) = prior.filter(|prior| !live && stream.is_none() && unchanged(prior, turn, &ctx)) {
            rows.push(prior.clone());
            continue;
        }
        let reuse = prior.map(Reuse::of);
        rows.push(build_turn(&ctx, &chat, turn, live, stream, reuse.as_ref()));
    }
    if rows.is_empty()
        && let Some(stream) = &snapshot.streaming
    {
        rows.push(build_turn(&ctx, &chat, &Turn { user: None, items: Vec::new() }, true, Some(stream), None));
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

/// Expanded tool cards and thinking blocks, by key.
pub type Expanded = Rc<HashSet<SharedString>>;

/// Called with a key when a fold is clicked, and the row index to measure again.
pub type Toggle = Rc<dyn Fn(SharedString, usize, &mut Window, &mut App)>;

fn chevron(open: bool) -> Icon {
    Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall()
}

/// A status color as text: the theme's green and red are fill colors, too light on a light surface.
fn readable(color: Hsla, cx: &App) -> Hsla {
    if cx.theme().mode.is_dark() { color } else { Hsla { l: color.l.min(0.32), ..color } }
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

fn clip_output(text: &str) -> (String, usize) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= OUTPUT_LINES {
        return (text.to_owned(), 0);
    }
    let head = lines[..OUTPUT_LINES / 2].join("\n");
    let tail = lines[lines.len() - OUTPUT_LINES / 2..].join("\n");
    (format!("{head}\n…\n{tail}"), lines.len() - OUTPUT_LINES)
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
        Some(key) => ide.update(cx, |ide, cx| ide.open(key, true, window, cx)),
        None => window.push_notification(tr(cx, "chat.fileOutsideProject").to_string(), cx),
    }
}

/// Undo one change (`chat.revert`) and tell the user what happened, as the changes bar does.
fn undo_change(session: SharedString, call: SharedString, window: &mut Window, cx: &mut App) {
    let request = Store::global(cx).read(cx).call("chat.revert", json!({ "sessionId": session.as_ref(), "toolCallIds": [call.as_ref()] }));
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

fn render_tool(tool: &ToolRow, open: bool, row: usize, toggle: &Toggle, style: &TurnStyle, cx: &App) -> AnyElement {
    let locale = style.locale;
    let theme = cx.theme();
    let key: SharedString = format!("tool:{}", tool.call_id).into();
    let toggle = toggle.clone();
    let mono = theme.mono_font_family.clone();
    let muted = theme.muted_foreground;
    let (good, bad) = (readable(theme.success, cx), readable(theme.danger, cx));
    let title = tool.command.clone().filter(|command| !command.is_empty() && tool.title == *command).unwrap_or(tool.title.clone());
    let result = tool.result.as_deref();
    let refused = result.and_then(|result| result.refused);
    let diff = result.and_then(|result| result.diff.as_ref());
    let exit = result.and_then(|result| result.exit.as_ref()).map(|exit| exit_label(exit, locale));
    let badge = |text: SharedString, color: Hsla| div().flex_shrink_0().text_xs().text_color(color).child(text);
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
                .when_some(exit.clone().filter(|(_, notable)| *notable), |this, (label, _)| this.child(badge(label.into(), bad)))
                .when(matches!(result.and_then(|result| result.exit.as_ref()), Some(Exit::Background)), |this| {
                    this.child(badge(tr(cx, "chat.runningInBackground"), muted))
                })
                .child(chevron(open)),
        )
        .when(open, |this| {
            let mut body = v_flex().px_3().pb_2().gap_1().text_xs();
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
                            .text_color(theme.link)
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
                    let busy = session.as_ref().is_none_or(|id| {
                        Store::global(cx).read(cx).summaries.iter().any(|summary| summary.id == id.as_ref() && matches!(summary.status.as_str(), "running" | "compacting"))
                    });
                    let call = tool.call_id.clone();
                    line = line.child(
                        Button::new(ElementId::Name(format!("tool-undo-{}", tool.call_id).into()))
                            .ghost()
                            .xsmall()
                            .label(tr(cx, "chat.undoChange"))
                            .disabled(busy)
                            .when(busy, |this| this.tooltip(tr(cx, "review.changesBusy")))
                            .on_click(move |_, window, cx| {
                                if let Some(session) = session.clone() {
                                    undo_change(session, call.clone(), window, cx);
                                }
                            }),
                    );
                }
                body = body.child(line);
            }
            if let Some(command) = &tool.command {
                body = body.child(div().font_family(mono.clone()).text_color(muted).child(format!("$ {command}")));
            }
            if let Some(refused) = refused {
                body = body.child(div().text_color(muted).child(tr(cx, if refused == Refusal::User { "chat.toolRefusedUser" } else { "chat.toolRefusedMode" })));
            } else if let Some(diff) = diff {
                body = body.child(v_flex().w_full().font_family(mono.clone()).children(diff.lines.iter().map(|(kind, text)| {
                    let tint = match kind {
                        LineKind::Added => Some(theme.success.opacity(0.16)),
                        LineKind::Removed => Some(theme.danger.opacity(0.16)),
                        LineKind::Context | LineKind::Gap => None,
                    };
                    div().px_1().when_some(tint, |this, tint| this.bg(tint)).when(*kind == LineKind::Gap, |this| this.text_color(muted)).child(text.clone())
                })));
                if diff.hidden > 0 {
                    body = body.child(div().text_color(muted).child(trf(cx, "chat.linesHidden", &[("count", &diff.hidden)])));
                }
            } else {
                let (output, hidden) = clip_output(tool.output.as_deref().unwrap_or_default());
                let output = if output.trim().is_empty() && tool.state != ToolState::Running {
                    i18n::text(locale, "chat.noOutput").to_owned()
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
            if let Some((label, notable)) = exit {
                body = body.child(div().text_color(if notable { bad } else { muted }).child(label));
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
    /// The chat shown, for actions on one tool call.
    pub session: Option<SharedString>,
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
                render_tool(tool, open, index, toggle, style, cx)
            }
            Block::Notice { level, text } => {
                let color = match level.as_ref() {
                    "error" => theme.danger,
                    "warning" => crate::theme::warning_text(cx),
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
            json!({"id": "u2", "kind": "user", "ts": 5, "text": "二"}),
            assistant("a3", json!([{"type": "text", "text": "先看看这个文件里现在写了什么"}, call("c2", "edit", json!({"file_path": "b"}))]), "toolUse"),
            result("r2", "c2", "edit", json!([{"type": "text", "text": "ok"}]), false, json!({"path": "b", "before": "1\n", "after": "2\n"})),
        ];
        let stream = |text: &str| assistant("x", json!([{"type": "text", "text": text}]), "pending")["message"].clone();
        let previous = build_rows(&view(Value::from(first.clone()), stream("正"), "running"), Locale::Zh);
        // A streamed token: the finished turn is taken over whole, the live one is built again around its finished parts.
        let token = view(Value::from(first.clone()), stream("正在"), "running");
        let rows = rebuild_rows(&token, Locale::Zh, &previous);
        assert_eq!(rows, build_rows(&token, Locale::Zh));
        assert!(Arc::ptr_eq(&rows[0].blocks, &previous[0].blocks));
        assert!(!Arc::ptr_eq(&rows[1].blocks, &previous[1].blocks));
        let (Block::Text { text: now, .. }, Block::Text { text: before, .. }) = (&rows[1].blocks[0], &previous[1].blocks[0]) else { panic!("text") };
        // Texts this long are shared, not copied (SharedString keeps up to 23 bytes inline).
        assert_eq!(now.as_ptr(), before.as_ptr());
        assert!(Arc::ptr_eq(tools(&rows[1])[0].result.as_ref().unwrap(), tools(&previous[1])[0].result.as_ref().unwrap()));
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
    fn clips_long_output() {
        let text = (0..100).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let (clipped, hidden) = clip_output(&text);
        assert_eq!(hidden, 40);
        assert!(clipped.starts_with("0\n") && clipped.ends_with("\n99"));
    }
}
