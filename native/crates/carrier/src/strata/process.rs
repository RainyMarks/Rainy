//! The Strata processes Rainy starts: the Terminal's `strata` commands and its management server, hidden, with the
//! environment of [`super::env`], tied to Rainy's own process so that none outlives it, and stopped with what they
//! started.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::Value;

use crate::locale::localized;

/// How the Terminal is started: its own Python, its entry point and the environment every Strata process gets.
#[derive(Debug, Clone)]
pub struct Terminal {
    pub python: PathBuf,
    /// The Control folder (`strata.py`, `tools`, `setup.py`).
    pub root: PathBuf,
    pub environment: Vec<(OsString, OsString)>,
    /// Where the commands' own error output goes.
    pub log: PathBuf,
}

/// Why a command did not give its answer.
#[derive(Debug)]
pub enum Failure {
    Cancelled,
    /// The command ran too long and was stopped.
    TimedOut,
    /// It ended without a JSON answer: its exit code and the last lines it wrote.
    Failed(Option<i32>, String),
    Other(anyhow::Error),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str(&localized("已取消。", "Cancelled.")),
            Self::TimedOut => f.write_str(&localized("Strata 没有在规定时间内完成。", "Strata did not finish in time.")),
            Self::Failed(_, detail) if !detail.is_empty() => f.write_str(detail),
            Self::Failed(code, _) => write!(f, "{}", localized(&format!("Strata 命令失败（退出代码 {code:?}）。"), &format!("The Strata command failed (exit code {code:?})."))),
            Self::Other(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for Failure {}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::Other(error.into())
    }
}

impl Terminal {
    /// `python <root>\strata.py <args>` as `bin\strata.cmd` runs it, without a console window.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.python);
        command.arg(self.root.join("strata.py")).args(args);
        self.prepare(&mut command);
        command
    }

    /// Strata's management server, `tools\strata_mcp.py`, over standard input and output, as Strata's own app runs it:
    /// its installs run setup in the app's mode, on the Terminal's own Python, with no environment of their own to
    /// build, no packages to install and no engine to compile (the engine is a component `strata update` installs).
    pub fn management(&self, install: &Path) -> Command {
        let mut command = Command::new(&self.python);
        command.arg("-I").arg(self.root.join("tools").join("strata_mcp.py")).arg("--root").arg(install).arg("--src").arg(&self.root);
        self.prepare(&mut command);
        command.env("STRATA_CONTROL_INSTALL", "1");
        command
    }

    fn prepare(&self, command: &mut Command) {
        command.current_dir(&self.root).env_clear().envs(self.environment.iter().map(|(name, value)| (name, value))).stdin(Stdio::null());
        hide(command);
    }

    fn error_output(&self) -> Stdio {
        if let Some(parent) = self.log.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::OpenOptions::new().create(true).append(true).open(&self.log).map(Stdio::from).unwrap_or_else(|_| Stdio::null())
    }

    /// Run a command that prints one JSON answer (`--output json`) and return it; `timeout` stops it.
    pub fn json(&self, args: &[&str], timeout: Duration, cancel: &AtomicBool) -> Result<Value, Failure> {
        let mut last = None;
        let code = self.run(args, timeout, cancel, &mut |line| {
            if let Ok(value) = serde_json::from_str::<Value>(line)
                && value.is_object()
            {
                last = Some(value);
            }
        })?;
        match last {
            Some(answer) => Ok(answer),
            None => Err(Failure::Failed(code, self.tail())),
        }
    }

    /// Run a command and hand each line it prints to `line`; returns its exit code.
    pub fn run(&self, args: &[&str], timeout: Duration, cancel: &AtomicBool, line: &mut dyn FnMut(&str)) -> Result<Option<i32>, Failure> {
        let mut command = self.command(args);
        command.stdout(Stdio::piped()).stderr(self.error_output());
        log::info!("Strata: strata {}", args.join(" "));
        let mut child = command.spawn().with_context(|| format!("Starting {}", self.python.display()))?;
        contain(&child);
        let stdout = child.stdout.take().context("No output of the Strata command")?;
        let owned = Owned::new(child);
        let (sender, receiver) = std::sync::mpsc::channel::<String>();
        let reader = std::thread::Builder::new().name("strata-output".into()).spawn(move || {
            for text in BufReader::new(stdout).lines() {
                let Ok(text) = text else { break };
                if sender.send(text).is_err() {
                    break;
                }
            }
        })?;
        let deadline = Instant::now() + timeout;
        loop {
            match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(text) => {
                    line(text.trim_end());
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // The output is closed: the process is ending.
                    let _ = reader.join();
                    return Ok(owned.wait(Duration::from_secs(30)));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            if owned.finished() {
                // A process it left running in the background may hold the output open: take what has arrived and
                // leave the reader to end with that process.
                while let Ok(text) = receiver.recv_timeout(Duration::from_millis(300)) {
                    line(text.trim_end());
                }
                return Ok(owned.code());
            }
            if cancel.load(Ordering::SeqCst) {
                owned.stop();
                return Err(Failure::Cancelled);
            }
            if Instant::now() >= deadline {
                owned.stop();
                return Err(Failure::TimedOut);
            }
        }
    }

    /// The last lines the commands wrote to their error output, for a failure message.
    pub fn tail(&self) -> String {
        tail(&self.log, 6)
    }
}

/// The last `lines` non-empty lines of a log, without the oldest part of a large one.
pub fn tail(path: &Path, lines: usize) -> String {
    let Ok(mut file) = File::open(path) else { return String::new() };
    let length = file.metadata().map(|info| info.len()).unwrap_or(0);
    let start = length.saturating_sub(64 * 1024);
    let mut bytes = Vec::new();
    use std::io::Seek as _;
    if file.seek(std::io::SeekFrom::Start(start)).is_err() || file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let kept: Vec<&str> = text.lines().map(str::trim_end).filter(|line| !line.is_empty()).collect();
    kept[kept.len().saturating_sub(lines)..].join("\n")
}

/// A process Rainy started, stopped with the processes it started.
pub struct Owned {
    child: Mutex<Child>,
}

impl Owned {
    pub fn new(child: Child) -> Self {
        Self { child: Mutex::new(child) }
    }

    pub fn finished(&self) -> bool {
        self.child.lock().unwrap().try_wait().ok().flatten().is_some()
    }

    /// The exit code once it has ended.
    pub fn code(&self) -> Option<i32> {
        self.child.lock().unwrap().try_wait().ok().flatten().and_then(|status| status.code())
    }

    /// Wait up to `timeout` for the process to end on its own; its exit code.
    pub fn wait(&self, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = self.child.lock().unwrap().try_wait() {
                return status.code();
            }
            if Instant::now() >= deadline {
                self.stop();
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Stop the process and its tree, and wait until it has exited.
    pub fn stop(&self) {
        let mut child = self.child.lock().unwrap();
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        #[cfg(windows)]
        {
            let mut taskkill = Command::new("taskkill.exe");
            taskkill.args(["/PID", &child.id().to_string(), "/T", "/F"]).stdout(Stdio::null()).stderr(Stdio::null());
            hide(&mut taskkill);
            let _ = taskkill.status();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn hide(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Tie a process Rainy started, and those it starts, to Rainy's own: they are in a job that ends them when its one
/// handle closes, which is when Rainy's process ends, also after a crash. The job allows no breakaway, so what Strata
/// starts in the background (its runtime host, a model download) ends with Rainy too. A process outside it (the job
/// could not be made) is only stopped by a normal quit.
#[cfg(windows)]
pub fn contain(child: &Child) {
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

    let job = job();
    // SAFETY: both handles are open: the job's for the life of the process, the child's as long as `child` lives.
    if job.is_null() || unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) } == 0 {
        log::warn!("A Strata process does not end with Rainy: {}", std::io::Error::last_os_error());
    }
}

/// The job of [`contain`], made once; null when it could not be.
#[cfg(windows)]
fn job() -> windows_sys::Win32::Foundation::HANDLE {
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    static JOB: OnceLock<usize> = OnceLock::new();
    // SAFETY: the job is created without a name and with a handle child processes do not inherit; the limits are
    // plain data of the size given. The handle stays open for the life of the process.
    let job = *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return 0;
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        if SetInformationJobObject(job, JobObjectExtendedLimitInformation, (&raw const limits).cast(), size) == 0 {
            CloseHandle(job);
            return 0;
        }
        job as usize
    });
    job as windows_sys::Win32::Foundation::HANDLE
}

#[cfg(not(windows))]
pub fn contain(_child: &Child) {}

/// The error a command reported in its JSON answer (`{"ok": false, "error": {"message": ...}}`), if any.
pub fn reported_error(answer: &Value) -> Option<String> {
    if answer.get("ok").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let error = answer.get("error")?;
    Some(error.get("message").and_then(Value::as_str).or_else(|| error.as_str()).map(str::to_owned).unwrap_or_else(|| error.to_string()))
}

/// An answer that is not an error, or the error it reports.
pub fn accepted(answer: Value) -> Result<Value> {
    match reported_error(&answer) {
        Some(message) => Err(anyhow!(message)),
        None => Ok(answer),
    }
}

/// Fail with `message` when `condition` holds; for the checks before a command.
pub fn refuse(condition: bool, message: String) -> Result<()> {
    if condition {
        bail!(message);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn reads_reported_errors() {
        assert_eq!(reported_error(&serde_json::json!({"ok": false, "error": {"code": 2, "message": "No model was chosen."}})).as_deref(), Some("No model was chosen."));
        assert_eq!(reported_error(&serde_json::json!({"ok": true, "running": []})), None);
        assert!(accepted(serde_json::json!({"ok": false, "error": "broken"})).is_err());
    }

    #[test]
    fn tails_a_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.log");
        fs::write(&path, "one\n\ntwo\nthree\n").unwrap();
        assert_eq!(tail(&path, 2), "two\nthree");
        assert_eq!(tail(&dir.path().join("missing.log"), 2), "");
    }

    #[cfg(windows)]
    #[test]
    fn started_processes_end_with_rainy() {
        use std::os::windows::io::AsRawHandle as _;

        use windows_sys::Win32::System::JobObjects::IsProcessInJob;
        let mut child = Command::new("cmd.exe").args(["/c", "ping -n 30 127.0.0.1 >NUL"]).stdout(Stdio::null()).spawn().unwrap();
        contain(&child);
        let mut inside = 0;
        assert!(!job().is_null());
        // SAFETY: the child's handle is open while `child` lives, and the job's for the life of the process.
        assert_ne!(unsafe { IsProcessInJob(child.as_raw_handle(), job(), &mut inside) }, 0);
        assert_ne!(inside, 0);
        let _ = child.kill();
        let _ = child.wait();
    }
}
