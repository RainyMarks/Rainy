//! What the agent did in each turn of a chat, for the map's footprint: every tool call in order, with the file it
//! touched, read from the transcript so it covers turns that ran before the page was opened.

use std::collections::HashMap;

use rainy_protocol::chat::{AssistantBlock, AssistantMessage, TranscriptEntry};
use serde::Serialize;
use serde_json::{Map, Value};

/// Turns returned, newest last.
const MAX_TURNS: usize = 30;
/// Characters of a turn's request and of a step's label.
const TEXT_CHARS: usize = 120;

/// One thing the agent did.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    /// `read`, `create`, `edit`, `delete`, `search`, `command`, `web`, `agent` or `other`.
    pub action: &'static str,
    /// What the step names besides its file: a command's description, a search pattern, a query or the tool's name.
    pub label: String,
    /// Absolute path on the Host of the file or directory the step names, when it names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `true` once it succeeded, `false` when it failed, absent while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
}

/// One request of the user and what the agent did for it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    /// The start of the request.
    pub text: String,
    pub ts: f64,
    pub steps: Vec<Step>,
}

fn clip(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= TEXT_CHARS {
        return line;
    }
    format!("{}…", line.chars().take(TEXT_CHARS).collect::<String>())
}

fn text<'a>(arguments: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty())
}

/// `path` as the Host reaches it: a relative one is in the chat's folder; `.` and `..` are folded away.
pub fn resolve(cwd: &str, path: &str) -> String {
    let absolute = path.starts_with('/') || path.starts_with('\\') || path.as_bytes().get(1) == Some(&b':');
    let windows = path.as_bytes().get(1) == Some(&b':') || (!absolute && (cwd.as_bytes().get(1) == Some(&b':') || cwd.contains('\\')));
    let joined = if absolute || cwd.is_empty() { path.to_owned() } else { format!("{cwd}/{path}") };
    let separator = if windows { '\\' } else { '/' };
    let mut parts: Vec<&str> = Vec::new();
    for (index, part) in joined.split(['/', '\\']).enumerate() {
        match part {
            "" if index > 0 => {}
            "." => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            _ => parts.push(part),
        }
    }
    let result = parts.join(&separator.to_string());
    if windows || result.starts_with('/') || result.is_empty() { result } else { format!("/{result}") }
}

/// The files an `apply_patch` input names, with what it does to each.
fn patch_files(input: &str) -> Vec<(&'static str, &str)> {
    input
        .lines()
        .filter_map(|line| {
            let line = line.trim_end();
            [("*** Add File: ", "create"), ("*** Update File: ", "edit"), ("*** Delete File: ", "delete")]
                .into_iter()
                .find_map(|(prefix, action)| line.strip_prefix(prefix).map(|path| (action, path.trim())))
        })
        .filter(|(_, path)| !path.is_empty())
        .collect()
}

/// The steps of one tool call; `result` is its outcome and details once it finished.
fn steps(name: &str, arguments: &Map<String, Value>, result: Option<(bool, Option<&Value>)>, cwd: &str) -> Vec<Step> {
    let ok = result.map(|(error, _)| !error);
    let step = |action: &'static str, label: String, path: Option<&str>| Step { action, label: clip(&label), path: path.map(|path| resolve(cwd, path)), ok };
    match name {
        "read" | "write" | "edit" => {
            let path = text(arguments, "file_path").or_else(|| text(arguments, "path"));
            let created = result.and_then(|(_, details)| details).and_then(|details| details.get("operation")).and_then(Value::as_str) == Some("create");
            let action = match name {
                "read" => "read",
                "write" if created => "create",
                _ => "edit",
            };
            vec![step(action, path.unwrap_or(name).to_owned(), path)]
        }
        "apply_patch" => {
            let input = text(arguments, "input").unwrap_or_default();
            let files = patch_files(input);
            if files.is_empty() {
                return vec![step("edit", name.to_owned(), None)];
            }
            files.into_iter().map(|(action, path)| step(action, path.to_owned(), Some(path))).collect()
        }
        "grep" | "glob" => {
            let pattern = text(arguments, "pattern").unwrap_or(name);
            vec![step("search", pattern.to_owned(), Some(text(arguments, "path").unwrap_or(".")))]
        }
        "bash" | "pwsh" => {
            let label = text(arguments, "description").or_else(|| text(arguments, "command")).unwrap_or(name);
            vec![step("command", label.to_owned(), None)]
        }
        "web_search" => vec![step("web", text(arguments, "query").unwrap_or(name).to_owned(), None)],
        "web_fetch" => vec![step("web", text(arguments, "url").unwrap_or(name).to_owned(), None)],
        "task" => vec![step("agent", text(arguments, "description").or_else(|| text(arguments, "prompt")).unwrap_or(name).to_owned(), None)],
        _ => {
            let label = name.strip_prefix("mcp__").map_or_else(|| name.to_owned(), |rest| rest.replacen("__", ": ", 1));
            vec![step("other", label, None)]
        }
    }
}

/// The chat's turns with their steps, newest last; `streaming` is the reply being written now.
pub fn footprint(entries: &[TranscriptEntry], streaming: Option<&AssistantMessage>, cwd: &str) -> Vec<Turn> {
    let results: HashMap<&str, (bool, Option<&Value>)> = entries
        .iter()
        .filter_map(|entry| match entry {
            TranscriptEntry::ToolResult { tool_call_id, is_error, details, .. } => Some((tool_call_id.as_str(), (*is_error, details.as_ref()))),
            _ => None,
        })
        .collect();
    let mut turns: Vec<Turn> = Vec::new();
    let calls = |message: &AssistantMessage, turns: &mut Vec<Turn>| {
        let Some(turn) = turns.last_mut() else { return };
        for block in &message.content {
            if let AssistantBlock::ToolCall { id, name, arguments, .. } = block {
                turn.steps.extend(steps(name, arguments, results.get(id.as_str()).copied(), cwd));
            }
        }
    };
    for entry in entries {
        match entry {
            TranscriptEntry::User { id, ts, text, .. } => turns.push(Turn { id: id.clone(), text: clip(text), ts: *ts, steps: Vec::new() }),
            TranscriptEntry::Assistant { message, .. } => calls(message, &mut turns),
            _ => {}
        }
    }
    if let Some(message) = streaming {
        calls(message, &mut turns);
    }
    let skip = turns.len().saturating_sub(MAX_TURNS);
    turns.split_off(skip)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry(value: Value) -> TranscriptEntry {
        serde_json::from_value(value).unwrap()
    }

    fn reply(content: Value) -> Value {
        json!({ "content": content, "api": "", "provider": "", "model": "", "usage": {}, "stopReason": "toolUse", "timestamp": 0 })
    }

    #[test]
    fn resolves_paths_against_the_chat_folder() {
        assert_eq!(resolve("C:\\work\\app", "src/main.rs"), "C:\\work\\app\\src\\main.rs");
        assert_eq!(resolve("C:\\work\\app", "../lib/./a.ts"), "C:\\work\\lib\\a.ts");
        assert_eq!(resolve("/home/u/app", "./src/a.py"), "/home/u/app/src/a.py");
        assert_eq!(resolve("/home/u/app", "/etc/hosts"), "/etc/hosts");
        assert_eq!(resolve("/home/u/app", "."), "/home/u/app");
    }

    #[test]
    fn lists_each_turn_with_its_steps_in_order() {
        let entries = [
            entry(json!({ "kind": "user", "id": "u1", "ts": 1, "text": "修一下\n登录" })),
            entry(json!({ "kind": "assistant", "id": "a1", "ts": 2, "message": reply(json!([
                { "type": "toolCall", "id": "c1", "name": "read", "arguments": { "file_path": "src/login.ts" } },
                { "type": "toolCall", "id": "c2", "name": "grep", "arguments": { "pattern": "token" } },
            ])) })),
            entry(json!({ "kind": "toolResult", "id": "t1", "ts": 3, "toolCallId": "c1", "toolName": "read", "content": [], "isError": false })),
            entry(json!({ "kind": "toolResult", "id": "t2", "ts": 3, "toolCallId": "c2", "toolName": "grep", "content": [], "isError": true })),
            entry(json!({ "kind": "assistant", "id": "a2", "ts": 4, "message": reply(json!([
                { "type": "toolCall", "id": "c3", "name": "write", "arguments": { "file_path": "src/new.ts", "content": "x" } },
                { "type": "toolCall", "id": "c4", "name": "apply_patch", "arguments": { "input": "*** Begin Patch\n*** Update File: src/a.ts\n@@\n*** Delete File: old.ts\n*** End Patch" } },
                { "type": "toolCall", "id": "c5", "name": "pwsh", "arguments": { "command": "npm test", "description": "Run the tests" } },
                { "type": "toolCall", "id": "c6", "name": "mcp__github__search_issues", "arguments": {} },
            ])) })),
            entry(json!({ "kind": "toolResult", "id": "t3", "ts": 5, "toolCallId": "c3", "toolName": "write", "content": [], "isError": false, "details": { "path": "src/new.ts", "operation": "create" } })),
            entry(json!({ "kind": "user", "id": "u2", "ts": 6, "text": "再看看" })),
        ];
        let turns = footprint(&entries, None, "/p");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].text, "修一下 登录");
        let shown: Vec<(&str, &str, Option<&str>, Option<bool>)> =
            turns[0].steps.iter().map(|step| (step.action, step.label.as_str(), step.path.as_deref(), step.ok)).collect();
        assert_eq!(shown, vec![
            ("read", "src/login.ts", Some("/p/src/login.ts"), Some(true)),
            ("search", "token", Some("/p"), Some(false)),
            ("create", "src/new.ts", Some("/p/src/new.ts"), Some(true)),
            ("edit", "src/a.ts", Some("/p/src/a.ts"), None),
            ("delete", "old.ts", Some("/p/old.ts"), None),
            ("command", "Run the tests", None, None),
            ("other", "github: search_issues", None, None),
        ]);
        assert!(turns[1].steps.is_empty());
    }

    #[test]
    fn includes_the_reply_being_written() {
        let entries = [entry(json!({ "kind": "user", "id": "u1", "ts": 1, "text": "hi" }))];
        let streaming: AssistantMessage = serde_json::from_value(reply(json!([
            { "type": "toolCall", "id": "c1", "name": "edit", "arguments": { "file_path": "a.txt" } },
        ])))
        .unwrap();
        let turns = footprint(&entries, Some(&streaming), "/p");
        assert_eq!(turns[0].steps[0].action, "edit");
        assert_eq!(turns[0].steps[0].ok, None);
    }
}
