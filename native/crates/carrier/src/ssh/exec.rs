//! OpenSSH command construction, remote Python loader, subprocess execution, timeout containment, and error diagnostics.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;

use super::{Destination, name};
use crate::locale::localized;
use crate::process_win;
use crate::targets::{DEFAULT_KEEPALIVE_COUNT_MAX, DEFAULT_KEEPALIVE_INTERVAL, SshHostProfile};

const REMOTE_COMMAND_CACHE_TTL: Duration = Duration::from_secs(30);
const REMOTE_COMMAND_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The login shell of the server (sh, bash, zsh, fish or csh) reads the command, so everything it sees is base64 except
/// a fixed loader quoted with single quotes and free of `\`, `$` and `!`.
pub(super) const LOADER: &str = "'import base64,sys;a=[base64.b64decode(x).decode() for x in sys.argv[1:]];sys.argv=[\"rainy\"]+a[1:];exec(a[0])'";

/// Starts the Host on the server with the carrier's environment, from the home directory.
pub const HOST_LAUNCHER: &str = "import json,os,sys
p=json.loads(sys.argv[1])
os.chdir(os.path.expanduser(\"~\"))
env=dict(os.environ)
env.update(p[\"env\"])
os.execve(p[\"host\"],[p[\"host\"]],env)
";

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

/// Parse `ssh -G` output: `remotecommand none` means no remote command is configured.
pub(super) fn has_configured_remote_command(stdout: &str) -> bool {
    stdout.lines().any(|line| {
        let trimmed = line.trim();
        let Some(prefix) = trimmed.get(..14) else { return false };
        if !prefix.eq_ignore_ascii_case("remotecommand ") {
            return false;
        }
        let value = trimmed[14..].trim();
        !value.is_empty() && !value.eq_ignore_ascii_case("none")
    })
}

/// Whether the user's config gives `target` a `RemoteCommand` (a `tmux attach`, say), next to which ssh refuses the
/// command Rainy passes. Cached briefly per `(target, RAINY_SSH_CONFIG)` and bounded by a 5s timeout.
pub(super) fn configured_remote_command(target: &Destination) -> bool {
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
    let Ok(command) = ssh_command(&args, &[]) else { return false };
    run_command(command, None, &mut |_| {}, Some(REMOTE_COMMAND_PROBE_TIMEOUT))
        .is_ok_and(|output| output.code == Some(0) && has_configured_remote_command(&output.stdout))
}

#[cfg(test)]
pub(super) fn with_options(target: &Destination, remote: &[String], forward: Option<u16>, clear_remote_command: bool) -> Vec<String> {
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

pub(super) fn with_options_mode(
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

/// Remote command that runs a Python script on the server, which receives `args` unchanged in `sys.argv[1:]`.
pub fn remote_python(script: &str, args: &[&str]) -> Vec<String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let mut words = vec!["python3".to_owned(), "-c".to_owned(), LOADER.to_owned()];
    words.extend(std::iter::once(script).chain(args.iter().copied()).map(|value| engine.encode(value.as_bytes())));
    words
}

fn ssh_command(args: &[String], env: &[(String, String)]) -> Result<Command> {
    let mut command = Command::new(executable()?);
    command.args(args).envs(env.iter().map(|(name, value)| (name, value)));
    process_win::hide(&mut command);
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

pub(super) fn run_with_profile(
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
pub(super) fn run_batch(
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

pub(super) fn run_command(
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
    process_win::contain(&child);
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let feeder = thread::Builder::new().name("ssh-stdin".into()).spawn(move || {
        if let Some(mut input) = input {
            // A connection that closes early is reported by the exit code.
            let _ = std::io::copy(&mut input, &mut stdin).and_then(|_| stdin.flush());
        }
    });
    let collector = thread::Builder::new().name("ssh-stdout".into()).spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let (feeder, collector) = match (feeder, collector) {
        (Ok(feeder), Ok(collector)) => (feeder, collector),
        (feeder_res, collector_res) => {
            kill_process_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
            if let Ok(handle) = feeder_res {
                let _ = handle.join();
            }
            if let Ok(handle) = collector_res {
                let _ = handle.join();
            }
            bail!(localized("无法启动 ssh I/O 线程。", "Starting ssh I/O threads failed."));
        }
    };
    let child = Arc::new(Mutex::new(child));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    if let Some(timeout) = timeout {
        let child = child.clone();
        let _ = thread::Builder::new().name("ssh-timeout".into()).spawn(move || {
            if done_rx.recv_timeout(timeout) == Err(mpsc::RecvTimeoutError::Timeout) {
                let mut child = child.lock().unwrap();
                // Verify the child is still running before targeting its PID so a recycled PID is never killed.
                if child.try_wait().ok().flatten().is_none() {
                    kill_process_tree(child.id());
                    let _ = child.kill();
                }
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
    // Poll instead of waiting with the lock held, so the timeout can still stop an `ssh` that closed its output.
    let status = loop {
        if let Some(status) = child.lock().unwrap().try_wait()? {
            break status;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let _ = done_tx.send(());
    let _ = feeder.join();
    Ok(RunOutput { code: status.code(), stdout: String::from_utf8_lossy(&stdout).into_owned(), stderr: Vec::from(errors).join("\n") })
}

pub(crate) fn kill_process_tree(pid: u32) {
    process_win::kill_process_tree(pid);
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
