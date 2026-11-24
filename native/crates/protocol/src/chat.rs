//! Chats: transcript entries, the streaming reply, run state and approvals.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::models::ModelSelection;

/// One block of an assistant message (pi-ai `TextContent | ThinkingContent | ToolCall`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum AssistantBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        /// Replay data of the OpenAI Responses API (`{"v":1,"id":…}`).
        #[serde(default, rename = "textSignature", skip_serializing_if = "Option::is_none")]
        text_signature: Option<String>,
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        /// Anthropic's signature, the Responses API's reasoning item, or the Chat Completions field the text came in.
        #[serde(default, rename = "thinkingSignature", skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
    #[serde(rename = "toolCall")]
    ToolCall {
        id: String,
        name: String,
        #[serde(default)]
        arguments: Map<String, Value>,
        #[serde(flatten)]
        rest: Map<String, Value>,
    },
}

impl AssistantBlock {
    /// Streamed text of a text or thinking block.
    pub fn streamed(&self) -> Option<&str> {
        match self {
            Self::Text { text, .. } => Some(text),
            Self::Thinking { thinking, .. } => Some(thinking),
            Self::ToolCall { .. } => None,
        }
    }

    fn streamed_mut(&mut self) -> Option<&mut String> {
        match self {
            Self::Text { text, .. } => Some(text),
            Self::Thinking { thinking, .. } => Some(thinking),
            Self::ToolCall { .. } => None,
        }
    }

    /// Append streamed characters; `at` is the length in UTF-16 code units the sender saw, as JavaScript counts.
    pub(crate) fn append(&mut self, at: usize, text: &str) -> bool {
        let Some(current) = self.streamed_mut() else { return false };
        if utf16_len(current) != at {
            return false;
        }
        current.push_str(text);
        true
    }
}

/// Length of a string as JavaScript's `String.length` counts it.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Token counts of a request.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<f64>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// pi-ai `AssistantMessage`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub content: Vec<AssistantBlock>,
    /// The protocol the reply came through: `openai-completions`, `openai-responses` or `anthropic-messages`.
    #[serde(default)]
    pub api: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub stop_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default)]
    pub timestamp: f64,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// Text or image content of a tool result.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum ResultContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

/// An image the user attached.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ImageContent {
    #[serde(rename = "type")]
    pub kind: String,
    pub data: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

/// Token usage of one model request or a whole turn.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

/// A transcript entry as persisted and as shown.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TranscriptEntry {
    #[serde(rename_all = "camelCase")]
    User {
        id: String,
        ts: f64,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
    },
    #[serde(rename_all = "camelCase")]
    Assistant {
        id: String,
        ts: f64,
        message: AssistantMessage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<f64>,
        /// How long each thinking block streamed, by content index (JSON object keys are strings).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking_ms: Option<Map<String, Value>>,
    },
    #[serde(rename_all = "camelCase")]
    ToolResult {
        id: String,
        ts: f64,
        tool_call_id: String,
        tool_name: String,
        content: Vec<ResultContent>,
        is_error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<f64>,
    },
    #[serde(rename_all = "camelCase")]
    Context {
        id: String,
        ts: f64,
        label: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reverted: Option<Vec<String>>,
    },
    #[serde(rename_all = "camelCase")]
    Compaction {
        id: String,
        ts: f64,
        first_id: String,
        last_id: String,
        summary: String,
        tokens_before: f64,
        trigger: String,
        /// How the checkpoint was made (`{ mode, usage }`): `mode` is `shared` or `separate` for a summary the model
        /// wrote, `recall` for an index the Host built, whose `summary` points the model to `recall`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<Value>,
    },
    /// Tool outputs taken out of what the model sees to free context. The entries stay in the file: the model reads a
    /// placeholder in their place and `recall` returns them. Versions before 4.6 skip this kind and see the outputs.
    #[serde(rename_all = "camelCase")]
    Cleared {
        id: String,
        ts: f64,
        /// The tool result entries the model no longer sees.
        entry_ids: Vec<String>,
        tokens_before: f64,
        tokens_after: f64,
        trigger: String,
    },
    #[serde(rename_all = "camelCase")]
    Notice {
        id: String,
        ts: f64,
        level: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Turn {
        id: String,
        ts: f64,
        duration_ms: f64,
        usage: UsageSummary,
        provider: String,
        model: String,
        requests: f64,
    },
}

impl TranscriptEntry {
    pub fn id(&self) -> &str {
        match self {
            Self::User { id, .. }
            | Self::Assistant { id, .. }
            | Self::ToolResult { id, .. }
            | Self::Context { id, .. }
            | Self::Compaction { id, .. }
            | Self::Cleared { id, .. }
            | Self::Notice { id, .. }
            | Self::Turn { id, .. } => id,
        }
    }
}

/// `idle`, `running`, `compacting` or `error`.
pub type SessionRunStatus = String;

/// History-list row.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub workspace_id: Option<String>,
    pub cwd: String,
    pub title: String,
    pub created_at: f64,
    pub updated_at: f64,
    pub archived: bool,
    pub pinned: bool,
    pub status: SessionRunStatus,
    #[serde(default)]
    pub awaiting_approval: Option<bool>,
    /// When the user last saw the chat (epoch ms); a later `updated_at` of an idle chat is a reply they have not seen.
    /// None for chats of 4.8 and earlier until they are opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<f64>,
    /// The Git worktree the chat works in, away from the project's own checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
    /// The swarm the chat belongs to: its coordinator, or one of its workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<SwarmLink>,
    /// Orchestration: the chat's model plans and this model carries the plan out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrate: Option<crate::models::ModelSelection>,
    /// External CLI agent backend driving this chat (`codex`, `antigravity` or `acp`), when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_backend: Option<ExternalBackendRef>,
    /// External thread or session identifier (`threadId` / ACP `sessionId`) used to resume across restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_thread_id: Option<String>,
}

fn default_true() -> bool {
    true
}

/// External CLI agent backend attached to a chat (`codex`, `antigravity` or generic `acp`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalBackendRef {
    /// `codex`, `antigravity` or `acp`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One model exposed by an external agent CLI (`external.status` / `external.probe`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
}

/// User configuration for one external CLI agent stored in `settings.json` (`external.configure`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalAgentConfig {
    /// `codex`, `antigravity` or `acp`.
    pub kind: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thinking: Option<String>,
}

/// A slash command an external agent offers (ACP `available_commands_update`), sent to it as `/name text`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What to write after it, when it takes text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Detection and configuration snapshot of one external agent (`external.status` / `external.probe`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalAgentStatus {
    pub kind: String,
    pub label: String,
    pub enabled: bool,
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ExternalModelInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The slash commands the agent last listed in this Host (Antigravity's `/swarm`, `/teamwork`…).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<ExternalCommand>,
}

/// Response of `external.status`, `external.probe`, `external.configure` and payload of `external.changed`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalStatusResponse {
    #[serde(default)]
    pub agents: Vec<ExternalAgentStatus>,
}

/// The team an external agent runs for a chat (`external.team`): an Antigravity swarm or teamwork, or Codex
/// sub-agents. Members form a tree under the chat's own agent; the board holds the posts they exchange.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalTeam {
    pub session_id: String,
    /// `antigravity` or `codex`.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// The chat's own agent first, then every member depth first.
    #[serde(default)]
    pub members: Vec<TeamMember>,
    /// Board posts newer than the `after` the caller gave (all of them without one), oldest first.
    #[serde(default)]
    pub posts: Vec<TeamPost>,
    /// Number of the newest post on the board.
    #[serde(default)]
    pub last_post: u64,
    /// The plan's groups of tasks (Antigravity swarms).
    #[serde(default)]
    pub threads: Vec<TeamThread>,
    /// Changes whenever anything above does.
    #[serde(default)]
    pub revision: u64,
}

/// One agent of an external team.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TeamMember {
    /// The external agent's own id of it (conversation or thread id).
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Its handle without `@` (`parser`, `verifier`), or a role name.
    pub name: String,
    /// `lead`, `worker`, `verifier`, `monitor`, `teamwork`, `orchestrator`, `explorer`, `challenger`, `interviewer`,
    /// `auditor`, `browser` or `subagent`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Its assignment from the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// `running`, `waiting` (asleep until mentioned), `idle`, `done`, `error` or `killed`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// What it said it focuses on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    /// Its latest action, one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// When it last did something, Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_at: Option<f64>,
    /// Its own checklist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<TeamStep>,
    #[serde(default)]
    pub depth: u32,
}

/// One item of a member's checklist.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeamStep {
    pub title: String,
    /// `pending`, `in_progress` or `completed`.
    pub status: String,
}

/// A post on an external team's board.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TeamPost {
    pub seq: u64,
    /// The member's name, or `lead` for the chat's own agent.
    pub from: String,
    /// The names it mentions, `all` for everyone.
    #[serde(default)]
    pub to: Vec<String>,
    pub text: String,
    /// Unix milliseconds, 0 when only `time` is known.
    #[serde(default)]
    pub ts: f64,
    /// Local time as the external agent wrote it (`2026-11-23 10:46:31`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
}

/// A group of tasks in an external team's plan.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeamThread {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// Member names.
    #[serde(default)]
    pub members: Vec<String>,
}

/// One recent step of a team member (`external.teamMember`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TeamActivity {
    /// Unix milliseconds.
    pub ts: f64,
    /// `tool`, `reply`, `thinking`, `message` (a message it received) or `error`.
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A chat's place in a swarm (`/swarm`): the coordinator splits a task among worker chats, which talk over one board.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SwarmLink {
    /// The coordinator's chat id; the coordinator's own link names itself.
    pub coordinator: String,
    /// `coordinator`, or the worker's name without the `@`.
    pub name: String,
}

impl SessionSummary {
    /// A reply came after the user last saw the chat and nothing runs any more.
    pub fn unread(&self) -> bool {
        self.status == "idle" && self.read_at.is_some_and(|read| self.updated_at > read + 1000.0)
    }
}

/// A chat's own Git worktree (the Codex app's "Worktree" mode): a detached checkout of the project's repository in the
/// Host's `worktrees` folder, which the chat can turn into a branch or hand off to the project's checkout.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    /// The worktree's folder, the chat's working folder.
    pub path: String,
    /// The project's checkout it was made from.
    pub repository: String,
    /// The commit it started at.
    pub base: String,
    /// The branch made from it, once one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// A tool call waiting for the user.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PendingApproval {
    pub tool_call_id: String,
    pub tool_name: String,
    /// `command`, `outside-write`, `outside-read`, `outside-workdir`, `mcp` or `web`.
    pub reason: String,
    #[serde(default)]
    pub rule: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// The member of an external agent's team that asks (`@alpha`), when it is not the chat's own agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// Token breakdown by context category for `session.contextBreakdown`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContextBreakdownSegments {
    #[serde(default)]
    pub system: u64,
    #[serde(default)]
    pub instructions: u64,
    #[serde(default)]
    pub mcp_tools: u64,
    #[serde(default)]
    pub core_tools: u64,
    #[serde(default)]
    pub history: u64,
    #[serde(default)]
    pub recall: u64,
}

/// Detailed context-window breakdown for one chat (`session.contextBreakdown`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContextBreakdown {
    pub session_id: String,
    pub context_window: u64,
    pub input_limit: u64,
    pub compact_at: u64,
    pub output_tokens: u64,
    pub total: u64,
    pub calibrated_total: u64,
    pub calibration_factor: f64,
    pub kind: String,
    pub segments: ContextBreakdownSegments,
}

/// A message waiting for the current run to reach a step boundary (steer) or to finish (queue).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueuedMessage {
    pub id: String,
    pub text: String,
    pub mode: String,
    pub image_count: u32,
}

/// Context-window accounting for the next request.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub tokens: f64,
    pub context_window: f64,
    pub input_limit: f64,
    pub output_tokens: f64,
    pub compact_at: f64,
    pub kind: String,
}

/// A command the agent started in the background.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundCommand {
    pub id: String,
    pub command: String,
    pub description: String,
    pub cwd: String,
    pub started_at: f64,
    pub state: String,
    pub exit_code: Option<i64>,
    pub signal: Option<String>,
    pub pending: f64,
}

/// A run waiting before it retries a request that failed for a passing reason.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RetryWait {
    pub attempt: u32,
    pub max: u32,
    pub at: f64,
    pub reason: String,
}

/// Everything needed to show one chat.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub summary: SessionSummary,
    pub entries: Vec<TranscriptEntry>,
    pub model: Option<ModelSelection>,
    pub queue: Vec<QueuedMessage>,
    pub context: Option<ContextUsage>,
    pub streaming: Option<AssistantMessage>,
    pub running_tools: Vec<String>,
    pub permission: String,
    pub approvals: Vec<PendingApproval>,
    #[serde(default)]
    pub background: Vec<BackgroundCommand>,
    #[serde(default)]
    pub retry: Option<RetryWait>,
    #[serde(default)]
    pub error: Option<String>,
}

/// `session.state`: run status, queue, model or approvals changed.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionStateEvent {
    pub session_id: String,
    pub status: SessionRunStatus,
    pub queue: Vec<QueuedMessage>,
    pub model: Option<ModelSelection>,
    pub context: Option<ContextUsage>,
    pub permission: String,
    pub approvals: Vec<PendingApproval>,
    #[serde(default)]
    pub background: Vec<BackgroundCommand>,
    #[serde(default)]
    pub retry: Option<RetryWait>,
    #[serde(default)]
    pub error: Option<String>,
}

/// `session.entry`: a transcript entry was appended.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntryEvent {
    pub session_id: String,
    pub entry: TranscriptEntry,
}

/// `session.tool`: partial output of a running tool.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionToolEvent {
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub phase: String,
    #[serde(default)]
    pub args: Option<Value>,
    #[serde(default)]
    pub partial: Option<String>,
}

/// A file the agent changed in a chat that the user has neither kept nor undone (`chat.changes`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatChange {
    /// Absolute path on the Host.
    pub path: String,
    /// The path as the chat shows it.
    pub display: String,
    pub tool_call_ids: Vec<String>,
    pub created: bool,
    pub deleted: bool,
    pub added: u64,
    pub removed: u64,
    /// The text before the changes is kept, so they can be reviewed.
    pub comparable: bool,
    /// Every call kept what it replaced, so the change can be undone.
    pub undoable: bool,
}

/// One file `chat.revert` could not put back.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RevertFailure {
    pub tool_call_id: String,
    pub path: String,
    /// `changed`, `unavailable` or `failed`.
    pub reason: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RevertResult {
    pub reverted: Vec<serde_json::Value>,
    pub failed: Vec<RevertFailure>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_summary_stays_compatible_with_older_chats_and_external_backends() {
        let legacy: SessionSummary = serde_json::from_value(json!({
            "id": "s1",
            "workspaceId": null,
            "cwd": "/proj",
            "title": "Old chat",
            "createdAt": 1.0,
            "updatedAt": 2.0,
            "archived": false,
            "pinned": false,
            "status": "idle"
        }))
        .unwrap();
        assert!(legacy.external_backend.is_none());
        assert!(legacy.external_thread_id.is_none());
        let encoded_legacy = serde_json::to_value(&legacy).unwrap();
        assert!(encoded_legacy.get("externalBackend").is_none());
        assert!(encoded_legacy.get("externalThreadId").is_none());

        let ext: SessionSummary = serde_json::from_value(json!({
            "id": "s2",
            "workspaceId": "w1",
            "cwd": "/proj",
            "title": "Codex chat",
            "createdAt": 1.0,
            "updatedAt": 2.0,
            "archived": false,
            "pinned": false,
            "status": "idle",
            "externalBackend": { "kind": "codex", "binary": "codex", "args": [] },
            "externalThreadId": "thr_123"
        }))
        .unwrap();
        assert_eq!(ext.external_backend.as_ref().map(|b| b.kind.as_str()), Some("codex"));
        assert_eq!(ext.external_thread_id.as_deref(), Some("thr_123"));

        let cfg: ExternalAgentConfig = serde_json::from_value(json!({ "kind": "antigravity" })).unwrap();
        assert!(cfg.enabled);
        let status: ExternalStatusResponse = serde_json::from_value(json!({
            "agents": [{
                "kind": "codex",
                "label": "Codex",
                "enabled": true,
                "installed": true,
                "version": "0.170.0",
                "models": [{ "id": "gpt-5.3-codex", "reasoningEfforts": ["low", "high"], "isDefault": true }]
            }]
        }))
        .unwrap();
        assert_eq!(status.agents.len(), 1);
        assert_eq!(status.agents[0].models[0].id, "gpt-5.3-codex");
    }
}
