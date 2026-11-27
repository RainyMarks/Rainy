//! Chat list management, bounded LRU chat cache, in-place summary updates, and lossless live event buffering during load.

use gpui_kit::*;
use rainy_protocol::{
    RpcError, SessionEntryEvent, SessionSnapshot, SessionStateEvent, SessionSummary, SessionToolEvent, StreamUpdate,
};
use serde_json::json;

use crate::store::chat::{ChatView, INITIAL_ENTRIES};
use crate::store::{ChatModel, Store, StoreEvent};

/// Maximum queued real-time events held while a chat's initial `sessions.get` is in flight.
const MAX_PENDING_EVENTS: usize = 512;

/// Maximum number of inactive chats kept loaded in memory simultaneously.
const MAX_CACHED_CHATS: usize = 24;

/// A live Host event that arrived for a chat whose initial snapshot has not returned yet.
pub(super) enum PendingChatEvent {
    Entry(SessionEntryEvent),
    Stream(StreamUpdate),
    Tool(SessionToolEvent),
    State(SessionStateEvent),
    Summary(SessionSummary),
}

impl PendingChatEvent {
    pub(super) fn apply(self, view: &mut ChatView) -> bool {
        match self {
            PendingChatEvent::Entry(event) => view.apply_entry(event),
            PendingChatEvent::Stream(update) => view.apply_stream(update),
            PendingChatEvent::Tool(event) => view.apply_tool(event),
            PendingChatEvent::State(event) => view.apply_state(event),
            PendingChatEvent::Summary(summary) => {
                view.snapshot.summary = summary;
                true
            }
        }
    }
}

/// Replay real-time events buffered while `sessions.get` was in flight without reviving pre-snapshot streams/tools
/// or regressing a settled (`idle`/`error`) snapshot back to stale live state.
fn replay_pending_events(view: &mut ChatView, pending: Vec<PendingChatEvent>) {
    if pending.is_empty() {
        return;
    }
    // Any event up to and including the last Entry whose id is already in the loaded snapshot occurred before/at the
    // snapshot watermark; drop all of them so pre-snapshot Stream/Tool/State events cannot replay onto newer state.
    let start = pending
        .iter()
        .rposition(|event| matches!(event, PendingChatEvent::Entry(ev) if view.has_entry(ev.entry.id())))
        .map_or(0, |idx| idx + 1);
    let snapshot_terminal = !view.busy() && view.snapshot.streaming.is_none();
    let mut resumed = !snapshot_terminal;
    for event in pending.into_iter().skip(start) {
        match event {
            PendingChatEvent::Entry(ev) => {
                resumed = true;
                view.apply_entry(ev);
            }
            PendingChatEvent::Stream(update) => {
                if resumed {
                    view.apply_stream(update);
                }
            }
            PendingChatEvent::Tool(ev) => {
                if resumed {
                    view.apply_tool(ev);
                }
            }
            PendingChatEvent::State(ev) => {
                if resumed {
                    view.apply_state(ev);
                }
            }
            PendingChatEvent::Summary(summary) => {
                if summary.updated_at >= view.snapshot.summary.updated_at {
                    view.snapshot.summary = summary;
                }
            }
        }
    }
}

/// Insert or replace `summary` in-place while maintaining descending `updated_at` order without cloning the list.
fn upsert_summary_in_place(summaries: &mut Vec<SessionSummary>, summary: SessionSummary) {
    if let Some(pos) = summaries.iter().position(|s| s.id == summary.id) {
        let same_ts = summaries[pos].updated_at.to_bits() == summary.updated_at.to_bits();
        summaries[pos] = summary;
        if same_ts {
            return;
        }
        // Move modified entry toward its sorted position in O(N) without extra allocation.
        let mut idx = pos;
        while idx > 0 && summaries[idx].updated_at.total_cmp(&summaries[idx - 1].updated_at).is_gt() {
            summaries.swap(idx, idx - 1);
            idx -= 1;
        }
        while idx + 1 < summaries.len() && summaries[idx].updated_at.total_cmp(&summaries[idx + 1].updated_at).is_lt() {
            summaries.swap(idx, idx + 1);
            idx += 1;
        }
    } else {
        let pos = summaries
            .iter()
            .position(|existing| summary.updated_at.total_cmp(&existing.updated_at).is_gt())
            .unwrap_or(summaries.len());
        summaries.insert(pos, summary);
    }
}

impl Store {
    pub(super) fn set_summaries(&mut self, mut summaries: Vec<SessionSummary>, cx: &mut Context<Self>) {
        summaries.sort_by(|a, b| b.updated_at.total_cmp(&a.updated_at));
        self.summaries = summaries;
        cx.emit(StoreEvent::SummariesChanged);
        self.mark_read(cx);
    }

    pub(super) fn upsert_summary(&mut self, summary: SessionSummary, cx: &mut Context<Self>) {
        upsert_summary_in_place(&mut self.summaries, summary);
        cx.emit(StoreEvent::SummariesChanged);
        self.mark_read(cx);
    }

    pub(super) fn remove_session(&mut self, id: &str, cx: &mut Context<Self>) {
        self.summaries.retain(|s| s.id != id);
        self.chats.remove(id);
        self.chat_used.remove(id);
        self.reading.remove(id);
        self.clear_team(id);
        if self.selected.as_deref() == Some(id) {
            self.selected = None;
            self.selections += 1;
            cx.emit(StoreEvent::SelectionChanged);
        }
        cx.emit(StoreEvent::SummariesChanged);
        cx.notify();
    }

    /// Record that the shown chat's replies were seen once it stops, so the sidebar marks only replies that came while
    /// another chat was shown.
    fn mark_read(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else { return };
        let Some(summary) = self.summaries.iter().find(|summary| summary.id == id) else { return };
        if summary.status != "idle" || summary.read_at.is_some_and(|read| read >= summary.updated_at) || !self.reading.insert(id.clone()) {
            return;
        }
        let call = self.call("sessions.read", json!({ "sessionId": id }));
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let _ = call.await;
            let _ = this.update(cx, |store, _| store.reading.remove(&id));
        })
        .detach();
    }

    /// Chats whose messages hold every word of `query`, newest first, with a line around the match.
    pub fn search_chats(&self, query: &str, cx: &mut Context<Self>) -> Task<Vec<(String, String)>> {
        let call = self.call("sessions.search", json!({ "query": query }));
        cx.spawn(async move |_: WeakEntity<Store>, _: &mut AsyncApp| {
            let Ok(found) = call.await else { return Vec::new() };
            found
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| Some((item["sessionId"].as_str()?.to_owned(), item["snippet"].as_str().unwrap_or_default().to_owned())))
                .collect()
        })
    }

    /// Apply a live Host event to a loaded chat, or buffer it while initial `sessions.get` is still in flight.
    pub(super) fn deliver_chat_event(&mut self, id: &str, event: PendingChatEvent, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.get(id) else { return };
        chat.update(cx, |chat, cx| {
            if let Some(view) = chat.view.as_mut() {
                if event.apply(view) {
                    cx.notify();
                }
            } else {
                if chat.pending_events.len() >= MAX_PENDING_EVENTS {
                    chat.pending_events.remove(0);
                }
                chat.pending_events.push(event);
            }
        });
    }

    fn touch_chat(&mut self, id: &str) {
        self.chat_tick = self.chat_tick.wrapping_add(1);
        self.chat_used.insert(id.to_owned(), self.chat_tick);
    }

    fn evict_lru_chats(&mut self, keep_id: &str, cx: &App) {
        while self.chats.len() > MAX_CACHED_CHATS {
            let victim = self
                .chats
                .iter()
                .filter(|(id, model)| {
                    if id.as_str() == keep_id || self.selected.as_deref() == Some(id.as_str()) {
                        return false;
                    }
                    let pinned = self.summaries.iter().any(|s| s.id == **id && s.pinned);
                    if pinned {
                        return false;
                    }
                    let active = model.read(cx).view.as_ref().is_some_and(|v| v.busy() || !v.snapshot.approvals.is_empty());
                    !active
                })
                .min_by_key(|(id, _)| self.chat_used.get(id.as_str()).copied().unwrap_or(0))
                .map(|(id, _)| id.clone());
            let Some(victim) = victim else { break };
            self.chats.remove(&victim);
            self.chat_used.remove(&victim);
            self.reading.remove(&victim);
            self.clear_team(&victim);
        }
    }

    /// The chat with this id, loading it the first time.
    pub fn chat(&mut self, id: &str, cx: &mut Context<Self>) -> Entity<ChatModel> {
        self.touch_chat(id);
        if let Some(chat) = self.chats.get(id) {
            return chat.clone();
        }
        let chat = cx.new(|_| ChatModel {
            id: id.to_owned(),
            view: None,
            error: None,
            whole: false,
            loading_whole: false,
            loaded: 0,
            load: 0,
            pending_events: Vec::new(),
        });
        self.chats.insert(id.to_owned(), chat.clone());
        self.load_chat(id, false, cx);
        self.evict_lru_chats(id, cx);
        chat
    }

    /// Load (or reload) a chat, its last `INITIAL_ENTRIES` entries or, once asked for, all of them; a newer load
    /// replaces an older one that finishes later.
    pub fn load_chat(&mut self, id: &str, whole: bool, cx: &mut Context<Self>) {
        let (Some(rpc), Some(chat)) = (self.rpc.clone(), self.chats.get(id).cloned()) else { return };
        let (load, whole) = chat.update(cx, |chat, cx| {
            chat.load += 1;
            chat.whole |= whole;
            chat.loading_whole = chat.whole;
            cx.notify();
            (chat.load, chat.whole)
        });
        let mut params = json!({ "sessionId": id });
        if !whole {
            params["limit"] = json!(INITIAL_ENTRIES);
        }
        let request = rpc.request::<SessionSnapshot>("sessions.get", params);
        let locale = self.locale();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            let result = request.await;
            chat.update(cx, |chat, cx| {
                if chat.load != load {
                    return;
                }
                chat.loading_whole = false;
                match result {
                    Ok(snapshot) => {
                        let mut view = ChatView::new(snapshot, whole);
                        let pending = std::mem::take(&mut chat.pending_events);
                        replay_pending_events(&mut view, pending);
                        chat.view = Some(view);
                        chat.loaded += 1;
                        chat.error = None;
                    }
                    Err(error) => {
                        chat.pending_events.clear();
                        // A failed whole load leaves the window shown and can be asked for again.
                        let key = if whole && chat.view.is_some() { "chat.loadOlderFailed" } else { "chat.chatLoadFailed" };
                        if chat.view.is_some() {
                            chat.whole = false;
                        }
                        chat.error = Some(crate::i18n::format(locale, key, &[("message", &crate::i18n::error_text(locale, &error))]).into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if let Some(id) = &id {
            self.chat(id, cx);
            // An external agent's chat may run a team (a swarm, teamwork, sub-agents); a chat just made may not be
            // listed yet, and is asked as well.
            if !self.summaries.iter().any(|summary| summary.id == *id && summary.external_backend.is_none()) {
                self.fetch_team(id.clone(), cx);
            }
        }
        if self.selected != id {
            self.selections += 1;
        }
        self.selected = id;
        cx.emit(StoreEvent::SelectionChanged);
        self.mark_read(cx);
        cx.notify();
    }

    /// Start a chat in a project, or outside any project, in the mode picked for it (`pending_permission`, used up
    /// once the chat exists); resolves with its id. The caller selects it, unless `selections` moved on meanwhile: a
    /// user who went to another chat stays there.
    pub fn create_chat(&mut self, workspace_id: Option<String>, cx: &mut Context<Self>) -> Task<Result<String, RpcError>> {
        let mut params = json!({ "workspaceId": workspace_id });
        if let Some(mode) = &self.pending_permission {
            params["permission"] = json!(mode);
        }
        if self.pending_worktree && workspace_id.is_some() {
            params["worktree"] = json!(true);
        }
        let external = self.pending_external.clone();
        let external_model = self.pending_external_model.clone();
        if let Some(backend) = &external
            && let Ok(val) = serde_json::to_value(backend)
        {
            params["externalBackend"] = val;
        }
        if external.is_some()
            && let Some(model) = &external_model
            && let Ok(val) = serde_json::to_value(model)
        {
            params["model"] = val;
        }
        let request = self.call("sessions.create", params);
        // Orchestration applies only to built-in model chats.
        let orchestrate = if external.is_some() { None } else { self.pending_orchestrate.clone() };
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let mut summary = serde_json::from_value::<SessionSummary>(request.await?)
                .map_err(|error| RpcError { code: "invalid-response".into(), message: error.to_string(), data: None })?;
            let id = summary.id.clone();
            // Orchestration picked before the first message is set before that message goes out.
            if let Some(executor) = orchestrate
                && let Ok(call) = this.update(cx, |store, _| store.call("chat.setOrchestrate", json!({ "sessionId": id, "executor": executor })))
                && call.await.is_ok()
            {
                summary.orchestrate = Some(executor);
            }
            let _ = this.update(cx, |store, cx| {
                store.pending_permission = None;
                store.pending_worktree = false;
                store.pending_orchestrate = None;
                store.pending_external = None;
                store.pending_external_model = None;
                store.upsert_summary(summary, cx);
                cx.notify();
            });
            Ok(id)
        })
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;
    use serde_json::json;

    use super::*;

    fn summary(id: &str, updated_at: f64) -> SessionSummary {
        serde_json::from_value(json!({
            "id": id, "workspaceId": null, "cwd": "/p", "title": id, "createdAt": 1, "updatedAt": updated_at,
            "archived": false, "pinned": false, "status": "idle"
        }))
        .unwrap()
    }

    #[test]
    fn upserts_summaries_in_place_descending() {
        let mut list = vec![summary("c", 30.), summary("b", 20.), summary("a", 10.)];
        // Update middle item to newest -> bubbles to front.
        upsert_summary_in_place(&mut list, summary("b", 40.));
        assert_eq!(list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["b", "c", "a"]);
        // Update same timestamp -> stays put.
        upsert_summary_in_place(&mut list, summary("c", 30.));
        assert_eq!(list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["b", "c", "a"]);
        // Insert new item in the middle.
        upsert_summary_in_place(&mut list, summary("d", 25.));
        assert_eq!(list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["b", "c", "d", "a"]);
    }

    #[test]
    fn buffered_pending_events_replay_idempotently_onto_loaded_view() {
        let snapshot: SessionSnapshot = serde_json::from_value(json!({
            "summary": {"id": "s", "workspaceId": null, "cwd": "/p", "title": "t", "createdAt": 1, "updatedAt": 2,
                        "archived": false, "pinned": false, "status": "running"},
            "entries": [{"id": "u1", "kind": "user", "ts": 1, "text": "hi"}],
            "model": null, "queue": [], "context": null, "streaming": null,
            "runningTools": [], "permission": "ask", "approvals": [], "background": []
        }))
        .unwrap();
        let entry: SessionEntryEvent = serde_json::from_value(json!({
            "sessionId": "s",
            "entry": {"id": "a1", "kind": "assistant", "ts": 2, "message": {
                "role": "assistant", "content": [{"type": "text", "text": "hello"}], "api": "x", "provider": "p", "model": "m",
                "stopReason": "stop", "timestamp": 2,
                "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                          "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}}
            }}
        }))
        .unwrap();
        let mut view = ChatView::new(snapshot, false);
        let pending = vec![PendingChatEvent::Entry(entry.clone()), PendingChatEvent::Entry(entry)];
        replay_pending_events(&mut view, pending);
        assert_eq!(view.snapshot.entries.len(), 2);
    }

    #[test]
    fn replay_drops_pre_snapshot_stream_and_prevents_ghost_streaming_copy() {
        let assistant_entry: serde_json::Value = json!({
            "id": "a1", "kind": "assistant", "ts": 2, "message": {
                "role": "assistant", "content": [{"type": "text", "text": "done step 1"}], "api": "x", "provider": "p", "model": "m",
                "stopReason": "toolUse", "timestamp": 2,
                "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                          "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}}
            }
        });
        // Snapshot already contains `a1` with `streaming: None`, while the run stays `running` for the next tool step.
        let snapshot: SessionSnapshot = serde_json::from_value(json!({
            "summary": {"id": "s", "workspaceId": null, "cwd": "/p", "title": "t", "createdAt": 1, "updatedAt": 3,
                        "archived": false, "pinned": false, "status": "running"},
            "entries": [{"id": "u1", "kind": "user", "ts": 1, "text": "hi"}, assistant_entry.clone()],
            "model": null, "queue": [], "context": null, "streaming": null,
            "runningTools": [], "permission": "ask", "approvals": [], "background": []
        }))
        .unwrap();
        let old_stream: rainy_protocol::AssistantMessage = serde_json::from_value(json!({
            "role": "assistant", "content": [{"type": "text", "text": "done step 1"}], "api": "x", "provider": "p", "model": "m",
            "stopReason": "stop", "timestamp": 2,
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                      "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}}
        }))
        .unwrap();
        let dup_entry: SessionEntryEvent = serde_json::from_value(json!({ "sessionId": "s", "entry": assistant_entry })).unwrap();
        let mut view = ChatView::new(snapshot, false);
        replay_pending_events(&mut view, vec![
            PendingChatEvent::Stream(StreamUpdate::Whole(Some(Box::new(old_stream)))),
            PendingChatEvent::Entry(dup_entry),
        ]);
        assert_eq!(view.snapshot.entries.len(), 2);
        assert!(view.snapshot.streaming.is_none(), "pre-snapshot stream must not resurrect after duplicate entry");
    }

    #[test]
    fn replay_ignores_stale_stream_and_running_state_on_idle_snapshot_until_new_entry() {
        let snapshot: SessionSnapshot = serde_json::from_value(json!({
            "summary": {"id": "s", "workspaceId": null, "cwd": "/p", "title": "t", "createdAt": 1, "updatedAt": 5,
                        "archived": false, "pinned": false, "status": "idle"},
            "entries": [{"id": "u1", "kind": "user", "ts": 1, "text": "hi"}],
            "model": null, "queue": [], "context": null, "streaming": null,
            "runningTools": [], "permission": "ask", "approvals": [], "background": []
        }))
        .unwrap();
        let stale_stream: rainy_protocol::AssistantMessage = serde_json::from_value(json!({
            "role": "assistant", "content": [{"type": "text", "text": "stale"}], "api": "x", "provider": "p", "model": "m",
            "stopReason": "stop", "timestamp": 4,
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                      "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}}
        }))
        .unwrap();
        let stale_state: SessionStateEvent = serde_json::from_value(json!({
            "sessionId": "s", "status": "running", "permission": "ask", "queue": [], "approvals": [], "background": []
        }))
        .unwrap();
        let stale_tool: SessionToolEvent = serde_json::from_value(json!({
            "sessionId": "s", "toolCallId": "tc1", "toolName": "bash", "phase": "start"
        }))
        .unwrap();
        let mut view = ChatView::new(snapshot, false);
        replay_pending_events(&mut view, vec![
            PendingChatEvent::Stream(StreamUpdate::Whole(Some(Box::new(stale_stream)))),
            PendingChatEvent::Tool(stale_tool),
            PendingChatEvent::State(stale_state),
        ]);
        assert_eq!(view.snapshot.summary.status, "idle");
        assert!(view.snapshot.streaming.is_none());
        assert!(view.tools.is_empty());

        // Once a genuinely newer entry arrives after the idle snapshot, subsequent running state and stream apply normally.
        let next_user: SessionEntryEvent = serde_json::from_value(json!({
            "sessionId": "s", "entry": {"id": "u2", "kind": "user", "ts": 6, "text": "next"}
        }))
        .unwrap();
        let next_state: SessionStateEvent = serde_json::from_value(json!({
            "sessionId": "s", "status": "running", "permission": "ask", "queue": [], "approvals": [], "background": []
        }))
        .unwrap();
        replay_pending_events(&mut view, vec![PendingChatEvent::Entry(next_user), PendingChatEvent::State(next_state)]);
        assert_eq!(view.snapshot.entries.len(), 2);
        assert_eq!(view.snapshot.summary.status, "running");
    }
}
