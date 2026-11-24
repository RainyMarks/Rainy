//! One chat as the interface sees it, kept current from Host events (`src/renderer/chat/chat-store.ts`).

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

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

/// Index range of one turn inside `SessionSnapshot::entries`, maintained incrementally on append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRange {
    pub user: Option<usize>,
    pub items: Range<usize>,
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
    /// When the chat last changed: while it runs with nothing streaming, how long it has waited for the agent.
    pub changed_at: std::time::Instant,
    /// O(1) entry-id membership set for incremental deduplication.
    entry_ids: HashSet<String>,
    /// Tool call ids undone by `context` entries, maintained incrementally to avoid O(N) scans during streaming.
    reverted_ids: HashSet<String>,
    /// Incrementally maintained turn boundaries over `snapshot.entries`.
    turn_ranges: Vec<TurnRange>,
}

impl ChatView {
    pub fn new(snapshot: SessionSnapshot, whole: bool) -> Self {
        let tools = snapshot.running_tools.iter().map(|id| (id.clone(), RunningTool::default())).collect();
        let truncated = !whole && snapshot.entries.len() >= INITIAL_ENTRIES;
        let mut entry_ids = HashSet::with_capacity(snapshot.entries.len());
        let mut reverted_ids = HashSet::new();
        let mut turn_ranges: Vec<TurnRange> = Vec::new();
        for (idx, entry) in snapshot.entries.iter().enumerate() {
            entry_ids.insert(entry.id().to_owned());
            if let TranscriptEntry::Context { reverted: Some(ids), .. } = entry {
                reverted_ids.extend(ids.iter().cloned());
            }
            push_turn_range(&mut turn_ranges, idx, entry);
        }
        Self { snapshot, tools, truncated, revision: 0, thinking_since: None, changed_at: std::time::Instant::now(), entry_ids, reverted_ids, turn_ranges }
    }

    pub fn reverted_ids(&self) -> &HashSet<String> {
        &self.reverted_ids
    }

    pub fn turn_ranges(&self) -> &[TurnRange] {
        &self.turn_ranges
    }

    pub fn busy(&self) -> bool {
        matches!(self.snapshot.summary.status.as_str(), "running" | "compacting")
    }

    fn touched(&mut self) -> bool {
        self.revision += 1;
        self.changed_at = std::time::Instant::now();
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
        let id = event.entry.id().to_owned();
        if !self.entry_ids.insert(id) {
            return false;
        }
        // A finished reply leaves the stream when its entry arrives, so the transcript never drops it in between.
        if matches!(event.entry, TranscriptEntry::Assistant { .. }) {
            self.snapshot.streaming = None;
        }
        if let TranscriptEntry::Context { reverted: Some(ids), .. } = &event.entry {
            self.reverted_ids.extend(ids.iter().cloned());
        }
        let idx = self.snapshot.entries.len();
        push_turn_range(&mut self.turn_ranges, idx, &event.entry);
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

fn push_turn_range(turns: &mut Vec<TurnRange>, idx: usize, entry: &TranscriptEntry) {
    if matches!(entry, TranscriptEntry::User { .. }) {
        turns.push(TurnRange { user: Some(idx), items: (idx + 1)..(idx + 1) });
    } else if let Some(last) = turns.last_mut() {
        last.items.end = idx + 1;
    } else {
        turns.push(TurnRange { user: None, items: idx..(idx + 1) });
    }
}

/// A turn: a user message and everything after it until the next one.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub struct Turn<'a> {
    pub user: Option<&'a TranscriptEntry>,
    pub items: Vec<&'a TranscriptEntry>,
}

/// Entries grouped into turns; entries before the first user message form a leading turn without one.
#[cfg(test)]
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
    fn a_full_window_has_older_entries() {
        let mut long = snapshot();
        long.entries = (0..INITIAL_ENTRIES).map(|index| assistant(&format!("a{index}"), "答")).collect();
        assert!(ChatView::new(long.clone(), false).truncated);
        assert!(!ChatView::new(long, true).truncated);
        assert!(!ChatView::new(snapshot(), false).truncated);
    }

    #[test]
    fn groups_turns() {
        let entries = vec![assistant("a0", "前言"), snapshot().entries[0].clone(), assistant("a1", "答")];
        let turns = group_turns(&entries);
        assert_eq!(turns.len(), 2);
        assert!(turns[0].user.is_none());
        assert_eq!(turns[1].items.len(), 1);
    }

    #[test]
    fn maintains_incremental_turn_ranges_and_reverted_ids() {
        let mut chat = ChatView::new(snapshot(), false);
        assert_eq!(chat.turn_ranges().len(), 1);
        assert_eq!(chat.turn_ranges()[0].user, Some(0));
        assert!(chat.turn_ranges()[0].items.is_empty());
        assert!(chat.apply_entry(SessionEntryEvent { session_id: "s".into(), entry: assistant("a1", "第一轮答复") }));
        assert_eq!(chat.turn_ranges().len(), 1);
        assert_eq!(chat.turn_ranges()[0].items, 1..2);
        let ctx_entry: TranscriptEntry = serde_json::from_value(json!({
            "id": "x1", "kind": "context", "ts": 3, "label": "notice", "text": "undone", "reverted": ["c0"]
        }))
        .unwrap();
        assert!(chat.apply_entry(SessionEntryEvent { session_id: "s".into(), entry: ctx_entry }));
        assert!(chat.reverted_ids().contains("c0"));
        let user2: TranscriptEntry = serde_json::from_value(json!({
            "id": "u2", "kind": "user", "ts": 4, "text": "第二问"
        }))
        .unwrap();
        assert!(chat.apply_entry(SessionEntryEvent { session_id: "s".into(), entry: user2 }));
        assert_eq!(chat.turn_ranges().len(), 2);
        assert_eq!(chat.turn_ranges()[1].user, Some(3));
        assert_eq!(chat.turn_ranges()[1].items, 4..4);
    }
}
