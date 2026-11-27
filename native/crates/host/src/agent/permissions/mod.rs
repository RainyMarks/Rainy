//! Which tool calls run, wait for the user, or are refused, by the chat's permission mode (3.x `permissions.ts`).

mod glob;
mod rules;
mod shell_parse;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub use glob::{canonical_path, path_denied, tool_denied, wildcard_match};
#[cfg(test)]
pub use rules::command_rule;
pub use rules::{command_allowed, command_denied, command_rule_for};

use super::changes::CHANGE_TOOLS;
use crate::settings::PermissionRules;

/// Tools that only read and carry no arbitrary host file path; they run in every mode unless denied by explicit rule.
const SAFE_READ_TOOLS: &[&str] = &[
    "web_search", "command_output", "stop_command", "task", "browser_open", "browser_read", "browser_act", "browser_screenshot", "todo_write", "update_plan", "write_todos", "recall",
];
/// Read tools whose target path must stay inside project roots unless approved by the user.
const PATH_READ_TOOLS: &[&str] = &["read", "grep", "glob", "lsp_diagnostics", "symbol_outline", "module_deps"];
const SHELL_TOOLS: &[&str] = &["bash", "pwsh"];

/// Model-facing reason for calls refused in read-only mode.
pub const READ_ONLY_REASON: &str = "This chat is in read-only mode: files cannot be changed, commands cannot run and tools that change state are unavailable. Describe the change or command you would make, or ask the user to switch the chat out of read-only mode.";

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
    /// This session's spill folder; reads inside it are always allowed so the model can inspect limited outputs.
    pub spill_dir: Option<&'a Path>,
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

/// Extract target paths a tool call inspects or writes, for deny-path and workspace-boundary checks.
fn tool_paths(tool: &str, args: &Value, cwd: &Path) -> Vec<PathBuf> {
    if tool == crate::agent::tools::patch::NAME {
        return crate::agent::tools::patch::targets(string_arg(args, "input").or_else(|| string_arg(args, "patch")).unwrap_or_default(), cwd);
    }
    if CHANGE_TOOLS.contains(&tool) || tool == "read" {
        return string_arg(args, "file_path")
            .filter(|path| !path.trim().is_empty())
            .map(|path| crate::paths::normalize(&cwd.join(path)))
            .into_iter()
            .collect();
    }
    if matches!(tool, "grep" | "glob" | "lsp_diagnostics" | "symbol_outline" | "module_deps") {
        // Both spellings are checked: the tools differ in which one wins when a call gives both.
        return ["path", "file_path"]
            .into_iter()
            .filter_map(|name| string_arg(args, name))
            .filter(|path| !path.trim().is_empty())
            .map(|path| crate::paths::normalize(&cwd.join(path)))
            .collect();
    }
    if SHELL_TOOLS.contains(&tool) {
        return string_arg(args, "workdir")
            .filter(|path| !path.trim().is_empty())
            .map(|path| crate::paths::normalize(&cwd.join(path)))
            .into_iter()
            .collect();
    }
    Vec::new()
}

fn inside_roots(target: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| crate::paths::relative_to(root, target).is_some())
}

/// Decide one tool call.
pub fn decide(input: &Input<'_>) -> Decision {
    // 1. Explicit deny rules have highest priority across every permission mode (including `auto` and `read-only`).
    if let Some(rule) = tool_denied(input.tool, &input.rules.deny_tools) {
        return Decision::Deny(format!("Tool `{}` is blocked by deny rule `{rule}`.", input.tool));
    }
    if SHELL_TOOLS.contains(&input.tool) {
        let command = string_arg(input.args, "command").unwrap_or_default();
        if let Some(rule) = command_denied(command, &input.rules.deny_commands, input.windows) {
            return Decision::Deny(format!("Command is blocked by deny rule `{rule}`."));
        }
    }
    let targets = tool_paths(input.tool, input.args, input.cwd);
    if !input.rules.deny_paths.is_empty() {
        for target in &targets {
            if let Some(rule) = path_denied(target, input.cwd, &input.rules.deny_paths, input.windows) {
                return Decision::Deny(format!("Access to `{}` is blocked by deny path rule `{rule}`.", target.display()));
            }
        }
    }

    // 2. Path-based read tools check workspace boundaries in `ask` and `read-only` modes.
    if PATH_READ_TOOLS.contains(&input.tool) {
        if input.mode == "auto" || targets.is_empty() {
            return Decision::Allow;
        }
        let mut allowed_roots = (input.roots)();
        if allowed_roots.is_empty() {
            allowed_roots.push(canonical_path(input.cwd));
        }
        if let Some(spill) = input.spill_dir {
            allowed_roots.push(canonical_path(spill));
        }
        let outside = targets.iter().map(|target| canonical_path(target)).find(|target| !inside_roots(target, &allowed_roots));
        return match outside {
            None => Decision::Allow,
            Some(target) => Decision::Ask(json!({ "reason": "outside-read", "path": target.to_string_lossy() })),
        };
    }

    if SAFE_READ_TOOLS.contains(&input.tool) || input.mode == "auto" {
        return Decision::Allow;
    }

    // 3. `read-only` mode refuses all mutating, shell, external-fetch and non-read-only MCP calls consistently.
    let read_only_tool = input.read_only_tools.contains(input.tool);
    if input.mode == "read-only" {
        return if read_only_tool { Decision::Allow } else { Decision::Deny(READ_ONLY_REASON.into()) };
    }

    // 4. `web_fetch` reaches a third party and asks once per host in `ask` mode.
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

    if CHANGE_TOOLS.contains(&input.tool) {
        let roots = (input.roots)();
        let outside = targets.iter().map(|target| canonical_path(target)).find(|target| !inside_roots(target, &roots));
        return match outside {
            None => Decision::Allow,
            Some(target) => Decision::Ask(json!({ "reason": "outside-write", "path": target.to_string_lossy() })),
        };
    }

    if SHELL_TOOLS.contains(&input.tool) {
        let explicit_workdir = targets.first().map(|p| canonical_path(p));
        if let Some(workdir) = &explicit_workdir {
            let roots = (input.roots)();
            if !inside_roots(workdir, &roots) {
                let text = workdir.to_string_lossy().into_owned();
                return Decision::Ask(json!({ "reason": "outside-workdir", "path": text, "workdir": text }));
            }
        }
        let command = string_arg(input.args, "command").unwrap_or_default();
        if command_allowed(command, &input.rules.commands, input.windows) {
            return Decision::Allow;
        }
        let mut approval = json!({ "reason": "command" });
        if let Some(rule) = command_rule_for(command, input.windows).filter(|_| input.can_remember) {
            approval["rule"] = json!(rule);
        }
        if let Some(workdir) = explicit_workdir {
            approval["workdir"] = json!(workdir.to_string_lossy());
        }
        return Decision::Ask(approval);
    }

    if read_only_tool || input.rules.tools.iter().any(|rule| rule == input.tool || wildcard_match(rule, input.tool, true)) {
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
    use base64::Engine as _;

    #[test]
    fn offers_and_matches_rules() {
        assert_eq!(command_rule("git status --short").as_deref(), Some("git status"));
        assert_eq!(command_rule("git -C /tmp status").as_deref(), Some("git status"));
        assert_eq!(command_rule("git --no-pager diff HEAD").as_deref(), Some("git diff"));
        assert_eq!(command_rule("git -c core.sshCommand=evil status"), None);
        assert_eq!(command_rule("git -ccore.sshCommand=evil status"), None);
        assert_eq!(command_rule("git --git-dir=/tmp/evil status"), None);
        assert_eq!(command_rule("git --work-tree /tmp/evil status"), None);
        assert_eq!(command_rule("cargo --version").as_deref(), Some("cargo --version"));
        assert_ne!(command_rule("cargo --version").as_deref(), Some("cargo"));
        assert_eq!(command_rule("cargo +nightly test").as_deref(), Some("cargo test"));
        assert_eq!(command_rule("cargo --manifest-path /tmp/Cargo.toml build"), None);
        assert_eq!(command_rule("cargo -Z unstable-options build"), None);
        assert_eq!(command_rule("make -f /tmp/evil.mk test"), None);
        assert_eq!(command_rule("make -C /tmp/evil test"), None);
        assert_eq!(command_rule("npm --prefix /tmp/evil test"), None);
        assert_eq!(command_rule("npm --userconfig /tmp/evil.npmrc test"), None);
        assert_eq!(command_rule("docker -H tcp://evil:2375 ps"), None);
        assert_eq!(command_rule("kubectl --kubeconfig /tmp/evil get pods"), None);
        assert_eq!(command_rule("ls -la").as_deref(), Some("ls"));
        assert_eq!(command_rule("ls | wc"), None);
        assert_eq!(command_rule("sudo rm x"), None);
        assert_eq!(command_rule("\"sudo\" rm x"), None);
        assert_eq!(command_rule("'env' bash -c id"), None);
        assert_eq!(command_rule("python -c 'print(1)'"), None);
        assert_eq!(command_rule("python3 -m pytest tests/").as_deref(), Some("python3 -m pytest"));
        assert_eq!(command_rule("python3 -um http.server").as_deref(), Some("python3 -m http.server"));
        assert_eq!(command_rule("python3 -X dev -m http.server").as_deref(), Some("python3 -m http.server"));
        assert_eq!(command_rule("python3 -W ignore -m http.server").as_deref(), Some("python3 -m http.server"));
        assert_eq!(command_rule("python3 -m"), None);

        assert!(command_allowed("git status -s", &["git status".into()], false));
        assert!(command_allowed("git -C /tmp status", &["git status".into()], false));
        assert!(!command_allowed("git -c core.sshCommand=evil status", &["git status".into()], false));
        assert!(!command_allowed("git -ccore.sshCommand=evil status", &["git status".into()], false));
        assert!(!command_allowed("git --git-dir=/tmp/evil status", &["git status".into()], false));
        assert!(!command_allowed("git --work-tree /tmp/evil status", &["git status".into()], false));
        assert!(!command_allowed("make -f /tmp/evil.mk test", &["make test".into()], false));
        assert!(!command_allowed("make -C /tmp/evil test", &["make test".into()], false));
        assert!(!command_allowed("cargo --manifest-path /tmp/Cargo.toml build", &["cargo build".into()], false));
        assert!(!command_allowed("cargo -Z unstable-options build", &["cargo build".into()], false));
        assert!(!command_allowed("npm --userconfig /tmp/evil.npmrc test", &["npm test".into()], false));
        assert!(!command_allowed("docker --config /tmp/evil ps", &["docker ps".into()], false));
        assert!(!command_allowed("kubectl --kubeconfig /tmp/evil get pods", &["kubectl get".into()], false));
        assert!(!command_allowed("git push", &["git status".into()], false));
        assert!(command_allowed("Git.exe Status", &["git status".into()], true));
        assert!(command_allowed("git diff src/main.rs", &["git diff *".into()], false));
        assert!(!command_allowed("git push origin main", &["git diff *".into()], false));
        assert!(!command_allowed("python3 -m http.server", &["python3".into()], false));
        assert!(!command_allowed("python3 -um http.server", &["python3".into()], false));
        assert!(!command_allowed("python3 -X dev -m http.server", &["python3".into()], false));
        assert!(!command_allowed("python3 -W ignore -m http.server", &["python3".into()], false));
        assert!(command_allowed("python3 -m pytest -q", &["python3 -m pytest".into()], false));
        assert!(!command_allowed("python3 -m http.server", &["python3 -m pytest".into()], false));
        assert!(command_denied("/bin/rm -rf /tmp/x", &["rm -rf *".into()], false).is_some());
        assert!(command_denied("sudo -u root rm -rf /", &["rm -rf".into()], false).is_some());

        // Regression: option value identical to subcommand name (`-C diff`) must not shift `effective_words` offset.
        assert_eq!(command_rule_for("git -C diff diff --stat", false).as_deref(), Some("git diff"));
        assert!(command_allowed("git -C diff diff --stat", &["git diff --stat".into()], false));
        assert!(!command_allowed("git -C diff diff --evil", &["git diff --stat".into()], false));
        assert!(!command_allowed("git -C diff diff --evil", &["git diff --stat*".into()], false));
    }

    #[test]
    fn blocks_nested_substitutions_control_flow_wrappers_and_escapes() {
        let wild_rules = vec!["rm -rf *".into(), "git push --force*".into()];
        let prefix_rules = vec!["rm -rf".into()];

        // Command substitutions (top-level and inside double quotes), subshells, brace groups, process substitutions.
        for cmd in [
            "echo $(rm -rf /)",
            "echo \"hello $(rm -rf /)\"",
            "echo `rm -rf /`",
            "echo \"`rm -rf /`\"",
            "(rm -rf /)",
            "{ rm -rf /; }",
            "cat <(rm -rf /)",
            "echo x >(rm -rf /)",
            "echo $(echo $(rm -rf /))",
            // Control-flow prefixes and env assignments.
            "if true; then rm -rf /; fi",
            "if rm -rf /; then echo ok; fi",
            "if false; then echo 1; else rm -rf /; fi",
            "for i in 1; do rm -rf /; done",
            "FOO=1 BAR=2 rm -rf /",
            "! rm -rf /",
            // Wrapper options and positional arguments.
            "timeout 10 rm -rf /",
            "timeout -s KILL -k 5 5s rm -rf /",
            "nice -n 10 rm -rf /",
            "nice --adjustment 10 rm -rf /",
            "nice +10 rm -rf /",
            "watch -n 1 rm -rf /",
            "watch -n 1 \"rm -rf /\"",
            "sudo --user root rm -rf /",
            "sudo -D /tmp -g wheel FOO=1 rm -rf /",
            "env -u PATH rm -rf /",
            "env -S \"rm -rf /\"",
            "echo / | xargs -I {} rm -rf {}",
            "setsid rm -rf /",
            "stdbuf -o0 rm -rf /",
            "ionice -c 3 rm -rf /",
            "taskset -c 0 rm -rf /",
            "taskset 0x1 rm -rf /",
            "busybox rm -rf /",
            "chroot /mnt rm -rf /",
            "unshare -r rm -rf /",
            "find . -exec rm -rf {} +",
            // Inline shell invocations including long options with values.
            "bash -c \"rm -rf /\"",
            "bash --rcfile /dev/null -c \"rm -rf /\"",
            "sh -lc 'echo ok && rm -rf /'",
            "sudo bash -c \"rm -rf /\"",
            "eval \"if true; then rm -rf /; fi\"",
            "pwsh -NoProfile -Command \"rm -rf /\"",
            // POSIX backslash, ANSI-C quoting, IFS and glob-obfuscated paths.
            "\\rm -rf /",
            "r\\m -rf /",
            "/bin/\\rm -rf /",
            "$'rm' -rf /",
            "rm${IFS}-rf${IFS}/",
            "/bin/r? -rf /",
            // Subcommand leading option normalization in deny rules.
            "/usr/bin/git -C /tmp push --force",
            "/usr/bin/git --work-tree /tmp push --force",
        ] {
            assert!(
                command_denied(cmd, &wild_rules, false).is_some(),
                "expected wildcard deny rule to block: {cmd}"
            );
        }

        // Prefix deny rules catch bare wrapper invocations without trailing args too.
        for cmd in ["timeout 10 rm -rf", "nice -n 10 rm -rf", "echo / | xargs -n 1 rm -rf", "\\rm -rf", "setsid rm -rf", "$'rm' -rf"] {
            assert!(
                command_denied(cmd, &prefix_rules, false).is_some(),
                "expected prefix deny rule to block: {cmd}"
            );
        }

        // PowerShell backtick escapes, prefix shorthands, EncodedCommand and cmd caret escapes on Windows.
        let utf16le: Vec<u8> = "rm -rf /".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&utf16le);
        let ps_enc = format!("pwsh -NoProfile -enc {encoded}");
        let ps_ec = format!("powershell -ec {encoded}");
        let ps_e = format!("pwsh -e:{encoded}");
        for cmd in [
            "r`m -rf /",
            "pwsh -comm \"rm -rf /\"",
            "powershell -co \"rm -rf /\"",
            ps_enc.as_str(),
            ps_ec.as_str(),
            ps_e.as_str(),
            "cmd /c r^m -rf /",
            "cmd /c \"r^m -rf /\"",
        ] {
            assert!(
                command_denied(cmd, &wild_rules, true).is_some(),
                "expected Windows deny rule to block: {cmd}"
            );
        }
        assert!(command_denied("Re`move-Item -Recurse C:\\tmp", &["Remove-Item *".into()], true).is_some());
        assert!(!command_allowed(&ps_enc, &["pwsh".into()], true));

        // Single-quoted literal text passed to `echo` (not an inline shell executor) must not false-positive.
        assert!(command_denied("echo 'rm -rf /'", &wild_rules, false).is_none());
        assert!(command_denied("echo \"\\$(rm -rf /)\"", &wild_rules, false).is_none());
        // Windows paths keep backslashes intact when `windows = true`.
        assert!(command_denied(r"C:\tools\rm.exe -rf /tmp", &wild_rules, true).is_some());
    }

    #[test]
    fn path_glob_double_star_slash_respects_directory_boundary_and_scales_iteratively() {
        assert!(glob::path_glob_match("**/.env", ".env", false));
        assert!(glob::path_glob_match("**/.env", "src/.env", false));
        assert!(glob::path_glob_match("**/.env", "a/b/c/.env", false));
        assert!(!glob::path_glob_match("**/.env", "src/my.env", false));
        assert!(!glob::path_glob_match("**/.env", "my.env", false));
        assert!(glob::path_glob_match("a/**/b", "a/b", false));
        assert!(glob::path_glob_match("a/**/b", "a/x/y/b", false));
        assert!(!glob::path_glob_match("a/**/b", "a/xb", false));
        // Long adversarial path does not overflow stack or explode memory.
        let deep = format!("{}/x", "a/".repeat(4000));
        assert!(glob::path_glob_match("**/x", &deep, false));
        assert!(!glob::path_glob_match("**/y", &deep, false));
    }

    #[test]
    fn enforces_deny_rules_and_workspace_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical_path(dir.path());
        let spill = root.join("spill");
        let rules = PermissionRules {
            commands: vec!["git status".into()],
            tools: vec![],
            deny_commands: vec!["rm -rf *".into(), "git push --force*".into()],
            deny_paths: vec![".env".into(), ".env.*".into(), "~/.ssh/**".into()],
            deny_tools: vec!["mcp__danger__*".into()],
        };
        let none = HashSet::new();
        let roots = || vec![root.clone()];
        let make = |mode: &'static str, tool: &'static str, args: &'static Value| Input {
            mode,
            tool,
            args,
            cwd: &root,
            roots: &roots,
            rules: &rules,
            read_only_tools: &none,
            can_remember: true,
            windows: false,
            spill_dir: Some(&spill),
        };

        // Deny rules beat even `auto` mode and compound/nested/control-flow/wrapper commands.
        for bad in [
            "echo ok && rm -rf /tmp/x",
            "echo $(rm -rf /)",
            "if true; then rm -rf /; fi",
            "timeout 10 rm -rf /",
            "\\rm -rf /",
        ] {
            let cmd = Box::leak(Box::new(json!({ "command": bad })));
            assert!(matches!(decide(&make("auto", "bash", cmd)), Decision::Deny(_)), "expected auto mode to deny: {bad}");
        }
        let env_read = Box::leak(Box::new(json!({ "file_path": root.join(".env").to_string_lossy() })));
        assert!(matches!(decide(&make("auto", "read", env_read)), Decision::Deny(_)));
        let false_env = Box::leak(Box::new(json!({ "file_path": root.join("src/my.env").to_string_lossy() })));
        assert_eq!(decide(&make("ask", "read", false_env)), Decision::Allow);
        let ssh_read = Box::leak(Box::new(json!({ "file_path": crate::env::home_dir().join(".ssh/id_rsa").to_string_lossy() })));
        assert!(matches!(decide(&make("read-only", "read", ssh_read)), Decision::Deny(_)));
        let mcp_bad = Box::leak(Box::new(json!({})));
        assert!(matches!(decide(&make("auto", "mcp__danger__wipe", mcp_bad)), Decision::Deny(_)));

        // Outside-workspace reads ask in `ask` and `read-only` modes.
        let outside_dir = tempfile::tempdir().unwrap();
        let outside_file = Box::leak(Box::new(json!({ "file_path": outside_dir.path().join("note.txt").to_string_lossy() })));
        assert!(matches!(decide(&make("ask", "read", outside_file)), Decision::Ask(a) if a["reason"] == "outside-read" && a["path"].is_string()));
        assert!(matches!(decide(&make("read-only", "read", outside_file)), Decision::Ask(a) if a["reason"] == "outside-read"));
        let outside_grep = Box::leak(Box::new(json!({ "pattern": "x", "path": outside_dir.path().to_string_lossy() })));
        assert!(matches!(decide(&make("ask", "grep", outside_grep)), Decision::Ask(a) if a["reason"] == "outside-read"));
        // lsp_diagnostics reads `file_path` before `path`: an inside `path` cannot cover an outside `file_path`.
        let both = Box::leak(Box::new(json!({ "path": root.join("src/main.rs").to_string_lossy(), "file_path": outside_dir.path().join("note.txt").to_string_lossy() })));
        assert!(matches!(decide(&make("read-only", "lsp_diagnostics", both)), Decision::Ask(a) if a["reason"] == "outside-read"));

        // Reads inside project root or spill dir remain allowed.
        let inside_file = Box::leak(Box::new(json!({ "file_path": root.join("src/main.rs").to_string_lossy() })));
        assert_eq!(decide(&make("ask", "read", inside_file)), Decision::Allow);
        let spill_file = Box::leak(Box::new(json!({ "file_path": spill.join("out.txt").to_string_lossy() })));
        assert_eq!(decide(&make("ask", "read", spill_file)), Decision::Allow);

        // Outside shell workdir asks even if the command itself is in allow rules.
        let outside_shell = Box::leak(Box::new(json!({ "command": "git status", "workdir": outside_dir.path().to_string_lossy() })));
        assert!(matches!(decide(&make("ask", "bash", outside_shell)), Decision::Ask(a) if a["reason"] == "outside-workdir" && a["workdir"].is_string() && a.get("rule").is_none()));
    }

    #[test]
    fn decides_by_mode() {
        let rules = PermissionRules::default();
        let none = HashSet::new();
        let temp = canonical_path(&std::env::temp_dir());
        let roots = || vec![temp.clone()];
        let input = |mode: &'static str, tool: &'static str, args: &'static Value| Input {
            mode,
            tool,
            args,
            cwd: &temp,
            roots: &roots,
            rules: &rules,
            read_only_tools: &none,
            can_remember: true,
            windows: false,
            spill_dir: None,
        };
        let command = Box::leak(Box::new(json!({ "command": "git status" })));
        assert_eq!(decide(&input("ask", "read", command)), Decision::Allow);
        assert_eq!(decide(&input("read-only", "bash", command)), Decision::Deny(READ_ONLY_REASON.into()));
        assert_eq!(decide(&input("ask", "bash", command)), Decision::Ask(json!({ "reason": "command", "rule": "git status" })));
        assert_eq!(decide(&input("auto", "bash", command)), Decision::Allow);
        let fetch = Box::leak(Box::new(json!({ "url": "https://Example.com/a" })));
        assert_eq!(decide(&input("auto", "web_fetch", fetch)), Decision::Allow);
        assert_eq!(decide(&input("read-only", "web_fetch", fetch)), Decision::Deny(READ_ONLY_REASON.into()));
        assert_eq!(decide(&input("ask", "web_fetch", fetch)), Decision::Ask(json!({ "reason": "web", "path": "https://Example.com/a", "rule": "web_fetch:example.com" })));
        let plan = Box::leak(Box::new(json!({ "plan": [] })));
        assert_eq!(decide(&input("read-only", "update_plan", plan)), Decision::Allow);
        let inside = Box::leak(Box::new(json!({ "input": format!("*** Begin Patch\n*** Add File: {}\n+x\n*** End Patch", temp.join("a.txt").display()) })));
        assert_eq!(decide(&input("ask", "apply_patch", inside)), Decision::Allow);
        let outside = Box::leak(Box::new(json!({ "input": "*** Begin Patch\n*** Update File: /elsewhere/b.txt\n-x\n+y\n*** End Patch" })));
        assert!(matches!(decide(&input("ask", "apply_patch", outside)), Decision::Ask(approval) if approval["reason"] == "outside-write"));
    }
}
