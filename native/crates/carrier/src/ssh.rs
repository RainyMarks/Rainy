//! The Windows OpenSSH client for SSH execution targets (`src/main/ssh.ts`, `SshHostTransport` and `prepareSsh`):
//! destinations, arguments, the remote Python loader, runs, the runtime installer on the server and the Host command.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::HostCommand;
use crate::locale::localized;
use crate::targets::{self, ExecutionTarget};
use crate::wsl::{Installed, RuntimeFiles};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
        _ => bail!("The selected execution target is not an SSH host"),
    }
}

/// Concrete host aliases of an OpenSSH config file; patterns with wildcards or negation are not hosts.
pub fn config_hosts(text: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some(keyword) = line.get(..4).filter(|keyword| keyword.eq_ignore_ascii_case("host")) else { continue };
        let rest = &line[keyword.len()..];
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        for name in rest.split_whitespace() {
            if name.contains(['*', '?', '!']) || !valid_destination(name) || hosts.iter().any(|known| known == name) {
                continue;
            }
            hosts.push(name.to_owned());
        }
    }
    hosts
}

/// The OpenSSH config file aliases are read from; development and tests point at their own.
fn config_path() -> Option<PathBuf> {
    match std::env::var_os("RAINY_SSH_CONFIG") {
        Some(path) => Some(PathBuf::from(path)),
        None => std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(|home| PathBuf::from(home).join(".ssh").join("config")),
    }
}

/// An SSH host Rainy offers, and whether it was added in Settings rather than read from `~/.ssh/config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub target: ExecutionTarget,
    pub saved: bool,
}

/// Hosts added in Settings, then the aliases of `~/.ssh/config`; a malformed entry is skipped (`sshTargets`).
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
    let config = config_path().and_then(|path| std::fs::read_to_string(path).ok()).unwrap_or_default();
    for alias in config_hosts(&config) {
        let target = target(&Destination { destination: alias, port: None });
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

/// Arguments of one `ssh` run; keep-alives notice a dropped connection within a minute (`sshArguments`).
pub fn arguments(target: &Destination, remote: &[String], forward: Option<u16>) -> Vec<String> {
    let mut args: Vec<String> = ["-T", "-o", "ConnectTimeout=20", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=4"].map(str::to_owned).to_vec();
    if let Some(config) = std::env::var_os("RAINY_SSH_CONFIG") {
        args.extend(["-F".to_owned(), config.to_string_lossy().into_owned()]);
    }
    if let Some(port) = target.port {
        args.extend(["-p".to_owned(), port.to_string()]);
    }
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
    let mut child = ssh_command(&arguments(target, remote, None), env)?
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Starting ssh")?;
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
                let _ = child.lock().unwrap().kill();
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
    /// The Host is installed: absolute paths of `node` and `host.js` on the server.
    Ready { host: String },
    /// The base of this version is missing there; `error` says why the server could not download it, when it tried.
    NeedsArchive { error: Option<String> },
    /// Not an x86_64 Linux server: its system and machine.
    Unsupported(String),
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
    }
}

/// Run the runtime installer on the server with `args` after the mode (`sshInstaller`).
fn installer(
    target: &Destination,
    files: &RuntimeFiles,
    mode: &str,
    extra: &[&str],
    env: &[(String, String)],
    input: Option<Box<dyn Read + Send>>,
    on_line: &mut dyn FnMut(&str),
) -> Result<std::result::Result<ServerRuntime, RunOutput>> {
    let script = std::fs::read_to_string(&files.installer).with_context(|| format!("Reading {}", files.installer.display()))?;
    let metadata = std::fs::read_to_string(&files.metadata).with_context(|| format!("Reading {}", files.metadata.display()))?;
    let mut args = vec![mode, metadata.as_str()];
    args.extend_from_slice(extra);
    let output = run(target, &remote_python(&script, &args), env, input, on_line, None)?;
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
    let name = name(target);
    let app = std::fs::File::open(&files.app).with_context(|| format!("Opening {}", files.app.display()))?;
    let pieces = pieces.to_string();
    installer(target, files, "--fetch", &[&pieces], env, Some(Box::new(app)), &mut progress_lines(&name, progress, log))
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
    let name = name(target);
    let size = std::fs::metadata(&files.app)?.len() + std::fs::metadata(base).with_context(|| format!("Reading {}", base.display()))?.len();
    // The application layer first, then the base.
    let chain = std::fs::File::open(&files.app)?.chain(std::fs::File::open(base).with_context(|| format!("Opening {}", base.display()))?);
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
        installer(target, files, "--receive", &[], env, Some(Box::new(counted)), &mut progress_lines(&name, progress, log))
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
pub fn prune_runtimes(target: &Destination, files: &RuntimeFiles, env: &[(String, String)]) -> Result<Vec<String>> {
    let script = std::fs::read_to_string(&files.installer)?;
    let metadata = std::fs::read_to_string(&files.metadata)?;
    let output = run(target, &remote_python(&script, &["--prune", &metadata]), env, None, &mut |_| {}, Some(Duration::from_secs(120)))?;
    if output.code != Some(0) {
        return Err(failure(target, output.code, &output.stderr));
    }
    let value: Value = serde_json::from_str(output.stdout.lines().last().unwrap_or("{}")).unwrap_or(Value::Null);
    Ok(value.get("removed").and_then(Value::as_array).map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect()).unwrap_or_default())
}

/// A loopback port free on this computer, used on both ends of the forward.
pub fn free_loopback_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// The Host on the server: its port is forwarded to the same loopback port here, so the page's origin matches the one
/// the Host checks; closing the session closes the Host's standard input, which stops it (`SshHostTransport`).
pub fn host_command(target: &Destination, host: &str, env: &[(String, String)], port: u16, askpass: &[(String, String)]) -> Result<HostCommand> {
    let mut variables: Map<String, Value> = env.iter().map(|(name, value)| (name.clone(), Value::String(value.clone()))).collect();
    variables.insert("RAINY_PORT".into(), Value::String(port.to_string()));
    let parameters = json!({ "host": host, "env": variables }).to_string();
    let args = arguments(target, &remote_python(HOST_LAUNCHER, &[&parameters]), Some(port));
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
        let text = "Host dev box\n  HostName 10.0.0.2\nHost *.internal !bad\nhost   prod\nHostName x\n";
        assert_eq!(config_hosts(text), ["dev", "box", "prod"]);
    }

    #[test]
    fn remote_words() {
        let words = remote_python("print(1)", &["a b"]);
        assert_eq!(&words[..3], ["python3", "-c", LOADER]);
        assert!(!LOADER[1..LOADER.len() - 1].contains(['\'', '\\', '$', '!']));
        let engine = base64::engine::general_purpose::STANDARD;
        assert_eq!(engine.decode(&words[4]).unwrap(), b"a b");
        let args = arguments(&Destination { destination: "box".into(), port: Some(2200) }, &words, Some(5000));
        assert!(args.windows(2).any(|pair| pair == ["-p", "2200"]));
        assert!(args.windows(2).any(|pair| pair == ["-L", "127.0.0.1:5000:127.0.0.1:5000"]));
        assert_eq!(args.iter().position(|arg| arg == "--").map(|at| &args[at + 1]), Some(&"box".to_owned()));
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
        let target = Destination { destination: "box".into(), port: None };
        crate::locale::set_chinese(false);
        let message = failure(&target, Some(255), "Warning: Permanently added x\nuser@box: Permission denied (publickey).").to_string();
        assert!(message.starts_with("box refused the login."));
        assert!(message.ends_with("Permission denied (publickey)."));
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
}
