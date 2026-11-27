//! The streaming reply as the Host sends it to the page (3.x `shared/stream-patch.ts`): a whole message, then only what
//! each update adds. Text and reasoning grow at their end, so an update carries the appended characters; anything else
//! that changed is sent whole. Lengths count UTF-16 units, as the page's JavaScript and `rainy-protocol` do.

use rainy_protocol::chat::{AssistantBlock, AssistantMessage, Usage, utf16_len};
use serde_json::{Map, Value, json};

/// Trailing UTF-8 byte window kept per streamed text block to detect provider rewrites in O(1) without rescanning.
const TAIL_BYTES: usize = 32;

/// Non-content fields of [`AssistantMessage`], compared structurally with zero JSON serialization.
#[derive(Clone, PartialEq)]
struct HeaderBaseline {
    api: String,
    provider: String,
    model: String,
    usage: Usage,
    stop_reason: String,
    error_message: Option<String>,
    timestamp: f64,
    rest: Map<String, Value>,
}

impl HeaderBaseline {
    fn from_message(message: &AssistantMessage) -> Self {
        Self {
            api: message.api.clone(),
            provider: message.provider.clone(),
            model: message.model.clone(),
            usage: message.usage.clone(),
            stop_reason: message.stop_reason.clone(),
            error_message: message.error_message.clone(),
            timestamp: message.timestamp,
            rest: message.rest.clone(),
        }
    }

    fn matches(&self, message: &AssistantMessage) -> bool {
        self.api == message.api
            && self.provider == message.provider
            && self.model == message.model
            && self.usage == message.usage
            && self.stop_reason == message.stop_reason
            && self.error_message == message.error_message
            && self.timestamp == message.timestamp
            && self.rest == message.rest
    }
}

/// What the page holds of one block, tracked incrementally by UTF-8 and UTF-16 offsets.
#[derive(Clone)]
enum BlockBaseline {
    Text {
        byte_len: usize,
        utf16_len: usize,
        tail: String,
        text_signature: Option<String>,
        rest: Map<String, Value>,
    },
    Thinking {
        byte_len: usize,
        utf16_len: usize,
        tail: String,
        thinking_signature: Option<String>,
        redacted: Option<bool>,
        rest: Map<String, Value>,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: Map<String, Value>,
        rest: Map<String, Value>,
    },
}

/// The sender's record of what the page received.
#[derive(Clone)]
pub struct Baseline {
    header: HeaderBaseline,
    blocks: Vec<BlockBaseline>,
}

fn tail_slice(text: &str) -> String {
    let start = super::util::ceil_char_boundary(text, text.len().saturating_sub(TAIL_BYTES));
    text[start..].to_owned()
}

fn block_baseline(block: &AssistantBlock) -> BlockBaseline {
    match block {
        AssistantBlock::Text { text, text_signature, rest } => BlockBaseline::Text {
            byte_len: text.len(),
            utf16_len: utf16_len(text),
            tail: tail_slice(text),
            text_signature: text_signature.clone(),
            rest: rest.clone(),
        },
        AssistantBlock::Thinking { thinking, thinking_signature, redacted, rest } => BlockBaseline::Thinking {
            byte_len: thinking.len(),
            utf16_len: utf16_len(thinking),
            tail: tail_slice(thinking),
            thinking_signature: thinking_signature.clone(),
            redacted: *redacted,
            rest: rest.clone(),
        },
        AssistantBlock::ToolCall { id, name, arguments, rest } => BlockBaseline::ToolCall {
            id: id.clone(),
            name: name.clone(),
            arguments: arguments.clone(),
            rest: rest.clone(),
        },
    }
}

fn is_append(text: &str, prev_byte_len: usize, prev_tail: &str) -> bool {
    text.len() >= prev_byte_len
        && text.is_char_boundary(prev_byte_len)
        && prev_byte_len >= prev_tail.len()
        && text.is_char_boundary(prev_byte_len - prev_tail.len())
        && &text[prev_byte_len - prev_tail.len()..prev_byte_len] == prev_tail
}

/// Record what the page holds after receiving `message`.
pub fn baseline(message: &AssistantMessage) -> Baseline {
    Baseline {
        header: HeaderBaseline::from_message(message),
        blocks: message.content.iter().map(block_baseline).collect(),
    }
}

/// What changed since `before`: the patch (empty when nothing did, `None` when only the whole message describes the
/// change), and the baseline of `message`, which the page holds once it applied either.
/// Pure text/thinking appends run in O(delta) with zero JSON serialization of unchanged history.
pub fn patch(before: &Baseline, message: &AssistantMessage) -> (Option<Value>, Baseline) {
    if !before.header.matches(message) || message.content.len() < before.blocks.len() {
        return (None, baseline(message));
    }
    let (mut append, mut blocks) = (Vec::new(), Vec::new());
    let mut next_blocks = Vec::with_capacity(message.content.len());

    for (index, block) in message.content.iter().enumerate() {
        let Some(previous) = before.blocks.get(index) else {
            blocks.push(json!({ "index": index, "block": block }));
            next_blocks.push(block_baseline(block));
            continue;
        };
        match (previous, block) {
            (
                BlockBaseline::Text { byte_len, utf16_len: prev_u16, tail, text_signature: prev_sig, rest: prev_rest },
                AssistantBlock::Text { text, text_signature, rest },
            ) => {
                if prev_sig == text_signature && prev_rest == rest && is_append(text, *byte_len, tail) {
                    let delta = &text[*byte_len..];
                    let new_u16 = if delta.is_empty() {
                        *prev_u16
                    } else {
                        append.push(json!({ "index": index, "at": *prev_u16, "text": delta }));
                        prev_u16 + utf16_len(delta)
                    };
                    next_blocks.push(BlockBaseline::Text {
                        byte_len: text.len(),
                        utf16_len: new_u16,
                        tail: if delta.is_empty() { tail.clone() } else { tail_slice(text) },
                        text_signature: text_signature.clone(),
                        rest: rest.clone(),
                    });
                } else {
                    blocks.push(json!({ "index": index, "block": block }));
                    next_blocks.push(block_baseline(block));
                }
            }
            (
                BlockBaseline::Thinking { byte_len, utf16_len: prev_u16, tail, thinking_signature: prev_sig, redacted: prev_redacted, rest: prev_rest },
                AssistantBlock::Thinking { thinking, thinking_signature, redacted, rest },
            ) => {
                if prev_sig == thinking_signature && prev_redacted == redacted && prev_rest == rest && is_append(thinking, *byte_len, tail) {
                    let delta = &thinking[*byte_len..];
                    let new_u16 = if delta.is_empty() {
                        *prev_u16
                    } else {
                        append.push(json!({ "index": index, "at": *prev_u16, "text": delta }));
                        prev_u16 + utf16_len(delta)
                    };
                    next_blocks.push(BlockBaseline::Thinking {
                        byte_len: thinking.len(),
                        utf16_len: new_u16,
                        tail: if delta.is_empty() { tail.clone() } else { tail_slice(thinking) },
                        thinking_signature: thinking_signature.clone(),
                        redacted: *redacted,
                        rest: rest.clone(),
                    });
                } else {
                    blocks.push(json!({ "index": index, "block": block }));
                    next_blocks.push(block_baseline(block));
                }
            }
            (
                BlockBaseline::ToolCall { id: prev_id, name: prev_name, arguments: prev_args, rest: prev_rest },
                AssistantBlock::ToolCall { id, name, arguments, rest },
            ) => {
                if prev_id != id || prev_name != name || prev_args != arguments || prev_rest != rest {
                    blocks.push(json!({ "index": index, "block": block }));
                }
                next_blocks.push(block_baseline(block));
            }
            // Block kind changed at this index: resend the whole message.
            _ => return (None, baseline(message)),
        }
    }

    let mut patch = serde_json::Map::new();
    if !append.is_empty() {
        patch.insert("append".into(), Value::Array(append));
    }
    if !blocks.is_empty() {
        patch.insert("blocks".into(), Value::Array(blocks));
    }
    (Some(Value::Object(patch)), Baseline { header: before.header.clone(), blocks: next_blocks })
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
        let (patch_value, next_base) = patch(&base, &grown);
        let patch_value = patch_value.unwrap();
        assert_eq!(patch_value["append"][0]["at"], 4);
        let mut page = held.clone();
        let parsed: StreamPatch = serde_json::from_value(patch_value).unwrap();
        assert!(apply_stream_patch(&mut page, &parsed));
        assert_eq!(page.content, grown.content);
        // Second incremental append advances UTF-16 offset without rescanning from zero.
        let grown2 = message("你好😀！ok🚀");
        let (patch2, _) = patch(&next_base, &grown2);
        let parsed2: StreamPatch = serde_json::from_value(patch2.unwrap()).unwrap();
        assert!(apply_stream_patch(&mut page, &parsed2));
        assert_eq!(page.content, grown2.content);
        let (rewritten, _) = patch(&base, &message("Hello"));
        assert!(rewritten.unwrap()["blocks"].is_array());
    }
}
