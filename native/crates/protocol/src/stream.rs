//! `session.stream`: the reply being streamed, as a whole message or as a patch (`src/shared/stream-patch.ts`).

use serde::Deserialize;
use serde_json::Value;

use crate::chat::{AssistantBlock, AssistantMessage};

/// Characters appended to a text or thinking block that held `at` characters (UTF-16 code units).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PatchAppend {
    pub index: usize,
    pub at: usize,
    pub text: String,
}

/// A block that is new or changed otherwise, whole.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PatchBlock {
    pub index: usize,
    pub block: AssistantBlock,
}

/// What one update adds to the message the interface holds.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct StreamPatch {
    #[serde(default)]
    pub append: Vec<PatchAppend>,
    #[serde(default)]
    pub blocks: Vec<PatchBlock>,
}

/// One `session.stream` event.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamUpdate {
    /// The whole partial message, or `None` once the request ended.
    Whole(Option<Box<AssistantMessage>>),
    Patch(StreamPatch),
}

/// Parse the data of a `session.stream` event into its chat id and update.
pub fn parse_stream_event(data: &Value) -> Option<(String, StreamUpdate)> {
    let session = data.get("sessionId")?.as_str()?.to_owned();
    if let Some(patch) = data.get("patch") {
        return Some((session, StreamUpdate::Patch(serde_json::from_value(patch.clone()).ok()?)));
    }
    let message = data.get("message")?;
    if message.is_null() {
        return Some((session, StreamUpdate::Whole(None)));
    }
    Some((session, StreamUpdate::Whole(Some(Box::new(serde_json::from_value(message.clone()).ok()?)))))
}

/// Apply a patch to the message the interface holds.
/// Returns `false`, leaving `message` as it was, when the patch does not fit this copy; the next whole message repairs it.
pub fn apply_stream_patch(message: &mut AssistantMessage, patch: &StreamPatch) -> bool {
    let mut content = message.content.clone();
    for PatchBlock { index, block } in &patch.blocks {
        match (*index).cmp(&content.len()) {
            std::cmp::Ordering::Less => content[*index] = block.clone(),
            std::cmp::Ordering::Equal => content.push(block.clone()),
            std::cmp::Ordering::Greater => return false,
        }
    }
    for PatchAppend { index, at, text } in &patch.append {
        let Some(block) = content.get_mut(*index) else { return false };
        if !block.append(*at, text) {
            return false;
        }
    }
    message.content = content;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message() -> AssistantMessage {
        serde_json::from_value(json!({
            "role": "assistant", "content": [{"type": "thinking", "thinking": "想"}, {"type": "text", "text": "你好😀"}],
            "api": "openai-completions", "provider": "deepseek", "model": "m", "stopReason": "pending", "timestamp": 1,
            "usage": {"input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 3,
                      "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}}
        }))
        .unwrap()
    }

    #[test]
    fn appends_with_javascript_lengths() {
        let mut held = message();
        // "你好😀" is 4 UTF-16 code units: the emoji counts twice.
        let patch: StreamPatch = serde_json::from_value(json!({"append": [{"index": 1, "at": 4, "text": "！"}]})).unwrap();
        assert!(apply_stream_patch(&mut held, &patch));
        assert_eq!(held.content[1].streamed(), Some("你好😀！"));
        let stale: StreamPatch = serde_json::from_value(json!({"append": [{"index": 1, "at": 4, "text": "x"}]})).unwrap();
        assert!(!apply_stream_patch(&mut held, &stale));
        assert_eq!(held.content[1].streamed(), Some("你好😀！"));
    }

    #[test]
    fn adds_blocks_and_rejects_gaps() {
        let mut held = message();
        let patch: StreamPatch = serde_json::from_value(json!({"blocks": [{"index": 2, "block":
            {"type": "toolCall", "id": "c1", "name": "read", "arguments": {"path": "a.txt"}}}]})).unwrap();
        assert!(apply_stream_patch(&mut held, &patch));
        assert!(matches!(&held.content[2], AssistantBlock::ToolCall { name, .. } if name == "read"));
        let gap: StreamPatch = serde_json::from_value(json!({"blocks": [{"index": 9, "block": {"type": "text", "text": ""}}]})).unwrap();
        assert!(!apply_stream_patch(&mut held, &gap));
    }

    #[test]
    fn parses_events() {
        let (id, update) = parse_stream_event(&json!({"sessionId": "s", "message": null})).unwrap();
        assert_eq!((id.as_str(), update), ("s", StreamUpdate::Whole(None)));
        let (_, update) = parse_stream_event(&json!({"sessionId": "s", "patch": {"append": []}})).unwrap();
        assert_eq!(update, StreamUpdate::Patch(StreamPatch::default()));
    }

    #[test]
    fn keeps_unknown_fields_through_a_round_trip() {
        let value = serde_json::to_value(message()).unwrap();
        assert_eq!(value["api"], "openai-completions");
        assert_eq!(value["usage"]["totalTokens"], 3);
    }
}
