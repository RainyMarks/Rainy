//! OpenSSH config parser with safe recursive `Include` expansion and concrete host alias summarization.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::valid_destination;
use crate::paths;
use crate::text::wildcard_match;

const MAX_INCLUDE_DEPTH: usize = 8;
const MAX_CONFIG_FILES: usize = 64;
const MAX_CONFIG_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Summary of one concrete `Host` alias parsed from OpenSSH config files (including `Include` directives).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHostSummary {
    pub alias: String,
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub proxy_jump: Option<String>,
}

/// Split one OpenSSH config line into `(keyword, args)`, respecting `#` comments outside quotes, `"..."` quoting and `Key=Value`.
fn split_config_line(line: &str) -> Option<(String, Vec<String>)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' && bytes[i] != b'#' {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let keyword = trimmed[..i].to_owned();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'=' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
    }
    let mut args = Vec::new();
    let chars: Vec<char> = trimmed[i..].chars().collect();
    let mut pos = 0;
    while pos < chars.len() {
        while pos < chars.len() && chars[pos].is_whitespace() {
            pos += 1;
        }
        if pos >= chars.len() || chars[pos] == '#' {
            break;
        }
        let mut token = String::new();
        let mut in_quotes = false;
        while pos < chars.len() {
            let ch = chars[pos];
            if in_quotes {
                if ch == '"' {
                    in_quotes = false;
                } else {
                    token.push(ch);
                }
                pos += 1;
            } else if ch == '"' {
                in_quotes = true;
                pos += 1;
            } else if ch == '#' || ch.is_whitespace() {
                break;
            } else {
                token.push(ch);
                pos += 1;
            }
        }
        if !token.is_empty() {
            args.push(token);
        }
    }
    Some((keyword, args))
}

fn host_patterns_match(patterns: &[String], alias: &str) -> bool {
    let mut matched = false;
    for pattern in patterns {
        if let Some(negated) = pattern.strip_prefix('!') {
            if !negated.is_empty() && wildcard_match(negated, alias, false, true) {
                return false;
            }
        } else if !pattern.is_empty() && wildcard_match(pattern, alias, false, true) {
            matched = true;
        }
    }
    matched
}

#[derive(Default)]
struct ConfigBlock {
    patterns: Vec<String>,
    is_match: bool,
    host_name: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    proxy_jump: Option<String>,
}

struct ConfigParserState {
    blocks: Vec<ConfigBlock>,
    active: HashSet<PathBuf>,
    files_read: usize,
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

fn path_within(candidate: &Path, root: &Path) -> bool {
    if cfg!(windows) {
        let norm = |p: &Path| {
            p.to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_ascii_lowercase()
        };
        let c = norm(candidate);
        let r = norm(root);
        c == r || (c.starts_with(&r) && c.as_bytes().get(r.len()) == Some(&b'\\'))
    } else {
        candidate.starts_with(root)
    }
}

fn expand_glob_segments(roots: Vec<PathBuf>, segments: &[String], budget: usize) -> Vec<PathBuf> {
    let mut current = roots;
    for segment in segments {
        if current.is_empty() {
            break;
        }
        let has_wildcard = segment.contains(['*', '?']);
        let mut next = Vec::new();
        for dir in current {
            if has_wildcard {
                let Ok(entries) = std::fs::read_dir(&dir) else { continue };
                let mut matched: Vec<PathBuf> = entries
                    .flatten()
                    .filter_map(|entry| {
                        let name = entry.file_name();
                        let name_str = name.to_str()?;
                        wildcard_match(segment, name_str, cfg!(windows), true).then(|| entry.path())
                    })
                    .collect();
                matched.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
                for candidate in matched {
                    if next.len() >= budget {
                        break;
                    }
                    next.push(candidate);
                }
            } else {
                next.push(dir.join(segment));
            }
            if next.len() >= budget {
                break;
            }
        }
        current = next;
    }
    current
}

/// Resolve one OpenSSH `Include` pattern safely: rejects `..` traversal, validates that the fixed non-wildcard prefix
/// lies inside `allowed_roots` *before* performing any glob traversal, and checks canonical containment on every result.
pub(super) fn resolve_include_pattern(raw_pattern: &str, base_dir: &Path, home_dir: Option<&Path>, budget: usize) -> Vec<PathBuf> {
    if raw_pattern.is_empty() || raw_pattern.chars().any(|c| matches!(c, '\0' | '\r' | '\n')) || budget == 0 {
        return Vec::new();
    }
    if raw_pattern.split(['/', '\\']).any(|segment| segment == "..") {
        return Vec::new();
    }
    let canonical_base = paths::canonical(base_dir).ok();
    let canonical_home = home_dir.and_then(|home| paths::canonical(home).ok());
    let canonical_dot_ssh = home_dir.and_then(|home| paths::canonical(&home.join(".ssh")).ok());

    let (start_root, segments, allowed_roots): (PathBuf, Vec<String>, Vec<PathBuf>) =
        if let Some(rest) = raw_pattern.strip_prefix("~/").or_else(|| raw_pattern.strip_prefix("~\\")) {
            let (Some(home), Some(allowed)) = (home_dir, canonical_home.clone()) else {
                return Vec::new();
            };
            let segs = rest.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".").map(str::to_owned).collect();
            (home.to_path_buf(), segs, vec![allowed])
        } else if Path::new(raw_pattern).is_absolute() {
            let mut root = PathBuf::new();
            let mut segs = Vec::new();
            for comp in Path::new(raw_pattern).components() {
                match comp {
                    Component::Prefix(prefix) => root.push(prefix.as_os_str()),
                    Component::RootDir => root.push(comp.as_os_str()),
                    Component::CurDir => {}
                    Component::ParentDir => return Vec::new(),
                    Component::Normal(part) => segs.push(part.to_string_lossy().into_owned()),
                }
            }
            let is_user_ssh_config = canonical_base
                .as_ref()
                .zip(canonical_dot_ssh.as_ref())
                .is_some_and(|(base, dot_ssh)| path_within(base, dot_ssh) && path_within(dot_ssh, base));
            let mut allowed: Vec<PathBuf> = [canonical_base, canonical_dot_ssh].into_iter().flatten().collect();
            // For the real `~/.ssh/config`, allow absolute includes inside the user's home directory (e.g. Colima/OrbStack).
            if is_user_ssh_config
                && let Some(home) = canonical_home
            {
                allowed.push(home);
            }
            if allowed.is_empty() {
                return Vec::new();
            }
            (root, segs, allowed)
        } else {
            let Some(allowed) = canonical_base else {
                return Vec::new();
            };
            let segs = raw_pattern.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".").map(str::to_owned).collect();
            (base_dir.to_path_buf(), segs, vec![allowed])
        };

    // Split off the fixed prefix before the first wildcard segment and validate containment before touching `read_dir`.
    let first_wildcard = segments.iter().position(|seg| seg.contains(['*', '?'])).unwrap_or(segments.len());
    let mut fixed_prefix = start_root;
    for seg in &segments[..first_wildcard] {
        fixed_prefix.push(seg);
    }
    let Ok(canonical_prefix) = paths::canonical(&fixed_prefix) else {
        return Vec::new();
    };
    if !allowed_roots.iter().any(|root| path_within(&canonical_prefix, root)) {
        return Vec::new();
    }

    let candidates = if first_wildcard == segments.len() {
        vec![canonical_prefix]
    } else {
        expand_glob_segments(vec![canonical_prefix], &segments[first_wildcard..], budget)
    };
    let mut safe = Vec::new();
    for candidate in candidates {
        let Ok(canonical) = paths::canonical(&candidate) else { continue };
        if !allowed_roots.iter().any(|root| path_within(&canonical, root)) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&canonical) else { continue };
        if meta.is_file() && meta.len() <= MAX_CONFIG_FILE_BYTES {
            safe.push(canonical);
        }
    }
    safe
}

fn parse_config_text(
    text: &str,
    base_dir: Option<&Path>,
    home_dir: Option<&Path>,
    depth: usize,
    enclosing_patterns: Option<&[String]>,
    enclosing_match: bool,
    state: &mut ConfigParserState,
) {
    let mut current = if enclosing_match {
        ConfigBlock { is_match: true, ..Default::default() }
    } else if let Some(patterns) = enclosing_patterns {
        ConfigBlock { patterns: patterns.to_vec(), ..Default::default() }
    } else {
        ConfigBlock { patterns: vec!["*".to_owned()], ..Default::default() }
    };
    let flush = |block: ConfigBlock, blocks: &mut Vec<ConfigBlock>| {
        let has_options = block.host_name.is_some() || block.user.is_some() || block.port.is_some() || block.proxy_jump.is_some();
        if !block.is_match && (!block.patterns.is_empty() || has_options) {
            blocks.push(block);
        }
    };
    for line in text.lines() {
        let Some((keyword, args)) = split_config_line(line) else { continue };
        if args.is_empty() {
            continue;
        }
        if keyword.eq_ignore_ascii_case("include") {
            if depth < MAX_INCLUDE_DEPTH
                && let Some(base_dir) = base_dir
            {
                let child_patterns = (!current.is_match && !current.patterns.is_empty()).then(|| current.patterns.clone());
                let child_match = current.is_match;
                let next_block = ConfigBlock {
                    patterns: child_patterns.clone().unwrap_or_default(),
                    is_match: child_match,
                    ..Default::default()
                };
                let finished = std::mem::replace(&mut current, next_block);
                flush(finished, &mut state.blocks);
                for pattern in &args {
                    let budget = MAX_CONFIG_FILES.saturating_sub(state.files_read);
                    if budget == 0 {
                        break;
                    }
                    for included in resolve_include_pattern(pattern, base_dir, home_dir, budget) {
                        parse_config_file(&included, base_dir, home_dir, depth + 1, child_patterns.as_deref(), child_match, state);
                    }
                }
            }
        } else if keyword.eq_ignore_ascii_case("host") {
            let finished = std::mem::replace(&mut current, ConfigBlock { patterns: args, is_match: false, ..Default::default() });
            flush(finished, &mut state.blocks);
        } else if keyword.eq_ignore_ascii_case("match") {
            let finished = std::mem::replace(&mut current, ConfigBlock { is_match: true, ..Default::default() });
            flush(finished, &mut state.blocks);
        } else if !current.is_match {
            if keyword.eq_ignore_ascii_case("hostname") {
                if current.host_name.is_none() {
                    current.host_name = Some(args[0].clone());
                }
            } else if keyword.eq_ignore_ascii_case("user") {
                if current.user.is_none() {
                    current.user = Some(args[0].clone());
                }
            } else if keyword.eq_ignore_ascii_case("port") {
                if current.port.is_none() {
                    current.port = args[0].parse::<u16>().ok().filter(|port| *port > 0);
                }
            } else if keyword.eq_ignore_ascii_case("proxyjump") && current.proxy_jump.is_none() {
                let value = args.join(",");
                if !value.eq_ignore_ascii_case("none") {
                    current.proxy_jump = Some(value);
                }
            }
        }
    }
    flush(current, &mut state.blocks);
}

fn parse_config_file(
    path: &Path,
    base_dir: &Path,
    home_dir: Option<&Path>,
    depth: usize,
    enclosing_patterns: Option<&[String]>,
    enclosing_match: bool,
    state: &mut ConfigParserState,
) {
    if depth > MAX_INCLUDE_DEPTH || state.files_read >= MAX_CONFIG_FILES {
        return;
    }
    let Ok(canonical) = paths::canonical(path) else { return };
    if !state.active.insert(canonical.clone()) {
        return;
    }
    let Ok(meta) = std::fs::metadata(&canonical) else {
        state.active.remove(&canonical);
        return;
    };
    if !meta.is_file() || meta.len() > MAX_CONFIG_FILE_BYTES {
        state.active.remove(&canonical);
        return;
    }
    let Ok(text) = std::fs::read_to_string(&canonical) else {
        state.active.remove(&canonical);
        return;
    };
    state.files_read += 1;
    parse_config_text(&text, Some(base_dir), home_dir, depth, enclosing_patterns, enclosing_match, state);
    state.active.remove(&canonical);
}

fn summarize_blocks(blocks: &[ConfigBlock]) -> Vec<SshHostSummary> {
    let mut aliases: Vec<String> = Vec::new();
    for block in blocks {
        if block.is_match {
            continue;
        }
        for name in &block.patterns {
            if name.contains(['*', '?', '!']) || !valid_destination(name) || aliases.iter().any(|known| known == name) {
                continue;
            }
            aliases.push(name.clone());
        }
    }
    aliases
        .into_iter()
        .map(|alias| {
            let mut summary = SshHostSummary { alias, host_name: None, user: None, port: None, proxy_jump: None };
            for block in blocks {
                if block.is_match || !host_patterns_match(&block.patterns, &summary.alias) {
                    continue;
                }
                if summary.host_name.is_none() {
                    summary.host_name = block.host_name.clone();
                }
                if summary.user.is_none() {
                    summary.user = block.user.clone();
                }
                if summary.port.is_none() {
                    summary.port = block.port;
                }
                if summary.proxy_jump.is_none() {
                    summary.proxy_jump = block.proxy_jump.clone();
                }
            }
            summary
        })
        .collect()
}

/// Parse an OpenSSH config file and its recursive `Include` files into ordered concrete host summaries.
pub fn parse_ssh_config(path: &Path) -> Vec<SshHostSummary> {
    let base_dir = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let home_dir = user_home_dir();
    parse_ssh_config_with_home(path, base_dir, home_dir.as_deref())
}

pub(super) fn parse_ssh_config_with_home(path: &Path, base_dir: &Path, home_dir: Option<&Path>) -> Vec<SshHostSummary> {
    let mut state = ConfigParserState { blocks: Vec::new(), active: HashSet::new(), files_read: 0 };
    parse_config_file(path, base_dir, home_dir, 0, None, false, &mut state);
    summarize_blocks(&state.blocks)
}

/// Concrete host aliases of an OpenSSH config file text; patterns with wildcards or negation and `#` comments are skipped.
pub fn config_hosts(text: &str) -> Vec<String> {
    let mut state = ConfigParserState { blocks: Vec::new(), active: HashSet::new(), files_read: 0 };
    parse_config_text(text, None, None, 0, None, false, &mut state);
    summarize_blocks(&state.blocks).into_iter().map(|summary| summary.alias).collect()
}

/// The OpenSSH config file aliases are read from; development and tests point at their own.
fn config_path() -> Option<PathBuf> {
    match std::env::var_os("RAINY_SSH_CONFIG") {
        Some(path) => Some(PathBuf::from(path)),
        None => user_home_dir().map(|home| home.join(".ssh").join("config")),
    }
}

/// Concrete host summaries parsed from `~/.ssh/config` (or `RAINY_SSH_CONFIG`), including `Include` files.
pub fn config_host_summaries() -> Vec<SshHostSummary> {
    config_path().map(|path| parse_ssh_config(&path)).unwrap_or_default()
}
