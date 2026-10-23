//! Memory evidence from chat entries (3.x `memory/evidence.ts`): user requests, assistant replies and successful tool
//! results, each bounded and redacted.

use std::collections::HashMap;
use std::path::Path;

use rainy_protocol::chat::{AssistantBlock, TranscriptEntry};

use super::store::redact;
use crate::agent::budget::estimate_text;
use crate::agent::llm::text_parts;

/// Evidence items kept per chat.
pub const EVIDENCE_ITEMS: usize = 64;
/// Tokens of one evidence item.
pub const EVIDENCE_ITEM_TOKENS: u64 = 256;

/// One bounded fragment of a chat; `seq` is the entry's position in the chat file.
#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    pub seq: u64,
    pub kind: &'static str,
    pub text: String,
    pub file_path: Option<String>,
}

/// Redact and bound a fragment without splitting characters; empty when not even a part fits.
pub fn bound(text: &str, tokens: u64) -> String {
    let clean = redact(text);
    if estimate_text(&clean) <= tokens {
        return clean;
    }
    const MARKER: &str = "\n[Excerpt; remaining text stays in the source chat.]";
    let points: Vec<char> = clean.chars().collect();
    let mut count = points.len().min(tokens as usize * 3);
    while count > 0 {
        let value: String = points[..count].iter().collect::<String>() + MARKER;
        if estimate_text(&value) <= tokens {
            return value;
        }
        count = count * 4 / 5;
    }
    String::new()
}

/// Evidence up to the chat's last completed turn (the most recent [`EVIDENCE_ITEMS`]), and that turn's position.
pub fn collect(entries: &[TranscriptEntry], cwd: &str) -> (Vec<Fragment>, Option<u64>) {
    let Some(last) = entries.iter().rposition(|entry| matches!(entry, TranscriptEntry::Turn { .. })) else { return (Vec::new(), None) };
    let mut files: HashMap<&str, String> = HashMap::new();
    let mut evidence = Vec::new();
    for (seq, entry) in entries[..=last].iter().enumerate() {
        let (kind, text, file_path) = match entry {
            TranscriptEntry::User { text, .. } => ("user", text.clone(), None),
            TranscriptEntry::Assistant { message, .. } => {
                for block in &message.content {
                    if let AssistantBlock::ToolCall { id, name, arguments, .. } = block
                        && matches!(name.as_str(), "read" | "write" | "edit")
                        && let Some(path) = arguments.get("file_path").and_then(|value| value.as_str())
                    {
                        let path = Path::new(path);
                        let full = if path.is_absolute() { path.to_path_buf() } else { Path::new(cwd).join(path) };
                        files.insert(id, full.to_string_lossy().into_owned());
                    }
                }
                if message.stop_reason == "error" || message.stop_reason == "aborted" {
                    continue;
                }
                let text = message.content.iter().filter_map(|block| if let AssistantBlock::Text { text, .. } = block { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join("\n");
                ("assistant", text, None)
            }
            TranscriptEntry::ToolResult { tool_call_id, content, is_error: false, .. } => ("tool", text_parts(content), files.get(tool_call_id.as_str()).cloned()),
            _ => continue,
        };
        let text = bound(&text, EVIDENCE_ITEM_TOKENS);
        if !text.is_empty() {
            evidence.push(Fragment { seq: seq as u64, kind, text, file_path });
        }
    }
    let start = evidence.len().saturating_sub(EVIDENCE_ITEMS);
    (evidence.split_off(start), Some(last as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_long_text() {
        let long = "字".repeat(4000);
        let bounded = bound(&long, 64);
        assert!(bounded.ends_with("source chat.]") && estimate_text(&bounded) <= 64);
        assert_eq!(bound("short", 64), "short");
    }
}
