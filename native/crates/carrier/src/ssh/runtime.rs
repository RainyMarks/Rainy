//! Remote runtime probe, installation, pruning, Host launch/reconnect, and remote process-tree force-stop over SSH.

use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::{Map, Value, json};

use super::exec::{HOST_LAUNCHER, LOADER, RunOutput, arguments_with_profile, executable, failure, login_refused, remote_python, run_batch, run_with_profile};
use super::{Destination, askpass_scope, name, valid_destination};
use crate::HostCommand;
use crate::locale::localized;
use crate::targets::SshHostProfile;
use crate::wsl::{Installed, RuntimeFiles};

/// Upper bound on remote runtime installation (upload/fetch + unpack) so a broken connection never hangs forever.
const INSTALLER_TIMEOUT: Duration = Duration::from_secs(30 * 60);

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

pub(super) fn probe_answer(stdout: &str) -> Result<RuntimeProbe> {
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

pub(super) fn installer_home(profile: Option<&SshHostProfile>) -> Option<String> {
    let profile = profile?;
    profile
        .host_env_vars()
        .into_iter()
        .find_map(|(key, value)| (key == "RAINY_HOME").then_some(value))
}

pub(super) fn installer_args<'a>(home: Option<&'a str>, mode: &'a str, metadata: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = Vec::with_capacity(extra.len() + 4);
    if let Some(home) = home {
        args.extend(["--home", home]);
    }
    args.extend([mode, metadata]);
    args.extend_from_slice(extra);
    args
}

/// Probe the server without streaming `linux-app.tar.gz` (`install-runtime.py --probe`).
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

fn reading(path: &Path) -> String {
    localized(&format!("无法读取 {}", path.display()), &format!("Reading {}", path.display()))
}

fn opening(path: &Path) -> String {
    localized(&format!("无法打开 {}", path.display()), &format!("Opening {}", path.display()))
}

/// The last `bytes` bytes of `text` or a little fewer, starting at a character.
pub(super) fn tail(text: &str, bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

pub(super) fn installer_answer(stdout: &str) -> Result<ServerRuntime> {
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
    let output = run_with_profile(target, &remote_python(&script, &args), env, profile, input, on_line, Some(INSTALLER_TIMEOUT))?;
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
/// `pieces` (`--fetch`; the application layer goes over standard input).
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
    let chain = std::fs::File::open(&files.app)?.chain(std::fs::File::open(base).with_context(|| opening(base))?);
    let (tx, rx) = mpsc::channel::<(u64, u64)>();
    let counted = Counted {
        inner: chain,
        done: 0,
        total: size,
        report: Box::new(move |done, total| {
            let _ = tx.send((done, total));
        }),
    };
    thread::scope(|scope| {
        scope.spawn(move || {
            while let Ok((done, total)) = rx.recv() {
                progress(InstallProgress::Uploading { done, total });
            }
        });
        installer(target, files, "--receive", &[], env, profile, Some(Box::new(counted)), &mut progress_lines(&name, progress, log))
    })
}

/// The Host's paths from the installer's answer, or why there are none.
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
    Ok(value
        .get("removed")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect())
        .unwrap_or_default())
}

/// OpenSSH single-letter flags that consume the next argv token as their value.
fn takes_option_value(flag: &str) -> bool {
    matches!(
        flag,
        "-B" | "-b" | "-c" | "-D" | "-E" | "-e" | "-F" | "-I" | "-i" | "-J" | "-L" | "-l" | "-m" | "-O" | "-o" | "-p" | "-Q" | "-R" | "-S" | "-W" | "-w"
    )
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
    while i < sep {
        if args[i] == "-p" && i + 1 < sep {
            port = args[i + 1].parse::<u16>().ok().filter(|p| *p != 22);
            i += 2;
        } else if takes_option_value(args[i]) && i + 1 < sep {
            i += 2;
        } else {
            i += 1;
        }
    }
    let destination = args.get(sep + 1)?;
    if !valid_destination(destination) {
        return None;
    }
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

fn force_stop_with_profile(target: &Destination, host: &str, pid: u32, profile: Option<&SshHostProfile>) -> Result<()> {
    let pid_text = pid.to_string();
    let output = run_batch(
        target,
        &remote_python(crate::wsl::FORCE_STOP, &[&pid_text, host]),
        10,
        Some(Duration::from_secs(15)),
        profile,
    )?;
    match output.code {
        Some(0) | Some(3) => Ok(()),
        _ => Err(failure(target, output.code, &output.stderr)),
    }
}

/// Best-effort non-interactive (`BatchMode=yes`) remote `/proc`-verified SIGKILL of an SSH Host process tree when
/// graceful stop timed out or startup failed after launch. Never triggers an interactive askpass/OTP prompt.
pub fn force_stop(target: &Destination, host: &str, pid: u32) -> Result<()> {
    force_stop_with_profile(target, host, pid, None)
}

pub(super) fn is_port_conflict_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("forwarding")
        || lower.contains("address already in use")
        || lower.contains("已被使用")
        || lower.contains("已被占用")
        || lower.contains("端口转发")
}

/// Start (or restart) an SSH Host process with automatic loopback port retry (up to 3 attempts).
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
        let session = askpass.session(&askpass_scope(target));
        clear_recent();
        let port = free_loopback_port()?;
        let command = host_command_with_profile(target, host, env, port, session.env(), profile)?;
        match crate::HostProcess::start(&command, events.clone()) {
            Ok(process) => return Ok(process),
            Err(error) => {
                let said = recent_output();
                if attempt < 3 && is_port_conflict_error(&format!("{said}\n{error:#}")) {
                    continue;
                }
                if login_refused(&said) {
                    session.failed();
                }
                return Err(if let Some(stopped) = error.downcast_ref::<crate::HostStopped>() {
                    failure(target, stopped.0, &said)
                } else {
                    error
                });
            }
        }
    }
}

/// Reconnect to an SSH server in-place without restarting `RainyAgent.exe`.
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
        && let Err(error) = force_stop_with_profile(target, prev_host, prev_pid, profile)
    {
        log(&format!("Best-effort SSH force-stop before reconnect skipped: {error:#}"));
    }
    let session = askpass.session(&askpass_scope(target));
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

/// The Host on the server: its port is forwarded to the same loopback port here.
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
