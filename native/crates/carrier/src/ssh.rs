//! The Windows OpenSSH client for SSH execution targets (`src/main/ssh.ts`, `SshHostTransport` and `prepareSsh`):
//! destinations, arguments, the remote Python loader, runs, the runtime installer on the server and the Host command.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::HostCommand;
use crate::locale::localized;
use crate::targets::{self, DEFAULT_KEEPALIVE_COUNT_MAX, DEFAULT_KEEPALIVE_INTERVAL, ExecutionTarget, SshHostProfile};
use crate::wsl::{Installed, RuntimeFiles};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_INCLUDE_DEPTH: usize = 8;
const MAX_CONFIG_FILES: usize = 64;
const MAX_CONFIG_FILE_BYTES: u64 = 2 * 1024 * 1024;
const REMOTE_COMMAND_CACHE_TTL: Duration = Duration::from_secs(30);

/// Where `ssh` connects: a `~/.ssh/config` alias or `[user@]host`, and a port other than 22.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub destination: String,
    pub port: Option<u16>,
}

/// `[user@]host` or an alias, never starting with `-`, so `ssh` cannot read it as an option.
fn valid_destination(text: &str) -> bool {
    let (user, host) = match text.split_once('@') {
        Some((user, host)) => (Some(user), host),
        None => (None, text),
    };
    let word = |text: &str, rest: &dyn Fn(char) -> bool| {
        let mut chars = text.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') && chars.all(rest)
    };
    let user_ok = user.is_none_or(|user| word(user, &|c| c.is_ascii_alphanumeric() || "._%+-".contains(c)));
    let host_ok = match host.strip_prefix('[').and_then(|inner| inner.strip_suffix(']')) {
        Some(address) => !address.is_empty() && address.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.'),
        None => word(host, &|c| c.is_ascii_alphanumeric() || "._-".contains(c)),
    };
    user_ok && host_ok
}

/// What the user typed: `user@host`, `host`, `user@host:port`, `ssh user@host` or a config alias.
pub fn parse_destination(text: &str, port: Option<u32>) -> Result<Destination> {
    let mut destination = text.trim();
    destination = destination.strip_prefix("ssh ").map_or(destination, str::trim_start);
    let mut parsed = port;
    if !destination.starts_with('[')
        && let Some((host, digits)) = destination.rsplit_once(':')
        && !host.is_empty()
        && !host.contains(':')
        && (1..=5).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
    {
        destination = host;
        parsed = parsed.or(digits.parse().ok());
    }
    if destination.len() > 255 || !valid_destination(destination) {
        bail!(localized(
            "SSH 主机格式无效，请填写 user@host 或 ~/.ssh/config 中的主机别名。",
            "Enter the SSH host as user@host or as a host alias from ~/.ssh/config."
        ));
    }
    let port = match parsed {
        None | Some(22) => None,
        Some(port @ 1..=65535) => Some(port as u16),
        Some(_) => bail!(localized("SSH 端口必须是 1 到 65535 之间的整数。", "The SSH port must be a whole number from 1 to 65535.")),
    };
    Ok(Destination { destination: destination.to_owned(), port })
}

/// `destination` or `destination:port`, the target's name.
pub fn name(target: &Destination) -> String {
    match target.port {
        Some(port) => format!("{}:{port}", target.destination),
        None => target.destination.clone(),
    }
}

/// The execution target of a destination (`sshTarget`).
pub fn target(destination: &Destination) -> ExecutionTarget {
    let name = name(destination);
    ExecutionTarget::Ssh { id: format!("ssh:{name}"), label: format!("SSH · {name}"), destination: destination.destination.clone(), port: destination.port }
}

/// Where an SSH target connects.
pub fn destination_of(target: &ExecutionTarget) -> Result<Destination> {
    match target {
        ExecutionTarget::Ssh { destination, port, .. } => parse_destination(destination, port.map(u32::from)),
        _ => bail!(localized("所选执行环境不是 SSH 主机。", "The selected execution environment is not an SSH host.")),
    }
}

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

/// Non-recursive `*`/`?` glob matching (O(n*m), immune to catastrophic backtracking).
fn wildcard_match(pattern: &str, text: &str, case_insensitive: bool) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star_p: Option<usize> = None;
    let mut star_t = 0usize;
    let eq = |a: char, b: char| if case_insensitive { a.eq_ignore_ascii_case(&b) } else { a == b };
    while t < txt.len() {
        if p < pat.len() && (pat[p] == '?' || eq(pat[p], txt[t])) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star_p = Some(p);
            p += 1;
            star_t = t;
        } else if let Some(sp) = star_p {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

fn host_patterns_match(patterns: &[String], alias: &str) -> bool {
    let mut matched = false;
    for pattern in patterns {
        if let Some(negated) = pattern.strip_prefix('!') {
            if !negated.is_empty() && wildcard_match(negated, alias, false) {
                return false;
            }
        } else if !pattern.is_empty() && wildcard_match(pattern, alias, false) {
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
                        wildcard_match(segment, name_str, cfg!(windows)).then(|| entry.path())
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

/// Resolve one OpenSSH `Include` pattern safely: rejects `..` traversal, enforces canonical root containment,
/// and supports `~/`, relative `~/.ssh/` paths and `*`/`?` globs.
fn resolve_include_pattern(raw_pattern: &str, base_dir: &Path, home_dir: Option<&Path>, budget: usize) -> Vec<PathBuf> {
    if raw_pattern.is_empty() || raw_pattern.chars().any(|c| matches!(c, '\0' | '\r' | '\n')) || budget == 0 {
        return Vec::new();
    }
    if raw_pattern.split(['/', '\\']).any(|segment| segment == "..") {
        return Vec::new();
    }
    let canonical_base = std::fs::canonicalize(base_dir).ok();
    let canonical_home = home_dir.and_then(|home| std::fs::canonicalize(home).ok());
    let canonical_dot_ssh = home_dir.and_then(|home| std::fs::canonicalize(home.join(".ssh")).ok());

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
            let allowed: Vec<PathBuf> = [canonical_base, canonical_dot_ssh].into_iter().flatten().collect();
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

    let candidates = expand_glob_segments(vec![start_root], &segments, budget);
    let mut safe = Vec::new();
    for candidate in candidates {
        let Ok(canonical) = std::fs::canonicalize(&candidate) else { continue };
        if !allowed_roots.iter().any(|root| canonical.starts_with(root)) {
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
    // At top-level, options before the first `Host`/`Match` apply globally (`Host *`).
    // Inside an `Include`, leading options inherit the enclosing `Host`/`Match` condition.
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
    let Ok(canonical) = std::fs::canonicalize(path) else { return };
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
    let mut state = ConfigParserState { blocks: Vec::new(), active: HashSet::new(), files_read: 0 };
    parse_config_file(path, base_dir, home_dir.as_deref(), 0, None, false, &mut state);
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

/// An SSH host Rainy offers, and whether it was added in Settings rather than read from `~/.ssh/config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub target: ExecutionTarget,
    pub saved: bool,
}

/// Hosts added in Settings, then the aliases of `~/.ssh/config` (including `Include` files); a malformed entry is skipped (`sshTargets`).
pub fn hosts(preferences: &Map<String, Value>) -> Vec<Host> {
    let mut found: Vec<Host> = Vec::new();
    for host in preferences.get("sshHosts").and_then(Value::as_array).into_iter().flatten() {
        let Some(text) = host.get("destination").and_then(Value::as_str) else { continue };
        let port = host.get("port").and_then(Value::as_u64).map(|port| port.min(u64::from(u32::MAX)) as u32);
        if let Ok(destination) = parse_destination(text, port) {
            let target = target(&destination);
            if !found.iter().any(|known| known.target.id() == target.id()) {
                found.push(Host { target, saved: true });
            }
        }
    }
    for summary in config_host_summaries() {
        let target = target(&Destination { destination: summary.alias, port: None });
        if !found.iter().any(|known| known.target.id() == target.id()) {
            found.push(Host { target, saved: false });
        }
    }
    found
}

fn saved_entries(preferences: &Map<String, Value>) -> Vec<Value> {
    preferences.get("sshHosts").and_then(Value::as_array).cloned().unwrap_or_default()
}

/// Remember a host added in Settings (`saveSshHost`).
pub fn save_host(path: &Path, destination: &Destination) -> Result<()> {
    targets::update_preferences(path, |preferences| {
        let mut hosts: Vec<Value> = saved_entries(preferences)
            .into_iter()
            .filter(|host| {
                let port = host.get("port").and_then(Value::as_u64).map(|port| port as u16);
                !(host.get("destination").and_then(Value::as_str) == Some(destination.destination.as_str()) && port == destination.port)
            })
            .collect();
        let mut entry = json!({ "destination": destination.destination });
        if let Some(port) = destination.port {
            entry["port"] = json!(port);
        }
        hosts.push(entry);
        Map::from_iter([("sshHosts".to_owned(), Value::Array(hosts))])
    })
}

/// Forget a host added in Settings; aliases from `~/.ssh/config` stay (`removeSshHost`).
pub fn remove_host(path: &Path, target_id: &str) -> Result<()> {
    targets::update_preferences(path, |preferences| {
        let hosts: Vec<Value> = saved_entries(preferences)
            .into_iter()
            .filter(|host| {
                let text = host.get("destination").and_then(Value::as_str).unwrap_or_default();
                let port = host.get("port").and_then(Value::as_u64).map(|port| port.min(u64::from(u32::MAX)) as u32);
                parse_destination(text, port).is_ok_and(|destination| target(&destination).id() != target_id)
            })
            .collect();
        Map::from_iter([("sshHosts".to_owned(), Value::Array(hosts))])
    })
}

/// The Windows OpenSSH client, or `ssh` from PATH elsewhere.
pub fn executable() -> Result<PathBuf> {
    if !cfg!(windows) {
        return Ok(PathBuf::from("ssh"));
    }
    let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let system = root.join("System32").join("OpenSSH").join("ssh.exe");
    if system.is_file() {
        return Ok(system);
    }
    bail!(localized(
        "未找到 Windows 自带的 OpenSSH 客户端。请在“设置 → 系统 → 可选功能”中添加“OpenSSH 客户端”。",
        "The Windows OpenSSH client was not found. Add \"OpenSSH Client\" under Settings → System → Optional features."
    ))
}

fn profile_keepalive(profile: Option<&SshHostProfile>) -> (u16, u8) {
    profile.map_or((DEFAULT_KEEPALIVE_INTERVAL, DEFAULT_KEEPALIVE_COUNT_MAX), SshHostProfile::keepalive)
}

/// Arguments of one `ssh` run; keep-alives default to 10s × 3 (30s drop detection).
pub fn arguments(target: &Destination, remote: &[String], forward: Option<u16>) -> Vec<String> {
    arguments_with_profile(target, remote, forward, None)
}

/// Arguments of one `ssh` run, applying optional per-host keepalive overrides from `profile`.
pub fn arguments_with_profile(target: &Destination, remote: &[String], forward: Option<u16>, profile: Option<&SshHostProfile>) -> Vec<String> {
    with_options_mode(target, remote, forward, configured_remote_command(target), false, 20, profile_keepalive(profile))
}

/// `-F` and `-p` as every run of `target` passes them.
pub(crate) fn config_options(target: &Destination) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(config) = std::env::var_os("RAINY_SSH_CONFIG") {
        args.extend(["-F".to_owned(), config.to_string_lossy().into_owned()]);
    }
    if let Some(port) = target.port {
        args.extend(["-p".to_owned(), port.to_string()]);
    }
    args
}

type RemoteCommandKey = (String, Option<u16>, Option<OsString>);

fn remote_command_cache() -> &'static Mutex<HashMap<RemoteCommandKey, (Instant, bool)>> {
    static CACHE: OnceLock<Mutex<HashMap<RemoteCommandKey, (Instant, bool)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remote_command_cache_key(target: &Destination) -> RemoteCommandKey {
    (target.destination.clone(), target.port, std::env::var_os("RAINY_SSH_CONFIG"))
}

/// Whether the user's config gives `target` a `RemoteCommand` (a `tmux attach`, say), next to which ssh refuses the
/// command Rainy passes: "Cannot execute command-line and remote command." `ssh -G` prints the options in effect
/// without connecting. Results are cached briefly per `(target, RAINY_SSH_CONFIG)` so probes, launches and retries in
/// one connection sequence do not fork `ssh.exe` repeatedly.
fn configured_remote_command(target: &Destination) -> bool {
    let key = remote_command_cache_key(target);
    let now = Instant::now();
    if let Ok(cache) = remote_command_cache().lock()
        && let Some((at, value)) = cache.get(&key)
        && now.saturating_duration_since(*at) < REMOTE_COMMAND_CACHE_TTL
    {
        return *value;
    }
    let detected = probe_configured_remote_command(target);
    if let Ok(mut cache) = remote_command_cache().lock() {
        if cache.len() >= 64 {
            cache.retain(|_, (at, _)| now.saturating_duration_since(*at) < REMOTE_COMMAND_CACHE_TTL);
        }
        cache.insert(key, (now, detected));
    }
    detected
}

fn probe_configured_remote_command(target: &Destination) -> bool {
    let mut args = vec!["-G".to_owned()];
    args.extend(config_options(target));
    args.extend(["--".to_owned(), target.destination.clone()]);
    let Ok(mut command) = ssh_command(&args, &[]) else { return false };
    command
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).lines().any(|line| line.to_ascii_lowercase().starts_with("remotecommand ")))
}

#[cfg(test)]
fn with_options(target: &Destination, remote: &[String], forward: Option<u16>, clear_remote_command: bool) -> Vec<String> {
    with_options_mode(
        target,
        remote,
        forward,
        clear_remote_command,
        false,
        20,
        (DEFAULT_KEEPALIVE_INTERVAL, DEFAULT_KEEPALIVE_COUNT_MAX),
    )
}

fn batch_arguments(target: &Destination, remote: &[String], connect_timeout_secs: u64, profile: Option<&SshHostProfile>) -> Vec<String> {
    with_options_mode(
        target,
        remote,
        None,
        configured_remote_command(target),
        true,
        connect_timeout_secs,
        profile_keepalive(profile),
    )
}

fn with_options_mode(
    target: &Destination,
    remote: &[String],
    forward: Option<u16>,
    clear_remote_command: bool,
    batch_mode: bool,
    connect_timeout_secs: u64,
    keepalive: (u16, u8),
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-T".to_owned(),
        "-o".to_owned(),
        format!("ConnectTimeout={connect_timeout_secs}"),
        "-o".to_owned(),
        format!("ServerAliveInterval={}", keepalive.0),
        "-o".to_owned(),
        format!("ServerAliveCountMax={}", keepalive.1),
    ];
    if batch_mode {
        args.extend([
            "-o".to_owned(),
            "BatchMode=yes".to_owned(),
            "-o".to_owned(),
            "KbdInteractiveAuthentication=no".to_owned(),
            "-o".to_owned(),
            "PasswordAuthentication=no".to_owned(),
        ]);
    }
    if clear_remote_command {
        args.extend(["-o".to_owned(), "RemoteCommand=none".to_owned()]);
    }
    args.extend(config_options(target));
    if let Some(port) = forward {
        args.extend(["-L".to_owned(), format!("127.0.0.1:{port}:127.0.0.1:{port}"), "-o".to_owned(), "ExitOnForwardFailure=yes".to_owned()]);
    }
    args.extend(["--".to_owned(), target.destination.clone()]);
    args.extend(remote.iter().cloned());
    args
}

/// The login shell of the server (sh, bash, zsh, fish or csh) reads the command, so everything it sees is base64 except
/// a fixed loader quoted with single quotes and free of `\`, `$` and `!`.
const LOADER: &str = "'import base64,sys;a=[base64.b64decode(x).decode() for x in sys.argv[1:]];sys.argv=[\"rainy\"]+a[1:];exec(a[0])'";

/// Remote command that runs a Python script on the server, which receives `args` unchanged in `sys.argv[1:]`.
pub fn remote_python(script: &str, args: &[&str]) -> Vec<String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let mut words = vec!["python3".to_owned(), "-c".to_owned(), LOADER.to_owned()];
    words.extend(std::iter::once(script).chain(args.iter().copied()).map(|value| engine.encode(value.as_bytes())));
    words
}

/// Starts the Host on the server with the carrier's environment, from the home directory.
pub const HOST_LAUNCHER: &str = "import json,os,sys
p=json.loads(sys.argv[1])
os.chdir(os.path.expanduser(\"~\"))
env=dict(os.environ)
env.update(p[\"env\"])
os.execve(p[\"host\"],[p[\"host\"]],env)
";

fn ssh_command(args: &[String], env: &[(String, String)]) -> Result<Command> {
    let mut command = Command::new(executable()?);
    command.args(args).envs(env.iter().map(|(name, value)| (name, value)));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(command)
}

/// How one `ssh` run ended.
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub code: Option<i32>,
    pub stdout: String,
    /// The last standard error lines, without progress reports.
    pub stderr: String,
}

/// Run `ssh` to completion: `input` goes to the remote command's standard input (closed at once when absent), each
/// standard error line to `on_line`, and the run is stopped after `timeout`.
pub fn run(
    target: &Destination,
    remote: &[String],
    env: &[(String, String)],
    input: Option<Box<dyn Read + Send>>,
    on_line: &mut dyn FnMut(&str),
    timeout: Option<Duration>,
) -> Result<RunOutput> {
    run_with_profile(target, remote, env, None, input, on_line, timeout)
}

fn run_with_profile(
    target: &Destination,
    remote: &[String],
    env: &[(String, String)],
    profile: Option<&SshHostProfile>,
    input: Option<Box<dyn Read + Send>>,
    on_line: &mut dyn FnMut(&str),
    timeout: Option<Duration>,
) -> Result<RunOutput> {
    let command = ssh_command(&arguments_with_profile(target, remote, None, profile), env)?;
    run_command(command, input, on_line, timeout)
}

/// Non-interactive `ssh` run (`BatchMode=yes`, no askpass environment) that fails fast instead of prompting for OTP/passwords.
fn run_batch(
    target: &Destination,
    remote: &[String],
    connect_timeout_secs: u64,
    timeout: Option<Duration>,
    profile: Option<&SshHostProfile>,
) -> Result<RunOutput> {
    let args = batch_arguments(target, remote, connect_timeout_secs, profile);
    let mut command = ssh_command(&args, &[])?;
    command.env_remove("SSH_ASKPASS").env_remove("SSH_ASKPASS_REQUIRE");
    run_command(command, None, &mut |_| {}, timeout)
}

fn run_command(
    mut command: Command,
    input: Option<Box<dyn Read + Send>>,
    on_line: &mut dyn FnMut(&str),
    timeout: Option<Duration>,
) -> Result<RunOutput> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| localized("无法启动 ssh", "Starting ssh"))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let feeder = thread::spawn(move || {
        if let Some(mut input) = input {
            // A connection that closes early is reported by the exit code.
            let _ = std::io::copy(&mut input, &mut stdin).and_then(|_| stdin.flush());
        }
    });
    let collector = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let child = Arc::new(Mutex::new(child));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    if let Some(timeout) = timeout {
        let child = child.clone();
        thread::spawn(move || {
            if done_rx.recv_timeout(timeout) == Err(mpsc::RecvTimeoutError::Timeout) {
                let mut child = child.lock().unwrap();
                kill_process_tree(child.id());
                let _ = child.kill();
            }
        });
    }
    let mut errors: VecDeque<String> = VecDeque::new();
    let mut reader = BufReader::new(stderr);
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line).map(|n| n > 0).unwrap_or(false) {
        let text = String::from_utf8_lossy(&line).trim_end().to_owned();
        if !text.starts_with("RAINY_PROGRESS ") {
            errors.push_back(text.clone());
            if errors.len() > 40 {
                errors.pop_front();
            }
        }
        on_line(&text);
        line.clear();
    }
    let stdout = collector.join().unwrap_or_default();
    let status = child.lock().unwrap().wait()?;
    let _ = done_tx.send(());
    let _ = feeder.join();
    Ok(RunOutput { code: status.code(), stdout: String::from_utf8_lossy(&stdout).into_owned(), stderr: Vec::from(errors).join("\n") })
}

pub(crate) fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        let _ = Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
    }
}

/// Whether the server refused the login, in ssh's own words (`user@host: Permission denied (publickey,password).`,
/// `Permission denied, please try again.`), not a program on the server that could not open a file
/// (`PermissionError: [Errno 13] Permission denied: '…'`), whose password is right.
pub fn login_refused(stderr: &str) -> bool {
    stderr.lines().map(str::trim).any(|line| {
        line.contains(": Permission denied (") || line.starts_with("Permission denied (") || line.starts_with("Permission denied, please try again")
    })
}

/// Why `ssh` failed, from its exit code and standard error, ending with ssh's own words (`sshFailure`).
pub fn failure(target: &Destination, code: Option<i32>, stderr: &str) -> anyhow::Error {
    let name = name(target);
    let said: Vec<&str> = stderr.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with("Warning: Permanently added")).collect();
    let said = said[said.len().saturating_sub(3)..].join("\n");
    let has = |text: &str| stderr.contains(text);
    let reason = if has("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        localized(
            &format!("{name} 的主机密钥与上次记录的不一致，可能有人冒充该服务器。确认服务器确实重装过后，再从 known_hosts 中删除旧记录。"),
            &format!("The host key of {name} differs from the one recorded earlier; someone may be impersonating the server. Remove the old entry from known_hosts only after confirming the server was reinstalled."),
        )
    } else if has("Host key verification failed") {
        localized(&format!("没有信任 {name} 的主机密钥，已取消连接。"), &format!("The host key of {name} was not trusted, so the connection was cancelled."))
    } else if login_refused(stderr) {
        localized(
            &format!("{name} 拒绝了登录。请检查用户名、密码或密钥；也可以先在终端里用 ssh 登录一次确认。"),
            &format!("{name} refused the login. Check the user name, password or key, or try logging in once with ssh in a terminal."),
        )
    } else if has("Could not resolve hostname") {
        localized(&format!("找不到主机 {name}。"), &format!("The host {name} cannot be found."))
    } else if ["Connection refused", "timed out", "No route to host", "Network is unreachable", "Connection reset", "Connection closed"].iter().any(|text| has(text)) {
        localized(&format!("无法连接 {name}，请检查网络、地址和端口。"), &format!("Rainy cannot reach {name}. Check the network, address and port."))
    } else if has("Traceback (most recent call last)") {
        // The connection worked; Rainy's installer or launcher failed on the server (a full disk, a folder it cannot
        // write), as its last lines say.
        localized(&format!("Rainy 在 {name} 上安装或启动远程后端时出错。"), &format!("Rainy failed to install or start its backend on {name}."))
    } else if code == Some(127) || has("python3: not found") || has("python3: command not found") {
        localized(
            &format!("{name} 上没有 python3。Rainy 需要它来安装和启动远程后端，请先在服务器上安装 python3。"),
            &format!("{name} has no python3. Rainy needs it to install and start its backend there; install python3 on the server first."),
        )
    } else {
        let code = code.map_or_else(|| localized("无", "none"), |code| code.to_string());
        localized(&format!("与 {name} 的 SSH 连接失败（退出代码 {code}）。"), &format!("The SSH connection to {name} failed (exit code {code})."))
    };
    if said.is_empty() { anyhow!(reason) } else { anyhow!("{reason}\n{said}") }
}

/// Check the address, the login, the system and python3 with one connection before a host is offered.
pub fn probe(target: &Destination, session: &crate::askpass::Session) -> Result<()> {
    let script = "import platform\nprint(platform.system(), platform.machine())\n";
    let output = run(target, &remote_python(script, &[]), session.env(), None, &mut |_| {}, Some(Duration::from_secs(180)))?;
    if output.code != Some(0) {
        if login_refused(&output.stderr) {
            session.failed();
        }
        return Err(failure(target, output.code, &output.stderr));
    }
    // The last line: a login script may have printed something first.
    let mut words = output.stdout.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or_default().split_whitespace();
    let (system, machine) = (words.next().unwrap_or("?"), words.next().unwrap_or("?"));
    if system != "Linux" || !matches!(machine, "x86_64" | "amd64") {
        let name = name(target);
        bail!(localized(
            &format!("{name} 是 {system} {machine}；SSH 远程开发目前只支持 x86_64 Linux 服务器。"),
            &format!("{name} runs {system} {machine}; SSH remote development supports x86_64 Linux servers only.")
        ));
    }
    Ok(())
}

/// Progress of installing the runtime on a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallProgress {
    /// The server downloads the base itself.
    Downloading { done: u64, total: u64 },
    /// The desktop uploads the application layer and the base.
    Uploading { done: u64, total: u64 },
}

/// Counts what passes through, for upload progress.
struct Counted<R> {
    inner: R,
    done: u64,
    total: u64,
    report: Box<dyn FnMut(u64, u64) + Send>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        let before = self.done;
        self.done += read as u64;
        if before / (1 << 20) != self.done / (1 << 20) || read == 0 {
            (self.report)(self.done, self.total);
        }
        Ok(read)
    }
}

/// What the runtime installer on the server answered to `--fetch` or `--receive`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerRuntime {
    /// The Host is installed: absolute path of `rainy-host` on the server.
    Ready { host: String },
    /// The base of this version is missing there; `error` says why the server could not download it, when it tried.
    NeedsArchive { error: Option<String> },
    /// Not an x86_64 Linux server: its system and machine.
    Unsupported(String),
}

/// Result of a zero-upload probe (`install-runtime.py --probe`) on an SSH server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeProbe {
    /// This version is already installed on the server (zero archive uploaded).
    Ready { host: String },
    /// This version is missing on the server and needs installation.
    NeedsInstall { needs_app: bool, needs_base: bool },
    /// Not an x86_64 Linux server: its system and machine.
    Unsupported(String),
}

/// Remote SSH Host identity tracked by [`crate::HostProcess`] for force-stop and in-place reconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHost {
    pub target: Destination,
    pub host: String,
}

/// Outcome of an in-place SSH reconnect attempt without restarting `RainyAgent.exe`.
#[derive(Debug)]
pub enum ReconnectOutcome {
    /// Reconnected to the SSH server with a fresh running HostProcess.
    Connected(crate::HostProcess),
    /// The runtime on the server is missing and needs full installation.
    NeedsInstall { needs_app: bool, needs_base: bool },
    /// Not an x86_64 Linux server: its system and machine.
    Unsupported(String),
}

/// Localized error for an unsupported SSH server platform (`RuntimeProbe::Unsupported`).
pub fn unsupported_error(target: &Destination, system: &str) -> anyhow::Error {
    let name = name(target);
    anyhow!(localized(
        &format!("{name} 是 {system}；SSH 远程开发目前只支持 x86_64 Linux 服务器。"),
        &format!("{name} runs {system}; SSH remote development supports x86_64 Linux servers only."),
    ))
}

fn probe_answer(stdout: &str) -> Result<RuntimeProbe> {
    let line = stdout.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or_default();
    let value: Value = serde_json::from_str(line.trim())
        .map_err(|_| anyhow!("{}\n{}", localized("服务器返回的探活结果无效。", "The server returned an invalid probe result."), tail(stdout, 400)))?;
    if let Some(system) = value.get("unsupported").and_then(Value::as_str) {
        return Ok(RuntimeProbe::Unsupported(system.to_owned()));
    }
    if value.get("needsApp").and_then(Value::as_bool) == Some(true) {
        let needs_base = value.get("needsBase").and_then(Value::as_bool).unwrap_or(true);
        return Ok(RuntimeProbe::NeedsInstall { needs_app: true, needs_base });
    }
    match value.get("host").and_then(Value::as_str).filter(|path| path.starts_with('/')) {
        Some(host) => Ok(RuntimeProbe::Ready { host: host.to_owned() }),
        None => bail!("{}\n{}", localized("服务器返回的探活结果无效。", "The server returned an invalid probe result."), tail(stdout, 400)),
    }
}

fn installer_home(profile: Option<&SshHostProfile>) -> Option<String> {
    let profile = profile?;
    profile
        .host_env_vars()
        .into_iter()
        .find_map(|(key, value)| (key == "RAINY_HOME").then_some(value))
}

fn installer_args<'a>(home: Option<&'a str>, mode: &'a str, metadata: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = Vec::with_capacity(extra.len() + 4);
    if let Some(home) = home {
        args.extend(["--home", home]);
    }
    args.extend([mode, metadata]);
    args.extend_from_slice(extra);
    args
}

/// Probe the server without streaming `linux-app.tar.gz` (`install-runtime.py --probe`).
/// When this version is already installed, returns `Ok(Ok(RuntimeProbe::Ready { host }))` with zero archive upload,
/// and opportunistically prunes earlier runtimes on the same SSH connection when `prune` is true.
pub fn probe_runtime(
    target: &Destination,
    files: &RuntimeFiles,
    env: &[(String, String)],
    prune: bool,
    log: &(dyn Fn(&str) + Sync),
) -> Result<std::result::Result<RuntimeProbe, RunOutput>> {
    probe_runtime_with_profile(target, files, env, prune, None, log)
}

/// Probe the server runtime with optional per-host profile overrides (`RAINY_HOME` and keepalive).
pub fn probe_runtime_with_profile(
    target: &Destination,
    files: &RuntimeFiles,
    env: &[(String, String)],
    prune: bool,
    profile: Option<&SshHostProfile>,
    log: &(dyn Fn(&str) + Sync),
) -> Result<std::result::Result<RuntimeProbe, RunOutput>> {
    let name = name(target);
    let script = std::fs::read_to_string(&files.installer).with_context(|| reading(&files.installer))?;
    let metadata = std::fs::read_to_string(&files.metadata).with_context(|| reading(&files.metadata))?;
    let home = installer_home(profile);
    let extra: &[&str] = if prune { &["--prune"] } else { &[] };
    let args = installer_args(home.as_deref(), "--probe", metadata.as_str(), extra);
    let output = run_with_profile(
        target,
        &remote_python(&script, &args),
        env,
        profile,
        None,
        &mut |line| log(&format!("ssh {name}: {line}")),
        Some(Duration::from_secs(120)),
    )?;
    if output.code != Some(0) {
        return Ok(Err(output));
    }
    probe_answer(&output.stdout).map(Ok)
}

/// The context of a runtime file of this installation that could not be read or opened.
fn reading(path: &Path) -> String {
    localized(&format!("无法读取 {}", path.display()), &format!("Reading {}", path.display()))
}

fn opening(path: &Path) -> String {
    localized(&format!("无法打开 {}", path.display()), &format!("Opening {}", path.display()))
}

/// The last `bytes` bytes of `text` or a little fewer, starting at a character.
fn tail(text: &str, bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn installer_answer(stdout: &str) -> Result<ServerRuntime> {
    let line = stdout.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or_default();
    let value: Value = serde_json::from_str(line.trim())
        .map_err(|_| anyhow!("{}\n{}", localized("服务器返回的安装结果无效。", "The server returned an invalid installation result."), tail(stdout, 400)))?;
    if let Some(system) = value.get("unsupported").and_then(Value::as_str) {
        return Ok(ServerRuntime::Unsupported(system.to_owned()));
    }
    if value.get("needsArchive").is_some() {
        return Ok(ServerRuntime::NeedsArchive { error: value.get("error").and_then(Value::as_str).map(str::to_owned) });
    }
    match crate::wsl::parse_installed(line)? {
        Installed::Ready { host } => Ok(ServerRuntime::Ready { host }),
        Installed::NeedsArchive => Ok(ServerRuntime::NeedsArchive { error: None }),
        // The application layer goes over standard input, so the server always has it.
        Installed::NeedsApp => bail!(localized("服务器没有收到 Rainy 的程序文件。", "The server did not receive Rainy's application files.")),
    }
}

/// Run the runtime installer on the server with `args` after the mode (`sshInstaller`).
#[allow(clippy::too_many_arguments)]
fn installer(
    target: &Destination,
    files: &RuntimeFiles,
    mode: &str,
    extra: &[&str],
    env: &[(String, String)],
    profile: Option<&SshHostProfile>,
    input: Option<Box<dyn Read + Send>>,
    on_line: &mut dyn FnMut(&str),
) -> Result<std::result::Result<ServerRuntime, RunOutput>> {
    let script = std::fs::read_to_string(&files.installer).with_context(|| reading(&files.installer))?;
    let metadata = std::fs::read_to_string(&files.metadata).with_context(|| reading(&files.metadata))?;
    let home = installer_home(profile);
    let args = installer_args(home.as_deref(), mode, metadata.as_str(), extra);
    let output = run_with_profile(target, &remote_python(&script, &args), env, profile, input, on_line, None)?;
    if output.code != Some(0) {
        return Ok(Err(output));
    }
    installer_answer(&output.stdout).map(Ok)
}

/// Reports `RAINY_PROGRESS <done> <total>` lines as the server's download progress and logs every other line.
fn progress_lines<'a>(name: &'a str, progress: &'a (dyn Fn(InstallProgress) + Sync), log: &'a (dyn Fn(&str) + Sync)) -> impl FnMut(&str) + 'a {
    move |line: &str| {
        let mut words = line.strip_prefix("RAINY_PROGRESS ").map(|rest| rest.split_whitespace().filter_map(|word| word.parse::<u64>().ok()));
        match words.as_mut().map(|words| (words.next(), words.next())) {
            Some((Some(done), Some(total))) => progress(InstallProgress::Downloading { done, total }),
            _ => log(&format!("ssh {name}: {line}")),
        }
    }
}

/// Install this version's Linux runtime on the server as it is, or with the base the server downloads itself from
/// `pieces` (`--fetch`; the application layer goes over standard input). An `Err(RunOutput)` is an `ssh` run that
/// failed, for the caller to explain.
pub fn fetch_runtime(
    target: &Destination,
    files: &RuntimeFiles,
    pieces: &Value,
    env: &[(String, String)],
    progress: &(dyn Fn(InstallProgress) + Sync),
    log: &(dyn Fn(&str) + Sync),
) -> Result<std::result::Result<ServerRuntime, RunOutput>> {
    fetch_runtime_with_profile(target, files, pieces, env, None, progress, log)
}

/// Install this version's Linux runtime on the server with optional per-host profile overrides.
pub fn fetch_runtime_with_profile(
    target: &Destination,
    files: &RuntimeFiles,
    pieces: &Value,
    env: &[(String, String)],
    profile: Option<&SshHostProfile>,
    progress: &(dyn Fn(InstallProgress) + Sync),
    log: &(dyn Fn(&str) + Sync),
) -> Result<std::result::Result<ServerRuntime, RunOutput>> {
    let name = name(target);
    let app = std::fs::File::open(&files.app).with_context(|| opening(&files.app))?;
    let pieces = pieces.to_string();
    installer(target, files, "--fetch", &[&pieces], env, profile, Some(Box::new(app)), &mut progress_lines(&name, progress, log))
}

/// Upload the application layer and the base archive from this computer and install them (`--receive`).
pub fn receive_runtime(
    target: &Destination,
    files: &RuntimeFiles,
    base: &Path,
    env: &[(String, String)],
    progress: &(dyn Fn(InstallProgress) + Sync),
    log: &(dyn Fn(&str) + Sync),
) -> Result<std::result::Result<ServerRuntime, RunOutput>> {
    receive_runtime_with_profile(target, files, base, env, None, progress, log)
}

/// Upload the application layer and the base archive from this computer and install them with optional per-host profile overrides.
pub fn receive_runtime_with_profile(
    target: &Destination,
    files: &RuntimeFiles,
    base: &Path,
    env: &[(String, String)],
    profile: Option<&SshHostProfile>,
    progress: &(dyn Fn(InstallProgress) + Sync),
    log: &(dyn Fn(&str) + Sync),
) -> Result<std::result::Result<ServerRuntime, RunOutput>> {
    let name = name(target);
    let size = std::fs::metadata(&files.app)?.len() + std::fs::metadata(base).with_context(|| reading(base))?.len();
    // The application layer first, then the base.
    let chain = std::fs::File::open(&files.app)?.chain(std::fs::File::open(base).with_context(|| opening(base))?);
    // The upload is read on another thread; its counts come back here to be reported.
    let (tx, rx) = mpsc::channel::<(u64, u64)>();
    let counted = Counted { inner: chain, done: 0, total: size, report: Box::new(move |done, total| {
        let _ = tx.send((done, total));
    }) };
    thread::scope(|scope| {
        scope.spawn(move || {
            while let Ok((done, total)) = rx.recv() {
                progress(InstallProgress::Uploading { done, total });
            }
        });
        installer(target, files, "--receive", &[], env, profile, Some(Box::new(counted)), &mut progress_lines(&name, progress, log))
    })
}

/// The Host's paths from the installer's answer, or why there are none. `uploaded`: the base was sent from here, so a
/// base still missing means the upload broke off.
pub fn installed_paths(target: &Destination, answer: ServerRuntime, uploaded: bool) -> Result<String> {
    let name = name(target);
    match answer {
        ServerRuntime::Ready { host } => Ok(host),
        ServerRuntime::Unsupported(system) => bail!(localized(
            &format!("{name} 是 {system}；SSH 远程开发目前只支持 x86_64 Linux 服务器。"),
            &format!("{name} runs {system}; SSH remote development supports x86_64 Linux servers only."),
        )),
        ServerRuntime::NeedsArchive { .. } if uploaded => bail!(localized(
            &format!("上传到 {name} 的 Linux 运行环境校验失败，连接可能中途断开了。"),
            &format!("The Linux runtime uploaded to {name} failed verification; the connection may have dropped."),
        )),
        ServerRuntime::NeedsArchive { .. } => bail!(localized(
            &format!("{name} 上的 Linux 运行环境没有安装成功。"),
            &format!("The Linux runtime was not installed on {name}."),
        )),
    }
}

/// Delete runtimes earlier versions installed on the server; the removed folders.
///
/// Runs non-interactively (`BatchMode=yes`, ignoring askpass variables) so background maintenance never pops an
/// unexpected password/OTP prompt after startup. If non-interactive authentication is refused, pruning is silently
/// skipped and `Ok(vec![])` is returned.
pub fn prune_runtimes(target: &Destination, files: &RuntimeFiles, env: &[(String, String)]) -> Result<Vec<String>> {
    prune_runtimes_with_profile(target, files, env, None)
}

/// Delete runtimes earlier versions installed on the server with optional per-host profile (`RAINY_HOME` and keepalive).
pub fn prune_runtimes_with_profile(
    target: &Destination,
    files: &RuntimeFiles,
    _env: &[(String, String)],
    profile: Option<&SshHostProfile>,
) -> Result<Vec<String>> {
    let script = std::fs::read_to_string(&files.installer)?;
    let metadata = std::fs::read_to_string(&files.metadata)?;
    let home = installer_home(profile);
    let args = installer_args(home.as_deref(), "--prune", &metadata, &[]);
    let output = run_batch(target, &remote_python(&script, &args), 10, Some(Duration::from_secs(120)), profile)?;
    if output.code != Some(0) {
        if login_refused(&output.stderr)
            || output.stderr.contains("Permission denied")
            || output.stderr.contains("Host key verification failed")
        {
            log::info!("Skipped background SSH runtime prune on {} (requires interactive authentication)", name(target));
            return Ok(Vec::new());
        }
        return Err(failure(target, output.code, &output.stderr));
    }
    let value: Value = serde_json::from_str(output.stdout.lines().last().unwrap_or("{}")).unwrap_or(Value::Null);
    Ok(value.get("removed").and_then(Value::as_array).map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect()).unwrap_or_default())
}

/// Extract the SSH target and remote Host path from a command built by [`host_command`], so [`crate::HostProcess`] can
/// force-stop the remote process tree when graceful shutdown fails.
pub(crate) fn host_of(command: &HostCommand) -> Option<RemoteHost> {
    let program = command.program.file_name()?.to_str()?;
    if !(program.eq_ignore_ascii_case("ssh.exe") || program.eq_ignore_ascii_case("ssh")) {
        return None;
    }
    let args: Vec<&str> = command.args.iter().map(|arg| arg.to_str()).collect::<Option<_>>()?;
    let sep = args.iter().position(|arg| *arg == "--")?;
    let mut port: Option<u16> = None;
    let mut i = 0;
    while i + 1 < sep {
        if args[i] == "-p" {
            port = args[i + 1].parse::<u16>().ok().filter(|p| *p != 22);
            i += 2;
        } else {
            i += 1;
        }
    }
    let destination = args.get(sep + 1)?;
    if !valid_destination(destination) {
        return None;
    }
    // Expected tail: `python3 -c <LOADER> <b64(HOST_LAUNCHER)> <b64(parameters)>`
    let tail = args.get(sep + 2..)?;
    if tail.len() != 5 || tail[0] != "python3" || tail[1] != "-c" || tail[2] != LOADER {
        return None;
    }
    let engine = base64::engine::general_purpose::STANDARD;
    let script = engine.decode(tail[3]).ok()?;
    if script.as_slice() != HOST_LAUNCHER.as_bytes() {
        return None;
    }
    let params_bytes = engine.decode(tail[4]).ok()?;
    let params: Value = serde_json::from_slice(&params_bytes).ok()?;
    let host = params.get("host").and_then(Value::as_str).filter(|path| path.starts_with('/'))?;
    Some(RemoteHost {
        target: Destination { destination: (*destination).to_owned(), port },
        host: host.to_owned(),
    })
}

/// Best-effort non-interactive (`BatchMode=yes`) remote `/proc`-verified SIGKILL of an SSH Host process tree when
/// graceful stop timed out or startup failed after launch. Never triggers an interactive askpass/OTP prompt.
pub fn force_stop(target: &Destination, host: &str, pid: u32) -> Result<()> {
    let pid_text = pid.to_string();
    let output = run_batch(
        target,
        &remote_python(crate::wsl::FORCE_STOP, &[&pid_text, host]),
        10,
        Some(Duration::from_secs(15)),
        None,
    )?;
    match output.code {
        Some(0) | Some(3) => Ok(()),
        _ => Err(failure(target, output.code, &output.stderr)),
    }
}

/// Start (or restart) an SSH Host process with automatic loopback port retry (up to 3 attempts).
/// Suitable both for initial startup and in-place reconnect without restarting the desktop process.
pub fn start_host_process(
    target: &Destination,
    host: &str,
    env: &[(String, String)],
    askpass: &Arc<crate::askpass::Askpass>,
    events: Arc<dyn crate::HostEvents>,
    recent_output: &(dyn Fn() -> String + Sync),
    clear_recent: &(dyn Fn() + Sync),
) -> Result<crate::HostProcess> {
    start_host_process_with_profile(target, host, env, None, askpass, events, recent_output, clear_recent)
}

/// Start (or restart) an SSH Host process with optional per-host profile overrides (`RAINY_HOME`, notes, extra env, keepalive).
#[allow(clippy::too_many_arguments)]
pub fn start_host_process_with_profile(
    target: &Destination,
    host: &str,
    env: &[(String, String)],
    profile: Option<&SshHostProfile>,
    askpass: &Arc<crate::askpass::Askpass>,
    events: Arc<dyn crate::HostEvents>,
    recent_output: &(dyn Fn() -> String + Sync),
    clear_recent: &(dyn Fn() + Sync),
) -> Result<crate::HostProcess> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let session = askpass.session();
        clear_recent();
        let port = free_loopback_port()?;
        let command = host_command_with_profile(target, host, env, port, session.env(), profile)?;
        match crate::HostProcess::start(&command, events.clone()) {
            Ok(process) => return Ok(process),
            Err(error) => {
                let said = recent_output();
                let lower = format!("{said}\n{error:#}").to_ascii_lowercase();
                if attempt < 3 && (lower.contains("forwarding") || lower.contains("address already in use")) {
                    continue;
                }
                if login_refused(&said) {
                    session.failed();
                }
                return Err(if error.downcast_ref::<crate::HostStopped>().is_some() {
                    failure(target, None, &said)
                } else {
                    error
                });
            }
        }
    }
}

/// Reconnect to an SSH server in-place without restarting `RainyAgent.exe`:
/// 1. Best-effort force-stops a stale remote Host PID (when `previous` is provided, non-interactive `BatchMode=yes`).
/// 2. Probes the server runtime with zero archive upload (`--probe`).
/// 3. Starts a fresh SSH Host tunnel when the runtime is ready, or reports `NeedsInstall` / `Unsupported`.
#[allow(clippy::too_many_arguments)]
pub fn reconnect_host(
    target: &Destination,
    files: &RuntimeFiles,
    env: &[(String, String)],
    askpass: &Arc<crate::askpass::Askpass>,
    events: Arc<dyn crate::HostEvents>,
    previous: Option<(&str, u32)>,
    recent_output: &(dyn Fn() -> String + Sync),
    clear_recent: &(dyn Fn() + Sync),
    log: &(dyn Fn(&str) + Sync),
) -> Result<ReconnectOutcome> {
    reconnect_host_with_profile(target, files, env, None, askpass, events, previous, recent_output, clear_recent, log)
}

/// Reconnect to an SSH server in-place with optional per-host profile overrides.
#[allow(clippy::too_many_arguments)]
pub fn reconnect_host_with_profile(
    target: &Destination,
    files: &RuntimeFiles,
    env: &[(String, String)],
    profile: Option<&SshHostProfile>,
    askpass: &Arc<crate::askpass::Askpass>,
    events: Arc<dyn crate::HostEvents>,
    previous: Option<(&str, u32)>,
    recent_output: &(dyn Fn() -> String + Sync),
    clear_recent: &(dyn Fn() + Sync),
    log: &(dyn Fn(&str) + Sync),
) -> Result<ReconnectOutcome> {
    if let Some((prev_host, prev_pid)) = previous
        && let Err(error) = force_stop(target, prev_host, prev_pid)
    {
        log(&format!("Best-effort SSH force-stop before reconnect skipped: {error:#}"));
    }
    let session = askpass.session();
    let probe = match probe_runtime_with_profile(target, files, session.env(), false, profile, log)? {
        Ok(probe) => probe,
        Err(output) => {
            if login_refused(&output.stderr) {
                session.failed();
            }
            return Err(failure(target, output.code, &output.stderr));
        }
    };
    match probe {
        RuntimeProbe::Ready { host } => {
            let process = start_host_process_with_profile(target, &host, env, profile, askpass, events, recent_output, clear_recent)?;
            Ok(ReconnectOutcome::Connected(process))
        }
        RuntimeProbe::NeedsInstall { needs_app, needs_base } => Ok(ReconnectOutcome::NeedsInstall { needs_app, needs_base }),
        RuntimeProbe::Unsupported(system) => Ok(ReconnectOutcome::Unsupported(system)),
    }
}

/// A loopback port free on this computer, used on both ends of the forward.
pub fn free_loopback_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// The Host on the server: its port is forwarded to the same loopback port here, so the page's origin matches the one
/// the Host checks; closing the session closes the Host's standard input, which stops it (`SshHostTransport`).
pub fn host_command(target: &Destination, host: &str, env: &[(String, String)], port: u16, askpass: &[(String, String)]) -> Result<HostCommand> {
    host_command_with_profile(target, host, env, port, askpass, None)
}

/// The Host on the server with optional per-host profile overrides (`RAINY_HOME`, `RAINY_SSH_NOTES`,
/// `RAINY_SSH_EXTRA_ENV`, and keepalive intervals).
pub fn host_command_with_profile(
    target: &Destination,
    host: &str,
    env: &[(String, String)],
    port: u16,
    askpass: &[(String, String)],
    profile: Option<&SshHostProfile>,
) -> Result<HostCommand> {
    let mut variables: Map<String, Value> = Map::new();
    if let Some(profile) = profile {
        for (name, value) in profile.host_env_vars() {
            variables.insert(name, Value::String(value));
        }
    }
    for (name, value) in env {
        variables.insert(name.clone(), Value::String(value.clone()));
    }
    variables.insert("RAINY_PORT".into(), Value::String(port.to_string()));
    let parameters = json!({ "host": host, "env": variables }).to_string();
    let args = arguments_with_profile(target, &remote_python(HOST_LAUNCHER, &[&parameters]), Some(port), profile);
    Ok(HostCommand {
        program: executable()?,
        args: args.into_iter().map(OsString::from).collect(),
        env: askpass.iter().map(|(name, value)| (name.into(), value.into())).collect(),
        cwd: None,
        scrub_environment: false,
        wait_for_loopback: true,
        expected_port: Some(port),
    })
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn destinations() {
        assert_eq!(parse_destination("ssh dev@example.com:2222", None).unwrap(), Destination { destination: "dev@example.com".into(), port: Some(2222) });
        assert_eq!(parse_destination("box", Some(22)).unwrap(), Destination { destination: "box".into(), port: None });
        assert_eq!(parse_destination("[::1]", None).unwrap().destination, "[::1]");
        assert!(parse_destination("-oProxyCommand=x", None).is_err());
        assert!(parse_destination("a b", None).is_err());
        assert!(parse_destination("host", Some(70000)).is_err());
        assert_eq!(name(&Destination { destination: "box".into(), port: Some(2200) }), "box:2200");
        assert!(matches!(target(&Destination { destination: "box".into(), port: None }), ExecutionTarget::Ssh { ref id, .. } if id == "ssh:box"));
    }

    #[test]
    fn config_aliases() {
        let text = "# Host ignored-line\nHost dev box # work server\n  HostName 10.0.0.2\nHost *.internal !bad\nhost   prod #comment trailing\nHost edge#inline extra\nHostName x\n";
        assert_eq!(config_hosts(text), ["dev", "box", "prod", "edge"]);
    }

    #[test]
    fn batch_mode_disables_interactive_prompts_and_tracks_remote_host() {
        let target = Destination { destination: "dev@box".into(), port: Some(2222) };
        let args = with_options_mode(&target, &["true".into()], None, false, true, 10, (10, 3));
        assert!(args.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]));
        assert!(args.windows(2).any(|pair| pair == ["-o", "KbdInteractiveAuthentication=no"]));
        assert!(args.windows(2).any(|pair| pair == ["-o", "PasswordAuthentication=no"]));
        assert!(args.windows(2).any(|pair| pair == ["-o", "ConnectTimeout=10"]));
        assert!(args.windows(2).any(|pair| pair == ["-o", "ServerAliveInterval=10"]));
        assert!(args.windows(2).any(|pair| pair == ["-o", "ServerAliveCountMax=3"]));
        // host_of extracts RemoteHost from a command created by host_command (without touching HostCommand fields).
        let parameters = json!({ "host": "/home/u/.rainy-agent/runtime/abc/rainy-host", "env": {} }).to_string();
        let cmd_args = with_options(&target, &remote_python(HOST_LAUNCHER, &[&parameters]), Some(5000), false);
        let command = HostCommand {
            program: PathBuf::from(r"C:\Windows\System32\OpenSSH\ssh.exe"),
            args: cmd_args.into_iter().map(OsString::from).collect(),
            env: Vec::new(),
            cwd: None,
            scrub_environment: false,
            wait_for_loopback: true,
            expected_port: Some(5000),
        };
        assert_eq!(
            host_of(&command),
            Some(RemoteHost {
                target: target.clone(),
                host: "/home/u/.rainy-agent/runtime/abc/rainy-host".into(),
            })
        );
        // Non-ssh commands or non-launcher remote commands are never matched.
        assert_eq!(host_of(&HostCommand { program: PathBuf::from("wsl.exe"), ..command.clone() }), None);
        let other = HostCommand { args: with_options(&target, &remote_python("print(1)", &[]), None, false).into_iter().map(OsString::from).collect(), ..command };
        assert_eq!(host_of(&other), None);
    }

    #[test]
    fn parses_zero_upload_probe_answers() {
        assert_eq!(
            probe_answer("motd\n{\"host\": \"/h/.rainy-agent/runtime/x/rainy-host\"}\n").unwrap(),
            RuntimeProbe::Ready { host: "/h/.rainy-agent/runtime/x/rainy-host".into() }
        );
        assert_eq!(
            probe_answer("{\"needsApp\": true, \"needsBase\": false}\n").unwrap(),
            RuntimeProbe::NeedsInstall { needs_app: true, needs_base: false }
        );
        assert_eq!(
            probe_answer("{\"needsApp\": true, \"needsBase\": true}\n").unwrap(),
            RuntimeProbe::NeedsInstall { needs_app: true, needs_base: true }
        );
        assert_eq!(
            probe_answer("{\"unsupported\": \"Linux aarch64\"}\n").unwrap(),
            RuntimeProbe::Unsupported("Linux aarch64".into())
        );
        assert!(probe_answer("{\"host\": \"relative\"}").is_err());
        assert!(probe_answer("garbage").is_err());
    }

    #[test]
    fn remote_words() {
        let words = remote_python("print(1)", &["a b"]);
        assert_eq!(&words[..3], ["python3", "-c", LOADER]);
        assert!(!LOADER[1..LOADER.len() - 1].contains(['\'', '\\', '$', '!']));
        let engine = base64::engine::general_purpose::STANDARD;
        assert_eq!(engine.decode(&words[4]).unwrap(), b"a b");
        let target = Destination { destination: "box".into(), port: Some(2200) };
        let args = with_options(&target, &words, Some(5000), false);
        assert!(args.windows(2).any(|pair| pair == ["-p", "2200"]));
        assert!(args.windows(2).any(|pair| pair == ["-L", "127.0.0.1:5000:127.0.0.1:5000"]));
        assert_eq!(args.iter().position(|arg| arg == "--").map(|at| &args[at + 1]), Some(&"box".to_owned()));
        assert!(!args.iter().any(|arg| arg.starts_with("RemoteCommand")));
        // A RemoteCommand of the user's config is overridden before the destination.
        let args = with_options(&target, &words, None, true);
        let at = args.windows(2).position(|pair| pair == ["-o", "RemoteCommand=none"]).unwrap();
        assert!(at < args.iter().position(|arg| arg == "--").unwrap());
    }

    /// A host whose config entry has a `RemoteCommand` still runs Rainy's commands. Run with `--ignored` and
    /// `RAINY_SSH_CONFIG` naming a config whose alias `RAINY_TEST_SSH_HOST` has one, such as the throwaway sshd's.
    #[test]
    #[ignore = "needs an SSH server"]
    fn runs_on_a_host_whose_config_has_a_remote_command() {
        let host = std::env::var("RAINY_TEST_SSH_HOST").unwrap();
        let target = Destination { destination: host, port: None };
        assert!(configured_remote_command(&target));
        let output = run(&target, &remote_python("import sys\nprint('ok', sys.stdin.isatty())\n", &[]), &[], None, &mut |_| {}, Some(Duration::from_secs(60))).unwrap();
        assert_eq!((output.code, output.stdout.trim()), (Some(0), "ok False"), "{}", output.stderr);
    }

    #[test]
    fn reads_installer_answers() {
        let ready = installer_answer("noise\n{\"host\": \"/h/r/rainy-host\"}\n").unwrap();
        assert_eq!(ready, ServerRuntime::Ready { host: "/h/r/rainy-host".into() });
        assert_eq!(installer_answer("{\"needsArchive\": true}").unwrap(), ServerRuntime::NeedsArchive { error: None });
        assert_eq!(
            installer_answer("{\"needsArchive\": true, \"error\": \"URLError: timed out\"}").unwrap(),
            ServerRuntime::NeedsArchive { error: Some("URLError: timed out".into()) }
        );
        assert_eq!(installer_answer("{\"unsupported\": \"Linux aarch64\"}").unwrap(), ServerRuntime::Unsupported("Linux aarch64".into()));
        assert!(installer_answer("not json").is_err());
        // What a login script printed after the installer, in Chinese, is quoted without cutting a character.
        let noise = format!("{{\"host\": \"/h\"}}\n{}", "欢迎使用服务器。".repeat(40));
        for skip in 0..3 {
            let error = installer_answer(&noise[..noise.len() - skip * 3]).unwrap_err().to_string();
            assert!(error.contains("欢迎") && error.len() < 500, "{error}");
        }
        assert_eq!(tail("ab中", 2), "");
        assert_eq!(tail("ab中", 3), "中");
        let target = Destination { destination: "box".into(), port: None };
        assert_eq!(installed_paths(&target, ready, false).unwrap(), "/h/r/rainy-host");
        assert!(installed_paths(&target, ServerRuntime::NeedsArchive { error: None }, true).is_err());
    }

    #[test]
    fn explains_failures() {
        // The language stays the default one: other tests read it at the same time.
        let target = Destination { destination: "box".into(), port: None };
        let message = failure(&target, Some(255), "Warning: Permanently added x\nuser@box: Permission denied (publickey).").to_string();
        assert!(message.starts_with(&crate::locale::localized("box 拒绝了登录。", "box refused the login.")), "{message}");
        assert!(message.ends_with("Permission denied (publickey)."));
        assert!(!message.contains("Permanently added"));
        crate::locale::set_chinese(true);
        assert!(login_refused("user@box: Permission denied (publickey,password)."));
        assert!(login_refused("Permission denied, please try again.\nuser@box: Permission denied (password)."));
        // A file the installer could not write is no refused login, and says where it failed.
        let traceback = "Traceback (most recent call last):\n  File \"<string>\", line 1\nPermissionError: [Errno 13] Permission denied: '/home/u/.rainy-agent'";
        assert!(!login_refused(traceback));
        let message = failure(&target, Some(1), traceback).to_string();
        assert!(message.ends_with("PermissionError: [Errno 13] Permission denied: '/home/u/.rainy-agent'"), "{message}");
        assert!(message.starts_with("Rainy 在 box 上") || message.starts_with("Rainy failed to install"), "{message}");
    }

    #[test]
    fn parses_recursive_include_globs_quotes_comments_and_summaries() {
        let root = tempfile::tempdir().unwrap();
        let ssh_dir = root.path().join("ssh");
        let conf_d = ssh_dir.join("conf.d");
        let spaced = ssh_dir.join("my hosts");
        std::fs::create_dir_all(&conf_d).unwrap();
        std::fs::create_dir_all(&spaced).unwrap();

        // Main config includes glob, quoted path with spaces, and a wildcard fallback block.
        let main_config = ssh_dir.join("config");
        std::fs::write(
            &main_config,
            r#"
# Top comment
Include conf.d/*.conf "my hosts/extra #1.conf" # trailing comment
Host=direct-box
  HostName=10.1.1.9
  Port=2200

Host *
  User fallback-user
  Port 2222
"#,
        )
        .unwrap();

        std::fs::write(
            conf_d.join("01-gpu.conf"),
            r#"
Host leaper
  HostName 192.168.10.20
  User root
  Port 22022
  ProxyJump bastion # jump host

Match host leaper
  User should-not-override
"#,
        )
        .unwrap();

        std::fs::write(
            conf_d.join("02-r5090.conf"),
            r#"
Host r5090 5090
  HostName 100.64.0.5
"#,
        )
        .unwrap();

        std::fs::write(
            spaced.join("extra #1.conf"),
            r#"
Host ncu-remote
  HostName 172.16.0.8
  User researcher
"#,
        )
        .unwrap();

        let summaries = parse_ssh_config(&main_config);
        let aliases: Vec<&str> = summaries.iter().map(|s| s.alias.as_str()).collect();
        assert_eq!(aliases, ["leaper", "r5090", "5090", "ncu-remote", "direct-box"]);

        let leaper = &summaries[0];
        assert_eq!(leaper.host_name.as_deref(), Some("192.168.10.20"));
        assert_eq!(leaper.user.as_deref(), Some("root"));
        assert_eq!(leaper.port, Some(22022));
        assert_eq!(leaper.proxy_jump.as_deref(), Some("bastion"));

        // r5090 inherits User & Port from the trailing `Host *` block.
        let r5090 = &summaries[1];
        assert_eq!(r5090.host_name.as_deref(), Some("100.64.0.5"));
        assert_eq!(r5090.user.as_deref(), Some("fallback-user"));
        assert_eq!(r5090.port, Some(2222));

        // direct-box keeps its first-wins Port 2200 while inheriting User.
        let direct = &summaries[4];
        assert_eq!(direct.host_name.as_deref(), Some("10.1.1.9"));
        assert_eq!(direct.port, Some(2200));
        assert_eq!(direct.user.as_deref(), Some("fallback-user"));
    }

    #[test]
    fn include_stops_cycles_depth_overflow_and_path_traversal() {
        let root = tempfile::tempdir().unwrap();
        let ssh_dir = root.path().join("ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();

        // Secret file outside ssh_dir that must never be included via `..` or cross-root absolute path.
        let outside = root.path().join("secret.conf");
        std::fs::write(&outside, "Host leaked-host\n  HostName 1.2.3.4\n").unwrap();

        // Cycle: a.conf -> b.conf -> a.conf
        std::fs::write(ssh_dir.join("a.conf"), "Include b.conf\nHost alpha\n").unwrap();
        std::fs::write(ssh_dir.join("b.conf"), "Include a.conf\nHost beta\n").unwrap();

        // Deep chain exceeding MAX_INCLUDE_DEPTH (8): d0 -> d1 -> ... -> d10
        for depth in 0..=10 {
            let mut body = format!("Host depth-{depth}\n");
            if depth < 10 {
                body.push_str(&format!("Include d{}.conf\n", depth + 1));
            }
            std::fs::write(ssh_dir.join(format!("d{depth}.conf")), body).unwrap();
        }

        // Absolute path inside ssh_dir is allowed; outside ssh_dir or with `..` is rejected.
        let inside_abs_file = ssh_dir.join("inside-abs.conf");
        std::fs::write(&inside_abs_file, "Host inside-abs-ok\n").unwrap();
        let inside_abs = inside_abs_file.display().to_string();

        let outside_abs = outside.display().to_string();
        let main_config = ssh_dir.join("config");
        std::fs::write(
            &main_config,
            format!("Include ../secret.conf\nInclude \"{outside_abs}\"\nInclude \"{inside_abs}\"\nInclude a.conf\nInclude d0.conf\nHost local-ok\n"),
        )
        .unwrap();

        let summaries = parse_ssh_config(&main_config);
        let aliases: Vec<&str> = summaries.iter().map(|s| s.alias.as_str()).collect();
        assert!(!aliases.contains(&"leaked-host"), "traversal must be rejected: {aliases:?}");
        assert!(aliases.contains(&"inside-abs-ok"));
        assert!(aliases.contains(&"alpha"));
        assert!(aliases.contains(&"beta"));
        assert!(aliases.contains(&"depth-0"));
        assert!(aliases.contains(&"depth-7"));
        assert!(!aliases.contains(&"depth-9"), "depth beyond limit must be capped: {aliases:?}");
        assert!(aliases.contains(&"local-ok"));
    }

    #[test]
    fn include_preserves_enclosing_host_options_and_allows_shared_reuse() {
        let root = tempfile::tempdir().unwrap();
        let ssh_dir = root.path().join("ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();

        std::fs::write(ssh_dir.join("shared.conf"), "ProxyJump lab-jump\n").unwrap();
        std::fs::write(ssh_dir.join("top.conf"), "Host top-box\n  HostName 10.0.0.1\n").unwrap();
        let main_config = ssh_dir.join("config");
        std::fs::write(
            &main_config,
            r#"
Include top.conf
Port 2201

Host alpha
  Include shared.conf
  User alpha-user
  HostName 10.0.0.2

Host beta
  Include shared.conf
  User beta-user
  HostName 10.0.0.3
"#,
        )
        .unwrap();

        let summaries = parse_ssh_config(&main_config);
        let by_alias = |name: &str| summaries.iter().find(|s| s.alias == name).cloned().unwrap();

        // Top-level option after Include still applies as a global default (`Host *`).
        let top = by_alias("top-box");
        assert_eq!(top.port, Some(2201));

        // Both Host blocks reuse `shared.conf` and keep options written after `Include`.
        let alpha = by_alias("alpha");
        assert_eq!(alpha.proxy_jump.as_deref(), Some("lab-jump"));
        assert_eq!(alpha.user.as_deref(), Some("alpha-user"));
        assert_eq!(alpha.host_name.as_deref(), Some("10.0.0.2"));
        assert_eq!(alpha.port, Some(2201));

        let beta = by_alias("beta");
        assert_eq!(beta.proxy_jump.as_deref(), Some("lab-jump"));
        assert_eq!(beta.user.as_deref(), Some("beta-user"));
        assert_eq!(beta.host_name.as_deref(), Some("10.0.0.3"));
        assert_eq!(beta.port, Some(2201));
    }

    #[test]
    fn profile_forwards_env_keepalive_and_keeps_host_of_intact() {
        use std::collections::BTreeMap;
        let target = Destination { destination: "leaper".into(), port: Some(2222) };
        let profile = SshHostProfile {
            rainy_home: Some("/root/kube-user/.rainy-agent".into()),
            env: BTreeMap::from([("HF_ENDPOINT".into(), "https://hf-mirror.com".into())]),
            keepalive_interval: Some(7),
            keepalive_count_max: Some(5),
            notes: Some("2号卡常被占".into()),
            ..Default::default()
        };
        let command = host_command_with_profile(
            &target,
            "/root/kube-user/.rainy-agent/runtime/abc/rainy-host",
            &[("RAINY_EXECUTION_TARGET_ID".into(), "ssh:leaper:2222".into())],
            5432,
            &[],
            Some(&profile),
        )
        .unwrap();
        let args: Vec<String> = command.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.windows(2).any(|pair| pair == ["-o", "ServerAliveInterval=7"]));
        assert!(args.windows(2).any(|pair| pair == ["-o", "ServerAliveCountMax=5"]));
        assert_eq!(
            host_of(&command),
            Some(RemoteHost {
                target: target.clone(),
                host: "/root/kube-user/.rainy-agent/runtime/abc/rainy-host".into(),
            })
        );
        // Decode launcher JSON payload and verify RAINY_HOME / RAINY_SSH_NOTES / RAINY_SSH_EXTRA_ENV / RAINY_PORT.
        let sep = args.iter().position(|arg| arg == "--").unwrap();
        let params_b64 = &args[sep + 6];
        let engine = base64::engine::general_purpose::STANDARD;
        let params: Value = serde_json::from_slice(&engine.decode(params_b64).unwrap()).unwrap();
        assert_eq!(params["env"]["RAINY_HOME"], "/root/kube-user/.rainy-agent");
        assert_eq!(params["env"]["RAINY_SSH_NOTES"], "2号卡常被占");
        assert_eq!(params["env"]["RAINY_PORT"], "5432");
        let extra: BTreeMap<String, String> = serde_json::from_str(params["env"]["RAINY_SSH_EXTRA_ENV"].as_str().unwrap()).unwrap();
        assert_eq!(extra.get("HF_ENDPOINT").map(String::as_str), Some("https://hf-mirror.com"));

        // Installer arguments receive `--home` only when `rainy_home` is set.
        let home = installer_home(Some(&profile));
        assert_eq!(home.as_deref(), Some("/root/kube-user/.rainy-agent"));
        assert_eq!(
            installer_args(home.as_deref(), "--probe", "{}", &["--prune"]),
            ["--home", "/root/kube-user/.rainy-agent", "--probe", "{}", "--prune"]
        );
        assert_eq!(installer_args(None, "--probe", "{}", &[]), ["--probe", "{}"]);
    }
}
