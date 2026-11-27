//! The Windows OpenSSH client for SSH execution targets (`src/main/ssh.ts`, `SshHostTransport` and `prepareSsh`):
//! destinations, arguments, the remote Python loader, runs, the runtime installer on the server and the Host command.

mod config;
mod exec;
mod runtime;
#[cfg(test)]
mod tests;

use std::path::Path;

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

use crate::locale::localized;
use crate::targets::{self, ExecutionTarget};

pub use self::config::{SshHostSummary, config_host_summaries, config_hosts, parse_ssh_config};
pub use self::exec::{
    HOST_LAUNCHER, RunOutput, arguments, arguments_with_profile, executable, failure, login_refused, probe, remote_python, run,
};
pub(crate) use self::exec::config_options;
pub(crate) use self::runtime::host_of;
pub use self::runtime::{
    InstallProgress, ReconnectOutcome, RemoteHost, RuntimeProbe, ServerRuntime, fetch_runtime, fetch_runtime_with_profile,
    force_stop, free_loopback_port, host_command, host_command_with_profile, installed_paths, probe_runtime,
    probe_runtime_with_profile, prune_runtimes, prune_runtimes_with_profile, receive_runtime, receive_runtime_with_profile,
    reconnect_host, reconnect_host_with_profile, start_host_process, start_host_process_with_profile, unsupported_error,
};

/// Where `ssh` connects: a `~/.ssh/config` alias or `[user@]host`, and a port other than 22.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub destination: String,
    pub port: Option<u16>,
}

/// The askpass scope of `target`: answers typed for one server are only reused for that same destination and port.
pub fn askpass_scope(target: &Destination) -> String {
    format!("{}:{}", target.destination, target.port.unwrap_or(22))
}

/// `[user@]host` or an alias, never starting with `-`, so `ssh` cannot read it as an option.
pub(super) fn valid_destination(text: &str) -> bool {
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

fn split_inline_port(text: &str) -> (&str, Option<&str>) {
    // Handle `[ipv6]:port` and `user@[ipv6]:port`.
    if let Some(close) = text.rfind(']') {
        let after = &text[close + 1..];
        if let Some(digits) = after.strip_prefix(':')
            && !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
        {
            return (&text[..=close], Some(digits));
        }
        return (text, None);
    }
    if let Some((host, digits)) = text.rsplit_once(':')
        && !host.is_empty()
        && !host.contains(':')
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
    {
        return (host, Some(digits));
    }
    (text, None)
}

/// What the user typed: `user@host`, `host`, `user@host:port`, `[ipv6]:port`, `ssh user@host` or a config alias.
pub fn parse_destination(text: &str, port: Option<u32>) -> Result<Destination> {
    let mut destination = text.trim();
    if destination.len() >= 4 && destination[..3].eq_ignore_ascii_case("ssh") && destination.as_bytes()[3].is_ascii_whitespace() {
        destination = destination[3..].trim_start();
    }
    let mut parsed = port;
    let (host_part, inline_digits) = split_inline_port(destination);
    if let Some(digits) = inline_digits {
        destination = host_part;
        if parsed.is_none() {
            let inline_port = digits.parse::<u32>().ok().filter(|value| (1..=65535).contains(value));
            let Some(inline_port) = inline_port else {
                bail!(localized("SSH 端口必须是 1 到 65535 之间的整数。", "The SSH port must be a whole number from 1 to 65535."));
            };
            parsed = Some(inline_port);
        }
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
    ExecutionTarget::Ssh {
        id: format!("ssh:{name}"),
        label: format!("SSH · {name}"),
        destination: destination.destination.clone(),
        port: destination.port,
    }
}

/// Where an SSH target connects.
pub fn destination_of(target: &ExecutionTarget) -> Result<Destination> {
    match target {
        ExecutionTarget::Ssh { destination, port, .. } => parse_destination(destination, port.map(u32::from)),
        _ => bail!(localized("所选执行环境不是 SSH 主机。", "The selected execution environment is not an SSH host.")),
    }
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

/// Remember a host added in Settings (`saveSshHost`), deduplicating by normalized target id.
pub fn save_host(path: &Path, destination: &Destination) -> Result<()> {
    let canonical_dest = parse_destination(&destination.destination, destination.port.map(u32::from))?;
    let wanted_id = target(&canonical_dest).id().to_owned();
    targets::update_preferences(path, |preferences| {
        let mut hosts: Vec<Value> = saved_entries(preferences)
            .into_iter()
            .filter(|host| {
                let text = host.get("destination").and_then(Value::as_str).unwrap_or_default();
                let port = host.get("port").and_then(Value::as_u64).map(|port| port.min(u64::from(u32::MAX)) as u32);
                parse_destination(text, port).is_ok_and(|existing| target(&existing).id() != wanted_id)
            })
            .collect();
        let mut entry = json!({ "destination": canonical_dest.destination });
        if let Some(port) = canonical_dest.port {
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
