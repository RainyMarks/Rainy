//! Child processes with whole-tree termination on Windows and Linux (3.x `process.ts`).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::locale::l;

/// The default wait between the polite and the forced stop.
pub const GRACE: Duration = Duration::from_secs(3);

/// Whether an environment name looks like a credential; such variables are never inherited by child processes.
pub fn sensitive(name: &str) -> bool {
    let upper = name.to_uppercase();
    ["KEY", "PASSWORD", "SECRET", "TOKEN"].iter().any(|word| upper.contains(word))
}

/// The Host's environment without credential-shaped variables.
pub fn scrubbed_env() -> BTreeMap<OsString, OsString> {
    std::env::vars_os().filter(|(name, _)| !sensitive(&name.to_string_lossy())).collect()
}

/// The environment of a child: the scrubbed Host environment with `overlay` on top. On Windows, names compare without
/// case, so an overlay's `PATH` replaces the inherited `Path`.
pub fn child_env(overlay: &BTreeMap<String, String>) -> BTreeMap<OsString, OsString> {
    let mut env = scrubbed_env();
    for (name, value) in overlay {
        if cfg!(windows) {
            env.retain(|known, _| !known.to_string_lossy().eq_ignore_ascii_case(name));
        }
        env.insert(name.into(), value.into());
    }
    env
}

/// The text of a program's output. On Windows, output that is not UTF-8, such as messages of programs that ignore the
/// UTF-8 console setting on a Chinese system, is read in the console's code page.
pub fn console_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) if cfg!(windows) => match legacy_encoding() {
            Some(encoding) => encoding.decode(bytes).0.into_owned(),
            None => String::from_utf8_lossy(bytes).into_owned(),
        },
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The encoding of the console's OEM code page when it is one of the East Asian multi-byte pages.
pub fn legacy_encoding() -> Option<&'static encoding_rs::Encoding> {
    #[cfg(windows)]
    {
        // SAFETY: no arguments; returns the code page number.
        let page = unsafe { windows_sys::Win32::Globalization::GetOEMCP() };
        match page {
            936 => Some(encoding_rs::GBK),
            950 => Some(encoding_rs::BIG5),
            932 => Some(encoding_rs::SHIFT_JIS),
            949 => Some(encoding_rs::EUC_KR),
            _ => None,
        }
    }
    #[cfg(not(windows))]
    None
}

/// The program could not be found on PATH.
pub fn executable_missing(command: &str) -> crate::error::HostError {
    crate::error::HostError::other(l(
        &format!("找不到可执行程序：{command}。请在 设置 → 运行环境 中选择或准备它。"),
        &format!("Executable not found: {command}. Choose or prepare it in Settings → Runtime."),
    ))
}

fn path_value(env: &BTreeMap<OsString, OsString>) -> OsString {
    env.iter().find(|(name, _)| if cfg!(windows) { name.to_string_lossy().eq_ignore_ascii_case("PATH") } else { name.as_os_str() == "PATH" }).map(|(_, value)| value.clone()).unwrap_or_default()
}

fn executable(path: &Path) -> bool {
    let Ok(info) = std::fs::metadata(path) else { return false };
    if !info.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        info.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

/// Find a program the way the platform's shell would, on the PATH of `env` (the Host's when `None`).
pub fn resolve_executable(command: &str, env: Option<&BTreeMap<OsString, OsString>>) -> Option<PathBuf> {
    let own;
    let env = match env {
        Some(env) => env,
        None => {
            own = scrubbed_env();
            &own
        }
    };
    let extensions: Vec<String> = if cfg!(windows) {
        let pathext = env.iter().find(|(name, _)| name.to_string_lossy().eq_ignore_ascii_case("PATHEXT")).map(|(_, value)| value.to_string_lossy().into_owned());
        std::iter::once(String::new()).chain(pathext.unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into()).split(';').filter(|ext| !ext.is_empty()).map(str::to_lowercase)).collect()
    } else {
        vec![String::new()]
    };
    let with = |base: &Path, ext: &str| -> PathBuf {
        let mut text = base.as_os_str().to_owned();
        text.push(ext);
        PathBuf::from(text)
    };
    let direct = Path::new(command).is_absolute() || command.contains('/') || (cfg!(windows) && command.contains('\\'));
    if direct {
        return extensions.iter().map(|ext| with(Path::new(command), ext)).find(|path| executable(path));
    }
    for directory in std::env::split_paths(&path_value(env)) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        let directory = PathBuf::from(directory.to_string_lossy().trim_matches('"'));
        for ext in &extensions {
            let candidate = with(&directory.join(command), ext);
            if executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// The PowerShell of commands and terminals on Windows: an explicit path, the Windows basic component's PowerShell 7,
/// Program Files, PATH, and finally the Windows PowerShell 5.1 every installation has. `true` for PowerShell 7+.
pub fn resolve_powershell(configured: Option<&Path>, component: Option<&Path>) -> (PathBuf, bool) {
    let mut candidates: Vec<PathBuf> = configured.into_iter().chain(component).map(Path::to_path_buf).collect();
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    candidates.push(program_files.join("PowerShell").join("7").join("pwsh.exe"));
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).filter(|entry| !entry.as_os_str().is_empty()).map(|entry| PathBuf::from(entry.to_string_lossy().trim_matches('"')).join("pwsh.exe")));
    }
    // symlink_metadata sees the Microsoft Store execution alias, which metadata reports as inaccessible.
    if let Some(found) = candidates.into_iter().find(|candidate| std::fs::symlink_metadata(candidate).is_ok()) {
        let core = !found.file_name().is_some_and(|name| name.eq_ignore_ascii_case("powershell.exe"));
        return (found, core);
    }
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    (root.join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe"), false)
}

/// A command ready to start: no shell, a hidden window on Windows, its own process group on Linux.
pub fn command(program: impl AsRef<std::ffi::OsStr>, args: &[impl AsRef<std::ffi::OsStr>], cwd: &Path, env: &BTreeMap<OsString, OsString>) -> Command {
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd).env_clear().envs(env).kill_on_drop(false);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    command.process_group(0);
    command
}

/// Start a program with piped output and, when `stdin`, piped input.
pub fn spawn(program: impl AsRef<std::ffi::OsStr>, args: &[impl AsRef<std::ffi::OsStr>], cwd: &Path, env: &BTreeMap<OsString, OsString>, stdin: bool) -> std::io::Result<Child> {
    let mut command = command(program, args, cwd, env);
    command.stdin(if stdin { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    command.spawn()
}

#[cfg(windows)]
async fn taskkill(pid: u32, force: bool) {
    let mut args = vec!["/PID".to_owned(), pid.to_string(), "/T".to_owned()];
    if force {
        args.push("/F".into());
    }
    let mut command = Command::new("taskkill.exe");
    command.args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
    let _ = command.status().await;
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: i32) {
    // SAFETY: plain signal delivery; a negative pid names the process group the child leads.
    unsafe {
        if libc::kill(-(pid as i32), signal) != 0 {
            libc::kill(pid as i32, signal);
        }
    }
}

/// Stop a process and everything it started: a polite request first, the forced stop after `grace` unless the process
/// exited by then. On Linux the process must lead its own group, as [`command`] arranges.
pub async fn kill_tree(child: &mut Child, grace: Duration) {
    let Some(pid) = child.id() else { return };
    #[cfg(windows)]
    {
        // Console programs ignore the polite request, so the forced stop follows after the grace period either way.
        taskkill(pid, false).await;
        if tokio::time::timeout(grace, child.wait()).await.is_err() {
            taskkill(pid, true).await;
            let _ = child.wait().await;
        }
    }
    #[cfg(unix)]
    {
        signal_group(pid, libc::SIGTERM);
        let exited = tokio::time::timeout(grace, child.wait()).await.is_ok();
        signal_group(pid, libc::SIGKILL);
        if !exited {
            let _ = child.wait().await;
        }
    }
}

/// Stop a process tree by id, when only the id is known (a terminal's shell).
pub async fn kill_pid_tree(pid: u32) {
    #[cfg(windows)]
    taskkill(pid, true).await;
    #[cfg(unix)]
    {
        signal_group(pid, libc::SIGTERM);
        tokio::time::sleep(Duration::from_millis(500)).await;
        signal_group(pid, libc::SIGKILL);
    }
}

/// Exit code and signal of a finished process, as the page shows them.
pub fn outcome(status: &std::process::ExitStatus) -> (Option<i32>, Option<String>) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            let name = match signal {
                libc::SIGTERM => "SIGTERM",
                libc::SIGKILL => "SIGKILL",
                libc::SIGINT => "SIGINT",
                libc::SIGHUP => "SIGHUP",
                libc::SIGSEGV => "SIGSEGV",
                libc::SIGABRT => "SIGABRT",
                _ => "SIGNAL",
            };
            return (None, Some(name.to_owned()));
        }
    }
    (status.code(), None)
}

/// Run a program to completion; output past `limit` bytes per stream is dropped.
pub async fn run(program: impl AsRef<std::ffi::OsStr>, args: &[impl AsRef<std::ffi::OsStr>], cwd: &Path, env: &BTreeMap<OsString, OsString>, limit: usize, timeout: Option<Duration>) -> std::io::Result<Output> {
    let mut child = spawn(program, args, cwd, env, false)?;
    let (stdout, stderr) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let work = async {
        let (out, err, status) = tokio::join!(read_limited(stdout, limit), read_limited(stderr, limit), child.wait());
        (out, err, status)
    };
    let (out, err, status) = match timeout {
        Some(limit) => match tokio::time::timeout(limit, work).await {
            Ok(result) => result,
            Err(_) => {
                kill_tree(&mut child, Duration::from_millis(500)).await;
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "The program did not finish in time."));
            }
        },
        None => work.await,
    };
    let status = status?;
    Ok(Output { code: status.code(), success: status.success(), stdout: out, stderr: err })
}

/// Everything a stream yields, keeping the first `limit` bytes.
pub async fn read_limited(mut stream: impl tokio::io::AsyncRead + Unpin, limit: usize) -> Vec<u8> {
    use tokio::io::AsyncReadExt as _;
    let mut kept = Vec::new();
    let mut buffer = [0u8; 16384];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break kept,
            Ok(read) => {
                let room = limit.saturating_sub(kept.len());
                kept.extend_from_slice(&buffer[..read.min(room)]);
            }
        }
    }
}

pub struct Output {
    pub code: Option<i32>,
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
