//! Host lifecycle and its control channel (`src/main/transport.ts`, `src/host/control.ts`).
//!
//! The Host prints `RAINY_CONTROL {json}` lines on stdout: `ready` once it listens, `fatal` when it cannot start, and its
//! own requests (`credential-key`, `system-proxy`, `preview`), which the carrier answers with `{"type":"reply",…}` lines on
//! stdin. The carrier's requests (`inspect-activity`, `inspect-project`, `stop`) go the same way and are matched by id.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::locale::localized;

/// How long the Host may take to report readiness.
const START_DEADLINE: Duration = Duration::from_secs(90);
/// How long a control request waits for its answer.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
/// How long a stopping Host gets before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(8);

/// How to start one Host.
#[derive(Debug, Clone)]
pub struct HostCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Added to the inherited environment.
    pub env: Vec<(OsString, OsString)>,
    pub cwd: Option<PathBuf>,
    /// Drop inherited variables that look like secrets (the native Windows Host, whose commands inherit them).
    pub scrub_environment: bool,
    /// WSL and SSH report readiness before Windows loopback forwarding accepts connections; wait for it.
    pub wait_for_loopback: bool,
    /// An SSH Host must listen on the forwarded port.
    pub expected_port: Option<u16>,
}

/// Verified loopback readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostReady {
    /// `http://127.0.0.1:<port>/?token=…`, opened once to exchange the launch token for the session cookie.
    pub url: String,
    pub port: u16,
    pub pid: u32,
    pub home: String,
}

/// Answers to the Host's own requests, and what the carrier hears about it. Called on carrier threads.
pub trait HostEvents: Send + Sync + 'static {
    /// The base64 key that encrypts the Host's API keys, or `None` to leave them unencrypted for this run.
    fn credential_key(&self) -> Option<String> {
        None
    }
    /// The system proxy for one `http(s)` URL, as `http(s)://host:port`, or `None` for direct.
    fn system_proxy(&self, _url: &str) -> Option<String> {
        None
    }
    /// Carry out one preview-window command for the agent.
    fn preview(&self, _command: Value) -> Value {
        json!({ "error": "The preview window is unavailable." })
    }
    /// A line of Host output that is not a control message.
    fn diagnostic(&self, _line: &str) {}
    /// The Host process ended.
    fn exited(&self, _code: Option<i32>) {}
}

type Pending = Arc<Mutex<HashMap<String, mpsc::Sender<Value>>>>;

/// The Host ended before it reported readiness, with its exit code; an SSH start explains it from what `ssh` said.
#[derive(Debug)]
pub struct StoppedEarly(pub Option<i32>);

impl std::fmt::Display for StoppedEarly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = self.0.map(|code| code.to_string());
        f.write_str(&localized(
            &format!("执行后端在就绪前退出了（退出代码 {}）。", code.as_deref().unwrap_or("未知")),
            &format!("The execution Host stopped ({}).", code.as_deref().unwrap_or("?")),
        ))
    }
}

impl std::error::Error for StoppedEarly {}

/// A running Host.
pub struct HostProcess {
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    pending: Pending,
    ready: HostReady,
    exited: Arc<Mutex<Option<Option<i32>>>>,
}

/// Validate the `ready` message, as `parseReady` in `transport.ts` does.
pub fn parse_ready(value: &Value) -> Result<HostReady> {
    let mismatch = || anyhow!(localized("执行后端的协议与此版本的 Rainy 不一致。", "Host protocol does not match this application."));
    if value.get("protocol").and_then(Value::as_u64) != Some(1) {
        return Err(mismatch());
    }
    let url = value.get("url").and_then(Value::as_str).ok_or_else(mismatch)?;
    let pid = value.get("pid").and_then(Value::as_u64).filter(|pid| *pid >= 1 && *pid <= u32::MAX as u64).ok_or_else(mismatch)?;
    let home = value.get("home").and_then(Value::as_str).ok_or_else(mismatch)?;
    let absolute = home.starts_with('/') || home.get(1..3) == Some(":\\") || home.get(1..3) == Some(":/") || home.starts_with("\\\\");
    if !absolute || home.contains('\0') {
        return Err(mismatch());
    }
    let invalid = || anyhow!(localized("执行后端返回的应用地址无效。", "Host returned an invalid application URL."));
    let rest = url.strip_prefix("http://127.0.0.1:").ok_or_else(invalid)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = &rest[digits.len()..];
    if !(after.is_empty() || after.starts_with('/') || after.starts_with('?')) {
        return Err(invalid());
    }
    let port: u16 = digits.parse().ok().filter(|port| *port > 0).ok_or_else(invalid)?;
    Ok(HostReady { url: url.to_owned(), port, pid: pid as u32, home: home.to_owned() })
}

/// Whether an inherited variable may reach the native Host (`/KEY|SECRET|TOKEN|PASSWORD|^NODE_OPTIONS$|^NODE_PATH$/i`).
pub fn inheritable(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    !(["KEY", "SECRET", "TOKEN", "PASSWORD"].iter().any(|word| upper.contains(word)) || upper == "NODE_OPTIONS" || upper == "NODE_PATH")
}

fn write_line(stdin: &Mutex<Option<ChildStdin>>, value: &Value) {
    if let Some(stdin) = stdin.lock().unwrap().as_mut() {
        let _ = writeln!(stdin, "{value}").and_then(|()| stdin.flush());
    }
}

impl HostProcess {
    /// Start the Host and wait until it is ready.
    pub fn start(command: &HostCommand, events: Arc<dyn HostEvents>) -> Result<Self> {
        let mut process = Command::new(&command.program);
        process.args(&command.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if command.scrub_environment {
            process.env_clear();
            for (name, value) in std::env::vars_os() {
                if name.to_str().is_some_and(inheritable) {
                    process.env(name, value);
                }
            }
        }
        process.envs(command.env.iter().map(|(name, value)| (name, value)));
        if let Some(cwd) = &command.cwd {
            process.current_dir(cwd);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            process.creation_flags(CREATE_NO_WINDOW);
        }
        let program = command.program.display();
        let mut child = process.spawn().with_context(|| localized(&format!("无法启动 {program}"), &format!("Starting {program}")))?;
        let stdin = Arc::new(Mutex::new(child.stdin.take()));
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let pending: Pending = Arc::default();
        let exited = Arc::new(Mutex::new(None));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<HostReady>>();
        let child = Arc::new(Mutex::new(child));

        {
            let events = events.clone();
            thread::Builder::new().name("host-stderr".into()).spawn(move || {
                let mut reader = BufReader::new(stderr);
                let mut line = Vec::new();
                while reader.read_until(b'\n', &mut line).map(|n| n > 0).unwrap_or(false) {
                    events.diagnostic(String::from_utf8_lossy(&line).trim_end());
                    line.clear();
                }
            })?;
        }
        {
            let events = events.clone();
            let stdin = stdin.clone();
            let pending = pending.clone();
            thread::Builder::new().name("host-control".into()).spawn(move || {
                read_control(stdout, &stdin, &pending, &ready_tx, &events);
            })?;
        }

        let deadline = Instant::now() + START_DEADLINE;
        let outcome = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match ready_rx.recv_timeout(left.min(Duration::from_millis(200))) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let code = child.lock().unwrap().wait().ok().and_then(|status| status.code());
                    break Err(StoppedEarly(code).into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) if left.is_zero() => {
                    break Err(anyhow!(localized("执行后端没有在限定时间内启动。", "The execution Host did not start before its deadline.")));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        };
        let ready = match outcome.and_then(|ready| {
            if let Some(port) = command.expected_port
                && ready.port != port
            {
                bail!(localized("远程执行后端没有使用转发的端口。", "The remote Host did not use the forwarded port."));
            }
            if command.wait_for_loopback {
                wait_for_loopback(ready.port, deadline)?;
            }
            Ok(ready)
        }) {
            Ok(ready) => ready,
            Err(error) => {
                let mut child = child.lock().unwrap();
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };

        let host = Self { child: child.clone(), stdin, pending, ready, exited: exited.clone() };
        // Report the exit once, from a thread that only polls the child.
        let child_ref = child;
        thread::Builder::new().name("host-exit".into()).spawn(move || {
            loop {
                let status = child_ref.lock().unwrap().try_wait();
                match status {
                    Ok(Some(status)) => {
                        *exited.lock().unwrap() = Some(status.code());
                        events.exited(status.code());
                        return;
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(250)),
                    Err(_) => return,
                }
            }
        })?;
        Ok(host)
    }

    pub fn ready(&self) -> &HostReady {
        &self.ready
    }

    /// Whether the Host process has ended.
    pub fn has_exited(&self) -> bool {
        self.exited.lock().unwrap().is_some()
    }

    /// Send one control request and wait for the answer with the same id.
    fn request(&self, mut message: Value) -> Result<Value> {
        let id = random_id();
        message["id"] = Value::String(id.clone());
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        write_line(&self.stdin, &message);
        let answer = rx.recv_timeout(REQUEST_DEADLINE);
        self.pending.lock().unwrap().remove(&id);
        answer.map_err(|_| anyhow!(localized("执行后端没有及时响应。", "The execution Host did not answer in time.")))
    }

    /// Observe whether the Host is busy; `freeze` also stops new work until `resume`.
    pub fn inspect_activity(&self, mode: &str) -> Result<bool> {
        let answer = self.request(json!({ "type": "inspect-activity", "mode": mode }))?;
        let invalid = || anyhow!(localized("执行后端返回的运行状态无效。", "Host activity inspection returned an invalid response."));
        answer.get("active").and_then(Value::as_bool).ok_or_else(invalid)
    }

    /// Canonical project roots, or `Value::Null` when no project is selected.
    pub fn inspect_project(&self, workspace_id: Option<&str>) -> Result<Value> {
        let mut message = json!({ "type": "inspect-project" });
        if let Some(id) = workspace_id {
            message["workspaceId"] = Value::String(id.to_owned());
        }
        Ok(self.request(message)?.get("project").cloned().unwrap_or(Value::Null))
    }

    /// Ask the Host to stop, and kill it when it has not exited after the grace period.
    pub fn stop(&self) {
        if self.has_exited() {
            return;
        }
        write_line(&self.stdin, &json!({ "type": "stop" }));
        self.stdin.lock().unwrap().take();
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            if self.has_exited() || matches!(self.child.lock().unwrap().try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let mut child = self.child.lock().unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            let _ = Command::new("taskkill.exe")
                .args(["/PID", &child.id().to_string(), "/T", "/F"])
                .creation_flags(0x0800_0000)
                .status();
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn read_control(
    stdout: impl Read,
    stdin: &Arc<Mutex<Option<ChildStdin>>>,
    pending: &Pending,
    ready: &mpsc::Sender<Result<HostReady>>,
    events: &Arc<dyn HostEvents>,
) {
    let mut reader = BufReader::new(stdout);
    let mut raw = Vec::new();
    let mut announced = false;
    while reader.read_until(b'\n', &mut raw).map(|n| n > 0).unwrap_or(false) {
        let line = String::from_utf8_lossy(&raw).trim_end_matches(['\r', '\n']).to_owned();
        raw.clear();
        let Some(body) = line.strip_prefix("RAINY_CONTROL ") else {
            events.diagnostic(&line);
            continue;
        };
        let Ok(message) = serde_json::from_str::<Value>(body) else {
            if !announced {
                let _ = ready.send(Err(anyhow!(localized("执行后端发送了无效的控制消息。", "Invalid Host control message."))));
            }
            continue;
        };
        let kind = message.get("type").and_then(Value::as_str).unwrap_or_default();
        let id = message.get("id").and_then(Value::as_str).map(str::to_owned);
        match (kind, id) {
            ("ready", _) if !announced => {
                announced = true;
                let _ = ready.send(parse_ready(&message));
            }
            ("fatal", _) if !announced => {
                let text = message.get("message").and_then(Value::as_str).map_or_else(|| localized("执行后端启动失败。", "Host startup failed."), str::to_owned);
                let _ = ready.send(Err(anyhow!(text)));
            }
            ("credential-key", Some(id)) => {
                let (events, stdin) = (events.clone(), stdin.clone());
                thread::spawn(move || {
                    let key = events.credential_key();
                    write_line(&stdin, &json!({ "type": "reply", "id": id, "key": key }));
                });
            }
            ("system-proxy", Some(id)) => {
                let url = message.get("url").and_then(Value::as_str).unwrap_or_default().to_owned();
                let (events, stdin) = (events.clone(), stdin.clone());
                thread::spawn(move || {
                    let lower = url.to_ascii_lowercase();
                    let proxy = if lower.starts_with("http://") || lower.starts_with("https://") { events.system_proxy(&url) } else { None };
                    write_line(&stdin, &json!({ "type": "reply", "id": id, "url": proxy }));
                });
            }
            ("preview", Some(id)) => {
                let command = message.get("command").filter(|value| value.is_object()).cloned().unwrap_or_else(|| json!({}));
                let (events, stdin) = (events.clone(), stdin.clone());
                thread::spawn(move || {
                    let answer = events.preview(command);
                    write_line(&stdin, &json!({ "type": "reply", "id": id, "answer": answer }));
                });
            }
            ("activity" | "project", Some(id)) => {
                if let Some(waiting) = pending.lock().unwrap().remove(&id) {
                    let _ = waiting.send(message);
                }
            }
            _ => {}
        }
    }
}

/// WSL and SSH can report readiness before Windows loopback forwarding accepts connections.
fn wait_for_loopback(port: u16, deadline: Instant) -> Result<()> {
    loop {
        match TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2)) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error).context(localized("无法通过本机回环地址连接执行后端。", "The Host is not reachable on this machine's loopback address.")),
        }
    }
}

fn random_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("system random source");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_loopback_readiness() {
        let ready = parse_ready(&json!({"protocol": 1, "url": "http://127.0.0.1:4310/?token=abc", "pid": 7, "home": "C:\\Users\\a\\h"})).unwrap();
        assert_eq!((ready.port, ready.pid), (4310, 7));
        assert!(parse_ready(&json!({"protocol": 1, "url": "http://127.0.0.1:4310/", "pid": 7, "home": "/home/a/.rainy-agent"})).is_ok());
        for bad in [
            json!({"protocol": 2, "url": "http://127.0.0.1:1/", "pid": 7, "home": "/h"}),
            json!({"protocol": 1, "url": "http://localhost:1/", "pid": 7, "home": "/h"}),
            json!({"protocol": 1, "url": "http://127.0.0.1:1@evil/", "pid": 7, "home": "/h"}),
            json!({"protocol": 1, "url": "http://127.0.0.1:0/", "pid": 7, "home": "/h"}),
            json!({"protocol": 1, "url": "http://127.0.0.1:1/", "pid": 0, "home": "/h"}),
            json!({"protocol": 1, "url": "http://127.0.0.1:1/", "pid": 7, "home": "relative"}),
        ] {
            assert!(parse_ready(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_early_stop_is_told_apart_from_other_failures() {
        let error = anyhow::Error::from(StoppedEarly(Some(3))).context("starting");
        assert!(error.downcast_ref::<StoppedEarly>().is_some_and(|stopped| stopped.0 == Some(3)));
        assert!(format!("{error:#}").contains('3'));
        assert!(anyhow!("other").downcast_ref::<StoppedEarly>().is_none());
    }

    #[test]
    fn scrubs_secret_variables() {
        assert!(inheritable("PATH"));
        assert!(!inheritable("DEEPSEEK_API_KEY"));
        assert!(!inheritable("github_token"));
        assert!(!inheritable("NODE_OPTIONS"));
        assert!(inheritable("NODE_ENV"));
    }
}
