//! Wildcard and path glob matching, tilde expansion and canonical path resolution.

use std::path::{Path, PathBuf};

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum GlobToken {
    /// `**/` at start or right after `/`: matches zero directory segments or any prefix ending with `/`.
    DoubleStarDir,
    /// `**`: matches any sequence of characters, including `/`.
    DoubleStar,
    /// `*`: matches any sequence of characters except `/`.
    Star,
    /// `?`: matches a single character other than `/`.
    Any,
    Lit(char),
}

fn tokenize_path_glob(pattern: &[char]) -> Vec<GlobToken> {
    let mut tokens = Vec::with_capacity(pattern.len());
    let mut i = 0;
    while i < pattern.len() {
        if pattern[i] == '*' {
            let start = i;
            while i < pattern.len() && pattern[i] == '*' {
                i += 1;
            }
            if i - start >= 2 {
                let at_dir_boundary = start == 0 || pattern[start - 1] == '/';
                if at_dir_boundary && i < pattern.len() && pattern[i] == '/' {
                    tokens.push(GlobToken::DoubleStarDir);
                    i += 1;
                } else {
                    tokens.push(GlobToken::DoubleStar);
                }
            } else {
                tokens.push(GlobToken::Star);
            }
        } else if pattern[i] == '?' {
            tokens.push(GlobToken::Any);
            i += 1;
        } else {
            tokens.push(GlobToken::Lit(pattern[i]));
            i += 1;
        }
    }
    tokens
}

/// Path glob matching supporting `*` (within segment), `?` (single non-`/` char), and `**` / `**/` (across segments).
/// Iterative two-row DP with O(text.len()) space and zero recursion stack.
pub(crate) fn path_glob_match(pattern: &str, text: &str, case_insensitive: bool) -> bool {
    let normalize = |s: &str| {
        let s = s.replace('\\', "/");
        if case_insensitive { s.to_lowercase() } else { s }
    };
    let p_chars: Vec<char> = normalize(pattern).chars().collect();
    let t: Vec<char> = normalize(text).chars().collect();
    let tokens = tokenize_path_glob(&p_chars);
    let n = t.len();
    let mut prev = vec![false; n + 1];
    let mut curr = vec![false; n + 1];
    prev[0] = true;

    for token in tokens {
        curr.fill(false);
        match token {
            GlobToken::DoubleStarDir => {
                // Zero directories (`prev[ti]`) or any prefix ending right after `/` (`seen && t[ti - 1] == '/'`).
                let mut seen = false;
                for ti in 0..=n {
                    if prev[ti] || (seen && ti > 0 && t[ti - 1] == '/') {
                        curr[ti] = true;
                    }
                    if prev[ti] {
                        seen = true;
                    }
                }
            }
            GlobToken::DoubleStar => {
                let mut seen = false;
                for ti in 0..=n {
                    if prev[ti] {
                        seen = true;
                    }
                    curr[ti] = seen;
                }
            }
            GlobToken::Star => {
                let mut active = false;
                for ti in 0..=n {
                    if ti > 0 && t[ti - 1] == '/' {
                        active = false;
                    }
                    if prev[ti] {
                        active = true;
                    }
                    curr[ti] = active;
                }
            }
            GlobToken::Any => {
                for ti in 1..=n {
                    if prev[ti - 1] && t[ti - 1] != '/' {
                        curr[ti] = true;
                    }
                }
            }
            GlobToken::Lit(ch) => {
                for ti in 1..=n {
                    if prev[ti - 1] && t[ti - 1] == ch {
                        curr[ti] = true;
                    }
                }
            }
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Expand leading `~` or `~/` in a path rule to both raw and canonical home directory forms.
fn expand_tilde_variants(pattern: &str, raw_home: &Path, canon_home: &Path) -> Vec<String> {
    let make = |home: &Path, rest: Option<&str>| -> String {
        let base = home.to_string_lossy().replace('\\', "/");
        match rest {
            Some(rest) => format!("{}/{}", base.trim_end_matches('/'), rest.replace('\\', "/")),
            None => base,
        }
    };
    if pattern == "~" {
        let a = make(raw_home, None);
        let b = make(canon_home, None);
        return if a == b { vec![a] } else { vec![a, b] };
    }
    if let Some(rest) = pattern.strip_prefix("~/").or_else(|| pattern.strip_prefix("~\\")) {
        let a = make(raw_home, Some(rest));
        let b = make(canon_home, Some(rest));
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
    let canon_cwd = canonical_path(cwd);
    let candidates: Vec<String> = {
        let mut list = vec![
            normalized.to_string_lossy().replace('\\', "/"),
            canonical.to_string_lossy().replace('\\', "/"),
        ];
        if let Some(rel) = crate::paths::relative_to(cwd, &normalized) {
            list.push(rel);
        }
        if let Some(rel) = crate::paths::relative_to(&canon_cwd, &canonical) {
            list.push(rel);
        }
        list
    };
    let file_name = canonical.file_name().or_else(|| normalized.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let needs_home = rules.iter().any(|rule| rule.trim().starts_with('~'));
    let (raw_home, canon_home) = if needs_home {
        let raw = crate::env::home_dir();
        let canon = canonical_path(&raw);
        (raw, canon)
    } else {
        (PathBuf::new(), PathBuf::new())
    };
    for rule in rules {
        let trimmed = rule.trim();
        if trimmed.is_empty() {
            continue;
        }
        for expanded in expand_tilde_variants(trimmed, &raw_home, &canon_home) {
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
