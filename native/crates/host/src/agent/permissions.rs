//! Which tool calls run, wait for the user, or are refused, by the chat's permission mode (3.x `permissions.ts`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::settings::PermissionRules;

/// Tools that only read and carry no arbitrary host file path; they run in every mode unless denied by explicit rule.
const SAFE_READ_TOOLS: &[&str] = &[
    "web_search", "command_output", "stop_command", "task", "browser_open", "browser_read", "browser_act", "browser_screenshot", "todo_write", "update_plan", "write_todos", "recall",
];
/// Read tools whose target path must stay inside project roots unless approved by the user.
const PATH_READ_TOOLS: &[&str] = &["read", "grep", "glob", "lsp_diagnostics", "symbol_outline", "module_deps"];
use super::changes::CHANGE_TOOLS;
const SHELL_TOOLS: &[&str] = &["bash", "pwsh"];

/// Characters that chain, redirect, substitute or group commands in Bash or PowerShell; such commands always ask.
static COMPOUND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[;&|<>`$(){}\r\n]").unwrap());
static INLINE_FLAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)^(?:-[a-z]*[cepr][a-z]*(?:[=:'"].*)?|--(?:eval|print|command|encodedcommand|require|import|loader)(?:=.*)?|/[ck])$"#).unwrap());
static SUBCOMMAND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z][\w:.-]*$").unwrap());
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\w+=").unwrap());

/// Programs whose first non-option argument names a subcommand, so their rule keeps both words (`git status`, `pip install`).
const SUBCOMMAND_PROGRAMS: &[&str] =
    &["git", "npm", "pnpm", "yarn", "npx", "pip", "pip3", "uv", "poetry", "cargo", "go", "dotnet", "docker", "kubectl", "apt", "apt-get", "brew", "conda", "gh", "winget", "choco", "scoop", "make"];
/// Programs that run their arguments as another command; rules never cover them.
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "su", "env", "xargs", "nohup", "timeout", "time", "nice", "watch", "exec", "eval", "command", "builtin", "source", ".", "wsl", "runas", "start", "start-process", "saps",
    "invoke-expression", "iex", "invoke-command", "icm", "invoke-item", "ii", "call",
];
/// Interpreters that run inline code after flags like `-c`; such a call always asks.
const INTERPRETERS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh", "fish", "pwsh", "powershell", "cmd", "python", "python3", "py", "node", "deno", "bun", "perl", "ruby", "php", "lua", "osascript", "java", "jshell"];
/// Python-family interpreters where `-m <module>` behaves as a controlled subcommand and must not degenerate to bare `python`.
const PYTHON_INTERPRETERS: &[&str] = &["python", "python3", "py"];
/// Harmless query-only flags that can form an explicit rule like `cargo --version` without degenerating to bare `cargo`.
const QUERY_FLAGS: &[&str] = &["-v", "-V", "--version", "-h", "--help"];

/// Model-facing reason for calls refused in read-only mode.
pub const READ_ONLY_REASON: &str = "This chat is in read-only mode: files cannot be changed, commands cannot run and tools that change state are unavailable. Describe the change or command you would make, or ask the user to switch the chat out of read-only mode.";

/// Program name as rules compare it: no directory, no `.exe`, lowercase.
fn program_name(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word).to_lowercase();
    name.strip_suffix(".exe").map(str::to_owned).unwrap_or(name)
}

/// Split a shell command into words while stripping matching single and double quotes.
/// Respects explicit `windows` semantics (POSIX backslash escapes when `!windows`; preserve path backslashes on Windows).
/// Returns `None` on unclosed quotes so malformed commands always ask.
fn split_shell_words(text: &str, windows: bool) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            Some('"') => {
                if ch == '"' {
                    quote = None;
                } else if ch == '\\'
                    && let Some(&next) = chars.peek()
                    && ((!windows && matches!(next, '"' | '\\' | '$' | '`')) || (windows && matches!(next, '"' | '\\')))
                {
                    if let Some(escaped) = chars.next() {
                        current.push(escaped);
                    }
                } else {
                    current.push(ch);
                }
            }
            None => {
                if ch.is_whitespace() {
                    if in_word {
                        words.push(std::mem::take(&mut current));
                        in_word = false;
                    }
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    in_word = true;
                } else if ch == '\\' && !windows {
                    in_word = true;
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else {
                    in_word = true;
                    current.push(ch);
                }
            }
            Some(_) => unreachable!(),
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_word {
        words.push(current);
    }
    (!words.is_empty()).then_some(words)
}

/// Leading options that can execute arbitrary code or redirect tool execution; never auto-allowed or remembered.
fn is_dangerous_leading_option(program: &str, arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    match program {
        "git" => {
            // `git -c key=val`, `git -ckey=val`, `--git-dir` (points to untrusted config/hooks), `--exec-path`, etc.
            arg == "-c"
                || arg.starts_with("-c")
                || lower == "--git-dir"
                || lower.starts_with("--git-dir=")
                || lower == "--config-env"
                || lower.starts_with("--config-env=")
                || lower == "--exec-path"
                || lower.starts_with("--exec-path=")
                || lower == "--upload-pack"
                || lower.starts_with("--upload-pack=")
        }
        "cargo" => lower == "--config" || lower.starts_with("--config="),
        _ => false,
    }
}

/// Leading options that consume the following word as their value (when not written as `--opt=val`).
fn leading_option_takes_value(program: &str, arg: &str) -> bool {
    if arg.contains('=') {
        return false;
    }
    match program {
        "git" => matches!(arg, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix" | "--config-env" | "--exec-path"),
        "npm" | "pnpm" | "yarn" => matches!(arg, "-C" | "--prefix" | "--dir" | "--cwd" | "--userconfig"),
        "cargo" => matches!(arg, "--manifest-path" | "--target-dir" | "--config" | "-Z"),
        "pip" | "pip3" | "uv" | "poetry" => matches!(arg, "--directory" | "-C" | "--python" | "--project" | "--config-file"),
        "docker" | "kubectl" => matches!(arg, "-H" | "--host" | "--context" | "--config" | "-n" | "--namespace" | "--kubeconfig"),
        "make" => matches!(arg, "-C" | "-f" | "--directory" | "--file"),
        "gh" => matches!(arg, "-R" | "--repo"),
        _ => false,
    }
}

/// Words of a command that neither chains, redirects nor substitutes, with quotes stripped.
fn simple_words(command: &str, windows: bool) -> Option<Vec<String>> {
    let text = command.trim();
    if text.is_empty() || COMPOUND.is_match(text) {
        return None;
    }
    let words = split_shell_words(text, windows)?;
    let program = program_name(&words[0]);
    if program.is_empty() || WRAPPERS.contains(&program.as_str()) || ASSIGNMENT.is_match(&words[0]) {
        return None;
    }
    if INTERPRETERS.contains(&program.as_str()) && words[1..].iter().any(|word| INLINE_FLAG.is_match(word)) {
        return None;
    }
    Some(words)
}

/// Extract the controlled subcommand `(index, subcommand)` of `words`, skipping leading short/long options.
/// Returns `Ok(Some((idx, sub)))` when found, `Ok(None)` when no subcommand exists, or `Err(())` on dangerous options.
fn extract_subcommand<'a>(program: &str, words: &'a [String]) -> Result<Option<(usize, &'a str)>, ()> {
    let mut index = 1;
    while index < words.len() {
        let arg = words[index].as_str();
        if is_dangerous_leading_option(program, arg) {
            return Err(());
        }
        if arg == "--" {
            return Ok(None);
        }
        if arg.starts_with('-') {
            if leading_option_takes_value(program, arg) {
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if program == "cargo" && arg.starts_with('+') {
            index += 1;
            continue;
        }
        return if SUBCOMMAND.is_match(arg) { Ok(Some((index, arg))) } else { Ok(None) };
    }
    Ok(None)
}

/// Extract Python `-m <module>` / `--module <module>` / bundled `-um <module>` when present.
/// Returns `Ok(Some(module))` for valid module invocation, `Ok(None)` when `-m` is not used, or `Err(())` for malformed `-m`.
fn python_module<'a>(words: &'a [String]) -> Result<Option<&'a str>, ()> {
    let mut index = 1;
    while index < words.len() {
        let arg = words[index].as_str();
        if arg == "--" {
            return Ok(None);
        }
        if arg == "-m" || arg == "--module" {
            let Some(module) = words.get(index + 1).map(String::as_str) else { return Err(()) };
            return if SUBCOMMAND.is_match(module) { Ok(Some(module)) } else { Err(()) };
        }
        if let Some(module) = arg.strip_prefix("--module=") {
            return if SUBCOMMAND.is_match(module) { Ok(Some(module)) } else { Err(()) };
        }
        // Handle bundled short flags such as `-um pytest` or `-umhttp.server`.
        if let Some(short) = arg.strip_prefix('-').filter(|s| !s.starts_with('-'))
            && let Some(pos) = short.find('m')
        {
            let rest = &short[pos + 1..];
            let module = if rest.is_empty() { words.get(index + 1).map(String::as_str) } else { Some(rest) };
            return match module {
                Some(module) if SUBCOMMAND.is_match(module) => Ok(Some(module)),
                _ => Err(()),
            };
        }
        if !arg.starts_with('-') {
            // Script path reached; any `-m` after the script belongs to the script itself.
            return Ok(None);
        }
        index += 1;
    }
    Ok(None)
}

/// The rule "always allow" offers: `program`, `program subcommand` for tools like git/cargo/pip (skipping leading
/// options so `git -C /tmp status` becomes `git status` and `cargo --version` never degenerates to bare `cargo`),
/// or `python3 -m <module>`; nothing when the command chains, redirects, substitutes, wraps another command or runs inline code.
#[cfg(test)]
pub fn command_rule(command: &str) -> Option<String> {
    command_rule_for(command, cfg!(windows))
}

/// Platform-aware variant of [`command_rule`].
pub fn command_rule_for(command: &str, windows: bool) -> Option<String> {
    let words = simple_words(command, windows)?;
    let program = program_name(&words[0]);
    if PYTHON_INTERPRETERS.contains(&program.as_str()) {
        return match python_module(&words) {
            Ok(Some(module)) => Some(format!("{} -m {module}", words[0])),
            Ok(None) => Some(words[0].clone()),
            Err(()) => None,
        };
    }
    if SUBCOMMAND_PROGRAMS.contains(&program.as_str()) {
        return match extract_subcommand(&program, &words) {
            Ok(Some((_, sub))) => Some(format!("{} {sub}", words[0])),
            Ok(None) => {
                // Never degenerate to bare `git` or `cargo`; only allow memorizing pure query invocations like `cargo --version`.
                if words.len() > 1 && words[1..].iter().all(|arg| QUERY_FLAGS.contains(&arg.as_str())) {
                    Some(words.join(" "))
                } else {
                    None
                }
            }
            Err(()) => None,
        };
    }
    Some(words[0].clone())
}

/// Wildcard matcher supporting `*` (any characters) and `?` (one character).
pub fn wildcard_match(pattern: &str, text: &str, case_insensitive: bool) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = if case_insensitive {
        (pattern.to_lowercase().chars().collect(), text.to_lowercase().chars().collect())
    } else {
        (pattern.chars().collect(), text.chars().collect())
    };
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_p, mut star_t) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_p = Some(pi);
            pi += 1;
            star_t = ti;
        } else if let Some(sp) = star_p {
            pi = sp + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Path glob matching supporting `*` (within segment), `?` (single non-`/` char), and `**` (across segments).
fn path_glob_match(pattern: &str, text: &str, case_insensitive: bool) -> bool {
    let normalize = |s: &str| {
        let s = s.replace('\\', "/");
        if case_insensitive { s.to_lowercase() } else { s }
    };
    let p: Vec<char> = normalize(pattern).chars().collect();
    let t: Vec<char> = normalize(text).chars().collect();
    fn dp(p: &[char], t: &[char], pi: usize, ti: usize, memo: &mut Vec<Vec<Option<bool>>>) -> bool {
        if let Some(cached) = memo[pi][ti] {
            return cached;
        }
        let ans = if pi == p.len() {
            ti == t.len()
        } else if pi + 1 < p.len() && p[pi] == '*' && p[pi + 1] == '*' {
            let mut next = pi + 2;
            while next < p.len() && p[next] == '*' {
                next += 1;
            }
            if next < p.len() && p[next] == '/' {
                // `**/` matches zero or more directories.
                dp(p, t, next + 1, ti, memo) || (ti < t.len() && dp(p, t, pi, ti + 1, memo))
            } else {
                dp(p, t, next, ti, memo) || (ti < t.len() && dp(p, t, pi, ti + 1, memo))
            }
        } else if p[pi] == '*' {
            dp(p, t, pi + 1, ti, memo) || (ti < t.len() && t[ti] != '/' && dp(p, t, pi, ti + 1, memo))
        } else if p[pi] == '?' {
            ti < t.len() && t[ti] != '/' && dp(p, t, pi + 1, ti + 1, memo)
        } else {
            ti < t.len() && p[pi] == t[ti] && dp(p, t, pi + 1, ti + 1, memo)
        };
        memo[pi][ti] = Some(ans);
        ans
    }
    let mut memo = vec![vec![None; t.len() + 1]; p.len() + 1];
    dp(&p, &t, 0, 0, &mut memo)
}

fn compare_word(value: &str, first: bool, windows: bool) -> String {
    if !windows {
        value.to_owned()
    } else if first {
        let lower = value.to_lowercase();
        lower.strip_suffix(".exe").map(str::to_owned).unwrap_or(lower)
    } else {
        value.to_lowercase()
    }
}

/// Canonical command tokens for prefix matching: strips safe leading options for subcommand programs so
/// `"git status"` also matches `"git -C /tmp status"`, while rejecting dangerous leading options like `git -c`.
fn effective_words(words: &[String]) -> Option<Vec<String>> {
    let program = program_name(&words[0]);
    if PYTHON_INTERPRETERS.contains(&program.as_str()) {
        if let Err(()) = python_module(words) {
            return None;
        }
        return Some(words.to_vec());
    }
    if SUBCOMMAND_PROGRAMS.contains(&program.as_str()) {
        match extract_subcommand(&program, words) {
            Ok(Some((sub_idx, sub))) => {
                // Keep `[program, subcommand, ...remaining args directly after sub_idx]` so an option value with the
                // same spelling as the subcommand (e.g. `git -C diff diff --stat`) never confuses the slice offset.
                let mut out = vec![words[0].clone(), sub.to_owned()];
                out.extend(words[sub_idx + 1..].iter().cloned());
                return Some(out);
            }
            Ok(None) => return Some(words.to_vec()),
            Err(()) => return None,
        }
    }
    Some(words.to_vec())
}

/// Whether remembered allow rules cover a command: it must be simple (no chaining/wrappers/inline code/dangerous flags)
/// and match either a wildcard rule (e.g. `git diff *`) or a word-prefix rule (`git status`).
pub fn command_allowed(command: &str, rules: &[String], windows: bool) -> bool {
    let Some(raw_words) = simple_words(command, windows) else { return false };
    let program = program_name(&raw_words[0]);
    let py_mod = if PYTHON_INTERPRETERS.contains(&program.as_str()) {
        match python_module(&raw_words) {
            Ok(module) => module,
            Err(()) => return false,
        }
    } else {
        None
    };
    let Some(words) = effective_words(&raw_words) else { return false };
    let joined_raw = raw_words.join(" ");
    let joined_eff = words.join(" ");
    rules.iter().any(|rule| {
        let rule = rule.trim();
        if rule.is_empty() {
            return false;
        }
        let Some(parts) = split_shell_words(rule, windows) else { return false };
        // A bare `python`/`python3` rule never covers `python -m <module>`; require an explicit `-m` or wildcard rule.
        if py_mod.is_some() && parts.len() == 1 && !parts[0].contains('*') && !parts[0].contains('?') {
            return false;
        }
        if rule.contains('*') || rule.contains('?') {
            let norm_first = |joined: &str| {
                if !windows {
                    return joined.to_owned();
                }
                let mut split = joined.splitn(2, ' ');
                let first = compare_word(split.next().unwrap_or_default(), true, true);
                match split.next() {
                    Some(rest) => format!("{first} {rest}"),
                    None => first,
                }
            };
            let norm_rule = if windows {
                let mut split = rule.splitn(2, ' ');
                let first = compare_word(split.next().unwrap_or_default(), true, true);
                match split.next() {
                    Some(rest) => format!("{first} {rest}"),
                    None => first,
                }
            } else {
                rule.to_owned()
            };
            return wildcard_match(&norm_rule, &norm_first(&joined_raw), windows) || wildcard_match(&norm_rule, &norm_first(&joined_eff), windows);
        }
        // Prefix word match against both raw words and option-stripped effective words.
        let matches_words = |candidate: &[String]| {
            parts.len() <= candidate.len()
                && parts.iter().enumerate().all(|(index, part)| compare_word(part, index == 0, windows) == compare_word(&candidate[index], index == 0, windows))
        };
        if let Some(module) = py_mod {
            // Normalize `-mfoo` or `--module=foo` to `program -m foo` when comparing against `-m` rules.
            let normalized = vec![raw_words[0].clone(), "-m".into(), module.to_owned()];
            return matches_words(&raw_words) || matches_words(&normalized);
        }
        matches_words(&raw_words) || matches_words(&words)
    })
}

const MAX_SEGMENT_DEPTH: usize = 16;
const MAX_SEGMENTS: usize = 256;

/// Shell control-flow keywords that can prefix a command inside a compound statement (e.g. `if true; then rm -rf /; fi`).
const SHELL_CONTROL_PREFIXES: &[&str] = &["if", "then", "elif", "else", "do", "while", "until", "!", "coproc", "{"];

/// Read until a matching closing delimiter (`)` or `}`), respecting quotes, escapes, and nested delimiters.
fn read_balanced_delim(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    open: char,
    close: char,
    windows: bool,
    depth: usize,
) -> (String, bool) {
    let mut inner = String::new();
    let mut nesting = 1usize;
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                }
                inner.push(ch);
            }
            Some('"') => {
                if ch == '"' {
                    quote = None;
                    inner.push(ch);
                } else if ch == '\\'
                    && let Some(&next) = chars.peek()
                    && ((!windows && matches!(next, '"' | '\\' | '$' | '`')) || (windows && matches!(next, '"' | '\\')))
                {
                    inner.push(ch);
                    if let Some(escaped) = chars.next() {
                        inner.push(escaped);
                    }
                } else if ch == '$' && chars.peek() == Some(&'(') && depth < MAX_SEGMENT_DEPTH {
                    chars.next();
                    let (sub, closed) = read_balanced_delim(chars, '(', ')', windows, depth + 1);
                    inner.push_str("$(");
                    inner.push_str(&sub);
                    if closed {
                        inner.push(')');
                    }
                } else {
                    inner.push(ch);
                }
            }
            None => {
                if ch == '\\' && !windows {
                    inner.push(ch);
                    if let Some(next) = chars.next() {
                        inner.push(next);
                    }
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    inner.push(ch);
                } else if ch == open {
                    nesting += 1;
                    inner.push(ch);
                } else if ch == close {
                    nesting -= 1;
                    if nesting == 0 {
                        return (inner, true);
                    }
                    inner.push(ch);
                } else {
                    inner.push(ch);
                }
            }
            Some(_) => unreachable!(),
        }
    }
    (inner, false)
}

/// Read until a matching closing backtick, returning `(raw_text, unescaped_command, closed)`.
fn read_backtick(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    in_double_quote: bool,
    windows: bool,
) -> (String, String, bool) {
    let mut raw = String::new();
    let mut unescaped = String::new();
    while let Some(ch) = chars.next() {
        if ch == '`' {
            return (raw, unescaped, true);
        }
        if ch == '\\'
            && !windows
            && let Some(&next) = chars.peek()
            && (matches!(next, '`' | '\\' | '$') || (in_double_quote && next == '"'))
        {
            if let Some(escaped) = chars.next() {
                raw.push(ch);
                raw.push(escaped);
                unescaped.push(escaped);
            }
            continue;
        }
        raw.push(ch);
        unescaped.push(ch);
    }
    (raw, unescaped, false)
}

/// Extract inline shell scripts passed to `bash -c`, `sh -c`, `eval`, `pwsh -Command`, `cmd /c`, `su -c`, `env -S`, or
/// single-argument `watch` invocations so nested commands cannot bypass deny rules.
fn extract_inline_scripts(segment: &str, windows: bool) -> Vec<String> {
    let Some(words) = split_shell_words(segment, windows) else { return Vec::new() };
    let mut scripts = Vec::new();
    for view in strip_leading_wrappers(&words) {
        let Some(first) = view.first() else { continue };
        let prog = program_name(first);
        match prog.as_str() {
            "eval" => {
                let mut i = 1;
                if view.get(i).map(String::as_str) == Some("--") {
                    i += 1;
                }
                if i < view.len() {
                    scripts.push(view[i..].join(" "));
                }
            }
            "bash" | "sh" | "zsh" | "dash" | "ksh" | "fish" => {
                let mut i = 1;
                while i < view.len() {
                    let arg = view[i].as_str();
                    if arg == "--" {
                        break;
                    }
                    if arg == "-o" || arg == "+o" || arg == "-O" || arg == "+O" {
                        i = (i + 2).min(view.len());
                        continue;
                    }
                    if arg == "-c" || (arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains('c')) {
                        let next_idx = if view.get(i + 1).map(String::as_str) == Some("--") { i + 2 } else { i + 1 };
                        if let Some(script) = view.get(next_idx) {
                            scripts.push(script.clone());
                        }
                        break;
                    }
                    if arg.starts_with('-') || arg.starts_with('+') {
                        i += 1;
                        continue;
                    }
                    break;
                }
            }
            "pwsh" | "powershell" => {
                let mut i = 1;
                while i < view.len() {
                    let lower = view[i].to_ascii_lowercase();
                    if matches!(lower.as_str(), "-c" | "-command" | "--command") {
                        if i + 1 < view.len() {
                            scripts.push(view[i + 1..].join(" "));
                        }
                        break;
                    }
                    if let Some(rest) = lower.strip_prefix("-command:").or_else(|| lower.strip_prefix("-c:")) {
                        let offset = view[i].len().saturating_sub(rest.len());
                        if view[i].is_char_boundary(offset) {
                            scripts.push(view[i][offset..].to_owned());
                        }
                        break;
                    }
                    if matches!(
                        lower.as_str(),
                        "-executionpolicy" | "-ep" | "-inputformat" | "-outputformat" | "-windowstyle" | "-workingdirectory" | "-wd" | "-configurationname" | "-settingsfile"
                    ) {
                        i = (i + 2).min(view.len());
                        continue;
                    }
                    if lower.starts_with('-') {
                        i += 1;
                        continue;
                    }
                    break;
                }
            }
            "cmd" => {
                let mut i = 1;
                while i < view.len() {
                    let lower = view[i].to_ascii_lowercase();
                    if matches!(lower.as_str(), "/c" | "/k") {
                        if i + 1 < view.len() {
                            scripts.push(view[i + 1..].join(" "));
                        }
                        break;
                    }
                    i += 1;
                }
            }
            "su" => {
                let mut i = 1;
                while i < view.len() {
                    let arg = view[i].as_str();
                    if matches!(arg, "-c" | "--command") {
                        if let Some(script) = view.get(i + 1) {
                            scripts.push(script.clone());
                        }
                        break;
                    }
                    if let Some(script) = arg.strip_prefix("--command=") {
                        scripts.push(script.to_owned());
                        break;
                    }
                    i += 1;
                }
            }
            "env" => {
                let mut i = 1;
                while i < view.len() {
                    let arg = view[i].as_str();
                    if arg == "--" {
                        break;
                    }
                    if matches!(arg, "-S" | "--split-string") {
                        if let Some(script) = view.get(i + 1) {
                            scripts.push(script.clone());
                        }
                        break;
                    }
                    if let Some(script) = arg.strip_prefix("--split-string=") {
                        scripts.push(script.to_owned());
                        break;
                    }
                    if matches!(arg, "-u" | "--unset" | "-C" | "--chdir" | "-P" | "-a" | "--argv0") {
                        i = (i + 2).min(view.len());
                        continue;
                    }
                    if arg.starts_with('-') || ASSIGNMENT.is_match(arg) {
                        i += 1;
                        continue;
                    }
                    break;
                }
            }
            "watch" => {
                let next = advance_wrapper_args("watch", view, 1);
                if next + 1 == view.len() && view[next].chars().any(char::is_whitespace) {
                    scripts.push(view[next].clone());
                }
            }
            _ => {}
        }
    }
    scripts
}

/// Recursively collect every executable command segment across separators (`&&`, `||`, `;`, `|`, newlines),
/// command substitutions (`$(...)`, `` `...` `` including inside double quotes), subshells (`(...)`), process
/// substitutions (`<(...)`, `>(...)`), brace groups (`{ ... }`), and inline shell invocations (`bash -c`, `eval`, etc.).
fn collect_segments(command: &str, windows: bool, depth: usize, out: &mut Vec<String>, seen: &mut HashSet<String>) {
    if depth > MAX_SEGMENT_DEPTH || out.len() >= MAX_SEGMENTS {
        return;
    }
    let mut local_segments = Vec::new();
    let mut nested = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                }
                current.push(ch);
            }
            Some('"') => {
                if ch == '"' {
                    quote = None;
                    current.push(ch);
                } else if ch == '\\'
                    && let Some(&next) = chars.peek()
                    && ((!windows && matches!(next, '"' | '\\' | '$' | '`')) || (windows && matches!(next, '"' | '\\')))
                {
                    current.push(ch);
                    if let Some(escaped) = chars.next() {
                        current.push(escaped);
                    }
                } else if ch == '$' && chars.peek() == Some(&'(') {
                    chars.next();
                    let (inner, closed) = read_balanced_delim(&mut chars, '(', ')', windows, depth + 1);
                    current.push_str("$(");
                    current.push_str(&inner);
                    if closed {
                        current.push(')');
                    }
                    if !inner.trim().is_empty() {
                        nested.push(inner);
                    }
                } else if ch == '`' {
                    let (raw, inner, closed) = read_backtick(&mut chars, true, windows);
                    current.push('`');
                    current.push_str(&raw);
                    if closed {
                        current.push('`');
                    }
                    if !inner.trim().is_empty() {
                        nested.push(inner);
                    }
                } else {
                    current.push(ch);
                }
            }
            None => {
                if ch == '\\' && !windows {
                    current.push(ch);
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    current.push(ch);
                } else if ch == '$' && chars.peek() == Some(&'(') {
                    chars.next();
                    let (inner, closed) = read_balanced_delim(&mut chars, '(', ')', windows, depth + 1);
                    current.push_str("$(");
                    current.push_str(&inner);
                    if closed {
                        current.push(')');
                    }
                    if !inner.trim().is_empty() {
                        nested.push(inner);
                    }
                } else if ch == '`' {
                    let (raw, inner, closed) = read_backtick(&mut chars, false, windows);
                    current.push('`');
                    current.push_str(&raw);
                    if closed {
                        current.push('`');
                    }
                    if !inner.trim().is_empty() {
                        nested.push(inner);
                    }
                } else if ch == '(' {
                    let (inner, closed) = read_balanced_delim(&mut chars, '(', ')', windows, depth + 1);
                    current.push('(');
                    current.push_str(&inner);
                    if closed {
                        current.push(')');
                    }
                    if !inner.trim().is_empty() {
                        nested.push(inner);
                    }
                } else if ch == '{' {
                    let (inner, closed) = read_balanced_delim(&mut chars, '{', '}', windows, depth + 1);
                    current.push('{');
                    current.push_str(&inner);
                    if closed {
                        current.push('}');
                    }
                    if !inner.trim().is_empty() {
                        nested.push(inner);
                    }
                } else if matches!(ch, ';' | '|' | '&' | '\n' | '\r') {
                    if matches!(ch, '|' | '&') && chars.peek() == Some(&ch) {
                        chars.next();
                    }
                    let trimmed = current.trim();
                    if !trimmed.is_empty() {
                        local_segments.push(trimmed.to_owned());
                    }
                    current.clear();
                } else {
                    current.push(ch);
                }
            }
            Some(_) => unreachable!(),
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        local_segments.push(trimmed.to_owned());
    }

    for seg in local_segments {
        if out.len() >= MAX_SEGMENTS {
            return;
        }
        if seen.insert(seg.clone()) {
            let inline = extract_inline_scripts(&seg, windows);
            out.push(seg);
            for script in inline {
                collect_segments(&script, windows, depth + 1, out, seen);
            }
        }
    }
    for sub in nested {
        if out.len() >= MAX_SEGMENTS {
            return;
        }
        collect_segments(&sub, windows, depth + 1, out, seen);
    }
}

fn command_segments(command: &str, windows: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    collect_segments(command, windows, 0, &mut out, &mut seen);
    out
}

/// Wrapper-specific flag that consumes the following token as its value (when not written as `--opt=val`).
fn wrapper_option_takes_value(wrapper: &str, flag: &str) -> bool {
    if flag.contains('=') {
        return false;
    }
    match wrapper {
        "sudo" => matches!(
            flag,
            "-u" | "--user"
                | "-g"
                | "--group"
                | "-h"
                | "--host"
                | "-p"
                | "--prompt"
                | "-r"
                | "--role"
                | "-t"
                | "--type"
                | "-C"
                | "--close-from"
                | "-U"
                | "--other-user"
                | "-T"
                | "--command-timeout"
                | "-R"
                | "--chroot"
                | "-D"
                | "--chdir"
                | "-a"
                | "--auth-type"
                | "-c"
                | "--login-class"
        ),
        "doas" => matches!(flag, "-u" | "-C"),
        "env" => matches!(flag, "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string" | "-P" | "-a" | "--argv0"),
        "timeout" => matches!(flag, "-k" | "--kill-after" | "-s" | "--signal"),
        "nice" => matches!(flag, "-n" | "--adjustment"),
        "watch" => matches!(flag, "-n" | "--interval" | "-q" | "--equexit"),
        "xargs" => matches!(
            flag,
            "-n" | "--max-args" | "-P" | "--max-procs" | "-I" | "--replace" | "-d" | "--delimiter" | "-L" | "--max-lines" | "-s" | "--max-chars" | "-a" | "--arg-file" | "-E" | "--eof" | "-R" | "-S" | "-J"
        ),
        "time" => matches!(flag, "-f" | "--format" | "-o" | "--output"),
        "su" => matches!(flag, "-c" | "--command" | "-s" | "--shell" | "-g" | "--group" | "-G" | "--supp-group" | "-w" | "--whitelist-environment"),
        "wsl" => matches!(flag, "-d" | "--distribution" | "-u" | "--user" | "--cd" | "--shell-type"),
        "exec" => flag == "-a",
        _ => false,
    }
}

/// Advance past options and mandatory positional arguments (e.g. `timeout`'s duration) belonging to `wrapper`.
fn advance_wrapper_args(wrapper: &str, words: &[String], mut idx: usize) -> usize {
    let accepts_assignments = matches!(wrapper, "sudo" | "env");
    while idx < words.len() {
        let w = words[idx].as_str();
        if accepts_assignments && ASSIGNMENT.is_match(w) {
            idx += 1;
            continue;
        }
        if w == "--" || (wrapper == "wsl" && matches!(w, "-e" | "--exec")) {
            idx += 1;
            break;
        }
        if wrapper == "nice" && w.starts_with('+') && w.len() > 1 && w[1..].chars().all(|c| c.is_ascii_digit()) {
            idx += 1;
            continue;
        }
        if w.starts_with('-') && w != "-" {
            if wrapper_option_takes_value(wrapper, w) {
                idx = (idx + 2).min(words.len());
            } else {
                idx += 1;
            }
            continue;
        }
        if wrapper == "env" && w == "-" {
            idx += 1;
            continue;
        }
        break;
    }
    // `timeout [OPTION] DURATION COMMAND [ARG]...` always takes a positional duration before the wrapped command.
    if wrapper == "timeout" && idx < words.len() {
        idx += 1;
    }
    idx
}

/// Unwrap leading shell control keywords (`if`, `then`, `!`, ...), env assignments (`FOO=1`), and command wrappers
/// (`sudo --user root`, `timeout 10`, `nice -n 10`, `watch -n 1`, `env -u X`, ...) so deny rules match the inner command.
fn strip_leading_wrappers(words: &[String]) -> Vec<&[String]> {
    let mut views = vec![words];
    let mut idx = 0;
    while idx < words.len() {
        let current = words[idx].as_str();
        if ASSIGNMENT.is_match(current) || SHELL_CONTROL_PREFIXES.contains(&current) {
            idx += 1;
            if idx < words.len() {
                views.push(&words[idx..]);
            }
            continue;
        }
        let prog = program_name(current);
        if !WRAPPERS.contains(&prog.as_str()) {
            break;
        }
        let next = advance_wrapper_args(&prog, words, idx + 1);
        if next <= idx {
            break;
        }
        idx = next;
        if idx < words.len() {
            views.push(&words[idx..]);
        }
    }
    views
}

/// Whether an explicit deny rule blocks `command`; checks both the full command and every chained/nested segment, with
/// quotes, POSIX backslash escapes, control-flow prefixes, wrappers, and executable directory paths normalized.
pub fn command_denied(command: &str, rules: &[String], windows: bool) -> Option<String> {
    if rules.is_empty() {
        return None;
    }
    let segments = command_segments(command, windows);
    for rule in rules {
        let rule_trimmed = rule.trim();
        if rule_trimmed.is_empty() {
            continue;
        }
        let rule_words = split_shell_words(rule_trimmed, windows);
        let has_wild = rule_trimmed.contains('*') || rule_trimmed.contains('?');
        for seg in &segments {
            let parsed = split_shell_words(seg, windows);
            let views: Vec<Vec<String>> = match &parsed {
                Some(words) => strip_leading_wrappers(words)
                    .into_iter()
                    .flat_map(|slice| {
                        let mut variants = vec![slice.to_vec()];
                        if let Some(first) = slice.first() {
                            let bare = program_name(first);
                            if !bare.is_empty() && bare != *first {
                                let mut normalized = slice.to_vec();
                                normalized[0] = bare;
                                variants.push(normalized);
                            }
                        }
                        if let Some(mut eff) = effective_words(slice) {
                            if !variants.contains(&eff) {
                                variants.push(eff.clone());
                            }
                            if let Some(first) = eff.first() {
                                let bare = program_name(first);
                                if !bare.is_empty() && bare != *first {
                                    eff[0] = bare;
                                    if !variants.contains(&eff) {
                                        variants.push(eff);
                                    }
                                }
                            }
                        }
                        variants
                    })
                    .collect(),
                None => Vec::new(),
            };
            if has_wild {
                if wildcard_match(rule_trimmed, seg, windows)
                    || views.iter().any(|v| wildcard_match(rule_trimmed, &v.join(" "), windows))
                {
                    return Some(rule_trimmed.to_owned());
                }
            } else if let Some(rwords) = &rule_words {
                let matched = views.iter().any(|swords| {
                    rwords.len() <= swords.len()
                        && rwords.iter().enumerate().all(|(i, part)| compare_word(part, i == 0, windows) == compare_word(&swords[i], i == 0, windows))
                });
                if matched {
                    return Some(rule_trimmed.to_owned());
                }
            } else if wildcard_match(&format!("{rule_trimmed}*"), seg, windows) {
                return Some(rule_trimmed.to_owned());
            }
        }
    }
    None
}

/// Expand leading `~` or `~/` in a path rule to both raw and canonical home directory forms.
fn expand_tilde_variants(pattern: &str) -> Vec<String> {
    let raw_home = crate::env::home_dir();
    let canon_home = canonical_path(&raw_home);
    let make = |home: &Path, rest: Option<&str>| -> String {
        let base = home.to_string_lossy().replace('\\', "/");
        match rest {
            Some(rest) => format!("{}/{}", base.trim_end_matches('/'), rest.replace('\\', "/")),
            None => base,
        }
    };
    if pattern == "~" {
        let a = make(&raw_home, None);
        let b = make(&canon_home, None);
        return if a == b { vec![a] } else { vec![a, b] };
    }
    if let Some(rest) = pattern.strip_prefix("~/").or_else(|| pattern.strip_prefix("~\\")) {
        let a = make(&raw_home, Some(rest));
        let b = make(&canon_home, Some(rest));
        return if a == b { vec![a] } else { vec![a, b] };
    }
    vec![pattern.replace('\\', "/")]
}

/// Whether `target` (normalized or canonical) matches any explicit deny path rule (e.g. `.env`, `.env.*`, `~/.ssh/**`).
pub fn path_denied(target: &Path, cwd: &Path, rules: &[String], windows: bool) -> Option<String> {
    if rules.is_empty() {
        return None;
    }
    let normalized = crate::paths::normalize(target);
    let canonical = canonical_path(&normalized);
    let candidates: Vec<String> = {
        let mut list = vec![
            normalized.to_string_lossy().replace('\\', "/"),
            canonical.to_string_lossy().replace('\\', "/"),
        ];
        if let Some(rel) = crate::paths::relative_to(cwd, &normalized) {
            list.push(rel);
        }
        if let Some(rel) = crate::paths::relative_to(&canonical_path(cwd), &canonical) {
            list.push(rel);
        }
        list
    };
    let file_name = canonical.file_name().or_else(|| normalized.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for rule in rules {
        let trimmed = rule.trim();
        if trimmed.is_empty() {
            continue;
        }
        for expanded in expand_tilde_variants(trimmed) {
            // Bare name / pattern without slash (e.g. `.env` or `.env.*`) matches any path whose basename matches or relative path matches.
            if !expanded.contains('/') {
                if !file_name.is_empty() && path_glob_match(&expanded, &file_name, windows) {
                    return Some(trimmed.to_owned());
                }
                let any_segment = format!("**/{expanded}");
                if candidates.iter().any(|c| path_glob_match(&expanded, c, windows) || path_glob_match(&any_segment, c, windows)) {
                    return Some(trimmed.to_owned());
                }
                continue;
            }
            // Check direct match, plus directory-prefix match when pattern ends with `/**` (so `~/.ssh/**` also blocks opening `~/.ssh` itself).
            let dir_self = expanded.strip_suffix("/**");
            if candidates.iter().any(|c| {
                path_glob_match(&expanded, c, windows)
                    || dir_self.is_some_and(|base| path_glob_match(base, c, windows))
                    || (!expanded.starts_with('/') && !expanded.contains(':') && path_glob_match(&format!("**/{expanded}"), c, windows))
            }) {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

/// Whether a tool name matches any explicit `deny_tools` rule (exact or wildcard).
pub fn tool_denied(tool: &str, rules: &[String]) -> Option<String> {
    rules.iter().map(String::as_str).map(str::trim).find(|rule| !rule.is_empty() && wildcard_match(rule, tool, true)).map(str::to_owned)
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
        return string_arg(args, "path")
            .or_else(|| string_arg(args, "file_path"))
            .filter(|path| !path.trim().is_empty())
            .map(|path| crate::paths::normalize(&cwd.join(path)))
            .into_iter()
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

    #[test]
    fn offers_and_matches_rules() {
        assert_eq!(command_rule("git status --short").as_deref(), Some("git status"));
        assert_eq!(command_rule("git -C /tmp status").as_deref(), Some("git status"));
        assert_eq!(command_rule("git --no-pager diff HEAD").as_deref(), Some("git diff"));
        assert_eq!(command_rule("git -c core.sshCommand=evil status"), None);
        assert_eq!(command_rule("git -ccore.sshCommand=evil status"), None);
        assert_eq!(command_rule("git --git-dir=/tmp/evil status"), None);
        assert_eq!(command_rule("cargo --version").as_deref(), Some("cargo --version"));
        assert_ne!(command_rule("cargo --version").as_deref(), Some("cargo"));
        assert_eq!(command_rule("cargo +nightly test").as_deref(), Some("cargo test"));
        assert_eq!(command_rule("ls -la").as_deref(), Some("ls"));
        assert_eq!(command_rule("ls | wc"), None);
        assert_eq!(command_rule("sudo rm x"), None);
        assert_eq!(command_rule("\"sudo\" rm x"), None);
        assert_eq!(command_rule("'env' bash -c id"), None);
        assert_eq!(command_rule("python -c 'print(1)'"), None);
        assert_eq!(command_rule("python3 -m pytest tests/").as_deref(), Some("python3 -m pytest"));
        assert_eq!(command_rule("python3 -um http.server").as_deref(), Some("python3 -m http.server"));
        assert_eq!(command_rule("python3 -m"), None);

        assert!(command_allowed("git status -s", &["git status".into()], false));
        assert!(command_allowed("git -C /tmp status", &["git status".into()], false));
        assert!(!command_allowed("git -c core.sshCommand=evil status", &["git status".into()], false));
        assert!(!command_allowed("git -ccore.sshCommand=evil status", &["git status".into()], false));
        assert!(!command_allowed("git --git-dir=/tmp/evil status", &["git status".into()], false));
        assert!(!command_allowed("git push", &["git status".into()], false));
        assert!(command_allowed("Git.exe Status", &["git status".into()], true));
        assert!(command_allowed("git diff src/main.rs", &["git diff *".into()], false));
        assert!(!command_allowed("git push origin main", &["git diff *".into()], false));
        assert!(!command_allowed("python3 -m http.server", &["python3".into()], false));
        assert!(!command_allowed("python3 -um http.server", &["python3".into()], false));
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
            // Inline shell invocations.
            "bash -c \"rm -rf /\"",
            "sh -lc 'echo ok && rm -rf /'",
            "sudo bash -c \"rm -rf /\"",
            "eval \"if true; then rm -rf /; fi\"",
            "pwsh -NoProfile -Command \"rm -rf /\"",
            // POSIX backslash escapes (must work when `windows = false` even if tests run on Windows).
            "\\rm -rf /",
            "r\\m -rf /",
            "/bin/\\rm -rf /",
            // Subcommand leading option normalization in deny rules.
            "/usr/bin/git -C /tmp push --force",
        ] {
            assert!(
                command_denied(cmd, &wild_rules, false).is_some(),
                "expected wildcard deny rule to block: {cmd}"
            );
        }

        // Prefix deny rules catch bare wrapper invocations without trailing args too.
        for cmd in ["timeout 10 rm -rf", "nice -n 10 rm -rf", "echo / | xargs -n 1 rm -rf", "\\rm -rf"] {
            assert!(
                command_denied(cmd, &prefix_rules, false).is_some(),
                "expected prefix deny rule to block: {cmd}"
            );
        }

        // Single-quoted literal text passed to `echo` (not an inline shell executor) must not false-positive.
        assert!(command_denied("echo 'rm -rf /'", &wild_rules, false).is_none());
        assert!(command_denied("echo \"\\$(rm -rf /)\"", &wild_rules, false).is_none());
        // Windows paths keep backslashes intact when `windows = true`.
        assert!(command_denied(r"C:\tools\rm.exe -rf /tmp", &wild_rules, true).is_some());
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
