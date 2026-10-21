//! Running the Linux Host in a WSL distribution (`prepareWsl` in `src/main/main.ts`, `WslHostTransport`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::Value;

use crate::HostCommand;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

fn checked(output: Output, what: &str) -> Result<String> {
    if !output.status.success() {
        bail!("{what} failed ({}): {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
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
    let output = checked(wsl(distro, &refs).output().context("Starting wsl.exe")?, "wslpath")?;
    let lines: Vec<String> = output.lines().map(|line| line.trim_end_matches('\r').to_owned()).filter(|line| !line.is_empty()).collect();
    if lines.len() != paths.len() || !lines.iter().all(|line| line.starts_with('/')) {
        bail!("WSL did not convert the paths correctly.");
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
    /// The Host is installed: absolute Linux paths of `node` and `host.js`.
    Ready { node: String, host: String },
    /// The base of this version is not installed and no archive was given; download it and try again.
    NeedsArchive,
}

/// Install (or find) this version's runtime in the distribution (`install-runtime.py BASE APP METADATA`).
pub fn install_runtime(distro: &str, files: &RuntimeFiles) -> Result<Installed> {
    // A missing base archive is passed as a path that does not exist; the installer then only checks what is installed.
    let base = files.base.clone().unwrap_or_else(|| files.metadata.with_file_name("linux-runtime.tar.xz"));
    let paths = linux_paths(distro, &[&base, &files.app, &files.metadata, &files.installer])?;
    let output = wsl(distro, &["python3", &paths[3], &paths[0], &paths[1], &paths[2]]).output().context("Starting wsl.exe")?;
    let text = checked(output, "Installing the Linux runtime")?;
    parse_installed(&text)
}

/// Parse the installer's last line.
pub fn parse_installed(text: &str) -> Result<Installed> {
    let line = text.lines().rev().find(|line| !line.trim().is_empty()).ok_or_else(|| anyhow!("The Linux runtime installer printed nothing."))?;
    let value: Value = serde_json::from_str(line.trim()).context("The Linux runtime installer returned invalid JSON")?;
    if value.get("needsArchive").is_some() {
        return Ok(Installed::NeedsArchive);
    }
    let node = value.get("node").and_then(Value::as_str).filter(|path| path.starts_with('/'));
    let host = value.get("host").and_then(Value::as_str).filter(|path| path.starts_with('/'));
    match (node, host) {
        (Some(node), Some(host)) => Ok(Installed::Ready { node: node.to_owned(), host: host.to_owned() }),
        _ => bail!("The runtime files of the execution environment are invalid: {}", line.trim()),
    }
}

/// Delete runtimes earlier versions unpacked (`install-runtime.py --prune METADATA`).
pub fn prune_runtimes(distro: &str, files: &RuntimeFiles) -> Result<Vec<String>> {
    let paths = linux_paths(distro, &[&files.installer, &files.metadata])?;
    let text = checked(wsl(distro, &["python3", &paths[0], "--prune", &paths[1]]).output()?, "Pruning old runtimes")?;
    let value: Value = serde_json::from_str(text.lines().last().unwrap_or("{}")).unwrap_or(Value::Null);
    Ok(value.get("removed").and_then(Value::as_array).map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect()).unwrap_or_default())
}

/// The Host inside the distribution: `wsl.exe --distribution D --exec env K=V… node host.js` (`WslHostTransport`).
pub fn host_command(distro: &str, node: &str, host: &str, env: &[(String, String)]) -> HostCommand {
    let mut args: Vec<OsString> = ["--distribution", distro, "--exec", "env"].into_iter().map(OsString::from).collect();
    if let Ok(home) = std::env::var("RAINY_DEV_WSL_HOME") {
        args.push(format!("HOME={home}").into());
    }
    args.extend(env.iter().map(|(name, value)| OsString::from(format!("{name}={value}"))));
    args.push(node.into());
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
    use super::*;

    #[test]
    fn parses_installer_answers() {
        assert_eq!(parse_installed("noise\n{\"needsArchive\": true}\n").unwrap(), Installed::NeedsArchive);
        assert_eq!(
            parse_installed(r#"{"node": "/h/.rainy-agent/runtime/x/node/bin/node", "host": "/h/.rainy-agent/runtime/x/app/dist/host.js"}"#).unwrap(),
            Installed::Ready { node: "/h/.rainy-agent/runtime/x/node/bin/node".into(), host: "/h/.rainy-agent/runtime/x/app/dist/host.js".into() }
        );
        assert!(parse_installed(r#"{"node": "relative", "host": "/x"}"#).is_err());
        assert!(parse_installed("").is_err());
    }

    #[test]
    fn builds_the_host_command() {
        let command = host_command("Ubuntu", "/n/node", "/a/host.js", &[("RAINY_HOME".into(), "/home/u/.rainy-agent".into())]);
        let args: Vec<String> = command.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(&args[..4], ["--distribution", "Ubuntu", "--exec", "env"]);
        assert!(args.contains(&"RAINY_HOME=/home/u/.rainy-agent".to_owned()));
        assert_eq!(&args[args.len() - 2..], ["/n/node", "/a/host.js"]);
        assert!(command.wait_for_loopback);
    }
}
