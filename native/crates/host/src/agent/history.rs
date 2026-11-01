//! The chat's own record as the model reads it back: entries by number (from 1, as the chat file holds them), the
//! search and the entry text of the `recall` tool, and the index a `recall` checkpoint holds in place of a summary.

use std::collections::BTreeMap;

use rainy_protocol::chat::{AssistantBlock, ResultContent, TranscriptEntry};
use regex::Regex;
use serde_json::Value;

use super::budget::estimate_text;
use super::changes;
use crate::text::truncate_chars;

/// Which entries a search looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    User,
    Assistant,
    Tool,
    All,
}

impl Kind {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "tool" => Self::Tool,
            "all" => Self::All,
            _ => return None,
        })
    }

    fn takes(self, entry: &TranscriptEntry) -> bool {
        match self {
            Self::All => true,
            Self::User => matches!(entry, TranscriptEntry::User { .. } | TranscriptEntry::Context { .. }),
            Self::Assistant => matches!(entry, TranscriptEntry::Assistant { .. }),
            Self::Tool => matches!(entry, TranscriptEntry::ToolResult { .. }),
        }
    }
}

/// The line that names an entry: its number, who wrote it and, for a tool output, the tool.
pub fn heading(number: usize, entry: &TranscriptEntry) -> Option<String> {
    Some(match entry {
        TranscriptEntry::User { .. } => format!("#{number} user"),
        TranscriptEntry::Context { label, .. } => format!("#{number} context ({label})"),
        TranscriptEntry::Assistant { .. } => format!("#{number} assistant"),
        TranscriptEntry::ToolResult { tool_name, is_error, .. } => format!("#{number} tool {tool_name}{}", if *is_error { " (error)" } else { "" }),
        _ => return None,
    })
}

/// An entry's text as the model saw it when it was made: messages whole, a reply's text and tool calls, a tool's
/// output; thinking and images are named, not shown. `None` for entries the model never saw.
pub fn entry_text(entry: &TranscriptEntry) -> Option<String> {
    Some(match entry {
        TranscriptEntry::User { text, images, .. } => {
            let images = images.as_ref().map_or(0, Vec::len);
            if images > 0 { format!("{text}\n[{images} attached image(s)]") } else { text.clone() }
        }
        TranscriptEntry::Context { text, .. } => text.clone(),
        TranscriptEntry::Assistant { message, .. } => {
            let mut parts = Vec::new();
            for block in &message.content {
                match block {
                    AssistantBlock::Text { text, .. } if !text.trim().is_empty() => parts.push(text.clone()),
                    AssistantBlock::ToolCall { name, arguments, .. } => parts.push(format!("-> {name} {}", Value::Object(arguments.clone()))),
                    _ => {}
                }
            }
            parts.join("\n")
        }
        TranscriptEntry::ToolResult { content, .. } => content
            .iter()
            .map(|part| match part {
                ResultContent::Text { text } => text.clone(),
                ResultContent::Image { mime_type, .. } => format!("[image {mime_type}]"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    })
}

/// A line around its match, at most `width` characters.
fn excerpt(line: &str, at: usize, width: usize) -> String {
    let line = line.trim_end();
    if line.chars().count() <= width {
        return line.to_owned();
    }
    let before = line.get(..at).map_or(0, |head| head.chars().count());
    let start = before.saturating_sub(width / 3);
    let text: String = line.chars().skip(start).take(width).collect();
    format!("{}{text}{}", if start > 0 { "…" } else { "" }, if start + width < line.chars().count() { "…" } else { "" })
}

/// How a search matches a line: the byte where the match starts.
pub enum Pattern {
    Text(String),
    Regex(Regex),
}

impl Pattern {
    pub fn new(query: &str, regex: bool) -> Result<Self, String> {
        if query.trim().is_empty() {
            return Err("query is empty".into());
        }
        if regex {
            regex::RegexBuilder::new(query).case_insensitive(true).size_limit(1 << 20).build().map(Self::Regex).map_err(|error| format!("invalid regular expression: {error}"))
        } else {
            Ok(Self::Text(query.to_lowercase()))
        }
    }

    fn find(&self, line: &str) -> Option<usize> {
        match self {
            Self::Text(text) => {
                let lower = line.to_lowercase();
                // Offsets into the lowercased line hold for the line only where lowercasing kept every length.
                lower.find(text.as_str()).map(|at| if lower.len() == line.len() && line.is_char_boundary(at) { at } else { 0 })
            }
            Self::Regex(regex) => regex.find(line).map(|found| found.start()),
        }
    }
}

/// Matching lines of the chat's entries, grouped by entry, within `limit` lines and about `tokens` tokens.
pub fn search(entries: &[TranscriptEntry], pattern: &Pattern, kind: Kind, limit: usize, tokens: u64) -> String {
    let mut out = String::new();
    let (mut shown, mut more_lines, mut more_entries) = (0, 0, 0);
    for (index, entry) in entries.iter().enumerate() {
        if !kind.takes(entry) {
            continue;
        }
        let (Some(heading), Some(text)) = (heading(index + 1, entry), entry_text(entry)) else { continue };
        let matches: Vec<(usize, &str, usize)> = text.lines().enumerate().filter_map(|(line_index, line)| pattern.find(line).map(|at| (line_index + 1, line, at))).collect();
        if matches.is_empty() {
            continue;
        }
        let room = limit.saturating_sub(shown);
        if room == 0 || estimate_text(&out) >= tokens {
            more_entries += 1;
            more_lines += matches.len();
            continue;
        }
        out.push_str(&heading);
        out.push('\n');
        for (line_number, line, at) in matches.iter().take(room) {
            out.push_str(&format!("  {line_number}: {}\n", excerpt(line, *at, 240)));
        }
        shown += matches.len().min(room);
        if matches.len() > room {
            more_lines += matches.len() - room;
        }
    }
    if out.is_empty() {
        return "No entry of this chat matches.".into();
    }
    if more_lines > 0 {
        out.push_str(&format!("[{more_lines} more matching line(s){}. Narrow the query or the kind, or read an entry by number.]\n", if more_entries > 0 { format!(" in {more_entries} more entries") } else { String::new() }));
    }
    out
}

/// Entry numbers from `12`, `40-45` or `3,7,10-12`, in order, at most `max`.
pub fn parse_numbers(spec: &str, count: usize, max: usize) -> Result<Vec<usize>, String> {
    let mut numbers = Vec::new();
    for part in spec.split(',').map(|part| part.trim().trim_start_matches('#')).filter(|part| !part.is_empty()) {
        let (from, to) = match part.split_once('-') {
            Some((from, to)) => (from.trim().trim_start_matches('#'), to.trim().trim_start_matches('#')),
            None => (part, part),
        };
        let (Ok(from), Ok(to)) = (from.parse::<usize>(), to.parse::<usize>()) else { return Err(format!("invalid entries \"{spec}\": use a number, a range like 40-45, or a comma-separated list")) };
        if from == 0 || from > to {
            return Err(format!("invalid entries \"{spec}\": entry numbers start at 1 and ranges go upwards"));
        }
        for number in from..=to.min(count) {
            if numbers.len() == max {
                return Ok(numbers);
            }
            numbers.push(number);
        }
    }
    if numbers.is_empty() {
        return Err(format!("this chat has {count} entries"));
    }
    Ok(numbers)
}

/// Lines `from..=to` (from 1) of a text.
pub fn line_window(text: &str, from: usize, to: usize) -> String {
    text.lines().enumerate().skip(from.saturating_sub(1)).take(to.saturating_sub(from) + 1).map(|(index, line)| format!("{}: {line}", index + 1)).collect::<Vec<_>>().join("\n")
}

/// Entries in full, each kept to its share of about `tokens` tokens: a longer one shows its head and tail and how
/// many lines are left out.
pub fn read(entries: &[TranscriptEntry], numbers: &[usize], lines: Option<(usize, usize)>, tokens: u64) -> String {
    let shown: Vec<(String, String)> = numbers.iter().filter_map(|number| {
        let entry = entries.get(number - 1)?;
        Some((heading(*number, entry)?, entry_text(entry)?))
    }).collect();
    if shown.is_empty() {
        return "Those entries hold nothing the model saw (notices, checkpoints and turn records are not kept for recall).".into();
    }
    let share = (tokens / shown.len() as u64).max(400);
    let mut out = String::new();
    for (heading, text) in shown {
        let total = text.lines().count();
        let body = match lines {
            Some((from, to)) => line_window(&text, from, to),
            None if estimate_text(&text) <= share => text,
            None => {
                let all: Vec<&str> = text.lines().collect();
                let (mut head, mut tail) = (all.len() * 2 / 3, all.len() / 3);
                loop {
                    let candidate = format!("{}\n[... {} of {total} lines left out; pass lines (e.g. \"{}-{}\") to read them, or search with query ...]\n{}", all[..head].join("\n"), all.len() - head - tail, head + 1, all.len() - tail, all[all.len() - tail..].join("\n"));
                    if estimate_text(&candidate) <= share || head + tail <= 2 {
                        break if head + tail <= 2 { truncate_chars(&text, (share * 3) as usize).to_owned() } else { candidate };
                    }
                    head = head * 3 / 4;
                    tail = tail * 3 / 4;
                }
            }
        };
        out.push_str(&format!("{heading} ({total} lines)\n{body}\n\n"));
    }
    out.trim_end().to_owned() + "\n"
}

/// The tool outputs that changed or read files, by display path: the entry numbers of each.
fn files(entries: &[(usize, &TranscriptEntry)]) -> (BTreeMap<String, Vec<usize>>, BTreeMap<String, Vec<usize>>) {
    let (mut changed, mut read) = (BTreeMap::<String, Vec<usize>>::new(), BTreeMap::<String, Vec<usize>>::new());
    for (number, entry) in entries {
        let TranscriptEntry::ToolResult { tool_name, tool_call_id, details: Some(details), is_error: false, .. } = entry else { continue };
        for change in changes::file_changes(tool_name, tool_call_id, details) {
            changed.entry(change.path.to_owned()).or_default().push(*number);
        }
        if tool_name == "read"
            && let Some(path) = details.get("path").and_then(Value::as_str)
        {
            read.entry(path.to_owned()).or_default().push(*number);
        }
    }
    (changed, read)
}

fn numbers_text(numbers: &[usize]) -> String {
    let shown: Vec<String> = numbers.iter().rev().take(3).rev().map(|number| format!("#{number}")).collect();
    if numbers.len() > 3 { format!("{} and {} earlier", shown.join(", "), numbers.len() - 3) } else { shown.join(", ") }
}

fn one_line(text: &str, width: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > width { format!("{}…", truncate_chars(&flat, width)) } else { flat }
}

/// Section sizes of an index, largest first; a smaller one is used when the index would exceed its budget.
const INDEX_SIZES: [(usize, usize, usize, usize); 4] = [(400, 30, 20, 12), (240, 20, 12, 8), (140, 12, 8, 5), (80, 8, 5, 3)];

/// An index of entries `first..=last` (numbers from 1), built from the record without a model: the user's
/// messages, the latest plan, the files changed and read, commands and errors, and the last thing the assistant
/// said; within about `tokens` tokens.
pub fn span_index(entries: &[TranscriptEntry], first: usize, last: usize, tokens: u64) -> String {
    let span: Vec<(usize, &TranscriptEntry)> = entries.iter().enumerate().map(|(index, entry)| (index + 1, entry)).filter(|(number, _)| (first..=last).contains(number)).collect();
    let users: Vec<(usize, &str)> = span.iter().filter_map(|(number, entry)| if let TranscriptEntry::User { text, .. } = entry { Some((*number, text.as_str())) } else { None }).collect();
    let plan = span.iter().rev().find_map(|(number, entry)| match entry {
        TranscriptEntry::ToolResult { details: Some(details), is_error: false, .. } => details.get("plan").and_then(Value::as_array).map(|plan| (*number, plan.clone())),
        _ => None,
    });
    let (changed, read) = files(&span);
    let commands: Vec<String> = span
        .iter()
        .filter_map(|(number, entry)| match entry {
            TranscriptEntry::ToolResult { details: Some(details), .. } => details.get("command").and_then(Value::as_str).map(|command| {
                let exit = details.get("exitCode").and_then(Value::as_i64).map_or(String::new(), |code| format!(" (exit {code})"));
                format!("#{number} {}{exit}", one_line(command, 140))
            }),
            _ => None,
        })
        .collect();
    let errors: Vec<String> = span
        .iter()
        .filter_map(|(number, entry)| match entry {
            TranscriptEntry::ToolResult { tool_name, is_error: true, .. } => {
                let text = entry_text(entry).unwrap_or_default();
                let first = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
                Some(format!("#{number} {tool_name}: {}", one_line(first, 160)))
            }
            _ => None,
        })
        .collect();
    let last_note = span.iter().rev().find_map(|(number, entry)| match entry {
        TranscriptEntry::Assistant { message, .. } => {
            let text: Vec<&str> = message.content.iter().filter_map(|block| if let AssistantBlock::Text { text, .. } = block { Some(text.as_str()) } else { None }).collect();
            let text = text.join("\n");
            (!text.trim().is_empty()).then(|| (*number, text))
        }
        _ => None,
    });
    let mut index = String::new();
    for (user_width, user_count, list_count, error_count) in INDEX_SIZES {
        let mut out = Vec::new();
        if !users.is_empty() {
            out.push("User messages:".to_owned());
            if users.len() > user_count {
                out.push(format!("({} earlier ones left out: recall with kind \"user\")", users.len() - user_count));
            }
            for (number, text) in users.iter().skip(users.len().saturating_sub(user_count)) {
                out.push(format!("#{number}: {}", one_line(text, user_width)));
            }
        }
        if let Some((number, plan)) = &plan {
            out.push(format!("Plan as of #{number}:"));
            for step in plan.iter().take(30) {
                let mark = match step["status"].as_str() {
                    Some("completed") => "x",
                    Some("in_progress") => ">",
                    _ => " ",
                };
                out.push(format!("[{mark}] {}", one_line(step["text"].as_str().unwrap_or_default(), 160)));
            }
        }
        let list = |title: &str, files: &BTreeMap<String, Vec<usize>>, out: &mut Vec<String>| {
            if files.is_empty() {
                return;
            }
            let mut sorted: Vec<(&String, &Vec<usize>)> = files.iter().collect();
            sorted.sort_by_key(|(_, numbers)| std::cmp::Reverse(numbers.last().copied()));
            let mut line = format!("{title}: ");
            line.push_str(&sorted.iter().take(list_count).map(|(path, numbers)| format!("{path} ({})", numbers_text(numbers))).collect::<Vec<_>>().join("; "));
            if sorted.len() > list_count {
                line.push_str(&format!("; and {} more", sorted.len() - list_count));
            }
            out.push(line);
        };
        list("Files changed", &changed, &mut out);
        list("Files read", &read, &mut out);
        if !commands.is_empty() {
            out.push(format!("Commands{}:", if commands.len() > list_count { format!(" (latest {list_count} of {})", commands.len()) } else { String::new() }));
            out.extend(commands.iter().skip(commands.len().saturating_sub(list_count)).cloned());
        }
        if !errors.is_empty() {
            out.push(format!("Errors{}:", if errors.len() > error_count { format!(" (latest {error_count} of {})", errors.len()) } else { String::new() }));
            out.extend(errors.iter().skip(errors.len().saturating_sub(error_count)).cloned());
        }
        if let Some((number, text)) = &last_note {
            out.push(format!("Last assistant message in the span (#{number}): {}", one_line(text, user_width * 2)));
        }
        index = out.join("\n");
        if estimate_text(&index) <= tokens {
            break;
        }
    }
    if estimate_text(&index) > tokens {
        index = truncate_chars(&index, (tokens * 3) as usize).to_owned();
    }
    if index.is_empty() { "(The span holds no user message, file change, command or error.)".into() } else { index }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(value: Value) -> TranscriptEntry {
        serde_json::from_value(value).unwrap()
    }

    fn chat() -> Vec<TranscriptEntry> {
        let reply = |content: Value| json!({ "role": "assistant", "content": content, "api": "x", "provider": "p", "model": "m", "stopReason": "toolUse", "timestamp": 1, "usage": {} });
        vec![
            entry(json!({ "kind": "user", "id": "u1", "ts": 1, "text": "Keep Python 3.11, not 3.12.\nFix the build." })),
            entry(json!({ "kind": "assistant", "id": "a1", "ts": 2, "message": reply(json!([{ "type": "text", "text": "Reading." }, { "type": "toolCall", "id": "c1", "name": "read", "arguments": { "file_path": "src/main.rs" } }])) })),
            entry(json!({ "kind": "toolResult", "id": "t1", "ts": 3, "toolCallId": "c1", "toolName": "read", "content": [{ "type": "text", "text": "fn main() {}\n" }], "isError": false, "details": { "path": "src/main.rs" } })),
            entry(json!({ "kind": "assistant", "id": "a2", "ts": 4, "message": reply(json!([{ "type": "toolCall", "id": "c2", "name": "bash", "arguments": { "command": "cargo build" } }])) })),
            entry(json!({ "kind": "toolResult", "id": "t2", "ts": 5, "toolCallId": "c2", "toolName": "bash", "content": [{ "type": "text", "text": "\nerror[E0425]: cannot find value `x` in this scope\n --> src/main.rs:3:5" }], "isError": true, "details": { "command": "cargo build", "exitCode": 101 } })),
            entry(json!({ "kind": "assistant", "id": "a3", "ts": 6, "message": reply(json!([{ "type": "toolCall", "id": "c3", "name": "edit", "arguments": {} }])) })),
            entry(json!({ "kind": "toolResult", "id": "t3", "ts": 7, "toolCallId": "c3", "toolName": "edit", "content": [{ "type": "text", "text": "ok" }], "isError": false, "details": { "path": "src/main.rs", "undoable": true } })),
            entry(json!({ "kind": "notice", "id": "n1", "ts": 8, "level": "info", "text": "x" })),
            entry(json!({ "kind": "assistant", "id": "a4", "ts": 9, "message": reply(json!([{ "type": "text", "text": "Defined x; next: run the tests." }])) })),
        ]
    }

    #[test]
    fn indexes_a_span_without_a_model() {
        let index = span_index(&chat(), 1, 9, 4000);
        assert_eq!(
            index,
            "User messages:\n#1: Keep Python 3.11, not 3.12. Fix the build.\nFiles changed: src/main.rs (#7)\nFiles read: src/main.rs (#3)\nCommands:\n#5 cargo build (exit 101)\nErrors:\n#5 bash: error[E0425]: cannot find value `x` in this scope\nLast assistant message in the span (#9): Defined x; next: run the tests."
        );
        assert!(estimate_text(&span_index(&chat(), 1, 9, 30)) <= 30);
        assert_eq!(span_index(&chat(), 8, 8, 4000), "(The span holds no user message, file change, command or error.)");
    }

    #[test]
    fn searches_and_reads_entries() {
        let entries = chat();
        let found = search(&entries, &Pattern::new("e0425", false).unwrap(), Kind::All, 20, 4000);
        assert_eq!(found, "#5 tool bash (error)\n  2: error[E0425]: cannot find value `x` in this scope\n");
        let found = search(&entries, &Pattern::new(r"python 3\.\d+", true).unwrap(), Kind::User, 20, 4000);
        assert_eq!(found, "#1 user\n  1: Keep Python 3.11, not 3.12.\n");
        assert_eq!(search(&entries, &Pattern::new("cargo", false).unwrap(), Kind::Tool, 20, 4000), "No entry of this chat matches.");
        assert!(search(&entries, &Pattern::new("cargo", false).unwrap(), Kind::Assistant, 20, 4000).starts_with("#4 assistant\n  1: -> bash {\"command\":\"cargo build\"}"));
        assert!(Pattern::new("(", true).is_err());
        assert_eq!(parse_numbers("#3, 5-6", 9, 30).unwrap(), [3, 5, 6]);
        assert_eq!(parse_numbers("8-20", 9, 30).unwrap(), [8, 9]);
        assert!(parse_numbers("0", 9, 30).is_err() && parse_numbers("x", 9, 30).is_err() && parse_numbers("12", 9, 30).is_err());
        assert_eq!(read(&entries, &[5], None, 4000), "#5 tool bash (error) (3 lines)\n\nerror[E0425]: cannot find value `x` in this scope\n --> src/main.rs:3:5\n");
        assert_eq!(read(&entries, &[5], Some((2, 2)), 4000), "#5 tool bash (error) (3 lines)\n2: error[E0425]: cannot find value `x` in this scope\n");
        assert!(read(&entries, &[8], None, 4000).starts_with("Those entries hold nothing"));
        let long: String = (1..=2000).map(|line| format!("line {line} of the output\n")).collect();
        let big = vec![entry(json!({ "kind": "toolResult", "id": "t", "ts": 1, "toolCallId": "c", "toolName": "bash", "content": [{ "type": "text", "text": long }], "isError": false }))];
        let shown = read(&big, &[1], None, 1000);
        assert!(estimate_text(&shown) < 1100 && shown.contains("line 1 of") && shown.contains("line 2000 of") && shown.contains("lines left out"));
    }
}
