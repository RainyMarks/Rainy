//! Where Rainy keeps its files, and how it starts the Host (`start()` in `src/main/main.ts`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use rainy_carrier::wsl::{self, Installed, RuntimeFiles};
use rainy_carrier::{ExecutionTarget, HostCommand, HostEvents, credential, proxy, targets};

/// Folders this run uses.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `%APPDATA%\RainyAgent`, shared with Rainy 3.x so settings, keys and chats carry over; `--user-data-dir` overrides it.
    pub user_data: PathBuf,
    /// `<install>\resources` when installed; the folder holding `windows-host` in development.
    pub resources: PathBuf,
    /// The installation folder (the working directory of the native Host).
    pub install_root: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        let exe = std::env::current_exe().context("Locating Rainy.exe")?;
        let install_root = exe.parent().context("Rainy.exe has no folder")?.to_path_buf();
        let user_data = match argument_value("--user-data-dir") {
            Some(path) => PathBuf::from(path),
            None => PathBuf::from(std::env::var_os("APPDATA").context("APPDATA is not set")?).join("RainyAgent"),
        };
        std::fs::create_dir_all(&user_data).with_context(|| format!("Creating {}", user_data.display()))?;
        let resources = match std::env::var_os("RAINY_DEV_RESOURCES") {
            Some(path) => PathBuf::from(path),
            None if install_root.join("resources").join("windows-host").is_dir() => install_root.join("resources"),
            None => dev_resources(&install_root).context("The Windows runtime files are missing. Install the full Rainy package.")?,
        };
        Ok(Self { user_data, resources, install_root })
    }

    pub fn log_path(&self) -> PathBuf {
        self.user_data.join("host.log")
    }

    /// `desktop.json`, shared with Rainy 3.x.
    pub fn desktop_settings(&self) -> PathBuf {
        self.user_data.join("desktop.json")
    }
}

/// A development build finds `tmp/dev-resources/windows-host` above `native/target/<profile>`.
fn dev_resources(start: &Path) -> Option<PathBuf> {
    start.ancestors().map(|dir| dir.join("tmp").join("dev-resources")).find(|dir| dir.join("windows-host").is_dir())
}

fn argument_value(name: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
            return Some(value.to_owned());
        }
        if arg == name {
            return args.next();
        }
    }
    None
}

/// The native Windows Host (`WindowsHostTransport`).
pub fn windows_host(paths: &Paths) -> Result<HostCommand> {
    let root = paths.resources.join("windows-host");
    let node = root.join("node").join("node.exe");
    let host = root.join("app").join("dist").join("host.js");
    if !node.is_file() || !host.is_file() {
        bail!("The files for running natively on Windows are missing. Install the full Rainy package.");
    }
    let data = |name: &str| paths.user_data.join(name).display().to_string();
    let php = paths.user_data.join("modules").join("php").join("php.exe");
    let mut env = shared_env(&ExecutionTarget::windows());
    env.extend(
        [
            ("RAINY_CARRIER_STATE_ROOT", data("carrier-state")),
            ("RAINY_HOME", data("native-home")),
            ("RAINY_TOOLCHAIN_ROOT", data("env")),
            ("RAINY_BUILTIN_PHP", php.display().to_string()),
            // Module compilation is cached across launches; resolution still dominates the native Host's startup.
            ("NODE_COMPILE_CACHE", data("node-compile-cache")),
            // Model requests follow the Windows system proxy, which the Host asks this process for.
            ("RAINY_CARRIER_PROXY", "1".to_owned()),
        ]
        .map(|(name, value)| (name.to_owned(), value)),
    );
    Ok(HostCommand {
        program: node,
        args: vec![host.into()],
        env: env.into_iter().map(|(name, value)| (name.into(), value.into())).collect(),
        cwd: Some(paths.install_root.clone()),
        scrub_environment: true,
        wait_for_loopback: false,
        expected_port: None,
    })
}

/// Variables every Host gets.
fn shared_env(target: &ExecutionTarget) -> Vec<(String, String)> {
    let mut env = vec![
        ("RAINY_EXECUTION_TARGET_ID".to_owned(), target.id().to_owned()),
        ("RAINY_EXECUTION_TARGET_LABEL".to_owned(), target.label().to_owned()),
        // The Host asks this process for the key that encrypts its API keys.
        ("RAINY_CARRIER_CREDENTIALS".to_owned(), "1".to_owned()),
    ];
    if std::env::var("RAINY_CONFIGURE_DEEPSEEK").as_deref() == Ok("1") {
        env.push(("RAINY_CONFIGURE_DEEPSEEK".to_owned(), "1".to_owned()));
    }
    env
}

/// This version's Linux runtime files.
pub fn runtime_files(paths: &Paths) -> RuntimeFiles {
    let base = paths.resources.join("linux-runtime.tar.xz");
    RuntimeFiles {
        metadata: paths.resources.join("linux-runtime.json"),
        app: paths.resources.join("linux-app.tar.gz"),
        installer: paths.resources.join("install-runtime.py"),
        base: base.is_file().then_some(base),
    }
}

/// Install this version's runtime in the distribution and describe its Host (`prepareWsl`).
pub fn wsl_host(paths: &Paths, target: &ExecutionTarget, progress: &dyn Fn(String)) -> Result<HostCommand> {
    let ExecutionTarget::Wsl { distro, .. } = target else { bail!("Not a WSL target") };
    progress(format!("WSL · {distro}"));
    let files = runtime_files(paths);
    let carrier_state = paths.user_data.join("carrier-state");
    std::fs::create_dir_all(&carrier_state)?;
    let Installed::Ready { node, host } = wsl::install_runtime(distro, &files)? else {
        bail!("The Linux runtime of this version is not installed in {distro}, and it could not be downloaded.");
    };
    let state = wsl::linux_paths(distro, &[&carrier_state])?.remove(0);
    let mut env = shared_env(target);
    env.push(("RAINY_CARRIER_STATE_ROOT".to_owned(), state));
    // Runtimes earlier versions unpacked are removed in the background; a failure only costs disk space.
    let (prune_distro, prune_files) = (distro.clone(), files.clone());
    std::thread::spawn(move || {
        let _ = wsl::prune_runtimes(&prune_distro, &prune_files);
    });
    Ok(wsl::host_command(distro, &node, &host, &env))
}

/// The target this launch uses and its Host.
pub fn prepare(paths: &Paths, progress: &dyn Fn(String)) -> Result<(ExecutionTarget, HostCommand)> {
    let preferences = targets::read_preferences(&paths.desktop_settings())?;
    let target = targets::saved_target(&preferences).unwrap_or_else(|_| ExecutionTarget::windows());
    match &target {
        ExecutionTarget::Windows { .. } => Ok((target.clone(), windows_host(paths)?)),
        ExecutionTarget::Wsl { .. } => Ok((target.clone(), wsl_host(paths, &target, progress)?)),
        ExecutionTarget::Ssh { label, .. } => bail!("{label}: SSH targets are not supported by this build yet."),
    }
}

/// Answers the Host's requests and writes its output to `host.log`.
pub struct CarrierEvents {
    user_data: PathBuf,
    log: Mutex<Option<std::fs::File>>,
    key: Mutex<Option<Option<String>>>,
    pub on_exit: Mutex<Option<Box<dyn Fn(Option<i32>) + Send>>>,
}

impl CarrierEvents {
    pub fn new(paths: &Paths) -> Arc<Self> {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(paths.log_path()).ok();
        Arc::new(Self { user_data: paths.user_data.clone(), log: Mutex::new(log), key: Mutex::new(None), on_exit: Mutex::new(None) })
    }

    pub fn write_log(&self, line: &str) {
        use std::io::Write as _;
        if let Some(file) = self.log.lock().unwrap().as_mut() {
            let _ = writeln!(file, "{line}");
        }
    }
}

impl HostEvents for CarrierEvents {
    fn credential_key(&self) -> Option<String> {
        // Read or created once per run; every Host started in this run encrypts API keys with it.
        let mut cached = self.key.lock().unwrap();
        if let Some(key) = cached.as_ref() {
            return key.clone();
        }
        let key = credential::credential_key(&self.user_data, &credential::Dpapi, &|line| self.write_log(line)).unwrap_or_else(|error| {
            self.write_log(&format!("API keys stay unencrypted for this run: {error:#}"));
            None
        });
        *cached = Some(key.clone());
        key
    }

    fn system_proxy(&self, url: &str) -> Option<String> {
        proxy::system_proxy(url)
    }

    fn diagnostic(&self, line: &str) {
        self.write_log(line);
    }

    fn exited(&self, code: Option<i32>) {
        self.write_log(&format!("Host exited: {code:?}"));
        if let Some(callback) = self.on_exit.lock().unwrap().as_ref() {
            callback(code);
        }
    }
}
