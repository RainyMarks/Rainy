//! Request budgets and token estimates (3.x `shared/budget.ts`): how a model's window is split between input, output
//! and a margin, and a conservative count of what a request costs.

use std::collections::HashMap;

use rainy_protocol::chat::{AssistantBlock, ResultContent};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::llm::{Message, Part, ToolSpec, UserContent};
use crate::error::{HostError, HostResult};
use crate::locale::l;

/// How a model's context window is spent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub context_window: u64,
    pub output_tokens: u64,
    pub margin_tokens: u64,
    pub input_limit: u64,
    pub compact_at: u64,
    pub keep_recent_tokens: u64,
    pub summary_tokens: u64,
    pub tool_tokens: u64,
}

/// Split a window; an explicit output limit is never lowered silently.
pub fn resolve_budget(context_window: u64, max_output: Option<u64>) -> HostResult<Budget> {
    if context_window < 4096 || context_window > (1 << 53) {
        return Err(HostError::invalid(l("上下文长度必须是至少 4096 的整数。", "The context length must be a whole number of at least 4096.")));
    }
    let output_tokens = max_output.unwrap_or_else(|| 16_000.min(context_window * 16 / 100));
    if output_tokens < 1 {
        return Err(HostError::invalid(l("输出上限必须是正整数。", "The output limit must be a positive whole number.")));
    }
    let margin_tokens = 512.max(context_window * 8 / 100);
    // Use checked unsigned arithmetic so `max_output > i64::MAX` can never wrap negative and bypass the check.
    let input_limit = context_window.checked_sub(output_tokens).and_then(|rem| rem.checked_sub(margin_tokens));
    let Some(input_limit) = input_limit.filter(|&limit| limit >= 1024) else {
        return Err(HostError::invalid(l(
            &format!("输出预留 {output_tokens} 过大，上下文 {context_window} 没有足够的输入空间。请调小“最大输出 tokens”，或留空自动分配。"),
            &format!("The {output_tokens}-token output reservation is too large for a {context_window}-token context. Lower the maximum output tokens, or leave it empty to allocate it automatically."),
        )));
    };
    Ok(Budget {
        context_window,
        output_tokens,
        margin_tokens,
        input_limit,
        compact_at: (context_window * 7 / 10).min(input_limit),
        keep_recent_tokens: (context_window / 5).min(input_limit * 2 / 5),
        summary_tokens: 4000.min(context_window * 4 / 100),
        tool_tokens: 4000.min(context_window * 4 / 100),
    })
}

/// ASCII characters, and half-tokens of all other characters.
#[derive(Clone, Copy, Default)]
struct TextCost {
    ascii: u64,
    halves: u64,
}

impl TextCost {
    fn of(text: &str) -> Self {
        let mut cost = TextCost::default();
        for char in text.chars() {
            if char.is_ascii() {
                cost.ascii += 1;
            } else {
                // A letter with an accent is a token, a CJK character one and a half, an emoji or rarer ideograph two.
                cost.halves += char.len_utf8() as u64;
            }
        }
        cost
    }

    fn add(&mut self, other: TextCost) {
        self.ascii += other.ascii;
        self.halves += other.halves;
    }

    fn tokens(self) -> u64 {
        self.ascii.div_ceil(3) + self.halves.div_ceil(2)
    }
}

/// A conservative multilingual count: an ASCII token per three characters, a token and a half per CJK character
/// (current tokenizers of Claude, GPT, DeepSeek and Qwen take about one, or less).
pub fn estimate_text(text: &str) -> u64 {
    TextCost::of(text).tokens()
}

/// What an image of unknown size costs: about what Anthropic charges for the largest image it keeps.
pub const UNKNOWN_IMAGE_TOKENS: u64 = 1600;

/// Width and height from the header of a PNG, JPEG, GIF, WebP or BMP file.
fn image_size(bytes: &[u8]) -> Option<(u64, u64)> {
    let be16 = |at: usize| Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as u64);
    let le16 = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as u64);
    let le24 = |at: usize| Some(bytes.get(at..at + 3)?.iter().rev().fold(0u64, |value, byte| value << 8 | *byte as u64));
    let byte = |at: usize| bytes.get(at).map(|byte| *byte as u64);
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
        let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
        return Some((width as u64, height as u64));
    }
    if bytes.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if bytes.starts_with(b"BM") {
        let width = i32::from_le_bytes(bytes.get(18..22)?.try_into().ok()?);
        let height = i32::from_le_bytes(bytes.get(22..26)?.try_into().ok()?);
        return Some((width.unsigned_abs() as u64, height.unsigned_abs() as u64));
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return match bytes.get(12..16)? {
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let (b1, b2, b3, b4) = (byte(21)?, byte(22)?, byte(23)?, byte(24)?);
                Some((1 + (((b2 & 0x3f) << 8) | b1), 1 + (((b4 & 0x0f) << 10) | (b3 << 2) | ((b2 & 0xc0) >> 6))))
            }
            b"VP8X" => Some((1 + le24(24)?, 1 + le24(27)?)),
            _ => None,
        };
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        let mut at = 2;
        while at + 9 < bytes.len() {
            if bytes[at] != 0xff {
                return None;
            }
            let marker = bytes[at + 1];
            if marker == 0xff {
                at += 1;
                continue;
            }
            // Start-of-frame markers carry the size; C4 (Huffman tables), C8 and CC (arithmetic coding) do not.
            if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                return Some((be16(at + 7)?, be16(at + 5)?));
            }
            at += 2 + be16(at + 2)? as usize;
        }
    }
    None
}

/// What an image costs as input: about one token per 750 pixels once its long edge fits 2048, the most a supported
/// vendor keeps (Anthropic scales to about 1.15 megapixels, OpenAI's high detail to 2048 × 768 tiles). Counting its
/// base64 text instead would price a screenshot at a hundred times its cost.
pub fn image_tokens(data: &str) -> u64 {
    use base64::Engine as _;
    let data = data.split_once(',').filter(|(head, _)| head.starts_with("data:")).map_or(data, |(_, body)| body);
    // The header is near the start; a JPEG's may follow tens of kilobytes of metadata.
    let mut prefix: String = data.chars().filter(|c| !c.is_ascii_whitespace()).take(96 * 1024).collect();
    prefix.truncate(prefix.len() / 4 * 4);
    let size = base64::engine::general_purpose::STANDARD.decode(&prefix).ok().and_then(|bytes| image_size(&bytes));
    let Some((width, height)) = size.filter(|(width, height)| *width > 0 && *height > 0) else {
        return UNKNOWN_IMAGE_TOKENS;
    };
    let scale = (2048.0 / width.max(height) as f64).min(1.0);
    let pixels = (width as f64 * scale) * (height as f64 * scale);
    ((pixels / 750.0).ceil() as u64).clamp(85, 6000)
}

/// A message's role and content as the estimate prices them, and the tokens of its images. Signatures and replay
/// data are opaque to tokenizers and left out; what they stand for shows in the provider's reported usage.
fn priced(message: &Message) -> (&'static str, Value, u64) {
    let mut images = 0;
    let (role, content) = match message {
        Message::User { content: UserContent::Text(text), .. } => ("user", json!([{ "type": "text", "text": text }])),
        Message::User { content: UserContent::Parts(parts), .. } => (
            "user",
            Value::Array(
                parts
                    .iter()
                    .map(|part| match part {
                        Part::Text(text) => json!({ "type": "text", "text": text }),
                        Part::Image { data, mime_type } => {
                            images += image_tokens(data);
                            json!({ "type": "image", "mimeType": mime_type })
                        }
                    })
                    .collect(),
            ),
        ),
        Message::Assistant(reply) => (
            "assistant",
            Value::Array(
                reply
                    .content
                    .iter()
                    .map(|block| match block {
                        AssistantBlock::Text { text, .. } => json!({ "type": "text", "text": text }),
                        AssistantBlock::Thinking { thinking, .. } => json!({ "type": "thinking", "thinking": thinking }),
                        AssistantBlock::ToolCall { id, name, arguments, .. } => json!({ "type": "toolCall", "id": id, "name": name, "arguments": arguments }),
                    })
                    .collect(),
            ),
        ),
        Message::ToolResult(result) => (
            "toolResult",
            Value::Array(
                result
                    .content
                    .iter()
                    .map(|part| match part {
                        ResultContent::Text { text } => json!({ "type": "text", "text": text }),
                        ResultContent::Image { data, mime_type } => {
                            images += image_tokens(data);
                            json!({ "type": "image", "mimeType": mime_type })
                        }
                    })
                    .collect(),
            ),
        ),
    };
    (role, content, images)
}

/// The estimated tokens of messages, without the request around them.
fn messages_cost(messages: &[Message]) -> (TextCost, u64) {
    let mut total = TextCost::default();
    let mut images = 0;
    for message in messages {
        let (role, content, image) = priced(message);
        total.add(TextCost::of(&format!("{{\"role\":\"{role}\",\"content\":}}")));
        total.add(TextCost::of(&content.to_string()));
        images += image;
    }
    total.ascii += messages.len() as u64;
    (total, images + 16 * messages.len() as u64)
}

/// Estimated input tokens of a request: its serialized system prompt, tools and messages, plus chat framing.
pub fn estimate_messages(system: &str, tools: &[ToolSpec], messages: &[Message]) -> u64 {
    let mut total = TextCost::of(&json!({ "system": system, "messages": [] }).to_string());
    let tools: Vec<Value> = tools.iter().map(|tool| json!({ "name": tool.name, "description": tool.description, "parameters": tool.parameters })).collect();
    total.add(TextCost::of(",\"tools\":"));
    total.add(TextCost::of(&Value::Array(tools).to_string()));
    let (cost, fixed) = messages_cost(messages);
    total.add(cost);
    total.tokens() + fixed + 16
}

/// Estimated tokens that messages add to a request.
pub fn estimate_added(messages: &[Message]) -> u64 {
    let (cost, fixed) = messages_cost(messages);
    cost.tokens() + fixed
}

/// What the provider reported for a run's latest request: its input and output tokens, and the messages they cover
/// (those the request sent and its reply), told apart from a rebuilt conversation by the last one's timestamp and
/// the request's system prompt and tools.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reported {
    pub tokens: u64,
    pub covered: usize,
    pub last_timestamp: f64,
    pub system_len: usize,
    pub tools: usize,
}

impl Reported {
    /// Whether these counts still describe the start of `messages` sent with this system prompt and tools.
    pub fn covers(&self, system: &str, tools: &[ToolSpec], messages: &[Message]) -> bool {
        self.covered > 0
            && self.covered <= messages.len()
            && messages[self.covered - 1].timestamp() == self.last_timestamp
            && self.system_len == system.len()
            && self.tools == tools.len()
    }
}

/// The input tokens of the next request: the provider's count for what it already saw, when that still holds, plus
/// the estimate of what came after it; otherwise the whole estimate. `ratio` scales local estimates.
pub fn expected_tokens(system: &str, tools: &[ToolSpec], messages: &[Message], reported: Option<&Reported>, ratio: f64) -> u64 {
    match reported.filter(|reported| reported.covers(system, tools, messages)) {
        Some(reported) => reported.tokens + (estimate_added(&messages[reported.covered..]) as f64 * ratio).ceil() as u64,
        None => (estimate_messages(system, tools, messages) as f64 * ratio).ceil() as u64,
    }
}

/// Estimated token share of each major prompt segment (for `session.contextBreakdown` / `usage_ring`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSegments {
    pub system: u64,
    pub instructions: u64,
    pub mcp_tools: u64,
    pub core_tools: u64,
    pub history: u64,
    pub recall: u64,
}

impl ContextSegments {
    pub fn total(&self) -> u64 {
        self.system + self.instructions + self.mcp_tools + self.core_tools + self.history + self.recall
    }
}

/// Estimate tokens per segment from the assembled system prompt, tool specs, rebuilt context items and transcript entries.
pub fn estimate_breakdown(system: &str, tools: &[ToolSpec], items: &[super::context::Item], entries: &[rainy_protocol::chat::TranscriptEntry]) -> ContextSegments {
    use rainy_protocol::chat::TranscriptEntry;
    let system_tokens = TextCost::of(&json!({ "system": system, "messages": [] }).to_string()).tokens() + 16;
    let tool_json = |tool: &ToolSpec| json!({ "name": tool.name, "description": tool.description, "parameters": tool.parameters }).to_string();
    let mut mcp_cost = TextCost::default();
    let mut core_cost = TextCost::default();
    for tool in tools {
        let cost = TextCost::of(&tool_json(tool));
        if tool.name.starts_with("mcp__") {
            mcp_cost.add(cost);
        } else {
            core_cost.add(cost);
        }
    }
    let mcp_tools = mcp_cost.tokens();
    let core_tools = core_cost.tokens();

    let mut labels: HashMap<&str, &str> = HashMap::new();
    for entry in entries {
        match entry {
            TranscriptEntry::Context { id, label, .. } => {
                labels.insert(id.as_str(), label.as_str());
            }
            TranscriptEntry::Compaction { id, .. } => {
                labels.insert(id.as_str(), "compaction");
            }
            _ => {}
        }
    }

    let (mut instr_msgs, mut recall_msgs, mut history_msgs) = (Vec::new(), Vec::new(), Vec::new());
    for item in items {
        let label = labels.get(item.entry_id.as_str()).copied();
        let is_recall_tool = matches!(&item.message, Message::ToolResult(result) if result.tool_name == "recall");
        match label {
            Some("instructions") => instr_msgs.push(item.message.clone()),
            Some("compaction" | "memory" | "reference") => recall_msgs.push(item.message.clone()),
            _ if is_recall_tool => recall_msgs.push(item.message.clone()),
            _ => history_msgs.push(item.message.clone()),
        }
    }

    ContextSegments {
        system: system_tokens,
        instructions: estimate_added(&instr_msgs),
        mcp_tools,
        core_tools,
        history: estimate_added(&history_msgs),
        recall: estimate_added(&recall_msgs),
    }
}

/// Requests whose reported input tokens calibrate the local estimate.
const CALIBRATION_WINDOW: usize = 5;
/// Maximum distinct model keys tracked per chat calibration map.
const MAX_CALIBRATION_MODELS: usize = 16;
/// Clamp single-request ratio outliers (e.g. opaque reasoning signatures) so one spike does not poison subsequent turns.
const MAX_CALIBRATION_RATIO: f64 = 3.0;

/// Scales local estimates by what the provider reported for the latest requests, isolated per `(provider/model)` key
/// so switching models or running orchestrated pairs never cross-contaminates calibration ratios.
#[derive(Default)]
pub struct Calibration {
    by_model: HashMap<String, Vec<f64>>,
}

impl Calibration {
    #[cfg(test)]
    pub fn observe(&mut self, estimated: u64, reported: u64) {
        self.observe_for("", estimated, reported);
    }

    pub fn observe_for(&mut self, model_key: &str, estimated: u64, reported: u64) {
        if estimated > 0 && reported > 0 {
            let ratio = (reported as f64 / estimated as f64).min(MAX_CALIBRATION_RATIO);
            if !self.by_model.contains_key(model_key) && self.by_model.len() >= MAX_CALIBRATION_MODELS {
                if let Some(evict) = self.by_model.keys().next().cloned() {
                    self.by_model.remove(&evict);
                }
            }
            let ratios = self.by_model.entry(model_key.to_owned()).or_default();
            ratios.push(ratio);
            if ratios.len() > CALIBRATION_WINDOW {
                ratios.remove(0);
            }
        }
    }

    /// The factor applied to local estimates for the default bucket.
    #[cfg(test)]
    pub fn factor(&self) -> f64 {
        self.factor_for("")
    }

    /// The factor applied to local estimates for one `provider/model`: 1 before any report, and never below one half.
    pub fn factor_for(&self, model_key: &str) -> f64 {
        let Some(ratios) = self.by_model.get(model_key).filter(|r| !r.is_empty()) else {
            return 1.0;
        };
        (ratios.iter().copied().fold(0.0, f64::max) * 1.1).max(0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_windows_like_3x() {
        let budget = resolve_budget(200_000, Some(64_000)).unwrap();
        assert_eq!((budget.margin_tokens, budget.input_limit, budget.compact_at, budget.summary_tokens), (16_000, 120_000, 120_000, 4000));
        let small = resolve_budget(32_768, None).unwrap();
        assert_eq!(small.output_tokens, 5242);
        assert!(resolve_budget(8192, Some(8000)).is_err());
        // Huge max_tokens (> i64::MAX) must never wrap negative and bypass budget checks.
        assert!(resolve_budget(200_000, Some(u64::MAX)).is_err());
        assert!(resolve_budget(200_000, Some((i64::MAX as u64) + 1)).is_err());
    }

    #[test]
    fn isolates_calibration_per_model_and_breaks_down_segments() {
        let mut cal = Calibration::default();
        cal.observe_for("p1/claude", 1000, 1500);
        cal.observe_for("p2/gpt", 1000, 600);
        assert!((cal.factor_for("p1/claude") - 1.65).abs() < 1e-9);
        assert!((cal.factor_for("p2/gpt") - 0.66).abs() < 1e-9);
        assert_eq!(cal.factor_for("p3/unseen"), 1.0);

        use rainy_protocol::chat::TranscriptEntry;
        let entries: Vec<TranscriptEntry> = vec![
            serde_json::from_value(json!({ "kind": "context", "id": "c1", "ts": 1, "label": "instructions", "text": "follow rules" })).unwrap(),
            serde_json::from_value(json!({ "kind": "context", "id": "m1", "ts": 2, "label": "memory", "text": "remembered facts" })).unwrap(),
            serde_json::from_value(json!({ "kind": "user", "id": "u1", "ts": 3, "text": "hello world" })).unwrap(),
        ];
        let items = super::super::context::build_context(&entries);
        let tools = vec![
            ToolSpec { name: "read".into(), description: "read file".into(), parameters: json!({}), freeform: None },
            ToolSpec { name: "mcp__db__query".into(), description: "run sql".into(), parameters: json!({}), freeform: None },
        ];
        let seg = estimate_breakdown("You are Rainy.", &tools, &items, &entries);
        assert!(seg.system > 0);
        assert!(seg.instructions > 0);
        assert!(seg.mcp_tools > 0);
        assert!(seg.core_tools > 0);
        assert!(seg.history > 0);
        assert!(seg.recall > 0);
    }

    #[test]
    fn estimates_text() {
        assert_eq!(estimate_text("abcdef"), 2);
        assert_eq!(estimate_text("雨天"), 3);
        assert_eq!(estimate_text("café"), 2);
        assert_eq!(estimate_messages("", &[], &[]), estimate_messages("", &[], &[]));
        assert!(estimate_messages("system", &[], &[Message::user("hello")]) > estimate_messages("system", &[], &[]));
    }

    fn png(width: u32, height: u32, padding: usize) -> String {
        use base64::Engine as _;
        let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend(std::iter::repeat_n(7u8, padding));
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn prices_images_by_their_pixels() {
        use base64::Engine as _;
        assert_eq!(image_tokens(&png(1000, 750, 0)), 1000);
        // A 4K screenshot fits 2048 on its long edge first.
        assert_eq!(image_tokens(&png(3840, 2160, 0)), 3146);
        assert_eq!(image_tokens(&png(10, 10, 0)), 85);
        assert_eq!(image_tokens("not base64"), UNKNOWN_IMAGE_TOKENS);
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x02, 0xee, 0x03, 0xe8, 0x03];
        assert_eq!(image_tokens(&base64::engine::general_purpose::STANDARD.encode(jpeg)), 1000);
        let gif = b"GIF89a\xe8\x03\xee\x02";
        assert_eq!(image_tokens(&format!("data:image/gif;base64,{}", base64::engine::general_purpose::STANDARD.encode(gif))), 1000);
        // Half a megabyte of screenshot is about a thousand tokens, not the 170 000 its base64 text would count.
        let shot = Message::ToolResult(super::super::llm::ToolResult {
            tool_call_id: "c".into(),
            tool_name: "browser_screenshot".into(),
            content: vec![ResultContent::Image { data: png(1000, 750, 400_000), mime_type: "image/png".into() }],
            is_error: false,
            details: None,
            timestamp: 1.0,
        });
        assert!(estimate_added(&[shot]) < 1100);
    }

    #[test]
    fn counts_from_the_reported_usage() {
        let messages = vec![Message::user("a".repeat(3000)), Message::user("b".repeat(300))];
        let reported = Reported { tokens: 5000, covered: 1, last_timestamp: messages[0].timestamp(), system_len: 6, tools: 0 };
        let added = estimate_added(&messages[1..]);
        assert_eq!(expected_tokens("system", &[], &messages, Some(&reported), 1.0), 5000 + added);
        // Another system prompt, or a conversation rebuilt shorter, falls back to the whole estimate.
        assert_eq!(expected_tokens("other prompt", &[], &messages, Some(&reported), 1.0), estimate_messages("other prompt", &[], &messages));
        assert_eq!(expected_tokens("system", &[], &messages[..0], Some(&reported), 1.0), estimate_messages("system", &[], &[]));
        let mut calibration = Calibration::default();
        assert_eq!(calibration.factor(), 1.0);
        calibration.observe(1000, 600);
        assert!((calibration.factor() - 0.66).abs() < 1e-9);
        calibration.observe(1000, 100);
        assert!((calibration.factor() - 0.66).abs() < 1e-9);
        calibration.observe(1000, 1500);
        assert!((calibration.factor() - 1.65).abs() < 1e-9);
    }
}
