//! Run event dispatch, reply recording, and incremental stream patch emission for [`Session`].

use std::sync::atomic::Ordering;

use rainy_protocol::chat::{AssistantBlock, AssistantMessage, TranscriptEntry};
use serde_json::json;

use super::super::budget::Reported;
use super::super::changes;
use super::super::compaction::{add_usage, usage_of};
use super::super::llm::{self, text_parts};
use super::super::orchestrate::FRICTION_ERRORS;
use super::super::run::Event;
use super::super::stream;
use super::{Meta, STREAM_THROTTLE, STREAM_WHOLE_MS, Session, TOOL_UPDATE_THROTTLE_MS};
use crate::locale::l;
use crate::text::now_ms;

impl Session {
    pub(super) async fn on_event(&self, event: Event<'_, Meta>) {
        match event {
            Event::Prompt(message, meta) => match meta {
                Meta::User { entry_id, text, images, queue_id } => {
                    if let Some(queue_id) = queue_id {
                        self.inner.lock().unwrap().queue.retain(|item| item.id != *queue_id);
                        self.publish_state();
                    }
                    let images = (!images.is_empty()).then(|| images.clone());
                    self.append(TranscriptEntry::User { id: entry_id.clone(), ts: message.timestamp(), text: text.clone(), images }).await;
                }
                Meta::Context { entry_id, label, text } => {
                    self.append(TranscriptEntry::Context { id: entry_id.clone(), ts: message.timestamp(), label: (*label).into(), text: text.clone(), reverted: None }).await;
                }
            },
            Event::ReplyStarted => {
                let mut inner = self.inner.lock().unwrap();
                inner.stream_sent = None;
                inner.message_started = now_ms();
                inner.thinking_started.clear();
                inner.thinking_ms.clear();
                self.services.model_activity.store(now_ms(), Ordering::SeqCst);
            }
            Event::Reply(message) => self.on_reply(message).await,
            Event::ToolStarted { id, name, args } => {
                self.inner.lock().unwrap().running_tools.insert(id.to_owned(), now_ms());
                self.services.rpc.emit("session.tool", json!({ "sessionId": self.id, "toolCallId": id, "toolName": name, "phase": "start", "args": args }));
            }
            Event::ToolProgress { id, name, output } => {
                let now = now_ms();
                {
                    let mut inner = self.inner.lock().unwrap();
                    // Output alone goes out at most every TOOL_UPDATE_THROTTLE_MS; a change of what the tool reports besides
                    // it (a sub-agent's next step) at once, or the card would miss the steps that follow a reply closely.
                    let changed = output.details.as_ref().is_some_and(|details| inner.tool_details.get(id) != Some(details));
                    if !changed && now.saturating_sub(inner.tool_updates.get(id).copied().unwrap_or(0)) < TOOL_UPDATE_THROTTLE_MS {
                        return;
                    }
                    inner.tool_updates.insert(id.to_owned(), now);
                    if let Some(details) = &output.details {
                        inner.tool_details.insert(id.to_owned(), details.clone());
                    }
                }
                let text = text_parts(&output.content);
                let partial = tail_chars(&text, 16384);
                let mut event = json!({ "sessionId": self.id, "toolCallId": id, "toolName": name, "phase": "update", "partial": partial });
                if let Some(details) = &output.details {
                    event["details"] = details.clone();
                }
                self.services.rpc.emit("session.tool", event);
            }
            Event::ToolEnded { id, name } => {
                {
                    let mut inner = self.inner.lock().unwrap();
                    inner.tool_updates.remove(id);
                    inner.tool_details.remove(id);
                }
                self.services.rpc.emit("session.tool", json!({ "sessionId": self.id, "toolCallId": id, "toolName": name, "phase": "end" }));
            }
            Event::Result(result) => {
                let (started, refused) = {
                    let mut inner = self.inner.lock().unwrap();
                    (inner.running_tools.remove(&result.tool_call_id), inner.refused.remove(&result.tool_call_id))
                };
                let details = match refused {
                    Some(by) => Some(json!({ "refused": by })),
                    None => result.details.clone(),
                };
                self.append(TranscriptEntry::ToolResult {
                    id: uuid::Uuid::new_v4().to_string(),
                    ts: result.timestamp,
                    tool_call_id: result.tool_call_id.clone(),
                    tool_name: result.tool_name.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                    details,
                    duration_ms: started.map(|started| now_ms().saturating_sub(started) as f64),
                })
                .await;
                if changes::CHANGE_TOOLS.contains(&result.tool_name.as_str()) && !result.is_error {
                    self.services.rpc.emit("session.changes", json!({ "sessionId": self.id }));
                }
                if self.executor().is_some() && self.friction(result.is_error) {
                    self.notice(
                        "orchestrate-replan",
                        l(
                            &format!("执行模型连续 {FRICTION_ERRORS} 次工具调用失败，交回规划模型。"),
                            &format!("The executor's last {FRICTION_ERRORS} tool calls failed, so the planner takes the turn back."),
                        ),
                    )
                    .await;
                }
            }
            Event::TurnEnded => self.inject_touched_instructions().await,
        }
    }

    pub(super) async fn on_reply(&self, message: &AssistantMessage) {
        let (duration, thinking_ms) = {
            let mut inner = self.inner.lock().unwrap();
            inner.streaming = None;
            inner.stream_sent = None;
            if let Some(run) = &mut inner.run {
                run.requests += 1;
                run.usage = add_usage(&run.usage, &usage_of(message));
            }
            let actual = message.usage.input + message.usage.cache_read + message.usage.cache_write;
            let estimate = inner.last_estimate;
            let model_key = inner.last_model_key.clone();
            inner.calibration.observe_for(&model_key, estimate, actual as u64);
            if actual > 0.0 && message.stop_reason != "error" && message.stop_reason != "aborted" {
                let (sent, system_len, tools) = inner.last_sent;
                // The next request repeats this one and the reply, so their counts stand in for the estimate.
                inner.reported = Some(Reported { tokens: (actual + message.usage.output) as u64, covered: sent + 1, last_timestamp: message.timestamp, system_len, tools });
            }
            if actual > 0.0
                && let Some(context) = &mut inner.context
            {
                context.tokens = actual + message.usage.output;
                context.kind = "exact".into();
            }
            // A reply that failed or was stopped while thinking never announces the end of that block.
            let now = now_ms();
            let open: Vec<(usize, u64)> = inner.thinking_started.drain().collect();
            for (index, started) in open {
                inner.thinking_ms.insert(index, now.saturating_sub(started));
            }
            (now.saturating_sub(inner.message_started), std::mem::take(&mut inner.thinking_ms))
        };
        self.services.rpc.emit("session.stream", json!({ "sessionId": self.id, "message": null }));
        self.publish_state();
        let thinking_ms = (!thinking_ms.is_empty()).then(|| thinking_ms.into_iter().map(|(index, ms)| (index.to_string(), json!(ms))).collect());
        self.append(TranscriptEntry::Assistant { id: uuid::Uuid::new_v4().to_string(), ts: message.timestamp, message: message.clone(), duration_ms: Some(duration as f64), thinking_ms })
            .await;
    }

    pub(super) fn on_stream(&self, event: llm::Event, reply: &AssistantMessage) {
        let mut inner = self.inner.lock().unwrap();
        match event {
            llm::Event::BlockStarted(index) if matches!(reply.content.get(index), Some(AssistantBlock::Thinking { .. })) => {
                inner.thinking_started.insert(index, now_ms());
            }
            llm::Event::BlockEnded(index) => {
                if let Some(started) = inner.thinking_started.remove(&index) {
                    inner.thinking_ms.insert(index, now_ms().saturating_sub(started));
                }
            }
            _ => {}
        }
        inner.streaming = Some(reply.clone());
        if !inner.stream_armed {
            inner.stream_armed = true;
            let session = self.this.clone();
            tokio::spawn(async move {
                tokio::time::sleep(STREAM_THROTTLE).await;
                if let Some(session) = session.upgrade() {
                    session.emit_stream();
                }
            });
        }
    }

    /// Send the streaming reply to the page: whole at first and every [`STREAM_WHOLE_MS`], otherwise only what was
    /// added, so a long reply is not sent again on every update (an SSH Host sends it over the tunnel).
    fn emit_stream(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.stream_armed = false;
        let Some(message) = inner.streaming.clone() else { return };
        let now = now_ms();
        if let Some((baseline, whole_at)) = &inner.stream_sent
            && now.saturating_sub(*whole_at) < STREAM_WHOLE_MS
        {
            let whole_at = *whole_at;
            if let (Some(patch), next) = stream::patch(baseline, &message) {
                inner.stream_sent = Some((next, whole_at));
                drop(inner);
                if patch.as_object().is_some_and(|patch| !patch.is_empty()) {
                    self.services.rpc.emit("session.stream", json!({ "sessionId": self.id, "patch": patch }));
                }
                return;
            }
        }
        inner.stream_sent = Some((stream::baseline(&message), now));
        drop(inner);
        self.services.rpc.emit("session.stream", json!({ "sessionId": self.id, "message": message }));
    }
}

/// Return the trailing `max_chars` Unicode scalar values of `text` in a single reverse scan.
fn tail_chars(text: &str, max_chars: usize) -> &str {
    match text.char_indices().rev().nth(max_chars) {
        Some((idx, ch)) => &text[idx + ch.len_utf8()..],
        None => text,
    }
}
