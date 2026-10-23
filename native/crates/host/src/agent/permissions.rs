//! Which tool calls run, wait for the user, or are refused, by the chat's permission mode (3.x `permissions.ts`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::settings::PermissionRules;

/// Tools that only read; they run in every mode. `stop_command` only ends a command this chat started, and a `task`
/// sub-agent only reads. The browser tools only reach the project's own server, in a window with no sign-ins.
const READ_TOOLS: &[&str] = &["read", "grep", "glob", "web_search", "command_output", "stop_command", "task", "browser_open", "browser_read", "browser_act", "browser_screenshot"];
const CHANGE_TOOLS: &[&str] = &["write", "edit"];
const SHELL_TOOLS: &[&str] = &["bash", "pwsh"];

/// Characters that chain, redirect, substitute or group commands in Bash or PowerShell; such commands always ask.
static COMPOUND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[;&|<>`$(){}\r\n]").unwrap());
static INLINE_FLAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)^(?:-[a-z]*[cepr][a-z]*(?:[=:'"].*)?|--(?:eval|print|command|encodedcommand)(?:=.*)?|/[ck])$"#).unwrap());
static SUBCOMMAND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z][\w:.-]*$").unwrap());
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\w+=").unwrap());

/// Programs whose first argument names a subcommand, so their rule keeps both words (`git status`, `pip install`).
const SUBCOMMAND_PROGRAMS: &[&str] =
    &["git", "npm", "pnpm", "yarn", "npx", "pip", "pip3", "uv", "poetry", "cargo", "go", "dotnet", "docker", "kubectl", "apt", "apt-get", "brew", "conda", "gh", "winget", "choco", "scoop", "make"];
/// Programs that run their arguments as another command; rules never cover them.
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "su", "env", "xargs", "nohup", "timeout", "time", "nice", "watch", "exec", "eval", "command", "builtin", "source", ".", "wsl", "runas", "start", "start-process", "saps",
    "invoke-expression", "iex", "invoke-command", "icm", "invoke-item", "ii", "call",
];
/// Interpreters that run inline code after flags like `-c`; such a call always asks.
const INTERPRETERS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh", "fish", "pwsh", "powershell", "cmd", "python", "python3", "py", "node", "deno", "bun", "perl", "ruby", "php", "lua", "osascript", "java", "jshell"];

/// Model-facing reason for calls refused in read-only mode.
pub const READ_ONLY_REASON: &str = "This chat is in read-only mode: files cannot be changed, commands cannot run and tools that change state are unavailable. Describe the change or command you would make, or ask the user to switch the chat out of read-only mode.";

/// Program name as rules compare it: no directory, no `.exe`, lowercase.
fn program_name(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word).to_lowercase();
    name.strip_suffix(".exe").map(str::to_owned).unwrap_or(name)
}

/// Words of a command that neither chains, redirects nor substitutes.
fn simple_words(command: &str) -> Option<Vec<&str>> {
    let text = command.trim();
    if text.is_empty() || COMPOUND.is_match(text) {
        return None;
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let program = program_name(words[0]);
    if WRAPPERS.contains(&program.as_str()) || ASSIGNMENT.is_match(words[0]) {
        return None;
    }
    if INTERPRETERS.contains(&program.as_str()) && words[1..].iter().any(|word| INLINE_FLAG.is_match(word)) {
        return None;
    }
    Some(words)
}

/// The rule "always allow" offers: `program`, or `program subcommand` for tools like git and pip; nothing when the
/// command chains, redirects, substitutes, wraps another command or runs inline code.
pub fn command_rule(command: &str) -> Option<String> {
    let words = simple_words(command)?;
    match words.get(1) {
        Some(sub) if SUBCOMMAND_PROGRAMS.contains(&program_name(words[0]).as_str()) && SUBCOMMAND.is_match(sub) => Some(format!("{} {sub}", words[0])),
        _ => Some(words[0].to_owned()),
    }
}

/// Whether remembered rules cover a command: it must be simple and start with every word of one rule.
pub fn command_allowed(command: &str, rules: &[String], windows: bool) -> bool {
    let Some(words) = simple_words(command) else { return false };
    // The program is compared as written, so a rule for `./solve` does not cover a `solve` elsewhere.
    let word = |value: &str, first: bool| -> String {
        if !windows {
            value.to_owned()
        } else if first {
            let lower = value.to_lowercase();
            lower.strip_suffix(".exe").map(str::to_owned).unwrap_or(lower)
        } else {
            value.to_lowercase()
        }
    };
    rules.iter().any(|rule| {
        let parts: Vec<&str> = rule.split_whitespace().collect();
        parts.len() <= words.len() && parts.iter().enumerate().all(|(index, part)| word(part, index == 0) == word(words[index], index == 0))
    })
}

/// Resolve links in the longest existing prefix of a path, so a link inside the project cannot hide a target outside.
pub fn canonical_path(path: &Path) -> PathBuf {
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        match crate::paths::realpath(&current) {
            Ok(real) => {
                let mut result = real;
                for part in rest.iter().rev() {
                    result.push(part);
                }
                return result;
            }
            Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => {
                let (Some(parent), Some(name)) = (current.parent(), current.file_name()) else { return path.to_path_buf() };
                rest.push(name.to_owned());
                current = parent.to_path_buf();
            }
            Err(_) => return path.to_path_buf(),
        }
    }
}

/// What the chat knows when a tool call is about to run.
pub struct Input<'a> {
    /// `read-only`, `ask` or `auto`.
    pub mode: &'a str,
    pub tool: &'a str,
    pub args: &'a Value,
    pub cwd: &'a Path,
    /// Canonical project directories: the working directory and the project's other roots.
    pub roots: &'a dyn Fn() -> Vec<PathBuf>,
    pub rules: &'a PermissionRules,
    /// MCP tools their server marks read-only.
    pub read_only_tools: &'a HashSet<String>,
    /// Approvals can be remembered: the chat belongs to a project.
    pub can_remember: bool,
    pub windows: bool,
}

/// Run, refuse with a reason for the model, or wait for the user with the approval card's fields.
#[derive(Debug, PartialEq)]
pub enum Decision {
    Allow,
    Deny(String),
    Ask(Value),
}

fn string_arg<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str)
}

/// Decide one tool call.
pub fn decide(input: &Input<'_>) -> Decision {
    if READ_TOOLS.contains(&input.tool) || input.mode == "auto" {
        return Decision::Allow;
    }
    // A fetched URL reaches a third party and could carry project data out, so each new site asks first, in every mode.
    if input.tool == "web_fetch" {
        let url = string_arg(input.args, "url");
        let Some(host) = url.and_then(|url| url::Url::parse(url.trim()).ok()).filter(|url| matches!(url.scheme(), "http" | "https")).and_then(|url| url.host_str().map(str::to_lowercase)) else {
            return Decision::Allow;
        };
        let rule = format!("web_fetch:{host}");
        if input.rules.tools.contains(&rule) {
            return Decision::Allow;
        }
        let mut approval = json!({ "reason": "web", "path": url });
        if input.can_remember {
            approval["rule"] = json!(rule);
        }
        return Decision::Ask(approval);
    }
    let read_only_tool = input.read_only_tools.contains(input.tool);
    if input.mode == "read-only" {
        return if read_only_tool { Decision::Allow } else { Decision::Deny(READ_ONLY_REASON.into()) };
    }
    if CHANGE_TOOLS.contains(&input.tool) {
        let Some(requested) = string_arg(input.args, "file_path").filter(|path| !path.trim().is_empty()) else { return Decision::Allow };
        let target = canonical_path(&crate::paths::normalize(&input.cwd.join(requested)));
        let inside = (input.roots)().iter().any(|root| crate::paths::relative_to(root, &target).is_some());
        return if inside { Decision::Allow } else { Decision::Ask(json!({ "reason": "outside-write", "path": target.to_string_lossy() })) };
    }
    if SHELL_TOOLS.contains(&input.tool) {
        let command = string_arg(input.args, "command").unwrap_or_default();
        if command_allowed(command, &input.rules.commands, input.windows) {
            return Decision::Allow;
        }
        let mut approval = json!({ "reason": "command" });
        if let Some(rule) = command_rule(command).filter(|_| input.can_remember) {
            approval["rule"] = json!(rule);
        }
        return Decision::Ask(approval);
    }
    if read_only_tool || input.rules.tools.iter().any(|rule| rule == input.tool) {
        return Decision::Allow;
    }
    let mut approval = json!({ "reason": "mcp" });
    if input.can_remember {
        approval["rule"] = json!(input.tool);
    }
    Decision::Ask(approval)
}

/// Model-facing text of a refused call.
pub fn denial_text(reason: &str) -> String {
    let said = reason.trim();
    if said.is_empty() {
        "The user denied this tool call. Do not retry it or work around it; stop and wait for the user's instructions.".into()
    } else {
        format!("The user denied this tool call and said: {said}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_and_matches_rules() {
        assert_eq!(command_rule("git status --short").as_deref(), Some("git status"));
        assert_eq!(command_rule("ls -la").as_deref(), Some("ls"));
        assert_eq!(command_rule("ls | wc"), None);
        assert_eq!(command_rule("sudo rm x"), None);
        assert_eq!(command_rule("python -c 'print(1)'"), None);
        assert!(command_allowed("git status -s", &["git status".into()], false));
        assert!(!command_allowed("git push", &["git status".into()], false));
        assert!(command_allowed("Git.exe Status", &["git status".into()], true));
    }

    #[test]
    fn decides_by_mode() {
        let rules = PermissionRules::default();
        let none = HashSet::new();
        let roots = || vec![std::env::temp_dir()];
        let input = |mode: &'static str, tool: &'static str, args: &'static Value| Input { mode, tool, args, cwd: Path::new("/"), roots: &roots, rules: &rules, read_only_tools: &none, can_remember: true, windows: false };
        let command = Box::leak(Box::new(json!({ "command": "git status" })));
        assert_eq!(decide(&input("ask", "read", command)), Decision::Allow);
        assert_eq!(decide(&input("read-only", "bash", command)), Decision::Deny(READ_ONLY_REASON.into()));
        assert_eq!(decide(&input("ask", "bash", command)), Decision::Ask(json!({ "reason": "command", "rule": "git status" })));
        assert_eq!(decide(&input("auto", "bash", command)), Decision::Allow);
        let fetch = Box::leak(Box::new(json!({ "url": "https://Example.com/a" })));
        assert_eq!(decide(&input("auto", "web_fetch", fetch)), Decision::Allow);
        assert_eq!(decide(&input("read-only", "web_fetch", fetch)), Decision::Ask(json!({ "reason": "web", "path": "https://Example.com/a", "rule": "web_fetch:example.com" })));
    }
}
