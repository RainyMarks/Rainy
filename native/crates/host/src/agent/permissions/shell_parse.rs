//! Shell command tokenization, nested segment extraction, wrapper unwrapping and inline script decoding
//! across POSIX shells, PowerShell and cmd.exe.

use std::collections::HashSet;

use super::rules::{ASSIGNMENT, WRAPPERS};

const MAX_SEGMENT_DEPTH: usize = 16;
const MAX_SEGMENTS: usize = 256;

/// Shell control-flow keywords that can prefix a command inside a compound statement (e.g. `if true; then rm -rf /; fi`).
const SHELL_CONTROL_PREFIXES: &[&str] = &["if", "then", "elif", "else", "do", "while", "until", "!", "coproc", "{"];

/// Program name as rules compare it: no directory, no `.exe`, lowercase.
pub(super) fn program_name(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word).to_lowercase();
    name.strip_suffix(".exe").map(str::to_owned).unwrap_or(name)
}

/// Decode a single PowerShell backtick escape character (e.g. `` `m `` -> `'m'`, `` `n `` -> `'\n'`).
fn decode_ps_backtick(next: char) -> char {
    match next {
        '0' => '\0',
        'a' => '\x07',
        'b' => '\x08',
        'f' => '\x0c',
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'v' => '\x0b',
        other => other,
    }
}

/// Decode a POSIX ANSI-C `$'...'` quoted literal; returns `None` if unclosed.
fn read_ansi_c_quote(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    let mut out = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            return Some(out);
        }
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let Some(esc) = chars.next() else { return None };
        match esc {
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'e' | 'E' => out.push('\x1b'),
            'f' => out.push('\x0c'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\x0b'),
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'x' => {
                let mut hex = String::new();
                for _ in 0..2 {
                    if let Some(&d) = chars.peek()
                        && d.is_ascii_hexdigit()
                    {
                        hex.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
                if let Ok(val) = u8::from_str_radix(&hex, 16) {
                    out.push(val as char);
                }
            }
            'u' | 'U' => {
                let max = if esc == 'u' { 4 } else { 8 };
                let mut hex = String::new();
                for _ in 0..max {
                    if let Some(&d) = chars.peek()
                        && d.is_ascii_hexdigit()
                    {
                        hex.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
                if let Ok(val) = u32::from_str_radix(&hex, 16)
                    && let Some(c) = char::from_u32(val)
                {
                    out.push(c);
                }
            }
            d if ('0'..='7').contains(&d) => {
                let mut oct = String::from(d);
                for _ in 0..2 {
                    if let Some(&c) = chars.peek()
                        && ('0'..='7').contains(&c)
                    {
                        oct.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
                if let Ok(val) = u32::from_str_radix(&oct, 8)
                    && let Some(c) = char::from_u32(val)
                {
                    out.push(c);
                }
            }
            other => out.push(other),
        }
    }
    None
}

/// Split a shell command into words while stripping matching single and double quotes.
/// Respects explicit `windows` semantics (POSIX backslash + `$'...'` escapes when `!windows`; PowerShell backtick escapes
/// when `windows`, preserving path backslashes). Returns `None` on unclosed quotes so malformed commands always ask.
pub(super) fn split_shell_words(text: &str, windows: bool) -> Option<Vec<String>> {
    split_shell_words_inner(text, windows, false)
}

/// Variant used by `deny_commands` inspection: additionally treats POSIX unquoted `$IFS` / `${IFS}` as word separators.
pub(super) fn split_shell_words_for_deny(text: &str, windows: bool) -> Option<Vec<String>> {
    split_shell_words_inner(text, windows, !windows)
}

fn split_shell_words_inner(text: &str, windows: bool, split_ifs: bool) -> Option<Vec<String>> {
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
                } else if windows && ch == '`' {
                    if let Some(next) = chars.next() {
                        current.push(decode_ps_backtick(next));
                    }
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
                } else if !windows && ch == '$' && chars.peek() == Some(&'\'') {
                    chars.next();
                    let decoded = read_ansi_c_quote(&mut chars)?;
                    current.push_str(&decoded);
                    in_word = true;
                } else if split_ifs && ch == '$' {
                    let rest: String = chars.clone().take(5).collect();
                    if rest.starts_with("{IFS}") {
                        for _ in 0..5 {
                            chars.next();
                        }
                        if in_word {
                            words.push(std::mem::take(&mut current));
                            in_word = false;
                        }
                    } else if rest.starts_with("IFS")
                        && !rest.chars().nth(3).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        for _ in 0..3 {
                            chars.next();
                        }
                        if in_word {
                            words.push(std::mem::take(&mut current));
                            in_word = false;
                        }
                    } else {
                        in_word = true;
                        current.push(ch);
                    }
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    in_word = true;
                } else if windows && ch == '`' {
                    in_word = true;
                    if let Some(next) = chars.next() {
                        current.push(decode_ps_backtick(next));
                    }
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

/// Strip `cmd.exe` caret (`^`) escapes outside double quotes so `r^m -rf /` resolves to `rm -rf /`.
fn unescape_cmd_carets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_double = false;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            in_double = !in_double;
            out.push(ch);
        } else if !in_double && ch == '^' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Decode a PowerShell `-EncodedCommand` Base64 UTF-16LE payload (with UTF-8 fallback).
fn decode_ps_encoded_command(raw: &str) -> Option<String> {
    use base64::Engine as _;
    let cleaned: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() || cleaned.len() > 128 * 1024 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&cleaned))
        .ok()?;
    if bytes.len() >= 2 && bytes.len() % 2 == 0 {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        if let Ok(text) = String::from_utf16(&units)
            && !text.trim().is_empty()
        {
            return Some(text);
        }
    }
    String::from_utf8(bytes).ok().filter(|text| !text.trim().is_empty())
}

/// Whether a lowercased PowerShell flag (without `:value`) is a prefix abbreviation of `-command` or `-encodedcommand`.
/// PowerShell accepts unambiguous prefixes (`-c`..`-command`, `-e`/`-ec`/`-en`..`-encodedcommand`).
fn ps_flag_kind(flag_lower: &str) -> Option<&'static str> {
    let body = flag_lower.strip_prefix("--").or_else(|| flag_lower.strip_prefix('-')).or_else(|| flag_lower.strip_prefix('/'))?;
    if body.is_empty() {
        return None;
    }
    if body == "ec" || ("encodedcommand".starts_with(body) && (body == "e" || body.starts_with("en"))) {
        return Some("encoded");
    }
    if "command".starts_with(body) {
        return Some("command");
    }
    None
}

/// Whether `command` contains a PowerShell `-EncodedCommand` (including prefix shorthands `-e`/`-ec`/`-enc`).
pub(super) fn has_ps_encoded_command(words: &[String]) -> bool {
    for word in words.iter().skip(1) {
        let lower = word.to_ascii_lowercase();
        let flag = lower.split(':').next().unwrap_or(&lower);
        if ps_flag_kind(flag) == Some("encoded") {
            return true;
        }
    }
    false
}

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
                } else if windows && ch == '`' {
                    inner.push(ch);
                    if let Some(next) = chars.next() {
                        inner.push(next);
                    }
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
                if windows && ch == '`' {
                    inner.push(ch);
                    if let Some(next) = chars.next() {
                        inner.push(next);
                    }
                } else if ch == '\\' && !windows {
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

/// Extract inline shell scripts passed to `bash -c`, `sh -c`, `eval`, `pwsh -Command`/`-EncodedCommand`, `cmd /c`,
/// `su -c`, `env -S`, `watch`, or `find -exec` so nested commands cannot bypass deny rules.
fn extract_inline_scripts(segment: &str, windows: bool) -> Vec<String> {
    let Some(words) = split_shell_words_for_deny(segment, windows) else { return Vec::new() };
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
                    if matches!(arg, "-o" | "+o" | "-O" | "+O" | "--rcfile" | "--init-file" | "--command") {
                        if arg == "--command" {
                            if let Some(script) = view.get(i + 1) {
                                scripts.push(script.clone());
                            }
                            break;
                        }
                        i = (i + 2).min(view.len());
                        continue;
                    }
                    if let Some(script) = arg.strip_prefix("--command=") {
                        scripts.push(script.to_owned());
                        break;
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
                    let (flag, inline_val) = match lower.split_once(':') {
                        Some((f, v)) => {
                            let offset = view[i].len().saturating_sub(v.len());
                            let raw_val = if view[i].is_char_boundary(offset) { Some(&view[i][offset..]) } else { None };
                            (f, raw_val)
                        }
                        None => (lower.as_str(), None),
                    };
                    if let Some(kind) = ps_flag_kind(flag) {
                        match (kind, inline_val) {
                            ("command", Some(val)) => scripts.push(val.to_owned()),
                            ("command", None) if i + 1 < view.len() => scripts.push(view[i + 1..].join(" ")),
                            ("encoded", Some(val)) => {
                                if let Some(decoded) = decode_ps_encoded_command(val) {
                                    scripts.push(decoded);
                                }
                            }
                            ("encoded", None) => {
                                if let Some(encoded) = view.get(i + 1)
                                    && let Some(decoded) = decode_ps_encoded_command(encoded)
                                {
                                    scripts.push(decoded);
                                }
                            }
                            _ => {}
                        }
                        break;
                    }
                    if inline_val.is_none()
                        && matches!(
                            flag,
                            "-executionpolicy" | "-ep" | "-inputformat" | "-outputformat" | "-windowstyle" | "-workingdirectory" | "-wd" | "-configurationname" | "-settingsfile" | "-custompipename"
                        )
                    {
                        i = (i + 2).min(view.len());
                        continue;
                    }
                    if lower.starts_with('-') || lower.starts_with('/') {
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
                            let joined = view[i + 1..].join(" ");
                            let unescaped = unescape_cmd_carets(&joined);
                            if unescaped != joined {
                                scripts.push(unescaped);
                            }
                            scripts.push(joined);
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
            "find" => {
                let mut i = 1;
                while i < view.len() {
                    if matches!(view[i].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                        let start = i + 1;
                        let mut end = start;
                        while end < view.len() && !matches!(view[end].as_str(), ";" | "+") {
                            end += 1;
                        }
                        if start < end {
                            scripts.push(view[start..end].join(" "));
                        }
                        i = end + 1;
                    } else {
                        i += 1;
                    }
                }
            }
            _ => {}
        }
    }
    scripts
}

/// Recursively collect every executable command segment across separators (`&&`, `||`, `;`, `|`, newlines),
/// command substitutions (`$(...)`, `` `...` `` on POSIX), subshells (`(...)`), process substitutions (`<(...)`, `>(...)`),
/// brace groups (`{ ... }`), and inline shell invocations (`bash -c`, `eval`, `pwsh -enc`, `cmd /c`, `find -exec`, etc.).
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
                } else if windows && ch == '`' {
                    // PowerShell backtick inside double quotes escapes the next character (not command substitution).
                    current.push(ch);
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
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
                } else if !windows && ch == '`' {
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
                if windows && ch == '`' {
                    // PowerShell backtick escape: keep pair intact for `split_shell_words` to decode.
                    current.push(ch);
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else if ch == '\\' && !windows {
                    current.push(ch);
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else if !windows && ch == '$' && chars.peek() == Some(&'\'') {
                    current.push(ch);
                    current.push(chars.next().unwrap());
                    while let Some(c) = chars.next() {
                        current.push(c);
                        if c == '\\' {
                            if let Some(next) = chars.next() {
                                current.push(next);
                            }
                        } else if c == '\'' {
                            break;
                        }
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
                } else if !windows && ch == '`' {
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

pub(super) fn command_segments(command: &str, windows: bool) -> Vec<String> {
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
        "stdbuf" => matches!(flag, "-i" | "--input" | "-o" | "--output" | "-e" | "--error"),
        "ionice" => matches!(flag, "-c" | "--class" | "-n" | "--classdata" | "-p" | "-P" | "-u"),
        "taskset" => matches!(flag, "-c" | "--cpu-list" | "-p" | "--pid"),
        "chroot" => matches!(flag, "--userspec" | "--groups"),
        "unshare" => matches!(
            flag,
            "-S" | "--setuid" | "-G" | "--setgid" | "-R" | "--root" | "-w" | "--wd" | "--propagation" | "--setgroups" | "--map-user" | "--map-group" | "--map-users" | "--map-groups"
        ),
        "flock" => matches!(flag, "-w" | "--wait" | "--timeout" | "-E" | "--conflict-exit-code"),
        "strace" => matches!(flag, "-e" | "-o" | "-p" | "-s" | "-u" | "-E" | "-P"),
        "ltrace" => matches!(flag, "-e" | "-o" | "-p" | "-s" | "-u" | "-l" | "-x"),
        "script" => matches!(flag, "-c" | "--command" | "-t" | "--timing" | "-T" | "--log-timing" | "-I" | "-O" | "-B"),
        _ => false,
    }
}

/// Advance past options and mandatory positional arguments (e.g. `timeout`'s duration, `chroot`'s newroot) belonging to `wrapper`.
fn advance_wrapper_args(wrapper: &str, words: &[String], mut idx: usize) -> usize {
    let accepts_assignments = matches!(wrapper, "sudo" | "env");
    let mut taskset_has_cpu_list = false;
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
        if wrapper == "taskset" && (w == "-c" || w.starts_with("--cpu-list")) {
            taskset_has_cpu_list = true;
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
    // Wrappers taking a mandatory positional argument before the wrapped command.
    if matches!(wrapper, "timeout" | "chroot" | "flock") && idx < words.len() {
        idx += 1;
    } else if wrapper == "taskset" && !taskset_has_cpu_list && idx < words.len() {
        // `taskset MASK COMMAND` (without `-c`, the mask is positional).
        idx += 1;
    }
    idx
}

/// Unwrap leading shell control keywords (`if`, `then`, `!`, ...), env assignments (`FOO=1`), and command wrappers
/// (`sudo --user root`, `timeout 10`, `nice -n 10`, `setsid`, `stdbuf`, `ionice`, `taskset`, `busybox`, `chroot`,
/// `unshare`, `env -u X`, ...) so deny rules match the inner command.
pub(super) fn strip_leading_wrappers(words: &[String]) -> Vec<&[String]> {
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
