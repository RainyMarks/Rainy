//! Request budgets and token estimates (3.x `shared/budget.ts`): how a model's window is split between input, output
//! and a margin, and a conservative count of what a request costs.

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
    let input_limit = context_window as i64 - output_tokens as i64 - margin_tokens as i64;
    if input_limit < 1024 {
        return Err(HostError::invalid(l(
            &format!("输出预留 {output_tokens} 过大，上下文 {context_window} 没有足够的输入空间。请调小“最大输出 tokens”，或留空自动分配。"),
            &format!("The {output_tokens}-token output reservation is too large for a {context_window}-token context. Lower the maximum output tokens, or leave it empty to allocate it automatically."),
        )));
    }
    let input_limit = input_limit as u64;
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

/// ASCII characters, and UTF-8 bytes of all other characters.
#[derive(Clone, Copy, Default)]
struct TextCost {
    ascii: u64,
    other: u64,
}

impl TextCost {
    fn of(text: &str) -> Self {
        let mut cost = TextCost::default();
        for char in text.chars() {
            if char.is_ascii() {
                cost.ascii += 1;
            } else {
                cost.other += char.len_utf8() as u64;
            }
        }
        cost
    }

    fn add(&mut self, other: TextCost) {
        self.ascii += other.ascii;
        self.other += other.other;
    }

    fn tokens(self) -> u64 {
        self.ascii.div_ceil(3) + self.other
    }
}

/// A conservative multilingual count: an ASCII token per three characters, other text up to a token a byte.
pub fn estimate_text(text: &str) -> u64 {
    TextCost::of(text).tokens()
}

/// A message's role and content as the estimate prices them.
fn priced(message: &Message) -> (&'static str, Value) {
    match message {
        Message::User { content: UserContent::Text(text), .. } => ("user", json!([{ "type": "text", "text": text }])),
        Message::User { content: UserContent::Parts(parts), .. } => (
            "user",
            Value::Array(
                parts
                    .iter()
                    .map(|part| match part {
                        Part::Text(text) => json!({ "type": "text", "text": text }),
                        Part::Image { data, mime_type } => json!({ "type": "image", "data": data, "mimeType": mime_type }),
                    })
                    .collect(),
            ),
        ),
        Message::Assistant(reply) => ("assistant", serde_json::to_value(&reply.content).unwrap_or(Value::Null)),
        Message::ToolResult(result) => ("toolResult", serde_json::to_value(&result.content).unwrap_or(Value::Null)),
    }
}

/// Estimated input tokens of a request: its serialized system prompt, tools and messages, plus chat framing.
pub fn estimate_messages(system: &str, tools: &[ToolSpec], messages: &[Message]) -> u64 {
    let mut total = TextCost::of(&json!({ "system": system, "messages": [] }).to_string());
    let tools: Vec<Value> = tools.iter().map(|tool| json!({ "name": tool.name, "description": tool.description, "parameters": tool.parameters })).collect();
    total.add(TextCost::of(",\"tools\":"));
    total.add(TextCost::of(&Value::Array(tools).to_string()));
    for message in messages {
        let (role, content) = priced(message);
        total.add(TextCost::of(&format!("{{\"role\":\"{role}\",\"content\":}}")));
        total.add(TextCost::of(&content.to_string()));
    }
    total.ascii += messages.len().saturating_sub(1) as u64;
    total.tokens() + 16 * (messages.len() as u64 + 1)
}

/// Requests whose reported input tokens calibrate the local estimate.
const CALIBRATION_WINDOW: usize = 5;

/// Scales local estimates by what the provider reported for the latest requests. An underestimate raises the factor
/// with a 10 % margin and ages out after a few requests.
#[derive(Default)]
pub struct Calibration {
    ratios: Vec<f64>,
}

impl Calibration {
    pub fn observe(&mut self, estimated: u64, reported: u64) {
        if estimated > 0 && reported > 0 {
            self.ratios.push(reported as f64 / estimated as f64);
            if self.ratios.len() > CALIBRATION_WINDOW {
                self.ratios.remove(0);
            }
        }
    }

    /// The factor applied to local estimates; never below 1.
    pub fn factor(&self) -> f64 {
        self.ratios.iter().filter(|ratio| **ratio > 1.0).map(|ratio| ratio * 1.1).fold(1.0, f64::max)
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
    }

    #[test]
    fn estimates_text() {
        assert_eq!(estimate_text("abcdef"), 2);
        assert_eq!(estimate_text("雨天"), 6);
        assert_eq!(estimate_messages("", &[], &[]), estimate_messages("", &[], &[]));
        assert!(estimate_messages("system", &[], &[Message::user("hello")]) > estimate_messages("system", &[], &[]));
    }
}
