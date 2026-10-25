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

/// Makes `wsl.exe` write its own messages in UTF-8 rather than UTF-16; Linux programs' output is unaffected, and the
/// variable does not reach the distribution.
const UTF8: (&str, &str) = ("WSL_UTF8", "1");

/// `wsl.exe` with arguments that are never shell-interpolated. A test can set `RAINY_DEV_WSL_HOME` so that everything
/// Rainy keeps in the distribution lives under that folder instead of the user's home.
fn wsl(distro: &str, args: &[&str]) -> Command {
    let mut command = Command::new("wsl.exe");
    command.env(UTF8.0, UTF8.1).args(["--distribution", distro, "--exec"]);
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
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| localized("无法启动 wsl.exe，请确认已安装 WSL。", "wsl.exe could not be started. Make sure WSL is installed."))?;
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

/// Output of `wsl.exe`: its own messages in UTF-16LE when an older WSL ignores `WSL_UTF8` (with or without a byte
/// order mark, possibly followed by a Linux program's UTF-8), otherwise UTF-8.
pub fn decode_output(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return encoding_rs::UTF_16LE.decode_without_bom_handling(rest).0.into_owned();
    }
    let mut text = String::new();
    let mut rest = bytes;
    while let Some(length) = utf16_line(rest) {
        text.push_str(&encoding_rs::UTF_16LE.decode_without_bom_handling(&rest[..length]).0);
        rest = &rest[length..];
    }
    text.push_str(&String::from_utf8_lossy(rest));
    text
}

/// The length of a leading UTF-16LE line, through its `\n` or to the end. An ASCII character of it has a zero high
/// byte, which UTF-8 text never has.
fn utf16_line(bytes: &[u8]) -> Option<usize> {
    let mut ascii = false;
    for (index, pair) in bytes.chunks(2).enumerate() {
        let [low, high] = *pair else { return None };
        match (low, high) {
            (0, 0) => return None,
            (b'\n', 0) => return Some(index * 2 + 2),
            (_, 0) => ascii = true,
            _ => {}
        }
    }
    ascii.then_some(bytes.len())
}

/// What the distribution or `wsl.exe` said about a failure: the last lines of both outputs.
fn said(output: &Output) -> String {
    let text = format!("{}\n{}", decode_output(&output.stderr), decode_output(&output.stdout));
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    lines[lines.len().saturating_sub(6)..].join("\n")
}

/// A failed `wsl.exe` call explained: a distribution that is gone, a missing `python3`, or what was said.
fn failure(distro: &str, output: &Output, what: &str) -> anyhow::Error {
    let said = said(output);
    if said.contains("WSL_E_DISTRO_NOT_FOUND") {
        return anyhow!(localized(
            &format!("找不到 WSL 发行版 {distro}，它可能已被卸载或改名。请改用 Windows 原生运行，或在设置中选择其他执行环境。"),
            &format!("The WSL distribution {distro} was not found; it may have been removed or renamed. Run on Windows instead, or choose another execution environment in Settings."),
        ));
    }
    if said.contains("execvpe(python3) failed") {
        return anyhow!(localized(
            &format!("WSL 发行版 {distro} 中没有 python3。Rainy 需要它来安装运行环境，请先在该发行版中安装 python3（例如 sudo apt install python3）。"),
            &format!("The WSL distribution {distro} has no python3, which Rainy needs to install its runtime. Install python3 there first (for example sudo apt install python3)."),
        ));
    }
    let code = output.status.code().map_or_else(|| localized("无", "none"), |code| code.to_string());
    anyhow!(localized(&format!("{what}失败（退出代码 {code}）。\n{said}"), &format!("{what} failed (exit code {code}).\n{said}")).trim_end().to_owned())
}

fn checked(output: Output, distro: &str, what: &str) -> Result<String> {
    if !output.status.success() {
        return Err(failure(distro, &output, what));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
    let what = localized("WSL 路径转换", "Converting paths for WSL");
    let output = checked(output_within(wsl(distro, &refs), PATHS_TIMEOUT, &what)?, distro, &what)?;
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
    let what = localized("在 WSL 中安装 Linux 运行环境", "Installing the Linux runtime in WSL");
    let output = output_within(wsl(distro, &["python3", &paths[3], &paths[0], &paths[1], &paths[2]]), INSTALL_TIMEOUT, &what)?;
    let text = checked(output, distro, &what)?;
    parse_installed(&text)
}

/// Parse the installer's last line.
pub fn parse_installed(text: &str) -> Result<Installed> {
    let invalid = || localized("Linux 运行环境安装程序的回答无效：", "The Linux runtime installer gave an invalid answer:");
    let line = text.lines().rev().find(|line| !line.trim().is_empty()).ok_or_else(|| anyhow!("{} {}", invalid(), localized("没有输出", "nothing")))?;
    let value: Value = serde_json::from_str(line.trim()).map_err(|_| anyhow!("{} {}", invalid(), line.trim()))?;
    if value.get("needsArchive").is_some() {
        return Ok(Installed::NeedsArchive);
    }
    match value.get("host").and_then(Value::as_str).filter(|path| path.starts_with('/')) {
        Some(host) => Ok(Installed::Ready { host: host.to_owned() }),
        None => bail!("{} {}", invalid(), line.trim()),
    }
}

/// Delete runtimes earlier versions unpacked (`install-runtime.py --prune METADATA`).
pub fn prune_runtimes(distro: &str, files: &RuntimeFiles) -> Result<Vec<String>> {
    let paths = linux_paths(distro, &[&files.installer, &files.metadata])?;
    let what = "install-runtime.py --prune";
    let text = checked(output_within(wsl(distro, &["python3", &paths[0], "--prune", &paths[1]]), PRUNE_TIMEOUT, what)?, distro, what)?;
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
        // What wsl.exe says about a distribution that cannot start reaches the log readable.
        env: vec![(UTF8.0.into(), UTF8.1.into())],
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

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn reads_what_wsl_exe_says_in_any_encoding() {
        // As wsl.exe wrote it here for a missing distribution: UTF-16LE without a byte order mark.
        let missing = "There is no distribution with the supplied name.\r\nError code: Wsl/Service/WSL_E_DISTRO_NOT_FOUND\r\n";
        assert_eq!(decode_output(&utf16(missing)), missing);
        let mut marked = vec![0xFF, 0xFE];
        marked.extend(utf16("不存在具有所提供名称的分发。\r\n"));
        assert_eq!(decode_output(&marked), "不存在具有所提供名称的分发。\r\n");
        // A Chinese line of wsl.exe, then the distribution's own UTF-8 error on the same stream.
        let mut mixed = utf16("wsl: 检测到 localhost 代理配置\r\n");
        mixed.extend("<3>WSL (1599 - Relay) ERROR: CreateProcessCommon:818: execvpe(python3) failed: No such file or directory\n".as_bytes());
        assert_eq!(decode_output(&mixed), "wsl: 检测到 localhost 代理配置\r\n<3>WSL (1599 - Relay) ERROR: CreateProcessCommon:818: execvpe(python3) failed: No such file or directory\n");
        for plain in ["", "a", "ok\n", "路径/中文\n", "/mnt/c/Users/x\n/mnt/c/y\n"] {
            assert_eq!(decode_output(plain.as_bytes()), plain);
        }
    }

    #[cfg(windows)]
    #[test]
    fn explains_failed_wsl_calls() {
        use std::os::windows::process::ExitStatusExt as _;
        let output = |code: u32, stdout: Vec<u8>, stderr: &[u8]| Output { status: std::process::ExitStatus::from_raw(code), stdout, stderr: stderr.to_vec() };
        // Other tests switch the language at any time, so either is accepted.
        let either = |text: &str, chinese: &str, english: &str| assert!(text.starts_with(chinese) || text.starts_with(english), "{text}");
        let gone = failure("Ubuntu", &output(0xFFFF_FFFF, utf16("There is no distribution with the supplied name.\r\nError code: Wsl/Service/WSL_E_DISTRO_NOT_FOUND\r\n"), b""), "x").to_string();
        either(&gone, "找不到 WSL 发行版 Ubuntu", "The WSL distribution Ubuntu was not found");
        let python = failure("Debian", &output(1, Vec::new(), b"<3>WSL (9 - Relay) ERROR: CreateProcessCommon:818: execvpe(python3) failed: No such file or directory\n"), "x").to_string();
        either(&python, "WSL 发行版 Debian 中没有 python3", "The WSL distribution Debian has no python3");
        let full = failure("Ubuntu", &output(1, Vec::new(), b"Traceback (most recent call last):\nOSError: [Errno 28] No space left on device\n"), "Install").to_string();
        either(&full, "Install失败（退出代码 1）。\n", "Install failed (exit code 1).\n");
        assert!(full.ends_with("\nTraceback (most recent call last):\nOSError: [Errno 28] No space left on device"), "{full}");
        let quiet = failure("Ubuntu", &output(1, Vec::new(), b""), "Install").to_string();
        assert!(quiet == "Install失败（退出代码 1）。" || quiet == "Install failed (exit code 1).", "{quiet}");
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
