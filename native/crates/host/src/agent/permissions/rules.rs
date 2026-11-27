//! Command allow-rule generation, allow matching and explicit deny matching.

use std::sync::LazyLock;

use regex::Regex;

use super::glob::wildcard_match;
use super::shell_parse::{command_segments, has_ps_encoded_command, program_name, split_shell_words, split_shell_words_for_deny, strip_leading_wrappers};

/// Characters that chain, redirect, substitute or group commands in Bash or PowerShell; such commands always ask.
static COMPOUND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[;&|<>`$(){}\r\n]").unwrap());
static INLINE_FLAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)^(?:-[a-z]*[cepr][a-z]*(?:[=:'"].*)?|--(?:eval|print|command|encodedcommand|require|import|loader)(?:=.*)?|/[ck])$"#).unwrap());
static SUBCOMMAND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z][\w:.-]*$").unwrap());
pub(super) static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\w+=").unwrap());

/// Programs whose first non-option argument names a subcommand, so their rule keeps both words (`git status`, `pip install`).
const SUBCOMMAND_PROGRAMS: &[&str] =
    &["git", "npm", "pnpm", "yarn", "npx", "pip", "pip3", "uv", "poetry", "cargo", "go", "dotnet", "docker", "kubectl", "apt", "apt-get", "brew", "conda", "gh", "winget", "choco", "scoop", "make"];
/// Programs that run their arguments as another command; rules never cover them.
pub(super) const WRAPPERS: &[&str] = &[
    "sudo", "doas", "su", "env", "xargs", "nohup", "timeout", "time", "nice", "watch", "exec", "eval", "command", "builtin", "source", ".", "wsl", "runas", "start", "start-process", "saps",
    "invoke-expression", "iex", "invoke-command", "icm", "invoke-item", "ii", "call",
    "setsid", "stdbuf", "ionice", "taskset", "busybox", "chroot", "unshare", "flock", "strace", "ltrace", "script",
];
/// Interpreters that run inline code after flags like `-c`; such a call always asks.
const INTERPRETERS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh", "fish", "pwsh", "powershell", "cmd", "python", "python3", "py", "node", "deno", "bun", "perl", "ruby", "php", "lua", "osascript", "java", "jshell"];
/// Python-family interpreters where `-m <module>` behaves as a controlled subcommand and must not degenerate to bare `python`.
const PYTHON_INTERPRETERS: &[&str] = &["python", "python3", "py"];
/// Harmless query-only flags that can form an explicit rule like `cargo --version` without degenerating to bare `cargo`.
const QUERY_FLAGS: &[&str] = &["-v", "-V", "--version", "-h", "--help"];

/// Leading options that can redirect tool execution, load untrusted manifests/Makefiles/configs, or execute
/// arbitrary code; never auto-allowed or remembered by allow rules.
fn is_dangerous_leading_option(program: &str, arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    let matches_opt = |opt: &str| lower == opt || lower.starts_with(&format!("{opt}="));
    match program {
        "git" => {
            arg == "-c"
                || arg.starts_with("-c")
                || matches_opt("--git-dir")
                || matches_opt("--work-tree")
                || matches_opt("--config-env")
                || matches_opt("--exec-path")
                || matches_opt("--upload-pack")
        }
        "cargo" => matches_opt("--config") || matches_opt("--manifest-path") || arg == "-Z" || arg.starts_with("-Z"),
        "npm" | "pnpm" | "yarn" => matches_opt("--prefix") || matches_opt("--userconfig") || matches_opt("--globalconfig"),
        "make" => {
            arg == "-f"
                || arg.starts_with("-f")
                || arg == "-C"
                || arg.starts_with("-C")
                || arg == "-I"
                || arg.starts_with("-I")
                || arg == "-E"
                || arg.starts_with("-E")
                || matches_opt("--file")
                || matches_opt("--makefile")
                || matches_opt("--directory")
                || matches_opt("--include-dir")
                || matches_opt("--eval")
        }
        "docker" => arg == "-H" || arg.starts_with("-H") || matches_opt("--host") || matches_opt("--config") || matches_opt("--context"),
        "kubectl" => matches_opt("--kubeconfig") || matches_opt("--context") || matches_opt("--server"),
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
        "npm" | "pnpm" | "yarn" => matches!(arg, "-C" | "--prefix" | "--dir" | "--cwd" | "--userconfig" | "--globalconfig"),
        "cargo" => matches!(arg, "--manifest-path" | "--target-dir" | "--config" | "-Z"),
        "pip" | "pip3" | "uv" | "poetry" => matches!(arg, "--directory" | "-C" | "--python" | "--project" | "--config-file"),
        "docker" | "kubectl" => matches!(arg, "-H" | "--host" | "--context" | "--config" | "-n" | "--namespace" | "--kubeconfig" | "--server"),
        "make" => matches!(arg, "-C" | "-f" | "-I" | "-E" | "--directory" | "--file" | "--makefile" | "--include-dir" | "--eval"),
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
    if INTERPRETERS.contains(&program.as_str())
        && (words[1..].iter().any(|word| INLINE_FLAG.is_match(word))
            || (matches!(program.as_str(), "pwsh" | "powershell") && has_ps_encoded_command(&words)))
    {
        return None;
    }
    Some(words)
}

/// Extract the controlled subcommand `(index, subcommand)` of `words`, skipping leading short/long options.
/// When `reject_dangerous` is true (allow-rule path), dangerous leading options return `Err(())`.
/// When `reject_dangerous` is false (deny-rule path), they are skipped so deny rules still catch the subcommand.
fn extract_subcommand<'a>(program: &str, words: &'a [String], reject_dangerous: bool) -> Result<Option<(usize, &'a str)>, ()> {
    let mut index = 1;
    while index < words.len() {
        let arg = words[index].as_str();
        if reject_dangerous && is_dangerous_leading_option(program, arg) {
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
/// Skips Python options that take a separate value (`-X`, `-W`, `--check-hash-based-pycs`) so their value is not
/// mistaken for a script path. Returns `Ok(Some(module))`, `Ok(None)`, or `Err(())` for malformed `-m`.
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
        if matches!(arg, "-X" | "-W" | "--check-hash-based-pycs") {
            index = (index + 2).min(words.len());
            continue;
        }
        // Handle bundled short flags such as `-um pytest` or `-umhttp.server` (skip `-X...`/`-W...` values).
        if let Some(short) = arg.strip_prefix('-').filter(|s| !s.starts_with('-'))
            && !short.starts_with('X')
            && !short.starts_with('W')
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

/// The rule "always allow" offers: `program`, `program subcommand` for tools like git/cargo/pip (skipping safe leading
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
        return match extract_subcommand(&program, &words, true) {
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

/// Canonical command tokens for matching: strips leading options for subcommand programs so
/// `"git status"` also matches `"git -C /tmp status"`. When `reject_dangerous` is true (allow path),
/// dangerous leading options return `None`.
fn effective_words(words: &[String], reject_dangerous: bool) -> Option<Vec<String>> {
    let program = program_name(&words[0]);
    if PYTHON_INTERPRETERS.contains(&program.as_str()) {
        if reject_dangerous && python_module(words).is_err() {
            return None;
        }
        return Some(words.to_vec());
    }
    if SUBCOMMAND_PROGRAMS.contains(&program.as_str()) {
        match extract_subcommand(&program, words, reject_dangerous) {
            Ok(Some((sub_idx, sub))) => {
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
    let Some(words) = effective_words(&raw_words, true) else { return false };
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

/// Precomputed normalized views of one command segment so `command_denied` parses each segment once across all rules.
struct SegmentViews {
    raw: String,
    views: Vec<Vec<String>>,
    joined: Vec<String>,
}

fn prepare_segment_views(seg: String, windows: bool) -> SegmentViews {
    let parsed = split_shell_words_for_deny(&seg, windows);
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
                if let Some(mut eff) = effective_words(slice, false) {
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
    let joined = views.iter().map(|v| v.join(" ")).collect();
    SegmentViews { raw: seg, views, joined }
}

/// Match one command word against a deny rule's word, also catching glob-obfuscated command names like `/bin/r?` -> `rm`.
fn deny_word_matches(rule_word: &str, cmd_word: &str, first: bool, windows: bool) -> bool {
    let rw = compare_word(rule_word, first, windows);
    let cw = compare_word(cmd_word, first, windows);
    if rw == cw {
        return true;
    }
    if first && !windows && (cw.contains('*') || cw.contains('?')) && cw.chars().any(|c| c != '*' && c != '?') {
        return wildcard_match(&cw, &rw, false);
    }
    false
}

/// Whether an explicit deny rule blocks `command`; checks both the full command and every chained/nested segment, with
/// quotes, POSIX/PowerShell/cmd escapes, control-flow prefixes, wrappers, and executable directory paths normalized.
pub fn command_denied(command: &str, rules: &[String], windows: bool) -> Option<String> {
    if rules.is_empty() {
        return None;
    }
    let segments: Vec<SegmentViews> = command_segments(command, windows).into_iter().map(|seg| prepare_segment_views(seg, windows)).collect();
    for rule in rules {
        let rule_trimmed = rule.trim();
        if rule_trimmed.is_empty() {
            continue;
        }
        let rule_words = split_shell_words(rule_trimmed, windows);
        let has_wild = rule_trimmed.contains('*') || rule_trimmed.contains('?');
        for seg in &segments {
            if has_wild {
                if wildcard_match(rule_trimmed, &seg.raw, windows)
                    || seg.joined.iter().any(|joined| wildcard_match(rule_trimmed, joined, windows))
                {
                    return Some(rule_trimmed.to_owned());
                }
                // Catch glob-obfuscated executable names (e.g. `/bin/r? -rf /` against `rm -rf *`).
                if !windows
                    && let Some(rwords) = &rule_words
                    && let Some(rfirst) = rwords.first()
                {
                    let matched = seg.views.iter().any(|swords| {
                        let Some(sfirst) = swords.first() else { return false };
                        if !(sfirst.contains('*') || sfirst.contains('?')) || !deny_word_matches(rfirst, sfirst, true, false) {
                            return false;
                        }
                        let mut expanded = swords.clone();
                        expanded[0] = rfirst.clone();
                        wildcard_match(rule_trimmed, &expanded.join(" "), false)
                    });
                    if matched {
                        return Some(rule_trimmed.to_owned());
                    }
                }
            } else if let Some(rwords) = &rule_words {
                let matched = seg.views.iter().any(|swords| {
                    rwords.len() <= swords.len()
                        && rwords.iter().enumerate().all(|(i, part)| deny_word_matches(part, &swords[i], i == 0, windows))
                });
                if matched {
                    return Some(rule_trimmed.to_owned());
                }
            } else if wildcard_match(&format!("{rule_trimmed}*"), &seg.raw, windows) {
                return Some(rule_trimmed.to_owned());
            }
        }
    }
    None
}
