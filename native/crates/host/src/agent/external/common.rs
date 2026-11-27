//! Shared lifecycle, approval bridging, and formatting helpers for external CLI agent drivers.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rainy_protocol::chat::PendingApproval;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::jsonrpc::{JsonRpcClient, JsonRpcError};
use super::{ApprovalDecision, ExternalEvent};
use crate::error::HostError;
use crate::locale::l;

/// Tracks active turn count and idle time for a pooled external agent subprocess.
pub struct TurnTracker {
    last_used: Mutex<Instant>,
    active_turns: Mutex<usize>,
}

impl TurnTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            last_used: Mutex::new(Instant::now()),
            active_turns: Mutex::new(0),
        })
    }

    pub fn touch(&self) {
        if let Ok(mut last) = self.last_used.lock() {
            *last = Instant::now();
        }
    }

    pub fn idle_for(&self) -> Duration {
        if self.active_count() > 0 {
            return Duration::ZERO;
        }
        self.last_used.lock().map(|t| t.elapsed()).unwrap_or(Duration::ZERO)
    }

    pub fn active_count(&self) -> usize {
        self.active_turns.lock().map(|n| *n).unwrap_or(0)
    }

    pub fn begin_turn(self: &Arc<Self>) -> ActiveGuard {
        if let Ok(mut n) = self.active_turns.lock() {
            *n += 1;
        }
        self.touch();
        ActiveGuard(self.clone())
    }
}

/// Decrements active turn count and refreshes idle timestamp when a turn finishes or cancels.
pub struct ActiveGuard(Arc<TurnTracker>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if let Ok(mut n) = self.0.active_turns.lock() {
            *n = n.saturating_sub(1);
        }
        self.0.touch();
    }
}

/// Cancels a turn-scoped token automatically when the turn exits, cleaning up pending approval tasks.
pub struct CancelOnDrop(pub CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Append `chunk` to `buf` and retain at most the trailing `max_chars` Unicode scalar values in place.
pub fn push_bounded_tail(buf: &mut String, chunk: &str, max_chars: usize) {
    if chunk.is_empty() || max_chars == 0 {
        return;
    }
    buf.push_str(chunk);
    // Every char is at least 1 UTF-8 byte, so byte len <= max_chars guarantees char count <= max_chars.
    if buf.len() <= max_chars {
        return;
    }
    let excess = buf.chars().count().saturating_sub(max_chars);
    if excess > 0
        && let Some((cut, _)) = buf.char_indices().nth(excess)
    {
        buf.drain(..cut);
    }
}

/// Forward a reverse approval request to the UI and reply to the external agent once decided or cancelled.
#[allow(clippy::too_many_arguments)]
pub fn spawn_approval_bridge<F>(
    rpc: &Arc<JsonRpcClient>,
    req_id: Value,
    approval: PendingApproval,
    events: &mpsc::UnboundedSender<ExternalEvent>,
    cancel: &CancellationToken,
    turn_done: &CancellationToken,
    fallback: Value,
    build_reply: F,
) where
    F: FnOnce(Option<ApprovalDecision>) -> Value + Send + 'static,
{
    let (tx, rx) = oneshot::channel::<ApprovalDecision>();
    if events
        .send(ExternalEvent::ApprovalRequested { approval, respond: Arc::new(Mutex::new(Some(tx))) })
        .is_err()
    {
        let _ = rpc.reply_result(&req_id, fallback);
        return;
    }
    let rpc = rpc.clone();
    let cancel = cancel.clone();
    let turn_done = turn_done.clone();
    tokio::spawn(async move {
        let decision = tokio::select! {
            ans = rx => ans.ok(),
            () = cancel.cancelled() => None,
            () = turn_done.cancelled() => None,
        };
        let reply = build_reply(decision);
        let _ = rpc.reply_result(&req_id, reply);
    });
}

/// Format a JSON-RPC failure into a localized `HostError` without duplicating stderr details.
pub fn format_rpc_error(
    agent_label_zh: &str,
    agent_label_en: &str,
    method: &str,
    err: JsonRpcError,
    stderr: &str,
    timeout_zh: String,
    timeout_en: String,
) -> HostError {
    match err {
        JsonRpcError::Cancelled => HostError::new("aborted", l("已停止。", "Stopped.")),
        JsonRpcError::Timeout => HostError::other(l(&timeout_zh, &timeout_en)),
        other => {
            let already_has_stderr = matches!(&other, JsonRpcError::Closed(detail) if !detail.is_empty());
            let suffix = if stderr.is_empty() || already_has_stderr { String::new() } else { format!(" ({stderr})") };
            HostError::other(l(
                &format!("{agent_label_zh} ({method}) 失败：{other}{suffix}"),
                &format!("{agent_label_en} ({method}) failed: {other}{suffix}"),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_bounded_tail_preserves_utf8_and_caps_chars() {
        let mut s = String::new();
        push_bounded_tail(&mut s, "你好世界", 3);
        assert_eq!(s, "好世界");
        push_bounded_tail(&mut s, "ab", 3);
        assert_eq!(s, "界ab");
    }

    #[test]
    fn format_rpc_error_avoids_duplicate_stderr() {
        let err = format_rpc_error(
            "Codex",
            "Codex",
            "initialize",
            JsonRpcError::Closed("boom".into()),
            "boom",
            "timeout".into(),
            "timeout".into(),
        );
        assert_eq!(err.message.matches("boom").count(), 1, "{}", err.message);
    }
}
