//! `session.stream`: the reply being streamed, as a whole message or as a patch (`src/shared/stream-patch.ts`).

use serde::Deserialize;
use serde_json::Value;

use crate::chat::{AssistantBlock, AssistantMessage, utf16_len, utf16_len_eq};

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
        return Some((session, StreamUpdate::Patch(StreamPatch::deserialize(patch).ok()?)));
    }
    let message = data.get("message")?;
    if message.is_null() {
        return Some((session, StreamUpdate::Whole(None)));
    }
    Some((session, StreamUpdate::Whole(Some(Box::new(AssistantMessage::deserialize(message).ok()?)))))
}

/// Verify that every block and append operation in `patch` fits `content` without mutating or cloning it.
fn validate_stream_patch(content: &[AssistantBlock], known_utf16_lens: Option<&[usize]>, patch: &StreamPatch) -> bool {
    let mut sim_len = content.len();
    let mut overrides: Vec<(usize, &AssistantBlock)> = Vec::new();
    for PatchBlock { index, block } in &patch.blocks {
        match (*index).cmp(&sim_len) {
            std::cmp::Ordering::Less => {
                if let Some(entry) = overrides.iter_mut().find(|(idx, _)| *idx == *index) {
                    entry.1 = block;
                } else {
                    overrides.push((*index, block));
                }
            }
            std::cmp::Ordering::Equal => {
                overrides.push((*index, block));
                sim_len += 1;
            }
            std::cmp::Ordering::Greater => return false,
        }
    }

    let mut tracked_lens: Vec<(usize, usize)> = Vec::new();
    for (pos, PatchAppend { index, at, text }) in patch.append.iter().enumerate() {
        if *index >= sim_len {
            return false;
        }
        let overridden = overrides.iter().find(|(idx, _)| *idx == *index).map(|(_, block)| *block);
        let Some(target) = overridden.or_else(|| content.get(*index)) else { return false };
        let Some(current_text) = target.streamed() else { return false };

        if let Some(entry) = tracked_lens.iter_mut().find(|(idx, _)| *idx == *index) {
            if entry.1 != *at {
                return false;
            }
            entry.1 = entry.1.saturating_add(utf16_len(text));
            continue;
        }

        let matched = if overridden.is_none() {
            match known_utf16_lens.and_then(|lens| lens.get(*index)) {
                Some(&cached) => cached == *at,
                None => utf16_len_eq(current_text, *at),
            }
        } else {
            utf16_len_eq(current_text, *at)
        };
        if !matched {
            return false;
        }

        if patch.append[pos + 1..].iter().any(|later| later.index == *index) {
            tracked_lens.push((*index, at.saturating_add(utf16_len(text))));
        }
    }
    true
}

/// Apply a patch to the message the interface holds in place, without cloning untouched blocks.
/// Returns `false`, leaving `message` as it was, when the patch does not fit this copy; the next whole message repairs it.
pub fn apply_stream_patch(message: &mut AssistantMessage, patch: &StreamPatch) -> bool {
    if patch.blocks.is_empty() && patch.append.len() == 1 {
        let first = &patch.append[0];
        let Some(block) = message.content.get_mut(first.index) else { return false };
        return block.append(first.at, &first.text);
    }
    if !validate_stream_patch(&message.content, None, patch) {
        return false;
    }
    for PatchBlock { index, block } in &patch.blocks {
        if let Some(slot) = message.content.get_mut(*index) {
            *slot = block.clone();
        } else {
            message.content.push(block.clone());
        }
    }
    for PatchAppend { index, text, .. } in &patch.append {
        if let Some(current) = message.content.get_mut(*index).and_then(AssistantBlock::streamed_mut) {
            current.push_str(text);
        }
    }
    true
}

/// Apply an owned patch in place, moving replaced or newly added blocks without cloning them.
/// Returns `false`, leaving `message` unchanged, when the patch does not fit.
pub fn apply_stream_patch_owned(message: &mut AssistantMessage, patch: StreamPatch) -> bool {
    if patch.blocks.is_empty() && patch.append.len() == 1 {
        let first = &patch.append[0];
        let Some(block) = message.content.get_mut(first.index) else { return false };
        return block.append(first.at, &first.text);
    }
    if !validate_stream_patch(&message.content, None, &patch) {
        return false;
    }
    for PatchBlock { index, block } in patch.blocks {
        if let Some(slot) = message.content.get_mut(index) {
            *slot = block;
        } else {
            message.content.push(block);
        }
    }
    for PatchAppend { index, text, .. } in patch.append {
        if let Some(current) = message.content.get_mut(index).and_then(AssistantBlock::streamed_mut) {
            current.push_str(&text);
        }
    }
    true
}

/// Stateful streaming message buffer that tracks each block's UTF-16 length incrementally.
///
/// Applying sequential patches validates `at` offsets in O(1) and updates blocks in place in O(|delta|),
/// avoiding both O(N²) block cloning and O(N²) UTF-16 rescans across long streamed replies while keeping
/// atomic rollback when a patch does not fit.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamBuffer {
    message: AssistantMessage,
    utf16_lens: Vec<usize>,
}

impl StreamBuffer {
    /// Wrap a whole streaming `AssistantMessage` and compute its initial per-block UTF-16 lengths once.
    pub fn new(message: AssistantMessage) -> Self {
        let utf16_lens = message.content.iter().map(|block| block.streamed().map_or(0, utf16_len)).collect();
        Self { message, utf16_lens }
    }

    /// Borrow the held message.
    pub fn message(&self) -> &AssistantMessage {
        &self.message
    }

    /// Consume the buffer and return the held message.
    pub fn into_message(self) -> AssistantMessage {
        self.message
    }

    /// Replace the held message with a fresh whole snapshot, recomputing per-block UTF-16 lengths.
    pub fn replace(&mut self, message: AssistantMessage) {
        self.utf16_lens.clear();
        self.utf16_lens.extend(message.content.iter().map(|block| block.streamed().map_or(0, utf16_len)));
        self.message = message;
    }

    /// Cached UTF-16 code-unit length of block `index` (`0` for non-streamed blocks).
    pub fn block_utf16_len(&self, index: usize) -> Option<usize> {
        self.utf16_lens.get(index).copied()
    }

    /// Apply a borrowed patch in place in O(|patch|); returns `false` leaving the buffer unchanged on mismatch.
    pub fn apply_patch(&mut self, patch: &StreamPatch) -> bool {
        if patch.blocks.is_empty() && patch.append.len() == 1 {
            let first = &patch.append[0];
            let Some(&cur_len) = self.utf16_lens.get(first.index) else { return false };
            if cur_len != first.at {
                return false;
            }
            let Some(current) = self.message.content.get_mut(first.index).and_then(AssistantBlock::streamed_mut) else {
                return false;
            };
            current.push_str(&first.text);
            self.utf16_lens[first.index] = cur_len.saturating_add(utf16_len(&first.text));
            return true;
        }
        if !validate_stream_patch(&self.message.content, Some(&self.utf16_lens), patch) {
            return false;
        }
        for PatchBlock { index, block } in &patch.blocks {
            let len = block.streamed().map_or(0, utf16_len);
            if let Some(slot) = self.message.content.get_mut(*index) {
                *slot = block.clone();
                if let Some(cached) = self.utf16_lens.get_mut(*index) {
                    *cached = len;
                }
            } else {
                self.message.content.push(block.clone());
                self.utf16_lens.push(len);
            }
        }
        for PatchAppend { index, text, .. } in &patch.append {
            if let Some(current) = self.message.content.get_mut(*index).and_then(AssistantBlock::streamed_mut) {
                current.push_str(text);
                if let Some(cached) = self.utf16_lens.get_mut(*index) {
                    *cached = cached.saturating_add(utf16_len(text));
                }
            }
        }
        true
    }

    /// Apply an owned patch in place in O(|patch|), moving blocks without cloning them.
    pub fn apply_patch_owned(&mut self, patch: StreamPatch) -> bool {
        if patch.blocks.is_empty() && patch.append.len() == 1 {
            let first = &patch.append[0];
            let Some(&cur_len) = self.utf16_lens.get(first.index) else { return false };
            if cur_len != first.at {
                return false;
            }
            let Some(current) = self.message.content.get_mut(first.index).and_then(AssistantBlock::streamed_mut) else {
                return false;
            };
            current.push_str(&first.text);
            self.utf16_lens[first.index] = cur_len.saturating_add(utf16_len(&first.text));
            return true;
        }
        if !validate_stream_patch(&self.message.content, Some(&self.utf16_lens), &patch) {
            return false;
        }
        for PatchBlock { index, block } in patch.blocks {
            let len = block.streamed().map_or(0, utf16_len);
            if let Some(slot) = self.message.content.get_mut(index) {
                *slot = block;
                if let Some(cached) = self.utf16_lens.get_mut(index) {
                    *cached = len;
                }
            } else {
                self.message.content.push(block);
                self.utf16_lens.push(len);
            }
        }
        for PatchAppend { index, text, .. } in patch.append {
            if let Some(current) = self.message.content.get_mut(index).and_then(AssistantBlock::streamed_mut) {
                current.push_str(&text);
                if let Some(cached) = self.utf16_lens.get_mut(index) {
                    *cached = cached.saturating_add(utf16_len(&text));
                }
            }
        }
        true
    }
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

    #[test]
    fn rolls_back_atomically_when_later_operation_fails() {
        let original = message();
        let mut held = original.clone();
        // First operation replaces block 0, second appends valid text to block 1, third has stale `at` -> whole patch fails.
        let bad_patch: StreamPatch = serde_json::from_value(json!({
            "blocks": [
                {"index": 0, "block": {"type": "thinking", "thinking": "改写了"}},
                {"index": 2, "block": {"type": "text", "text": "新块"}}
            ],
            "append": [
                {"index": 1, "at": 4, "text": "！"},
                {"index": 1, "at": 4, "text": "错位"}
            ]
        }))
        .unwrap();
        assert!(!apply_stream_patch(&mut held, &bad_patch));
        assert_eq!(held, original);

        let mut owned_held = original.clone();
        assert!(!apply_stream_patch_owned(&mut owned_held, bad_patch.clone()));
        assert_eq!(owned_held, original);

        let mut buf = StreamBuffer::new(original.clone());
        assert!(!buf.apply_patch(&bad_patch));
        assert_eq!(buf.message(), &original);
        assert_eq!(buf.block_utf16_len(0), Some(1));
        assert_eq!(buf.block_utf16_len(1), Some(4));
        assert_eq!(buf.block_utf16_len(2), None);
    }

    #[test]
    fn supports_block_replace_plus_chained_appends_to_same_block() {
        let mut held = message();
        let mut buf = StreamBuffer::new(message());
        let patch: StreamPatch = serde_json::from_value(json!({
            "blocks": [
                {"index": 0, "block": {"type": "thinking", "thinking": "思考😀"}},
                {"index": 2, "block": {"type": "text", "text": "a"}}
            ],
            "append": [
                {"index": 0, "at": 4, "text": "中"},
                {"index": 0, "at": 5, "text": "🚀"},
                {"index": 2, "at": 1, "text": "bc"}
            ]
        }))
        .unwrap();
        assert!(apply_stream_patch(&mut held, &patch));
        assert!(buf.apply_patch(&patch));
        assert_eq!(held.content[0].streamed(), Some("思考😀中🚀"));
        assert_eq!(held.content[2].streamed(), Some("abc"));
        assert_eq!(buf.message(), &held);
        assert_eq!(buf.block_utf16_len(0), Some(7));
        assert_eq!(buf.block_utf16_len(2), Some(3));
    }

    #[test]
    fn rejects_append_targeting_tool_call_block() {
        let mut held = message();
        let add_tool: StreamPatch = serde_json::from_value(json!({
            "blocks": [{"index": 2, "block": {"type": "toolCall", "id": "t1", "name": "read", "arguments": {}}}]
        }))
        .unwrap();
        assert!(apply_stream_patch(&mut held, &add_tool));
        let mut buf = StreamBuffer::new(held.clone());
        let append_tool: StreamPatch = serde_json::from_value(json!({
            "append": [{"index": 2, "at": 0, "text": "nope"}]
        }))
        .unwrap();
        assert!(!apply_stream_patch(&mut held, &append_tool));
        assert!(!buf.apply_patch(&append_tool));
    }

    #[test]
    fn streams_long_sequence_of_mixed_utf16_deltas_consistently() {
        let initial: AssistantMessage = serde_json::from_value(json!({
            "role": "assistant",
            "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": ""}],
            "api": "openai-completions", "provider": "deepseek", "model": "m", "stopReason": "pending", "timestamp": 1,
            "usage": {}
        }))
        .unwrap();
        let mut free_msg = initial.clone();
        let mut owned_msg = initial.clone();
        let mut buf = StreamBuffer::new(initial);

        // 1,500 sequential deltas alternating ASCII, CJK, 2-byte Latin and astral emoji (surrogate pairs).
        let chunks = ["hello ", "世界", "😀", "café", "01234567", "👨‍👩‍👧‍👦", "Rust🦀", "\n"];
        let mut expected_thinking = String::new();
        let mut expected_text = String::new();
        let mut thinking_u16 = 0usize;
        let mut text_u16 = 0usize;

        for i in 0..1500 {
            let delta = chunks[i % chunks.len()];
            let delta_u16: usize = delta.chars().map(char::len_utf16).sum();
            let (index, at) = if i % 4 == 0 {
                let at = thinking_u16;
                thinking_u16 += delta_u16;
                expected_thinking.push_str(delta);
                (0usize, at)
            } else {
                let at = text_u16;
                text_u16 += delta_u16;
                expected_text.push_str(delta);
                (1usize, at)
            };
            let patch = StreamPatch {
                append: vec![PatchAppend { index, at, text: delta.to_owned() }],
                blocks: Vec::new(),
            };
            assert!(apply_stream_patch(&mut free_msg, &patch), "free patch failed at step {i}");
            assert!(buf.apply_patch(&patch), "buffer patch failed at step {i}");
            assert!(apply_stream_patch_owned(&mut owned_msg, patch), "owned patch failed at step {i}");
        }

        assert_eq!(free_msg.content[0].streamed(), Some(expected_thinking.as_str()));
        assert_eq!(free_msg.content[1].streamed(), Some(expected_text.as_str()));
        assert_eq!(owned_msg, free_msg);
        assert_eq!(buf.message(), &free_msg);
        assert_eq!(buf.block_utf16_len(0), Some(thinking_u16));
        assert_eq!(buf.block_utf16_len(1), Some(text_u16));
    }
}
