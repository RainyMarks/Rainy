//! Declarative lifecycle hooks (Claude Code-style): `PreToolUse`, `PostToolUse`, `PostEdit` and `Stop`.
//! Hooks load from user `settings.json` and, for trusted workspaces only, `<cwd>/.rainy/hooks.json`.
//! Execution reuses `crate::process::command` and `crate::ide::format` with zero extra dependencies.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio_util::sync::CancellationToken;

use super::permissions::wildcard_match;
use crate::error::{HostError, HostResult};
use crate::ide::format;
use crate::ide::tools::Tools as IdeTools;
use crate::locale::l;
use crate::process::{console_text, kill_tree, read_limited, resolve_powershell};

/// Maximum number of hook rules loaded from one source.
pub const MAX_HOOKS: usize = 64;
/// Largest `.rainy/hooks.json` file read.
const MAX_HOOKS_FILE_BYTES: u64 = 128 * 1024;
/// Largest edited file formatted in place by the built-in `format` action.
const MAX_FORMAT_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Default and maximum timeout per hook invocation.
const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
/// Cap on captured stdout/stderr per hook invocation so feedback never bloats context.
const MAX_OUTPUT_BYTES: usize = 8 * 1024;

/// Supported lifecycle event names.
pub const EVENTS: [&str; 4] = ["PreToolUse", "PostToolUse", "PostEdit", "Stop"];

fn default_true() -> bool {
    true
}

/// One declarative hook rule in `settings.json` or `.rainy/hooks.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookRule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `PreToolUse`, `PostToolUse`, `PostEdit` or `Stop`.
    pub event: String,
    /// Tool name filter: exact, `a|b`, or wildcard (`mcp__*`). Empty matches every tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// File extensions or globs for `PostEdit` (e.g. `[".py", ".ts", "*.rs"]`). Empty matches every file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// Built-in action (`"format"` uses `ide::format`) or `"command"` (default when `command` is present).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Command line (or executable when `args` is non-empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// What a `PreToolUse` hook decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreHookOutcome {
    Allow,
    /// Exit code 2 blocked the tool call; `feedback` goes back to the model.
    Block(String),
}

/// Summary of `PostToolUse` / `PostEdit` hook execution for model feedback or logging.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostHookReport {
    /// Text emitted by hooks that may help the model (e.g. lint/format notes).
    pub notes: Vec<String>,
    /// Files modified on disk by a built-in `format` action.
    pub formatted_files: Vec<PathBuf>,
}

/// Runtime context for running hooks of one session.
#[derive(Clone)]
pub struct HookContext {
    pub cwd: PathBuf,
    pub session_id: String,
    pub windows: bool,
    pub pwsh_path: Option<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub ide_tools: Option<IdeTools>,
}

/// Validate one hook rule before storing or running it.
pub fn check_rule(rule: &HookRule) -> HostResult<()> {
    if !EVENTS.contains(&rule.event.as_str()) {
        return Err(HostError::invalid(l("Hook 事件名称无效。", "The hook event name is not valid.")));
    }
    let action = rule.action.as_deref().unwrap_or("command");
    if !matches!(action, "command" | "format") {
        return Err(HostError::invalid(l("Hook 动作类型无效。", "The hook action kind is not valid.")));
    }
    if action == "command" {
        let cmd = rule.command.as_deref().map(str::trim).unwrap_or_default();
        if cmd.is_empty() || cmd.chars().count() > 2000 {
            return Err(HostError::invalid(l("命令型 Hook 必须提供有效的命令。", "A command hook requires a valid command.")));
        }
    }
    if action == "format" && rule.event != "PostEdit" {
        return Err(HostError::invalid(l("内置 format 动作仅支持 PostEdit 事件。", "The built-in format action is only supported for PostEdit.")));
    }
    if rule.args.len() > 64 || rule.args.iter().any(|arg| arg.chars().count() > 1024) {
        return Err(HostError::invalid(l("Hook 参数过多或过长。", "Hook arguments are too many or too long.")));
    }
    if rule.extensions.len() > 64 || rule.extensions.iter().any(|ext| ext.is_empty() || ext.chars().count() > 64) {
        return Err(HostError::invalid(l("Hook 扩展名列表无效。", "The hook extension list is invalid.")));
    }
    if rule.matcher.as_deref().is_some_and(|m| m.chars().count() > 200) {
        return Err(HostError::invalid(l("Hook 匹配表达式过长。", "The hook matcher pattern is too long.")));
    }
    Ok(())
}

/// Read project hooks from `<cwd>/.rainy/hooks.json` when present and valid.
/// Accepts either `{ "hooks": [...] }` or a bare JSON array `[...]`.
pub fn load_project_hooks(cwd: &Path) -> Vec<HookRule> {
    let path = cwd.join(".rainy").join("hooks.json");
    let Ok(meta) = std::fs::metadata(&path) else { return Vec::new() };
    if !meta.is_file() || meta.len() > MAX_HOOKS_FILE_BYTES {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(&path) else { return Vec::new() };
    let Ok(raw) = serde_json::from_slice::<Value>(&bytes) else {
        log::warn!("[hooks] invalid JSON in {}", path.display());
        return Vec::new();
    };
    let items = match raw {
        Value::Array(items) => items,
        Value::Object(mut map) => match map.remove("hooks") {
            Some(Value::Array(items)) => items,
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    items
        .into_iter()
        .take(MAX_HOOKS)
        .filter_map(|item| serde_json::from_value::<HookRule>(item).ok())
        .filter(|rule| check_rule(rule).is_ok())
        .collect()
}

/// Whether `<cwd>/.rainy/hooks.json` exists on disk.
pub fn has_project_hooks(cwd: &Path) -> bool {
    cwd.join(".rainy").join("hooks.json").is_file()
}

/// Effective hooks for a session: global rules from Settings plus project hooks only when `trusted` is true.
pub fn effective_hooks(global: &[HookRule], cwd: &Path, trusted: bool) -> Vec<HookRule> {
    let mut rules: Vec<HookRule> = global.iter().filter(|rule| rule.enabled && check_rule(rule).is_ok()).cloned().collect();
    if trusted {
        rules.extend(load_project_hooks(cwd).into_iter().filter(|rule| rule.enabled));
    }
    rules
}

/// Match a tool name against a rule's `matcher` (`"bash"`, `"edit|write"`, `"mcp__*"`, or empty = all).
pub fn matches_tool(matcher: Option<&str>, tool: &str) -> bool {
    let Some(matcher) = matcher.map(str::trim).filter(|m| !m.is_empty() && *m != "*") else {
        return true;
    };
    matcher.split('|').map(str::trim).filter(|p| !p.is_empty()).any(|pattern| wildcard_match(pattern, tool, true))
}

/// Match a file path against `extensions` (e.g. `".py"`, `"py"`, `"*.ts"`, `"src/**/*.rs"`).
pub fn matches_file(extensions: &[String], file_path: &Path) -> bool {
    if extensions.is_empty() {
        return true;
    }
    let normalized = file_path.to_string_lossy().replace('\\', "/");
    let file_name = file_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let ext_lower = file_path.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default();
    extensions.iter().any(|raw| {
        let pattern = raw.trim();
        if pattern.is_empty() {
            return false;
        }
        if pattern == "*" {
            return true;
        }
        if pattern.contains('*') || pattern.contains('?') {
            let pat = pattern.replace('\\', "/");
            if pat.contains('/') {
                wildcard_match(&pat, &normalized, cfg!(windows))
            } else {
                wildcard_match(&pat, &file_name, cfg!(windows))
            }
        } else {
            let wanted = if pattern.starts_with('.') { pattern.to_lowercase() } else { format!(".{}", pattern.to_lowercase()) };
            !ext_lower.is_empty() && ext_lower == wanted
        }
    })
}

/// Map a file extension to an `ide::format` language id.
pub fn format_language(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "py" | "pyi" => Some("python"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "tsx" | "mts" | "cts" => Some("typescript"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Some("cpp"),
        _ => None,
    }
}

/// Run matching `PreToolUse` hooks. First hook exiting with code 2 blocks execution and feeds output to the model.
#[allow(dead_code)]
pub async fn run_pre_tool(rules: &[HookRule], ctx: &HookContext, tool: &str, args: &Value) -> PreHookOutcome {
    run_pre_tool_cancellable(rules, ctx, tool, args, None).await
}

pub async fn run_pre_tool_cancellable(rules: &[HookRule], ctx: &HookContext, tool: &str, args: &Value, cancel: Option<&CancellationToken>) -> PreHookOutcome {
    for rule in rules {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            break;
        }
        if !rule.enabled || rule.event != "PreToolUse" || !matches_tool(rule.matcher.as_deref(), tool) {
            continue;
        }
        let payload = json!({
            "event": "PreToolUse",
            "tool": tool,
            "args": args,
            "cwd": ctx.cwd.to_string_lossy(),
            "sessionId": ctx.session_id,
        });
        let file_path = args.get("file_path").or_else(|| args.get("path")).and_then(Value::as_str);
        match exec_command_hook(rule, ctx, "PreToolUse", Some(tool), file_path, &payload, cancel).await {
            Ok((Some(2), output)) => {
                let feedback = if output.trim().is_empty() {
                    format!("Blocked by PreToolUse hook (exit code 2) for tool `{tool}`.")
                } else {
                    format!("Blocked by PreToolUse hook:\n{}", output.trim())
                };
                return PreHookOutcome::Block(feedback);
            }
            Ok((Some(0), _)) => {}
            Ok((code, output)) => {
                log::warn!("[hooks] PreToolUse hook exited with {:?}: {}", code, output.trim());
            }
            Err(error) => {
                log::warn!("[hooks] PreToolUse hook failed: {error}");
            }
        }
    }
    PreHookOutcome::Allow
}

/// Run matching `PostToolUse` hooks after a tool finishes.
#[allow(dead_code)]
pub async fn run_post_tool(rules: &[HookRule], ctx: &HookContext, tool: &str, args: &Value, is_error: bool) -> PostHookReport {
    run_post_tool_cancellable(rules, ctx, tool, args, is_error, None).await
}

pub async fn run_post_tool_cancellable(rules: &[HookRule], ctx: &HookContext, tool: &str, args: &Value, is_error: bool, cancel: Option<&CancellationToken>) -> PostHookReport {
    let mut report = PostHookReport::default();
    for rule in rules {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            break;
        }
        if !rule.enabled || rule.event != "PostToolUse" || !matches_tool(rule.matcher.as_deref(), tool) {
            continue;
        }
        let payload = json!({
            "event": "PostToolUse",
            "tool": tool,
            "args": args,
            "isError": is_error,
            "cwd": ctx.cwd.to_string_lossy(),
            "sessionId": ctx.session_id,
        });
        let file_path = args.get("file_path").or_else(|| args.get("path")).and_then(Value::as_str);
        match exec_command_hook(rule, ctx, "PostToolUse", Some(tool), file_path, &payload, cancel).await {
            Ok((code, output)) => {
                let trimmed = output.trim();
                if !trimmed.is_empty() && (code == Some(2) || code != Some(0)) {
                    report.notes.push(format!("[hook PostToolUse] {trimmed}"));
                }
            }
            Err(error) => log::warn!("[hooks] PostToolUse hook failed: {error}"),
        }
    }
    report
}

/// Run matching `PostEdit` hooks for each edited file (supports built-in `ide::format` or user command).
#[allow(dead_code)]
pub async fn run_post_edit(rules: &[HookRule], ctx: &HookContext, tool: &str, edited_files: &[PathBuf]) -> PostHookReport {
    run_post_edit_cancellable(rules, ctx, tool, edited_files, None).await
}

pub async fn run_post_edit_cancellable(rules: &[HookRule], ctx: &HookContext, tool: &str, edited_files: &[PathBuf], cancel: Option<&CancellationToken>) -> PostHookReport {
    let mut report = PostHookReport::default();
    if edited_files.is_empty() {
        return report;
    }
    for file in edited_files {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            break;
        }
        if !file.is_file() {
            continue;
        }
        let file_str = file.to_string_lossy().into_owned();
        for rule in rules {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                break;
            }
            if !rule.enabled || rule.event != "PostEdit" || !matches_tool(rule.matcher.as_deref(), tool) || !matches_file(&rule.extensions, file) {
                continue;
            }
            let action = rule.action.as_deref().unwrap_or("command");
            if action == "format" {
                if run_builtin_format(ctx, file).await {
                    report.formatted_files.push(file.clone());
                }
                continue;
            }
            let payload = json!({
                "event": "PostEdit",
                "tool": tool,
                "path": file_str,
                "cwd": ctx.cwd.to_string_lossy(),
                "sessionId": ctx.session_id,
            });
            match exec_command_hook(rule, ctx, "PostEdit", Some(tool), Some(&file_str), &payload, cancel).await {
                Ok((code, output)) => {
                    let trimmed = output.trim();
                    if !trimmed.is_empty() && code != Some(0) {
                        report.notes.push(format!("[hook PostEdit ({})] {trimmed}", file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| file_str.clone())));
                    }
                }
                Err(error) => log::warn!("[hooks] PostEdit hook failed for {}: {error}", file.display()),
            }
        }
    }
    report
}

/// Run matching `Stop` hooks when a session run finishes.
#[allow(dead_code)]
pub async fn run_stop(rules: &[HookRule], ctx: &HookContext, aborted: bool) {
    run_stop_cancellable(rules, ctx, aborted, None).await;
}

pub async fn run_stop_cancellable(rules: &[HookRule], ctx: &HookContext, aborted: bool, cancel: Option<&CancellationToken>) {
    for rule in rules {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            break;
        }
        if !rule.enabled || rule.event != "Stop" {
            continue;
        }
        let payload = json!({
            "event": "Stop",
            "aborted": aborted,
            "cwd": ctx.cwd.to_string_lossy(),
            "sessionId": ctx.session_id,
        });
        if let Err(error) = exec_command_hook(rule, ctx, "Stop", None, None, &payload, cancel).await {
            log::warn!("[hooks] Stop hook failed: {error}");
        }
    }
}

/// Format one file in place using `ide::format`; returns `true` if file content changed on disk.
async fn run_builtin_format(ctx: &HookContext, file: &Path) -> bool {
    let Some(tools) = &ctx.ide_tools else { return false };
    let Some(language) = format_language(file) else { return false };
    let Some(relative) = crate::paths::relative_to(&ctx.cwd, file).filter(|rel| !rel.is_empty()) else {
        return false;
    };
    let Ok(meta) = std::fs::metadata(file) else { return false };
    if !meta.is_file() || meta.len() > MAX_FORMAT_FILE_BYTES {
        return false;
    }
    let Ok(bytes) = std::fs::read(file) else { return false };
    let Ok(text) = std::str::from_utf8(&bytes) else { return false };
    let request = json!({ "path": relative, "language": language, "text": text });
    let Ok(formatted) = format::format(&request, &ctx.cwd, tools, &ctx.environment).await else {
        return false;
    };
    let Some(next) = formatted.get("text").and_then(Value::as_str) else { return false };
    if next == text {
        return false;
    }
    std::fs::write(file, next.as_bytes()).is_ok()
}

/// Execute one command hook using `crate::process::command`; returns `(exit_code, combined_output)`.
async fn exec_command_hook(
    rule: &HookRule,
    ctx: &HookContext,
    event: &str,
    tool: Option<&str>,
    file_path: Option<&str>,
    payload: &Value,
    cancel: Option<&CancellationToken>,
) -> Result<(Option<i32>, String), String> {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err("hook cancelled".into());
    }
    let command_text = rule.command.as_deref().map(str::trim).filter(|c| !c.is_empty()).ok_or("hook command is empty")?;
    let mut env = crate::agent::util::scrubbed_child_env(&ctx.environment);
    env.insert("RAINY_HOOK_EVENT".into(), event.into());
    env.insert("RAINY_SESSION_ID".into(), ctx.session_id.clone().into());
    env.insert("RAINY_CWD".into(), ctx.cwd.as_os_str().to_owned());
    if let Some(tool) = tool {
        env.insert("RAINY_TOOL_NAME".into(), tool.into());
    }
    if let Some(path) = file_path {
        env.insert("RAINY_FILE_PATH".into(), path.into());
    }
    let mut cmd = if !rule.args.is_empty() {
        crate::process::command(command_text, &rule.args, &ctx.cwd, &env)
    } else if ctx.windows {
        let (pwsh, _) = resolve_powershell(ctx.pwsh_path.as_deref(), None);
        let args = ["-NoLogo".to_owned(), "-NoProfile".to_owned(), "-NonInteractive".to_owned(), "-Command".to_owned(), command_text.to_owned()];
        crate::process::command(&pwsh, &args, &ctx.cwd, &env)
    } else {
        let args = ["-c".to_owned(), command_text.to_owned()];
        crate::process::command("bash", &args, &ctx.cwd, &env)
    };
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|error| error.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        let body = payload.to_string();
        tokio::spawn(async move {
            let _ = stdin.write_all(body.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    let stdout = child.stdout.take().ok_or("hook stdout missing")?;
    let stderr = child.stderr.take().ok_or("hook stderr missing")?;
    let timeout = Duration::from_millis(rule.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).clamp(100, MAX_TIMEOUT_MS));
    let work = async {
        let (out, err, status) = tokio::join!(read_limited(stdout, MAX_OUTPUT_BYTES), read_limited(stderr, MAX_OUTPUT_BYTES), child.wait());
        (out, err, status)
    };
    let cancelled_fut = async {
        match cancel {
            Some(token) => token.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    let (out, err, status) = tokio::select! {
        res = tokio::time::timeout(timeout, work) => match res {
            Ok(done) => done,
            Err(_) => {
                kill_tree(&mut child, Duration::from_secs(2)).await;
                let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                return Err(format!("hook timed out after {}ms", timeout.as_millis()));
            }
        },
        () = cancelled_fut => {
            kill_tree(&mut child, Duration::from_secs(2)).await;
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
            return Err("hook cancelled".into());
        }
    };
    let status = status.map_err(|error| error.to_string())?;
    let err_text = console_text(&err);
    let out_text = console_text(&out);
    let combined = match (err_text.trim().is_empty(), out_text.trim().is_empty()) {
        (false, false) => format!("{}\n{}", err_text.trim(), out_text.trim()),
        (false, true) => err_text.trim().to_owned(),
        (true, false) => out_text.trim().to_owned(),
        (true, true) => String::new(),
    };
    Ok((status.code(), combined))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx(cwd: &Path) -> HookContext {
        HookContext {
            cwd: cwd.to_path_buf(),
            session_id: "s-1".into(),
            windows: cfg!(windows),
            pwsh_path: None,
            environment: BTreeMap::new(),
            ide_tools: None,
        }
    }

    #[test]
    fn validates_and_matches_rules() {
        let ok = HookRule {
            id: Some("fmt".into()),
            event: "PostEdit".into(),
            matcher: Some("edit|write".into()),
            extensions: vec![".py".into(), "*.ts".into()],
            action: Some("format".into()),
            command: None,
            args: Vec::new(),
            timeout_ms: None,
            enabled: true,
        };
        assert!(check_rule(&ok).is_ok());
        assert!(matches_tool(ok.matcher.as_deref(), "edit"));
        assert!(matches_tool(ok.matcher.as_deref(), "write"));
        assert!(!matches_tool(ok.matcher.as_deref(), "bash"));
        assert!(matches_file(&ok.extensions, Path::new("src/main.py")));
        assert!(matches_file(&ok.extensions, Path::new("web/app.ts")));
        assert!(!matches_file(&ok.extensions, Path::new("README.md")));
        assert!(matches_tool(Some("mcp__*"), "mcp__fs__read"));

        let bad_event = HookRule { event: "Unknown".into(), ..ok.clone() };
        assert!(check_rule(&bad_event).is_err());
        let bad_format = HookRule { event: "PreToolUse".into(), ..ok };
        assert!(check_rule(&bad_format).is_err());
    }

    #[test]
    fn loads_project_hooks_only_when_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let rainy = dir.path().join(".rainy");
        std::fs::create_dir_all(&rainy).unwrap();
        std::fs::write(
            rainy.join("hooks.json"),
            r#"{"hooks":[{"event":"PreToolUse","matcher":"bash","command":"echo hi"}]}"#,
        )
        .unwrap();
        let global = vec![HookRule {
            id: Some("g".into()),
            event: "Stop".into(),
            matcher: None,
            extensions: Vec::new(),
            action: None,
            command: Some("echo done".into()),
            args: Vec::new(),
            timeout_ms: None,
            enabled: true,
        }];
        assert_eq!(effective_hooks(&global, dir.path(), false).len(), 1);
        assert_eq!(effective_hooks(&global, dir.path(), true).len(), 2);
    }

    #[tokio::test]
    async fn pre_tool_exit_code_2_blocks_and_returns_feedback() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let block_cmd = if cfg!(windows) {
            "[Console]::Error.WriteLine('policy denied'); exit 2"
        } else {
            "echo 'policy denied' >&2; exit 2"
        };
        let allow_cmd = "exit 0";
        let rules = vec![
            HookRule {
                id: Some("allow".into()),
                event: "PreToolUse".into(),
                matcher: Some("read".into()),
                extensions: Vec::new(),
                action: None,
                command: Some(allow_cmd.into()),
                args: Vec::new(),
                timeout_ms: Some(10_000),
                enabled: true,
            },
            HookRule {
                id: Some("block".into()),
                event: "PreToolUse".into(),
                matcher: Some("bash|pwsh".into()),
                extensions: Vec::new(),
                action: None,
                command: Some(block_cmd.into()),
                args: Vec::new(),
                timeout_ms: Some(10_000),
                enabled: true,
            },
        ];
        assert_eq!(run_pre_tool(&rules, &ctx, "read", &json!({})).await, PreHookOutcome::Allow);
        let outcome = run_pre_tool(&rules, &ctx, "pwsh", &json!({ "command": "echo x" })).await;
        match outcome {
            PreHookOutcome::Block(msg) => assert!(msg.contains("policy denied"), "unexpected feedback: {msg}"),
            PreHookOutcome::Allow => panic!("expected exit code 2 to block"),
        }
    }
}
