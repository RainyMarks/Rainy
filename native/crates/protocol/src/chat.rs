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
}

/// A tool call waiting for the user.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PendingApproval {
    pub tool_call_id: String,
    pub tool_name: String,
    /// `command`, `outside-write`, `mcp` or `web`.
    pub reason: String,
    #[serde(default)]
    pub rule: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
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
