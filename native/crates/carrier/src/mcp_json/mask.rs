//! Secret masking for MCP URLs and command lines shown in import previews.

use std::sync::LazyLock;

use regex::Regex;

use super::js_json::{has_js_space, js_regex};
use super::normalize::{CODE_RUNNER, LAUNCHER, SCRIPT_FLAG, seen_by, shell_code};

/// Names of query parameters, options and variables whose values are secrets.
static SECRET_NAME: LazyLock<Regex> = LazyLock::new(|| js_regex(r"(?i)key|token|secret|passw|auth|bearer|credential|sig|(?:^|[-_])pat$"));

/// Path segments that are keys: `sk-…`, `ghp_…`, or long runs of letters and digits such as per-user endpoint IDs.
fn secret_segment(segment: &str) -> bool {
    let prefixed = ["sk", "pk", "ak", "rk", "ghp", "gho", "ghs", "github_pat", "xoxa", "xoxb", "xoxp"]
        .iter()
        .any(|prefix| segment.strip_prefix(prefix).is_some_and(|rest| rest.starts_with(['-', '_'])));
    prefixed
        || (segment.len() >= 24
            && segment.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            && segment.chars().any(|c| c.is_ascii_digit())
            && segment.chars().any(|c| c.is_ascii_alphabetic()))
}

/// `scheme://user:password@host`: the password hidden (uses the last `@` inside the authority so passwords containing
/// `@` are masked in full).
fn masked_password(authority: &str) -> String {
    let masked = || -> Option<String> {
        let slash = authority.find('/')?;
        let user = slash + 2;
        if !authority[slash..].starts_with("//") {
            return None;
        }
        let end = authority[user..].find('/').map_or(authority.len(), |offset| user + offset);
        let auth = &authority[user..end];
        let at_rel = auth.rfind('@')?;
        let userinfo = &auth[..at_rel];
        let colon_rel = userinfo.find(':')?;
        let colon = user + colon_rel;
        let at = user + at_rel;
        Some(format!("{}•••@{}", &authority[..colon + 1], &authority[at + 1..]))
    };
    masked().unwrap_or_else(|| authority.to_owned())
}

/// An address for showing on screen, with its password, key-like path segments and secret query values hidden.
pub fn masked_url(url: &str) -> String {
    let hash_at = url.find('#');
    let before_hash = &url[..hash_at.unwrap_or(url.len())];
    let query_at = before_hash.find('?');
    let base = &before_hash[..query_at.unwrap_or(before_hash.len())];
    let path_at = base.find("//").and_then(|at| base[at + 2..].find('/').map(|path| at + 2 + path));
    let authority = masked_password(&base[..path_at.unwrap_or(base.len())]);
    let path = path_at
        .map(|at| base[at..].split('/').map(|segment| if secret_segment(segment) { "•••" } else { segment }).collect::<Vec<_>>().join("/"))
        .unwrap_or_default();
    let query = query_at
        .map(|at| {
            let pairs: Vec<String> = before_hash[at + 1..]
                .split('&')
                .map(|pair| match pair.find('=') {
                    Some(equals) if equals > 0 && SECRET_NAME.is_match(&pair[..equals]) => format!("{}=•••", &pair[..equals]),
                    _ => pair.to_owned(),
                })
                .collect();
            format!("?{}", pairs.join("&"))
        })
        .unwrap_or_default();
    format!("{authority}{path}{query}{}", hash_at.map_or("", |at| &url[at..]))
}

static URL_WORD: LazyLock<Regex> = LazyLock::new(|| js_regex(r"(?i)^[a-z][a-z0-9+.-]*://\S*$"));
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| js_regex(r"^(-{0,2})([^=\s]+)=([^\n\r\x{2028}\x{2029}]+)$"));

/// The value of `--header "Name: value"` hidden: `Name: •••`, or `Authorization: Bearer •••`.
fn masked_header(word: &str) -> Option<String> {
    let colon = word.find(':')?;
    if has_js_space(&word[..colon]) {
        return None;
    }
    let rest = &word[colon + 1..];
    let value = rest.trim_start_matches([' ', '\t']);
    let start = word.len() - value.len();
    if !value.is_empty() && !has_js_space(value) {
        return Some(format!("{}•••", &word[..start]));
    }
    let token = value.get(..6).filter(|bearer| bearer.eq_ignore_ascii_case("bearer"))?;
    let after = &value[token.len()..];
    let secret = after.trim_start_matches([' ', '\t']);
    (secret.len() < after.len() && !secret.is_empty() && !has_js_space(secret)).then(|| format!("{}•••", &word[..word.len() - secret.len()]))
}

/// `Authorization: Bearer token` as one word, its token hidden.
fn masked_authorization(word: &str) -> Option<String> {
    let label = word.get(..14).filter(|label| label.eq_ignore_ascii_case("authorization:"))?;
    let rest = word[label.len()..].trim_start_matches([' ', '\t']);
    let bearer = rest.get(..6).filter(|bearer| bearer.eq_ignore_ascii_case("bearer")).and_then(|bearer| {
        let after = &rest[bearer.len()..];
        let token = after.trim_start_matches([' ', '\t']);
        (token.len() < after.len() && !token.is_empty() && !has_js_space(token)).then_some(token)
    });
    let secret = bearer.or_else(|| (!rest.is_empty() && !has_js_space(rest)).then_some(rest))?;
    Some(format!("{}•••", &word[..word.len() - secret.len()]))
}

/// A command line for showing on screen, with the values of secret-looking options, variables, headers and URL
/// passwords hidden. A word with spaces in it is code or a phrase the server runs, so it is always shown in full.
pub fn masked_command(words: &[String]) -> String {
    let secret_flag = |word: &str| {
        let name = word.strip_prefix("--").or_else(|| word.strip_prefix('-'));
        name.is_some_and(|name| name.starts_with(|c: char| c.is_ascii_alphabetic()) && !word.contains('=') && SECRET_NAME.is_match(name))
    };
    let url = |word: &str| URL_WORD.is_match(word);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let seen = seen_by(&words, |word| CODE_RUNNER.is_match(word) || LAUNCHER.is_match(word));
    words
        .iter()
        .enumerate()
        .map(|(index, word)| {
            let previous = if index > 0 { words[index - 1] } else { "" };
            // `--header "X-API-Key: …"`, mcp-remote's way of passing a token.
            if previous == "--header" || previous == "-H" {
                return masked_header(word).unwrap_or_else(|| (*word).to_owned());
            }
            if let Some(masked) = masked_authorization(word) {
                return masked;
            }
            // `postgresql://user:password@host/db`, `https://…?api_key=…`
            if url(word) {
                return masked_url(word);
            }
            // Once an interpreter runs, code given to `-c`, `-lc`, `-e` or `-Command`, or written with spaces or shell punctuation,
            // is shown as it is: the user has to see what will run. Without one, arguments are values and `docker -e` is not code.
            if has_js_space(word) || (index > 0 && seen[index - 1] && (SCRIPT_FLAG.is_match(previous) || shell_code(word))) {
                return (*word).to_owned();
            }
            if secret_flag(previous) {
                return "•••".to_owned();
            }
            if let Some(assignment) = ASSIGNMENT.captures(word) {
                let (dashes, name, value) = (&assignment[1], &assignment[2], &assignment[3]);
                if url(value) {
                    return format!("{dashes}{name}={}", masked_url(value));
                }
                if SECRET_NAME.is_match(name) {
                    return format!("{dashes}{name}=•••");
                }
            }
            (*word).to_owned()
        })
        .collect::<Vec<_>>()
        .join(" ")
}
