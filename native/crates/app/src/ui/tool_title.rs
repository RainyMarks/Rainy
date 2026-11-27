//! One-line summaries of tool calls (`toolTitle` in `src/renderer/chat/ToolCard.tsx`).

use std::borrow::Cow;

use serde_json::{Map, Value};

use crate::i18n::{Locale, format, text};

fn string<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn number(args: &Map<String, Value>, key: &str) -> Option<i64> {
    args.get(key).and_then(Value::as_i64)
}

/// `path` relative to `cwd` when inside it; Windows paths match case-insensitively and across slashes.
pub fn relative_to(path: &str, cwd: &str) -> Option<String> {
    let cwd = cwd.trim_end_matches(['/', '\\']);
    if cwd.is_empty() || path.len() <= cwd.len() || !path.is_char_boundary(cwd.len()) {
        return None;
    }
    let (prefix, rest) = path.split_at(cwd.len());
    let windows = cwd.contains('\\') || cwd.as_bytes().get(1) == Some(&b':');
    let same = if windows {
        prefix.chars().map(|c| if c == '/' { '\\' } else { c.to_ascii_lowercase() }).eq(cwd.chars().map(|c| if c == '/' { '\\' } else { c.to_ascii_lowercase() }))
    } else {
        prefix == cwd
    };
    (same && rest.starts_with(['/', '\\'])).then(|| rest[1..].to_owned())
}

fn path_arg<'a>(args: &'a Map<String, Value>, key: &str, cwd: Option<&str>) -> Option<Cow<'a, str>> {
    let raw = string(args, key)?;
    match cwd.and_then(|dir| relative_to(raw, dir)) {
        Some(rel) => Some(Cow::Owned(rel)),
        None => Some(Cow::Borrowed(raw)),
    }
}

/// The summary of a call to `name` with `args`; the transcript calls [`tool_title_in`] with the chat's folder.
#[cfg(test)]
pub fn tool_title(locale: Locale, name: &str, args: &Map<String, Value>) -> String {
    tool_title_in(locale, name, args, None)
}

/// The summary of a call to `name` with `args`, relativizing `file_path` / `path` against `cwd` without cloning `args`.
pub fn tool_title_in(locale: Locale, name: &str, args: &Map<String, Value>, cwd: Option<&str>) -> String {
    let t = |key: &str, vars: &[(&str, &dyn std::fmt::Display)]| format(locale, &format!("chat.{key}"), vars);
    // The Host runs a built-in tool called with the wrong letter case (`Read`), so it is titled as one.
    let lower = name.to_ascii_lowercase();
    let name = if name.starts_with("mcp__") { name } else { lower.as_str() };
    match name {
        "bash" | "pwsh" => string(args, "description").or_else(|| string(args, "command")).unwrap_or(name).to_owned(),
        "read" => {
            let path = path_arg(args, "file_path", cwd).unwrap_or_default();
            match (number(args, "offset"), number(args, "limit")) {
                (offset, Some(limit)) => {
                    let from = offset.unwrap_or(1);
                    t("toolReadLines", &[("path", &path), ("from", &from), ("to", &(from + limit - 1))])
                }
                (Some(from), None) => t("toolReadFrom", &[("path", &path), ("from", &from)]),
                (None, None) => t("toolRead", &[("path", &path)]),
            }
        }
        "write" => {
            let path = path_arg(args, "file_path", cwd).unwrap_or_default();
            t("toolWrite", &[("path", &path)])
        }
        "apply_patch" => {
            let files = patch_files(string(args, "input").unwrap_or_default());
            match files.as_slice() {
                // An external agent's edit names its file instead of carrying a patch.
                [] => {
                    let path = path_arg(args, "file_path", cwd).unwrap_or(Cow::Borrowed("…"));
                    t("toolPatch", &[("path", &path)])
                }
                [path] => t("toolPatch", &[("path", path)]),
                _ => t("toolPatchFiles", &[("count", &files.len())]),
            }
        }
        "todo_write" | "update_plan" | "write_todos" => {
            let items = plan_items(name, args);
            if items.is_empty() {
                t("toolPlanEmpty", &[])
            } else {
                let done = items.iter().filter(|(_, status)| *status == "completed").count();
                t("toolPlan", &[("done", &done), ("total", &items.len())])
            }
        }
        "edit" => {
            let path = path_arg(args, "file_path", cwd).unwrap_or_default();
            t("toolEdit", &[("path", &path)])
        }
        "grep" | "glob" => {
            let pattern = string(args, "pattern").unwrap_or_default();
            let path = path_arg(args, "path", cwd);
            let (plain, within) = if name == "grep" { ("toolGrep", "toolGrepIn") } else { ("toolGlob", "toolGlobIn") };
            match path {
                Some(path) => t(within, &[("pattern", &pattern), ("path", &path)]),
                None => t(plain, &[("pattern", &pattern)]),
            }
        }
        "task" => t("toolTask", &[("description", &string(args, "description").unwrap_or_default())]),
        // An external agent (Codex) waiting for its sub-agents.
        "agent_wait" => t("toolAgentWait", &[]),
        "browser_open" => t("toolBrowserOpen", &[("url", &string(args, "url").unwrap_or_default())]),
        "browser_read" => t("toolBrowserRead", &[]),
        "browser_screenshot" => t("toolBrowserShot", &[]),
        "browser_act" => {
            let action = match string(args, "action") {
                Some("type") => "chat.toolBrowserActType",
                Some("press") => "chat.toolBrowserActPress",
                _ => "chat.toolBrowserActClick",
            };
            t("toolBrowserAct", &[("action", &text(locale, action)), ("target", &string(args, "target").unwrap_or_default())])
        }
        "command_output" => t("toolCommandOutput", &[("id", &string(args, "id").unwrap_or_default())]),
        "stop_command" => t("toolStopCommand", &[("id", &string(args, "id").unwrap_or_default())]),
        "web_search" => t("toolWebSearch", &[("query", &string(args, "query").unwrap_or_default())]),
        "web_fetch" => t("toolWebFetch", &[("url", &string(args, "url").unwrap_or_default())]),
        "hand_off" => t("toolHandOff", &[]),
        "replan" => t("toolReplan", &[]),
        "swarm_add" => t("toolSwarmAdd", &[("name", &string(args, "name").unwrap_or_default())]),
        "swarm_post" => {
            let to: Vec<String> = args.get("to").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(|to| format!("@{to}")).collect();
            t("toolSwarmPost", &[("to", &to.join(" "))])
        }
        "swarm_wait" => t("toolSwarmWait", &[]),
        "swarm_members" => t("toolSwarmMembers", &[]),
        "swarm_land" => t("toolSwarmLand", &[("name", &string(args, "name").unwrap_or_default())]),
        "swarm_remove" => t("toolSwarmRemove", &[("name", &string(args, "name").unwrap_or_default())]),
        "recall" => match (string(args, "query").filter(|query| !query.trim().is_empty()), string(args, "entries")) {
            (Some(query), _) => t("toolRecallSearch", &[("query", &query)]),
            (None, Some(entries)) => t("toolRecallEntries", &[("entries", &entries)]),
            (None, None) => t("toolRecall", &[]),
        },
        _ => match name.strip_prefix("mcp__") {
            Some(rest) => rest.replacen("__", ": ", 1),
            None => name.to_owned(),
        },
    }
}

/// The files an `apply_patch` call adds, updates or deletes, in order.
pub fn patch_files(patch: &str) -> Vec<&str> {
    patch
        .lines()
        .filter_map(|line| ["*** Add File: ", "*** Update File: ", "*** Delete File: "].iter().find_map(|header| line.trim().strip_prefix(header)))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .collect()
}

/// The items of a plan tool's call, with their status, whichever agent's arguments it takes.
pub fn plan_items<'a>(name: &str, args: &'a Map<String, Value>) -> Vec<(&'a str, &'a str)> {
    let (list, field) = match name {
        "update_plan" => ("plan", "step"),
        "write_todos" => ("todos", "description"),
        _ => ("todos", "content"),
    };
    args.get(list)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| Some((item.get(field)?.as_str()?, item.get("status").and_then(Value::as_str).unwrap_or("pending"))))
        .collect()
}

/// Whether the call runs a shell command, whose card shows the command and its output as a terminal.
pub fn is_command(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "bash" | "pwsh")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn summarizes_calls() {
        let zh = Locale::Zh;
        assert_eq!(tool_title(zh, "read", &args(json!({"file_path": "a.py"}))), "读取 a.py");
        assert_eq!(tool_title(zh, "read", &args(json!({"file_path": "a.py", "offset": 10, "limit": 5}))), "读取 a.py（第 10–14 行）");
        assert_eq!(tool_title(zh, "bash", &args(json!({"command": "ls", "description": "列出文件"}))), "列出文件");
        assert_eq!(tool_title(zh, "grep", &args(json!({"pattern": "fn", "path": "src"}))), "在 src 中搜索内容 fn");
        assert_eq!(tool_title(zh, "mcp__files__read_file", &Map::new()), "files: read_file");
        assert_eq!(tool_title(Locale::En, "write", &args(json!({"file_path": "b"}))), "Write b");
        assert_eq!(tool_title(zh, "Read", &args(json!({"file_path": "a.py"}))), "读取 a.py");
        let patch = "*** Begin Patch\n*** Update File: a.rs\n*** Move to: b.rs\n-x\n+y\n*** Delete File: c.rs\n*** End Patch";
        assert_eq!(tool_title(zh, "apply_patch", &args(json!({"input": patch}))), "修改 2 个文件");
        assert_eq!(tool_title(zh, "apply_patch", &args(json!({"Path": "C:\\w\\calc\\ops.py", "file_path": "calc\\ops.py"}))), "修改 calc\\ops.py");
        assert_eq!(tool_title(Locale::En, "apply_patch", &args(json!({"input": "*** Begin Patch\n*** Add File: n.md\n+x\n*** End Patch"}))), "Patch n.md");
        let plan = json!({"plan": [{"step": "Read", "status": "completed"}, {"step": "Fix", "status": "in_progress"}]});
        assert_eq!(tool_title(zh, "update_plan", &args(plan)), "更新计划（1/2）");
        assert_eq!(tool_title(Locale::En, "todo_write", &args(json!({"todos": []}))), "Clear plan");
        assert_eq!(tool_title(zh, "recall", &args(json!({"query": "E0425"}))), "查阅历史：E0425");
        assert_eq!(tool_title(Locale::En, "recall", &args(json!({"entries": "40-45"}))), "Read back entries 40-45");
        assert_eq!(
            tool_title_in(zh, "read", &args(json!({"file_path": r"C:\Work\App\src\main.rs"})), Some(r"c:\work\app")),
            "读取 src\\main.rs"
        );
        assert_eq!(
            tool_title_in(Locale::En, "grep", &args(json!({"pattern": "fn", "path": "/repo/src"})), Some("/repo")),
            "Grep fn in src"
        );
    }
}
