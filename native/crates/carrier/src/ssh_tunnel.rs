//! Dynamic standalone SSH local port forwarding (`-L`) manager for remote services
//! (TensorBoard, Jupyter, Gradio, Web UIs) without interrupting the primary SSH Host connection.

use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::askpass::{Askpass, Session};
use crate::locale::localized;
use crate::ssh::{self, Destination};
pub use crate::targets::PortForwardRule;
use crate::targets::{DEFAULT_KEEPALIVE_COUNT_MAX, DEFAULT_KEEPALIVE_INTERVAL, SshHostProfile};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const STARTUP_CHECK_WINDOW: Duration = Duration::from_millis(350);

/// Lifecycle state of the standalone SSH port-forwarding process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelState {
    Stopped,
    Active,
    Error(String),
}

/// Snapshot of currently configured forwarding rules and tunnel state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelStatus {
    pub target: Option<Destination>,
    pub rules: Vec<PortForwardRule>,
    pub state: TunnelState,
}

/// Build validated `ssh -N -T ... -L ... -- <destination>` arguments for one or more forwarding rules.
///
/// Every option is placed strictly before `--`, local bindings are pinned to `127.0.0.1`, and duplicate local ports
/// or unsafe host tokens are rejected before spawning `ssh`.
pub fn tunnel_arguments(target: &Destination, rules: &[PortForwardRule], keepalive: (u16, u8)) -> Result<Vec<String>> {
    let target = ssh::parse_destination(&target.destination, target.port.map(u32::from))?;
    if rules.is_empty() {
        bail!(localized("至少需要一条端口转发规则。", "At least one port forwarding rule is required."));
    }
    let normalized = normalize_and_check_rules(rules)?;
    let interval = keepalive.0.clamp(1, 600);
    let count = keepalive.1.clamp(1, 60);
    let mut args = vec![
        "-N".to_owned(),
        "-T".to_owned(),
        "-o".to_owned(),
        "ConnectTimeout=20".to_owned(),
        "-o".to_owned(),
        "ExitOnForwardFailure=yes".to_owned(),
        "-o".to_owned(),
        format!("ServerAliveInterval={interval}"),
        "-o".to_owned(),
        format!("ServerAliveCountMax={count}"),
    ];
    args.extend(ssh::config_options(&target));
    for rule in &normalized {
        args.extend([
            "-L".to_owned(),
            format!("127.0.0.1:{}:{}:{}", rule.local_port, rule.remote_host, rule.remote_port),
        ]);
    }
    args.extend(["--".to_owned(), target.destination]);
    Ok(args)
}

fn normalize_and_check_rules(rules: &[PortForwardRule]) -> Result<Vec<PortForwardRule>> {
    let mut seen_local = HashSet::with_capacity(rules.len());
    let mut normalized = Vec::with_capacity(rules.len());
    for rule in rules {
        let clean = rule.normalized()?;
        if !seen_local.insert(clean.local_port) {
            bail!(localized(
                &format!("本地端口 {} 在转发规则中重复出现。", clean.local_port),
                &format!("Local port {} appears more than once in the forwarding rules.", clean.local_port),
            ));
        }
        normalized.push(clean);
    }
    Ok(normalized)
}

fn check_local_ports(rules: &[PortForwardRule]) -> Result<()> {
    for rule in rules {
        if let Err(error) = TcpListener::bind(("127.0.0.1", rule.local_port)) {
            if error.kind() == std::io::ErrorKind::AddrInUse {
                bail!(localized(
                    &format!("本地端口 {} 已被占用，请更换本地端口。", rule.local_port),
                    &format!("Local port {} is already in use; choose another local port.", rule.local_port),
                ));
            }
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                bail!(localized(
                    &format!("没有权限绑定本地端口 {}，请改用 1024 以上的端口。", rule.local_port),
                    &format!("Permission denied binding local port {}; choose a port above 1024.", rule.local_port),
                ));
            }
            return Err(error).with_context(|| {
                localized(
                    &format!("无法绑定本地端口 {}", rule.local_port),
                    &format!("Binding local port {}", rule.local_port),
                )
            });
        }
    }
    Ok(())
}

fn terminate_child(mut child: Child) {
    ssh::kill_process_tree(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

struct TunnelInner {
    generation: u64,
    child: Option<Child>,
    target: Option<Destination>,
    rules: Vec<PortForwardRule>,
    state: TunnelState,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

/// Manages one lightweight standalone `ssh -N` child process for dynamic `-L` port forwarding.
///
/// Applying new rules smoothly restarts only this lightweight tunnel child without touching the primary Host tunnel.
/// Dropping the manager forcefully cleans up the child process tree.
pub struct TunnelManager {
    askpass: Option<Arc<Askpass>>,
    inner: Arc<Mutex<TunnelInner>>,
}

impl std::fmt::Debug for TunnelManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let status = self.status();
        f.debug_struct("TunnelManager")
            .field("target", &status.target)
            .field("rules", &status.rules.len())
            .field("state", &status.state)
            .finish()
    }
}

impl TunnelManager {
    pub fn new(askpass: Option<Arc<Askpass>>) -> Self {
        Self {
            askpass,
            inner: Arc::new(Mutex::new(TunnelInner {
                generation: 0,
                child: None,
                target: None,
                rules: Vec::new(),
                state: TunnelState::Stopped,
                stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
            })),
        }
    }

    /// Replace the active forwarding rules for `target`. Passing an empty list stops the tunnel cleanly.
    pub fn apply_rules(&self, target: &Destination, rules: Vec<PortForwardRule>, profile: Option<&SshHostProfile>) -> Result<TunnelStatus> {
        if rules.is_empty() {
            self.stop_all();
            return Ok(self.status());
        }
        let target = ssh::parse_destination(&target.destination, target.port.map(u32::from))?;
        let normalized = normalize_and_check_rules(&rules)?;
        let keepalive = profile.map_or((DEFAULT_KEEPALIVE_INTERVAL, DEFAULT_KEEPALIVE_COUNT_MAX), SshHostProfile::keepalive);
        let args = tunnel_arguments(&target, &normalized, keepalive)?;

        // Stop any existing forwarding child first so ports owned by our previous rules are released before preflight.
        self.stop_all();

        if let Err(error) = check_local_ports(&normalized) {
            let message = format!("{error:#}");
            let mut inner = self.inner.lock().unwrap();
            inner.target = Some(target);
            inner.rules = normalized;
            inner.state = TunnelState::Error(message);
            return Err(error);
        }

        let session: Option<Session> = self.askpass.as_ref().map(|askpass| askpass.session());
        let mut command = Command::new(ssh::executable()?);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(session) = &session {
            command.envs(session.env().iter().map(|(name, value)| (name, value)));
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command.spawn().with_context(|| localized("无法启动 SSH 端口转发进程。", "Starting the SSH port forwarding process."))?;
        let stderr = child.stderr.take();
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let generation = {
            let mut inner = self.inner.lock().unwrap();
            inner.generation += 1;
            inner.child = Some(child);
            inner.target = Some(target.clone());
            inner.rules = normalized;
            inner.state = TunnelState::Active;
            inner.stderr_tail = stderr_tail.clone();
            inner.generation
        };

        let (eof_tx, eof_rx) = mpsc::channel::<()>();
        if let Some(stderr) = stderr {
            let inner_ref = self.inner.clone();
            let target_ref = target.clone();
            let tail_ref = stderr_tail.clone();
            let _ = thread::Builder::new().name("ssh-tunnel-stderr".into()).spawn(move || {
                let mut reader = BufReader::new(stderr);
                let mut line = Vec::new();
                while reader.read_until(b'\n', &mut line).map(|n| n > 0).unwrap_or(false) {
                    let text = String::from_utf8_lossy(&line).trim_end().to_owned();
                    if !text.is_empty() {
                        let mut tail = tail_ref.lock().unwrap();
                        tail.push_back(text);
                        if tail.len() > 20 {
                            tail.pop_front();
                        }
                    }
                    line.clear();
                }
                // Pipe closed: process has exited (or is exiting). Report unexpected termination for this generation.
                let child_to_reap: Option<Child> = {
                    let mut inner = inner_ref.lock().unwrap();
                    if inner.generation != generation {
                        return;
                    }
                    inner.child.take()
                };
                let code = child_to_reap.and_then(|mut child| child.wait().ok().and_then(|status| status.code()));
                let said = Vec::from(tail_ref.lock().unwrap().clone()).join("\n");
                if ssh::login_refused(&said)
                    && let Some(session) = &session
                {
                    session.failed();
                }
                let reason = format_tunnel_failure(&target_ref, code, &said);
                let mut inner = inner_ref.lock().unwrap();
                if inner.generation == generation && inner.state == TunnelState::Active {
                    log::warn!("SSH port forwarding tunnel to {} stopped: {reason}", ssh::name(&target_ref));
                    inner.state = TunnelState::Error(reason);
                }
                let _ = eof_tx.send(());
            });
        } else {
            drop(eof_tx);
        }

        // Wait briefly for immediate startup failure (e.g. ExitOnForwardFailure or invalid host); when the stderr
        // thread signals EOF, the full stderr tail and TunnelState::Error have already been recorded without races.
        if eof_rx.recv_timeout(STARTUP_CHECK_WINDOW).is_ok() {
            let status = self.status();
            if let TunnelState::Error(message) = status.state {
                return Err(anyhow!(message));
            }
            return Ok(status);
        }

        Ok(self.status())
    }

    /// Current forwarding status; polls the active child non-blockingly so exited tunnels are reflected promptly.
    pub fn status(&self) -> TunnelStatus {
        let mut inner = self.inner.lock().unwrap();
        if inner.state == TunnelState::Active
            && let Some(status) = inner.child.as_mut().and_then(|child| child.try_wait().ok().flatten())
        {
            inner.child = None;
            let said = Vec::from(inner.stderr_tail.lock().unwrap().clone()).join("\n");
            let reason = match &inner.target {
                Some(target) => format_tunnel_failure(target, status.code(), &said),
                None => localized("SSH 端口转发已意外退出。", "SSH port forwarding exited unexpectedly."),
            };
            inner.state = TunnelState::Error(reason);
        }
        TunnelStatus { target: inner.target.clone(), rules: inner.rules.clone(), state: inner.state.clone() }
    }

    /// Stop the active forwarding child process tree and clear all active rules.
    pub fn stop_all(&self) {
        let child = {
            let mut inner = self.inner.lock().unwrap();
            inner.generation += 1;
            inner.target = None;
            inner.rules.clear();
            inner.state = TunnelState::Stopped;
            inner.child.take()
        };
        if let Some(child) = child {
            terminate_child(child);
        }
    }
}

impl Drop for TunnelManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}

fn format_tunnel_failure(target: &Destination, code: Option<i32>, stderr: &str) -> String {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("could not request local forwarding") || lower.contains("port forwarding failed") || lower.contains("cannot listen to port") {
        let base = localized(
            "SSH 端口转发绑定失败，本地端口可能已被占用。",
            "SSH local port forwarding failed; the local port may already be in use.",
        );
        let tail = stderr.lines().map(str::trim).filter(|line| !line.is_empty()).last().unwrap_or_default();
        return if tail.is_empty() { base } else { format!("{base}\n{tail}") };
    }
    format!("{:#}", ssh::failure(target, code, stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_isolated_arguments_and_blocks_injection() {
        let target = Destination { destination: "leaper".into(), port: Some(2222) };
        let rules = vec![
            PortForwardRule {
                id: "tb".into(),
                local_port: 6006,
                remote_port: 6006,
                remote_host: "127.0.0.1".into(),
                label: Some("TensorBoard".into()),
                auto_start: true,
            },
            PortForwardRule {
                id: "jup".into(),
                local_port: 8888,
                remote_port: 8889,
                remote_host: "localhost".into(),
                label: Some("Jupyter".into()),
                auto_start: false,
            },
        ];
        let args = tunnel_arguments(&target, &rules, (12, 4)).unwrap();
        let sep = args.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(args[sep + 1], "leaper");
        assert_eq!(args.len(), sep + 2, "nothing may follow the destination after --");
        assert!(args[..sep].contains(&"-N".to_owned()));
        assert!(args[..sep].contains(&"-T".to_owned()));
        assert!(args[..sep].windows(2).any(|p| p == ["-o", "ExitOnForwardFailure=yes"]));
        assert!(args[..sep].windows(2).any(|p| p == ["-o", "ServerAliveInterval=12"]));
        assert!(args[..sep].windows(2).any(|p| p == ["-o", "ServerAliveCountMax=4"]));
        assert!(args[..sep].windows(2).any(|p| p == ["-p", "2222"]));
        assert!(args[..sep].windows(2).any(|p| p == ["-L", "127.0.0.1:6006:127.0.0.1:6006"]));
        assert!(args[..sep].windows(2).any(|p| p == ["-L", "127.0.0.1:8888:localhost:8889"]));

        // Reject destination injection starting with `-`.
        let bad_target = Destination { destination: "-oProxyCommand=calc".into(), port: None };
        assert!(tunnel_arguments(&bad_target, &rules, (10, 3)).is_err());

        // Reject duplicate local ports.
        let dup = vec![PortForwardRule::new(6006, 6006), PortForwardRule::new(6006, 6007)];
        assert!(tunnel_arguments(&target, &dup, (10, 3)).is_err());

        // Reject unsafe remote_host.
        let bad_host = vec![PortForwardRule { remote_host: "127.0.0.1 -oFoo=bar".into(), ..PortForwardRule::new(6006, 6006) }];
        assert!(tunnel_arguments(&target, &bad_host, (10, 3)).is_err());

        // Reject empty rule list for tunnel_arguments directly.
        assert!(tunnel_arguments(&target, &[], (10, 3)).is_err());
    }

    #[test]
    fn detects_local_port_conflicts_and_handles_empty_apply() {
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = occupied.local_addr().unwrap().port();

        let manager = TunnelManager::new(None);
        assert_eq!(manager.status().state, TunnelState::Stopped);

        let target = Destination { destination: "leaper".into(), port: None };
        let err = manager.apply_rules(&target, vec![PortForwardRule::new(port, 6006)], None).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(&port.to_string()), "error should name occupied port: {msg}");
        assert!(matches!(manager.status().state, TunnelState::Error(_)));

        // Empty rules clear state back to Stopped cleanly.
        let cleared = manager.apply_rules(&target, Vec::new(), None).unwrap();
        assert_eq!(cleared.state, TunnelState::Stopped);
        assert!(cleared.rules.is_empty());
        assert!(cleared.target.is_none());
    }
}
