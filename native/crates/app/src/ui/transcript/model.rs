//! Data types for rendered chat turns, tool cards, diffs, and presentation options.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::*;
use rainy_protocol::UsageSummary;

use crate::i18n::Locale;

/// At most this many lines of a tool's output or a change are shown.
pub(super) const OUTPUT_LINES: usize = 60;
/// A longer line of a change is cut.
pub(super) const LINE_CHARS: usize = 400;
/// Unchanged lines shown around a change.
pub(super) const DIFF_CONTEXT: usize = 2;
/// A diff taking longer is approximated; it runs while the transcript is built, once per result.
pub(super) const DIFF_TIMEOUT: Duration = Duration::from_millis(30);

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
    /// Pre-clipped output text and hidden line count, computed once when building the row.
    pub clipped_output: Option<(SharedString, usize)>,
    /// The file the call names, as the model wrote it and as a Host path, to open it in the editor.
    pub file: Option<(SharedString, SharedString)>,
    pub result: Option<Arc<ResultView>>,
    /// The user undid the change.
    pub reverted: bool,
    /// A `task` call: what its sub-agent did.
    pub subagent: Option<Arc<SubagentView>>,
}

/// What a `task` call's sub-agent did, from its progress while it runs and from its result once it reported.
#[derive(Debug, Default, PartialEq)]
pub struct SubagentView {
    /// Its latest tool calls: what each did and how it went.
    pub steps: Vec<(SharedString, ToolState)>,
    /// Calls made before those listed.
    pub earlier: usize,
    /// Calls made in all.
    pub calls: usize,
    /// The configured model the call chose for it.
    pub model: Option<SharedString>,
    /// The limit that ended it: `steps`, `time` or `budget`.
    pub stopped: Option<SharedString>,
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
    /// The file the following lines belong to, in a change of several files.
    File,
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
    /// Every file change the call made, by the id its undo goes by: the call's own id for `write` and `edit`, one
    /// per file for `apply_patch`; `undo` those that can be undone.
    pub changes: Vec<SharedString>,
    pub undo: Vec<SharedString>,
    /// A plan tool's checklist: each item and its status.
    pub plan: Option<Vec<(SharedString, SharedString)>>,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Thinking {
        key: SharedString,
        text: SharedString,
        /// Pre-normalized Markdown for pointer-identical `TextView` caching.
        prepared: SharedString,
        redacted: bool,
        live: bool,
        duration_ms: Option<f64>,
        /// Live start instant so elapsed seconds tick on repaint without rebuilding the row list.
        started_at: Option<Instant>,
    },
    Text {
        key: SharedString,
        text: SharedString,
        /// Pre-normalized Markdown for pointer-identical `TextView` caching.
        prepared: SharedString,
    },
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
    pub(super) chat: SharedString,
    pub(super) locale: Locale,
    /// Ids of the turn's entries in order. An entry never changes once written (the Host appends to the chat file,
    /// `ChatView` only appends, a reload brings the same records), so the same ids are the same entries.
    pub(super) entries: Arc<[SharedString]>,
    /// Built with the live reply or the stream in it, so the next build makes it again.
    pub(super) live: bool,
}

/// One user message and everything the agent did for it. Cloning one is cheap: its parts are shared.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnRow {
    pub key: SharedString,
    pub user: Option<(SharedString, usize)>,
    /// Pre-escaped `<pre>` HTML for `user`, preserving `SharedString` pointer identity across frames.
    pub user_html: Option<SharedString>,
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
    /// The live turn waits for the agent with nothing to show (it starts, or thinks between steps): for how many
    /// milliseconds.
    pub waiting: Option<f64>,
    /// Start instant for `waiting` so elapsed seconds update on cheap repaint without full `rebuild_rows`.
    pub waiting_since: Option<Instant>,
    /// The chat compacts its earlier conversation while this turn is the last.
    pub compacting: bool,
    pub source: RowSource,
}

/// Expanded tool cards and thinking blocks, by key.
pub type Expanded = Rc<HashSet<SharedString>>;

/// Called with a key when a fold is clicked, and the row index to measure again.
pub type Toggle = Rc<dyn Fn(SharedString, usize, &mut Window, &mut App)>;

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
    /// Whether the active chat is currently running/compacting (avoids per-card linear scans of `Store.summaries`).
    pub chat_busy: bool,
    /// Who a live turn waits for: the external agent or the model.
    pub waiting_for: SharedString,
}
