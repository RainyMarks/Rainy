//! The streaming reply as the Host sends it to the page (3.x `shared/stream-patch.ts`): a whole message, then only what
//! each update adds. Text and reasoning grow at their end, so an update carries the appended characters; anything else
//! that changed is sent whole. Lengths count UTF-16 units, as the page's JavaScript and `rainy-protocol` do.

use rainy_protocol::chat::{AssistantBlock, AssistantMessage};
use serde_json::{Value, json};

const TAIL: usize = 16;

/// What the page holds of one block.
#[derive(Clone)]
struct BlockBaseline {
    kind: &'static str,
    /// UTF-16 units of streamed text.
    length: usize,
    /// The last units of that text, to notice a provider that rewrote rather than appended.
    tail: Vec<u16>,
    /// The block without its streamed text (all of it for tool calls).
    other: String,
}

/// The sender's record of what the page received.
#[derive(Clone)]
pub struct Baseline {
    rest: String,
    blocks: Vec<BlockBaseline>,
}

fn kind(block: &AssistantBlock) -> &'static str {
    match block {
        AssistantBlock::Text { .. } => "text",
        AssistantBlock::Thinking { .. } => "thinking",
        AssistantBlock::ToolCall { .. } => "toolCall",
    }
}

/// Record what the page holds after receiving `message`.
pub fn baseline(message: &AssistantMessage) -> Baseline {
    let mut rest = serde_json::to_value(message).unwrap_or(Value::Null);
    if let Some(object) = rest.as_object_mut() {
        object.remove("content");
    }
    let blocks = message
        .content
        .iter()
        .map(|block| {
            let mut other = serde_json::to_value(block).unwrap_or(Value::Null);
            match block.streamed() {
                Some(text) => {
                    if let Some(object) = other.as_object_mut() {
                        object.remove(if matches!(block, AssistantBlock::Text { .. }) { "text" } else { "thinking" });
                    }
                    let units: Vec<u16> = text.encode_utf16().collect();
                    BlockBaseline { kind: kind(block), length: units.len(), tail: units[units.len().saturating_sub(TAIL)..].to_vec(), other: other.to_string() }
                }
                None => BlockBaseline { kind: kind(block), length: 0, tail: Vec::new(), other: other.to_string() },
            }
        })
        .collect();
    Baseline { rest: rest.to_string(), blocks }
}

/// What changed since `before`: the patch (empty when nothing did, `None` when only the whole message describes the
/// change), and the baseline of `message`, which the page holds once it applied either.
pub fn patch(before: &Baseline, message: &AssistantMessage) -> (Option<Value>, Baseline) {
    let next = baseline(message);
    if next.rest != before.rest || next.blocks.len() < before.blocks.len() {
        return (None, next);
    }
    let (mut append, mut blocks) = (Vec::new(), Vec::new());
    for (index, block) in message.content.iter().enumerate() {
        let now = &next.blocks[index];
        let Some(previous) = before.blocks.get(index) else {
            blocks.push(json!({ "index": index, "block": block }));
            continue;
        };
        if previous.kind != now.kind {
            return (None, next);
        }
        let Some(text) = block.streamed() else {
            if now.other != previous.other {
                blocks.push(json!({ "index": index, "block": block }));
            }
            continue;
        };
        let units: Vec<u16> = text.encode_utf16().collect();
        let kept_tail = units.len() >= previous.length && units[previous.length.saturating_sub(TAIL)..previous.length] == previous.tail[..];
        if now.other != previous.other || !kept_tail {
            blocks.push(json!({ "index": index, "block": block }));
        } else if units.len() > previous.length {
            append.push(json!({ "index": index, "at": previous.length, "text": String::from_utf16_lossy(&units[previous.length..]) }));
        }
    }
    let mut patch = serde_json::Map::new();
    if !append.is_empty() {
        patch.insert("append".into(), Value::Array(append));
    }
    if !blocks.is_empty() {
        patch.insert("blocks".into(), Value::Array(blocks));
    }
    (Some(Value::Object(patch)), next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rainy_protocol::stream::{StreamPatch, apply_stream_patch};

    fn message(text: &str) -> AssistantMessage {
        serde_json::from_value(json!({ "role": "assistant", "content": [{ "type": "text", "text": text }], "api": "a", "provider": "p", "model": "m", "stopReason": "pending", "timestamp": 1, "usage": {} })).unwrap()
    }

    #[test]
    fn patches_what_the_page_applies() {
        let held = message("你好😀");
        let base = baseline(&held);
        let grown = message("你好😀！ok");
        let (patch_value, _) = patch(&base, &grown);
        let patch_value = patch_value.unwrap();
        assert_eq!(patch_value["append"][0]["at"], 4);
        let mut page = held.clone();
        let parsed: StreamPatch = serde_json::from_value(patch_value).unwrap();
        assert!(apply_stream_patch(&mut page, &parsed));
        assert_eq!(page.content, grown.content);
        let (rewritten, _) = patch(&base, &message("Hello"));
        assert!(rewritten.unwrap()["blocks"].is_array());
    }
}
