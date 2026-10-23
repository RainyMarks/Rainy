//! What the carrier chose for this Host process (3.x `env.ts`): its home, its execution target and its files.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

/// Resolved paths and identities of one Host.
#[derive(Debug, Clone)]
pub struct HostEnv {
    pub version: &'static str,
    /// Host data root (`RAINY_HOME`, else `~/.rainy-agent`).
    pub home: PathBuf,
    /// The folder holding `dist/renderer` and `resources`.
    pub app_root: PathBuf,
    /// `<app root>/resources`.
    pub resources: PathBuf,
    pub windows: bool,
    /// `windows-local`, `wsl:<id>` or `ssh:<destination>`.
    pub target_id: String,
    /// Shown in the status bar, such as `WSL · Ubuntu`.
    pub target_label: Option<String>,
    /// Shared by the Windows and WSL Hosts for project identity and memory.
    pub carrier_state_root: PathBuf,
    /// Installed offline components (`RAINY_TOOLCHAIN_ROOT`).
    pub toolchain_root: PathBuf,
    /// PHP from the optional module, when installed.
    pub builtin_php: Option<PathBuf>,
    /// PowerShell chosen with `RAINY_PWSH_PATH`.
    pub pwsh_path: Option<PathBuf>,
    /// Save and select the DeepSeek preset at startup (`RAINY_CONFIGURE_DEEPSEEK=1`), for development.
    pub configure_deepseek: bool,
    /// Temporary files of this process.
    pub tmp: PathBuf,
    pub port: u16,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// The application folder: `RAINY_APP_ROOT`, else `app` beside the executable, else the executable's own folder.
fn app_root() -> Result<PathBuf> {
    if let Some(root) = var("RAINY_APP_ROOT") {
        return Ok(PathBuf::from(root));
    }
    let exe = std::env::current_exe().context("The Host cannot find its own program file")?;
    let folder = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    for candidate in [folder.join("app"), folder.clone()] {
        if candidate.join("resources").is_dir() {
            return Ok(candidate);
        }
    }
    bail!("Rainy application files not found beside {}", exe.display())
}

pub fn home_dir() -> PathBuf {
    let home = if cfg!(windows) { var("USERPROFILE") } else { var("HOME") };
    home.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

impl HostEnv {
    pub fn read() -> Result<Self> {
        let app_root = app_root()?;
        let home = var("RAINY_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".rainy-agent"));
        let home = std::path::absolute(&home).unwrap_or(home);
        let windows = cfg!(windows);
        let port = match var("RAINY_PORT") {
            Some(port) => port.parse::<u16>().context("RAINY_PORT must be a TCP port number")?,
            None => 0,
        };
        Ok(Self {
            version: env!("CARGO_PKG_VERSION"),
            resources: app_root.join("resources"),
            app_root,
            windows,
            target_id: var("RAINY_EXECUTION_TARGET_ID").unwrap_or_else(|| if windows { "windows-local" } else { "wsl:legacy" }.to_owned()),
            target_label: var("RAINY_EXECUTION_TARGET_LABEL"),
            carrier_state_root: var("RAINY_CARRIER_STATE_ROOT").map(PathBuf::from).unwrap_or_else(|| home.join("carrier-state")),
            toolchain_root: var("RAINY_TOOLCHAIN_ROOT").map(PathBuf::from).unwrap_or_else(|| home.join("components")),
            builtin_php: var("RAINY_BUILTIN_PHP").map(PathBuf::from),
            pwsh_path: var("RAINY_PWSH_PATH").map(PathBuf::from),
            configure_deepseek: var("RAINY_CONFIGURE_DEEPSEEK").as_deref() == Some("1"),
            tmp: std::env::temp_dir().join(format!("rainy-agent-{}", std::process::id())),
            home,
            port,
        })
    }

    /// `Windows`, `WSL` or `SSH`, as the status bar and the model name the machine.
    pub fn kind(&self) -> &'static str {
        if self.target_id.starts_with("ssh:") {
            "SSH"
        } else if self.windows {
            "Windows"
        } else {
            "WSL"
        }
    }

    pub fn platform(&self) -> &'static str {
        if self.windows { "win32" } else { "linux" }
    }

    /// The folder of the built pages the WebView2 panes load.
    pub fn renderer_root(&self) -> PathBuf {
        self.app_root.join("dist").join("renderer")
    }
}
