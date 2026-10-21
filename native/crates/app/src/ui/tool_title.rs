//! One-line summaries of tool calls (`toolTitle` in `src/renderer/chat/ToolCard.tsx`).

use serde_json::{Map, Value};

use crate::i18n::{Locale, format, text};

fn string<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn number(args: &Map<String, Value>, key: &str) -> Option<i64> {
    args.get(key).and_then(Value::as_i64)
}

/// The summary of a call to `name` with `args`.
pub fn tool_title(locale: Locale, name: &str, args: &Map<String, Value>) -> String {
    let t = |key: &str, vars: &[(&str, &dyn std::fmt::Display)]| format(locale, &format!("chat.{key}"), vars);
    match name {
        "bash" | "pwsh" => string(args, "description").or_else(|| string(args, "command")).unwrap_or(name).to_owned(),
        "read" => {
            let path = string(args, "file_path").unwrap_or_default();
            match (number(args, "offset"), number(args, "limit")) {
                (offset, Some(limit)) => {
                    let from = offset.unwrap_or(1);
                    t("toolReadLines", &[("path", &path), ("from", &from), ("to", &(from + limit - 1))])
                }
                (Some(from), None) => t("toolReadFrom", &[("path", &path), ("from", &from)]),
                (None, None) => t("toolRead", &[("path", &path)]),
            }
        }
        "write" => t("toolWrite", &[("path", &string(args, "file_path").unwrap_or_default())]),
        "edit" => t("toolEdit", &[("path", &string(args, "file_path").unwrap_or_default())]),
        "grep" | "glob" => {
            let pattern = string(args, "pattern").unwrap_or_default();
            let path = string(args, "path");
            let (plain, within) = if name == "grep" { ("toolGrep", "toolGrepIn") } else { ("toolGlob", "toolGlobIn") };
            match path {
                Some(path) => t(within, &[("pattern", &pattern), ("path", &path)]),
                None => t(plain, &[("pattern", &pattern)]),
            }
        }
        "task" => t("toolTask", &[("description", &string(args, "description").unwrap_or_default())]),
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
        _ => match name.strip_prefix("mcp__") {
            Some(rest) => rest.replacen("__", ": ", 1),
            None => name.to_owned(),
        },
    }
}

/// Whether the call runs a shell command, whose card shows the command and its output as a terminal.
pub fn is_command(name: &str) -> bool {
    matches!(name, "bash" | "pwsh")
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
    }
}
