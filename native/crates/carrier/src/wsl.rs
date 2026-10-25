//! Running the Linux Host in a WSL distribution (`prepareWsl` in `src/main/main.ts`, `WslHostTransport`).

use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::Value;

use crate::HostCommand;
use crate::locale::localized;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Converting paths also waits for a cold VM to start (3.x `wslPaths`).
const PATHS_TIMEOUT: Duration = Duration::from_secs(120);
/// Installing unpacks at most the application layer and the base (3.x `prepareWsl`).
const INSTALL_TIMEOUT: Duration = Duration::from_secs(180);
const PRUNE_TIMEOUT: Duration = Duration::from_secs(600);

/// `wsl.exe` with arguments that are never shell-interpolated. A test can set `RAINY_DEV_WSL_HOME` so that everything
/// Rainy keeps in the distribution lives under that folder instead of the user's home.
fn wsl(distro: &str, args: &[&str]) -> Command {
    let mut command = Command::new("wsl.exe");
    command.args(["--distribution", distro, "--exec"]);
    if let Ok(home) = std::env::var("RAINY_DEV_WSL_HOME") {
        command.args(["env", &format!("HOME={home}")]);
    }
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Run `command` to completion with its output captured; it is stopped after `timeout`.
fn output_within(mut command: Command, timeout: Duration, what: &str) -> Result<Output> {
    let mut child =
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().with_context(|| localized("无法启动 wsl.exe", "Starting wsl.exe"))?;
    let collect = |mut pipe: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    };
    let stdout = collect(Box::new(child.stdout.take().expect("piped stdout")));
    let stderr = collect(Box::new(child.stderr.take().expect("piped stderr")));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let seconds = timeout.as_secs();
            bail!(localized(&format!("{what}在 {seconds} 秒内没有完成。"), &format!("{what} did not finish within {seconds} s.")));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    Ok(Output { status, stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() })
}

/// The output of a failed run as one text; `what` names the step in the interface language.
fn checked(output: Output, what: &str) -> Result<String> {
    if !output.status.success() {
        let code = output.status.code().map_or_else(|| "?".to_owned(), |code| code.to_string());
        let said = console_text(&output.stderr);
        let said = said.trim();
        bail!(localized(&format!("{what}失败（退出代码 {code}）：{said}"), &format!("{what} failed (exit code {code}): {said}")));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What a program printed: `wsl.exe` writes its own messages, such as an unknown distribution, in UTF-16 (with NUL
/// bytes, or not UTF-8 at all); the Linux side writes UTF-8.
fn console_text(bytes: &[u8]) -> String {
    let wide = !bytes.is_empty() && bytes.len().is_multiple_of(2) && (bytes.contains(&0) || std::str::from_utf8(bytes).is_err());
    if wide {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        String::from_utf16_lossy(&units).trim_start_matches('\u{feff}').to_owned()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Start the distribution's VM early; the first `wsl.exe` call of a boot can take seconds.
pub fn warm_up(distro: &str) {
    let _ = wsl(distro, &["true"]).output();
}

/// Translate Windows paths with one `wsl.exe` call; one absolute Linux path per input.
pub fn linux_paths(distro: &str, paths: &[&Path]) -> Result<Vec<String>> {
    let mut args: Vec<String> = vec!["sh".into(), "-c".into(), r#"for p in "$@"; do wslpath -u "$p"; done"#.into(), "sh".into()];
    args.extend(paths.iter().map(|path| path.display().to_string()));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = checked(output_within(wsl(distro, &refs), PATHS_TIMEOUT, "wslpath")?, &localized("WSL 路径转换", "wslpath"))?;
    let lines: Vec<String> = output.lines().map(|line| line.trim_end_matches('\r').to_owned()).filter(|line| !line.is_empty()).collect();
    if lines.len() != paths.len() || !lines.iter().all(|line| line.starts_with('/')) {
        bail!(localized("WSL 没有正确转换路径。", "WSL did not convert the paths correctly."));
    }
    Ok(lines)
}

/// Files of this version's Linux runtime that ship with the desktop.
#[derive(Debug, Clone)]
pub struct RuntimeFiles {
    /// `linux-runtime.json`: digests of the base and the application layer.
    pub metadata: PathBuf,
    /// `linux-app.tar.gz`: the Host, the interface and the small resources.
    pub app: PathBuf,
    /// `install-runtime.py`.
    pub installer: PathBuf,
    /// The base archive, when it was downloaded or a development checkout has it.
    pub base: Option<PathBuf>,
}

/// What the installer answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// The Host is installed: the absolute Linux path of `rainy-host`.
    Ready { host: String },
    /// The base of this version is not installed and no archive was given; download it and try again.
    NeedsArchive,
}

/// Install (or find) this version's runtime in the distribution (`install-runtime.py BASE APP METADATA`).
pub fn install_runtime(distro: &str, files: &RuntimeFiles) -> Result<Installed> {
    // A missing base archive is passed as a path that does not exist; the installer then only checks what is installed.
    let base = files.base.clone().unwrap_or_else(|| files.metadata.with_file_name("linux-runtime.tar.xz"));
    let paths = linux_paths(distro, &[&base, &files.app, &files.metadata, &files.installer])?;
    let output = output_within(wsl(distro, &["python3", &paths[3], &paths[0], &paths[1], &paths[2]]), INSTALL_TIMEOUT, "install-runtime.py")?;
    let text = checked(output, &localized("安装 Linux 运行环境", "Installing the Linux runtime"))?;
    parse_installed(&text)
}

/// Parse the installer's last line.
pub fn parse_installed(text: &str) -> Result<Installed> {
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| anyhow!(localized("Linux 运行环境安装程序没有输出任何结果。", "The Linux runtime installer printed nothing.")))?;
    let value: Value = serde_json::from_str(line.trim())
        .with_context(|| localized("Linux 运行环境安装程序返回的结果无效", "The Linux runtime installer returned invalid JSON"))?;
    if value.get("needsArchive").is_some() {
        return Ok(Installed::NeedsArchive);
    }
    match value.get("host").and_then(Value::as_str).filter(|path| path.starts_with('/')) {
        Some(host) => Ok(Installed::Ready { host: host.to_owned() }),
        None => {
            let line = line.trim();
            bail!(localized(&format!("执行环境的运行文件无效：{line}"), &format!("The runtime files of the execution environment are invalid: {line}")))
        }
    }
}

/// Delete runtimes earlier versions unpacked (`install-runtime.py --prune METADATA`).
pub fn prune_runtimes(distro: &str, files: &RuntimeFiles) -> Result<Vec<String>> {
    let paths = linux_paths(distro, &[&files.installer, &files.metadata])?;
    let pruned = output_within(wsl(distro, &["python3", &paths[0], "--prune", &paths[1]]), PRUNE_TIMEOUT, "install-runtime.py --prune")?;
    let text = checked(pruned, &localized("清理旧的运行环境", "Pruning old runtimes"))?;
    let value: Value = serde_json::from_str(text.lines().last().unwrap_or("{}")).unwrap_or(Value::Null);
    Ok(value.get("removed").and_then(Value::as_array).map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect()).unwrap_or_default())
}

/// The Host inside the distribution: `wsl.exe --distribution D --exec env K=V… rainy-host` (`WslHostTransport`).
pub fn host_command(distro: &str, host: &str, env: &[(String, String)]) -> HostCommand {
    let mut args: Vec<OsString> = ["--distribution", distro, "--exec", "env"].into_iter().map(OsString::from).collect();
    if let Ok(home) = std::env::var("RAINY_DEV_WSL_HOME") {
        args.push(format!("HOME={home}").into());
    }
    args.extend(env.iter().map(|(name, value)| OsString::from(format!("{name}={value}"))));
    args.push(host.into());
    HostCommand {
        program: "wsl.exe".into(),
        args,
        env: Vec::new(),
        cwd: None,
        scrub_environment: false,
        // WSL reports readiness before Windows loopback forwarding accepts connections.
        wait_for_loopback: true,
        expected_port: None,
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn parses_installer_answers() {
        assert_eq!(parse_installed("noise\n{\"needsArchive\": true}\n").unwrap(), Installed::NeedsArchive);
        assert_eq!(
            parse_installed(r#"{"host": "/h/.rainy-agent/runtime/x/rainy-host"}"#).unwrap(),
            Installed::Ready { host: "/h/.rainy-agent/runtime/x/rainy-host".into() }
        );
        assert!(parse_installed(r#"{"host": "relative"}"#).is_err());
        assert!(parse_installed("").is_err());
    }

    #[test]
    fn reads_what_wsl_exe_prints_in_utf16() {
        let utf16 = |text: &str| text.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
        assert_eq!(console_text(&utf16("There is no distribution with the supplied name.\r\n")), "There is no distribution with the supplied name.\r\n");
        assert_eq!(console_text(&utf16("\u{feff}不存在具有所提供名称的分发。")), "不存在具有所提供名称的分发。");
        assert_eq!(console_text("sh: 1: wslpath: not found\n".as_bytes()), "sh: 1: wslpath: not found\n");
        assert_eq!(console_text("python3：未找到命令".as_bytes()), "python3：未找到命令");
        assert_eq!(console_text(b""), "");
    }

    #[cfg(windows)]
    #[test]
    fn stops_a_command_that_takes_too_long() {
        let mut slow = Command::new("cmd.exe");
        slow.args(["/c", "ping -n 30 127.0.0.1 >NUL"]);
        let started = Instant::now();
        let error = output_within(slow, Duration::from_secs(1), "ping").unwrap_err().to_string();
        assert!(error.starts_with("ping") && error.contains('1'), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let mut quick = Command::new("cmd.exe");
        quick.args(["/c", "echo done"]);
        let output = output_within(quick, Duration::from_secs(30), "echo").unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "done");
    }

    #[test]
    fn builds_the_host_command() {
        let command = host_command("Ubuntu", "/r/rainy-host", &[("RAINY_HOME".into(), "/home/u/.rainy-agent".into())]);
        let args: Vec<String> = command.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(&args[..4], ["--distribution", "Ubuntu", "--exec", "env"]);
        assert!(args.contains(&"RAINY_HOME=/home/u/.rainy-agent".to_owned()));
        assert_eq!(args.last().map(String::as_str), Some("/r/rainy-host"));
        assert!(command.wait_for_loopback);
    }
}
