//! One chat as the interface sees it, kept current from Host events (`src/renderer/chat/chat-store.ts`).

use std::collections::BTreeMap;

use rainy_protocol::{
    AssistantMessage, SessionEntryEvent, SessionSnapshot, SessionStateEvent, SessionToolEvent, StreamUpdate,
    TranscriptEntry, apply_stream_patch,
};
use serde_json::Value;

/// Entries loaded when a chat opens; older ones load on demand.
pub const INITIAL_ENTRIES: usize = 400;

/// Live output of a running tool.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunningTool {
    pub tool_name: String,
    pub args: Option<Value>,
    pub partial: Option<String>,
}

/// One chat.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatView {
    pub snapshot: SessionSnapshot,
    pub tools: BTreeMap<String, RunningTool>,
    /// Entries before the loaded window exist on the Host.
    pub truncated: bool,
    /// Bumped on every change, so views can tell whether to rebuild.
    pub revision: u64,
    /// The streaming thinking block (`timestamp:index`) and when it began, for its timer.
    pub thinking_since: Option<(String, std::time::Instant)>,
}

impl ChatView {
    pub fn new(snapshot: SessionSnapshot, whole: bool) -> Self {
        let tools = snapshot.running_tools.iter().map(|id| (id.clone(), RunningTool::default())).collect();
        let truncated = !whole && snapshot.entries.len() >= INITIAL_ENTRIES;
        Self { snapshot, tools, truncated, revision: 0, thinking_since: None }
    }

    pub fn busy(&self) -> bool {
        matches!(self.snapshot.summary.status.as_str(), "running" | "compacting")
    }

    fn touched(&mut self) -> bool {
        self.revision += 1;
        self.follow_thinking();
        true
    }

    /// Note when the streaming reply's last block began thinking.
    fn follow_thinking(&mut self) {
        let live = self.snapshot.streaming.as_ref().and_then(|message| {
            let index = message.content.len().checked_sub(1)?;
            matches!(message.content[index], rainy_protocol::AssistantBlock::Thinking { .. }).then(|| format!("{}:{index}", message.timestamp))
        });
        match live {
            None => self.thinking_since = None,
            Some(key) if self.thinking_since.as_ref().is_none_or(|(known, _)| *known != key) => {
                self.thinking_since = Some((key, std::time::Instant::now()));
            }
            Some(_) => {}
        }
    }

    /// `session.entry`; returns whether anything changed.
    pub fn apply_entry(&mut self, event: SessionEntryEvent) -> bool {
        if self.snapshot.entries.iter().any(|item| item.id() == event.entry.id()) {
            return false;
        }
        // A finished reply leaves the stream when its entry arrives, so the transcript never drops it in between.
        if matches!(event.entry, TranscriptEntry::Assistant { .. }) {
            self.snapshot.streaming = None;
        }
        self.snapshot.entries.push(event.entry);
        self.touched()
    }

    /// `session.stream`.
    pub fn apply_stream(&mut self, update: StreamUpdate) -> bool {
        match update {
            StreamUpdate::Whole(None) => false,
            StreamUpdate::Whole(Some(message)) => {
                self.snapshot.streaming = Some(*message);
                self.touched()
            }
            // A patch that does not fit (the chat was loaded between two updates) waits for the next whole message.
            StreamUpdate::Patch(patch) => {
                let applied = self.snapshot.streaming.as_mut().is_some_and(|streaming| apply_stream_patch(streaming, &patch));
                applied && self.touched()
            }
        }
    }

    /// `session.tool`.
    pub fn apply_tool(&mut self, event: SessionToolEvent) -> bool {
        if event.phase == "end" {
            self.tools.remove(&event.tool_call_id);
        } else {
            let tool = self.tools.entry(event.tool_call_id).or_default();
            tool.tool_name = event.tool_name;
            if event.args.is_some() {
                tool.args = event.args;
            }
            if event.partial.is_some() {
                tool.partial = event.partial;
            }
        }
        self.touched()
    }

    /// `session.state`.
    pub fn apply_state(&mut self, event: SessionStateEvent) -> bool {
        let snapshot = &mut self.snapshot;
        snapshot.queue = event.queue;
        snapshot.model = event.model;
        snapshot.context = event.context;
        snapshot.error = event.error;
        snapshot.permission = event.permission;
        snapshot.background = event.background;
        snapshot.retry = event.retry;
        snapshot.summary.awaiting_approval = (!event.approvals.is_empty()).then_some(true);
        snapshot.approvals = event.approvals;
        if event.status == "idle" || event.status == "error" {
            self.tools.clear();
            snapshot.streaming = None;
        }
        snapshot.summary.status = event.status;
        self.touched()
    }
}

/// A turn: a user message and everything after it until the next one.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn<'a> {
    pub user: Option<&'a TranscriptEntry>,
    pub items: Vec<&'a TranscriptEntry>,
}

/// Entries grouped into turns; entries before the first user message form a leading turn without one.
pub fn group_turns(entries: &[TranscriptEntry]) -> Vec<Turn<'_>> {
    let mut turns: Vec<Turn<'_>> = Vec::new();
    for entry in entries {
        if matches!(entry, TranscriptEntry::User { .. }) {
            turns.push(Turn { user: Some(entry), items: Vec::new() });
        } else {
            if turns.is_empty() {
                turns.push(Turn { user: None, items: Vec::new() });
            }
            turns.last_mut().expect("a turn").items.push(entry);
        }
    }
    turns
}

/// The visible text of an assistant message.
pub fn assistant_text(message: &AssistantMessage) -> String {
    message.content.iter().filter_map(|block| match block {
        rainy_protocol::AssistantBlock::Text { text, .. } => Some(text.as_str()),
        _ => None,
    }).collect::<Vec<_>>().join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rainy_protocol::parse_stream_event;
    use serde_json::json;

    fn snapshot() -> SessionSnapshot {
        serde_json::from_value(json!({
            "summary": {"id": "s", "workspaceId": null, "cwd": "C:\\p", "title": "t", "createdAt": 1, "updatedAt": 2,
                        "archived": false, "pinned": false, "status": "running"},
            "entries": [{"id": "u1", "kind": "user", "ts": 1, "text": "你好"}],
            "model": {"provider": "deepseek", "model": "deepseek-chat"}, "queue": [], "context": null,
            "streaming": null, "runningTools": ["c0"], "permission": "ask", "approvals": [], "background": []
        }))
        .unwrap()
    }

    fn assistant(id: &str, text: &str) -> TranscriptEntry {
        serde_json::from_value(json!({"id": id, "kind": "assistant", "ts": 2, "message": {
            "role": "assistant", "content": [{"type": "text", "text": text}], "api": "x", "provider": "p", "model": "m",
            "stopReason": "stop", "timestamp": 2,
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                      "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}}}}))
        .unwrap()
    }

    #[test]
    fn streams_then_settles_into_an_entry() {
        let mut chat = ChatView::new(snapshot(), false);
        assert!(chat.tools.contains_key("c0"));
        let (_, whole) = parse_stream_event(&json!({"sessionId": "s", "message": {
            "role": "assistant", "content": [{"type": "text", "text": "你"}], "api": "x", "provider": "p", "model": "m",
            "stopReason": "pending", "timestamp": 2,
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                      "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}}}}))
        .unwrap();
        assert!(chat.apply_stream(whole));
        let (_, patch) = parse_stream_event(&json!({"sessionId": "s", "patch": {"append": [{"index": 0, "at": 1, "text": "好"}]}})).unwrap();
        assert!(chat.apply_stream(patch));
        assert_eq!(assistant_text(chat.snapshot.streaming.as_ref().unwrap()), "你好");
        // A patch that does not fit is ignored.
        let (_, stale) = parse_stream_event(&json!({"sessionId": "s", "patch": {"append": [{"index": 0, "at": 1, "text": "x"}]}})).unwrap();
        assert!(!chat.apply_stream(stale));
        assert!(chat.apply_entry(SessionEntryEvent { session_id: "s".into(), entry: assistant("a1", "你好") }));
        assert!(chat.snapshot.streaming.is_none());
        assert!(!chat.apply_entry(SessionEntryEvent { session_id: "s".into(), entry: assistant("a1", "你好") }));
        assert_eq!(chat.snapshot.entries.len(), 2);
    }

    #[test]
    fn idle_clears_tools_and_stream() {
        let mut chat = ChatView::new(snapshot(), false);
        let state: SessionStateEvent = serde_json::from_value(json!({"sessionId": "s", "status": "idle", "queue": [],
            "model": null, "context": null, "permission": "auto", "approvals": [], "background": []})).unwrap();
        chat.apply_state(state);
        assert!(chat.tools.is_empty());
        assert_eq!(chat.snapshot.permission, "auto");
        assert!(!chat.busy());
    }

    #[test]
    fn groups_turns() {
        let entries = vec![assistant("a0", "前言"), snapshot().entries[0].clone(), assistant("a1", "答")];
        let turns = group_turns(&entries);
        assert_eq!(turns.len(), 2);
        assert!(turns[0].user.is_none());
        assert_eq!(turns[1].items.len(), 1);
    }
}
