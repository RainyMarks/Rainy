//! Where Rainy keeps its files, and how it starts the Host (`start()` in `src/main/main.ts`).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use rainy_carrier::askpass::Askpass;
use rainy_carrier::download::DownloadError;
use rainy_carrier::modules::{ModuleError, Modules};
use rainy_carrier::ssh::{self, InstallProgress, ServerRuntime};
use rainy_carrier::trust::{self, Integrity, Recheck, TrustError, Verified};
use rainy_carrier::wsl::{self, Installed, RuntimeFiles};
use rainy_carrier::{ExecutionTarget, HostCommand, HostEvents, HostProcess, credential, launch, maintenance, proxy, targets};
use serde_json::{Value, json};

use crate::i18n::{self, Locale};

/// How long after the workbench is ready the background maintenance starts (3.x `BACKGROUND_CHECK_DELAY_MS`).
pub const MAINTENANCE_DELAY: Duration = Duration::from_secs(60);
/// Startup progress is shown at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);
/// The module holding the base of the Linux runtime.
const LINUX_RUNTIME: &str = "linux-runtime";

/// Folders this run uses.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `%APPDATA%\RainyAgent`, shared with Rainy 3.x so settings, keys and chats carry over; `--user-data-dir` overrides it.
    pub user_data: PathBuf,
    /// `<install>\resources` when installed; the folder holding `windows-host` in development.
    pub resources: PathBuf,
    /// The installation folder (the working directory of the native Host).
    pub install_root: PathBuf,
    /// Whether `resources` are an installation's rather than development ones.
    pub installed: bool,
}

/// `%APPDATA%\RainyAgent`, or the `--user-data-dir` given; created if missing.
pub fn user_data_dir() -> Result<PathBuf> {
    let user_data = user_data_path()?;
    std::fs::create_dir_all(&user_data).with_context(|| format!("Creating {}", user_data.display()))?;
    Ok(user_data)
}

/// `%APPDATA%\RainyAgent`, or the `--user-data-dir` given, whether or not it exists.
pub fn user_data_path() -> Result<PathBuf> {
    Ok(match launch::switch_value(&std::env::args_os().collect::<Vec<_>>(), "--user-data-dir") {
        // The Host runs in the installation folder, so a relative folder would mean a different place to it.
        Some(path) => std::path::absolute(&path).with_context(|| format!("Resolving {}", path.display()))?,
        None => PathBuf::from(std::env::var_os("APPDATA").context("APPDATA is not set")?).join("RainyAgent"),
    })
}

impl Paths {
    pub fn discover(user_data: PathBuf) -> Result<Self> {
        let exe = std::env::current_exe().context("Locating Rainy.exe")?;
        let install_root = exe.parent().context("Rainy.exe has no folder")?.to_path_buf();
        let development = std::env::var_os("RAINY_DEV_RESOURCES").map(PathBuf::from);
        let Some((resources, installed)) = find_resources(&install_root, development, checks_resources()) else {
            bail!(i18n::text(saved_locale(&user_data), "native.windowsHostMissing"));
        };
        Ok(Self { user_data, resources, install_root, installed })
    }

    /// An installed release build (3.x `app.isPackaged`): its resources are checked against their signed list, and what
    /// earlier versions left behind is removed.
    pub fn packaged(&self) -> bool {
        self.installed && !cfg!(debug_assertions)
    }

    pub fn log_path(&self) -> PathBuf {
        self.user_data.join("host.log")
    }

    /// `desktop.json`, shared with Rainy 3.x.
    pub fn desktop_settings(&self) -> PathBuf {
        desktop_settings(&self.user_data)
    }
}

fn desktop_settings(user_data: &Path) -> PathBuf {
    user_data.join("desktop.json")
}

/// Whether this build runs only the resources it is installed with, and checks them before use: a release build with
/// release keys.
fn checks_resources() -> bool {
    !cfg!(debug_assertions) && crate::release::keyring().is_some()
}

/// The resources this run uses, and whether they are an installation's. `installed_only` takes only `<install>\resources`
/// (3.x `process.resourcesPath`); otherwise `development` (`RAINY_DEV_RESOURCES`) comes first, and resources copied next
/// to a build and the repository's `tmp\dev-resources` are used too.
fn find_resources(install_root: &Path, development: Option<PathBuf>, installed_only: bool) -> Option<(PathBuf, bool)> {
    if installed_only {
        if development.is_some() {
            log::warn!("RAINY_DEV_RESOURCES is ignored: this build runs only the resources installed with it.");
        }
        return maintenance::installed_resources(install_root).map(|resources| (resources, true));
    }
    if let Some(path) = development {
        return Some((path, false));
    }
    if let Some(resources) = maintenance::installed_resources(install_root) {
        return Some((resources, true));
    }
    let copied = install_root.join("resources");
    if copied.join("windows-host").is_dir() {
        return Some((copied, false));
    }
    dev_resources(install_root).map(|resources| (resources, false))
}

/// A development build finds `tmp\dev-resources\windows-host` in the repository it was built in.
fn dev_resources(install_root: &Path) -> Option<PathBuf> {
    let dir = maintenance::checkout(install_root)?.join("tmp").join("dev-resources");
    dir.join("windows-host").is_dir().then_some(dir)
}

/// The native Windows Host (`WindowsHostTransport`).
pub fn windows_host(paths: &Paths) -> Result<HostCommand> {
    let root = paths.resources.join("windows-host");
    let native = root.join("rainy-host.exe");
    let node = root.join("node").join("node.exe");
    let host = root.join("app").join("dist").join("host.js");
    if !native.is_file() && (!node.is_file() || !host.is_file()) {
        bail!(i18n::text(saved_locale(&paths.user_data), "native.windowsHostMissing"));
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
    let (program, args) = if native.is_file() { (native, Vec::new()) } else { (node, vec![host.into()]) };
    Ok(HostCommand {
        program,
        args,
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
    // The agent can open the project's own development server in a hidden page and look at it; an SSH server's
    // localhost is not this machine's, so a remote Host is not offered it.
    if !matches!(target, ExecutionTarget::Ssh { .. }) {
        env.push(("RAINY_CARRIER_PREVIEW".to_owned(), "1".to_owned()));
    }
    if std::env::var("RAINY_CONFIGURE_DEEPSEEK").as_deref() == Ok("1") {
        env.push(("RAINY_CONFIGURE_DEEPSEEK".to_owned(), "1".to_owned()));
    }
    env
}

/// This version's Linux runtime files. The base is the downloaded `linux-runtime` module, which may not be there yet;
/// a development checkout stages it as `linux-runtime.tar.xz` beside the other resources.
pub fn runtime_files(paths: &Paths, modules: &Modules) -> RuntimeFiles {
    RuntimeFiles {
        metadata: paths.resources.join("linux-runtime.json"),
        app: paths.resources.join("linux-app.tar.gz"),
        installer: paths.resources.join("install-runtime.py"),
        base: development_base(paths).or_else(|| modules.path(LINUX_RUNTIME).ok()),
    }
}

/// The base archive a development checkout staged (3.x `linuxRuntimeArchive` when not packaged).
fn development_base(paths: &Paths) -> Option<PathBuf> {
    let base = paths.resources.join("linux-runtime.tar.xz");
    (!paths.packaged() && base.is_file()).then_some(base)
}

/// What the user chose after the Linux runtime could not be downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeChoice {
    Retry,
    /// Start the Windows Host instead, and keep using Windows.
    Windows,
    Quit,
}

/// Asks the window what to do after the Linux runtime could not be downloaded, showing why.
pub type AskRuntime<'a> = dyn Fn(String) -> RuntimeChoice + Sync + 'a;

/// Whole megabytes, for progress text.
fn megabytes(bytes: u64) -> u64 {
    (bytes as f64 / 1_048_576.0).round() as u64
}

/// Whether progress is due: at most every [`PROGRESS_INTERVAL`], and always the last step.
fn due(shown: &Mutex<Option<Instant>>, last: bool) -> bool {
    let mut shown = shown.lock().unwrap();
    if !last && shown.is_some_and(|at| at.elapsed() < PROGRESS_INTERVAL) {
        return false;
    }
    *shown = Some(Instant::now());
    true
}

/// Download this version's Linux runtime base with its progress (`downloadLinuxRuntime`). After a failure the window
/// asks: retry, run on Windows instead (`Ok(false)`), or quit, which stops startup.
fn download_linux_runtime(modules: &Modules, locale: Locale, ask: &AskRuntime<'_>, progress: &(dyn Fn(Step) + Sync)) -> Result<bool> {
    let shown = Mutex::new(None);
    let report = |done: u64, total: u64| {
        if due(&shown, done >= total) {
            let text = i18n::format(locale, "native.linuxDownloading", &[("done", &megabytes(done)), ("total", &megabytes(total))]);
            progress(Step { text, fraction: (total > 0).then(|| done as f32 / total as f32) });
        }
    };
    loop {
        match modules.install(LINUX_RUNTIME, &report) {
            Ok(()) => return Ok(true),
            Err(error) => {
                log::warn!("The Linux runtime was not downloaded: {error}");
                // Only quitting Rainy stops this download.
                if error.cancelled() || error == ModuleError::Closing {
                    bail!(i18n::text(locale, "native.linuxNotDownloaded"));
                }
                match ask(module_message(locale, &error)) {
                    RuntimeChoice::Retry => {}
                    RuntimeChoice::Windows => return Ok(false),
                    RuntimeChoice::Quit => bail!(i18n::text(locale, "native.linuxNotDownloaded")),
                }
            }
        }
    }
}

/// The unpacked runtime is all a target needs; only the next version downloads an archive again.
fn remove_linux_runtime(paths: &Paths, modules: &Modules, events: &CarrierEvents) {
    if paths.packaged()
        && let Err(error) = modules.remove(LINUX_RUNTIME)
    {
        events.write_log(&format!("The Linux runtime archive was not removed: {error}"));
    }
}

/// Install this version's runtime in the distribution and describe its Host (`prepareWsl`), with the removal of the
/// runtimes earlier versions unpacked there. A missing base is downloaded first; `None` when the user then chose to
/// run on Windows instead.
pub fn wsl_host(
    paths: &Paths,
    target: &ExecutionTarget,
    modules: &Modules,
    events: &CarrierEvents,
    ask: &AskRuntime<'_>,
    progress: &(dyn Fn(Step) + Sync),
) -> Result<Option<(HostCommand, Option<Prune>)>> {
    let ExecutionTarget::Wsl { distro, .. } = target else { bail!("Not a WSL target") };
    let locale = saved_locale(&paths.user_data);
    let starting = || progress(i18n::format(locale, "native.startingWsl", &[("distro", distro)]).into());
    starting();
    let mut files = runtime_files(paths, modules);
    let carrier_state = paths.user_data.join("carrier-state");
    std::fs::create_dir_all(&carrier_state)?;
    let mut installed = wsl::install_runtime(distro, &files)?;
    if installed == Installed::NeedsArchive {
        if !download_linux_runtime(modules, locale, ask, progress)? {
            return Ok(None);
        }
        starting();
        files.base = Some(modules.path(LINUX_RUNTIME)?);
        installed = wsl::install_runtime(distro, &files)?;
    }
    let Installed::Ready { node, host } = installed else { bail!(i18n::text(locale, "native.linuxMismatch")) };
    remove_linux_runtime(paths, modules, events);
    let state = wsl::linux_paths(distro, &[&carrier_state])?.remove(0);
    let mut env = shared_env(target);
    env.push(("RAINY_CARRIER_STATE_ROOT".to_owned(), state));
    // A failure only costs disk space.
    let prune = paths.packaged().then(|| {
        let (distro, files) = (distro.clone(), files.clone());
        Box::new(move || match wsl::prune_runtimes(&distro, &files) {
            Ok(removed) if !removed.is_empty() => log::info!("Removed {} WSL runtimes that earlier versions unpacked", removed.len()),
            Ok(_) => {}
            Err(error) => log::warn!("Earlier WSL runtimes were not removed: {error:#}"),
        }) as Prune
    });
    Ok(Some((wsl::host_command(distro, &node, &host, &env), prune)))
}

/// The language of messages shown before the Host answers, as the last run recorded it.
pub fn saved_locale(user_data: &Path) -> Locale {
    let preferences = targets::read_preferences(&desktop_settings(user_data)).unwrap_or_default();
    Locale::from_code(preferences.get("locale").and_then(Value::as_str).unwrap_or("zh"))
}

/// One startup step for the window: what is happening and, when it is known, how much of it is done (0 to 1).
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub text: String,
    pub fraction: Option<f32>,
}

impl From<String> for Step {
    fn from(text: String) -> Self {
        Self { text, fraction: None }
    }
}

/// Work a Host's start leaves for the background maintenance: removing the Linux runtimes earlier versions installed
/// for its target (`--prune`). Only an installed release build has any.
pub type Prune = Box<dyn FnOnce() + Send>;

/// A started Host.
pub struct Started {
    pub target: ExecutionTarget,
    pub host: HostProcess,
    pub prune: Option<Prune>,
}

/// Check the installed resources against their signed list before anything from them runs (`ensureReleaseIntegrity`),
/// showing the progress of a full check. A full check is the first start of a version, so the files an update left in
/// `windows-host` that the new list does not cover are removed before the Host can load them. `None` when this run
/// checks nothing: a debug build, development resources, or a release build made without keys
/// (`RAINY_ALLOW_UNSIGNED=1`). The error is the message the window shows.
pub fn verify_resources(paths: &Paths, progress: &(dyn Fn(Step) + Sync)) -> Result<Option<Integrity>> {
    if !paths.packaged() {
        let build = if cfg!(debug_assertions) { "a debug build" } else { "development resources" };
        log::info!("The resources in {} are not checked: {build}", paths.resources.display());
        return Ok(None);
    }
    let Some(keyring) = crate::release::keyring() else {
        log::warn!("This build has no release keys, so the installed resources are not checked.");
        return Ok(None);
    };
    let locale = saved_locale(&paths.user_data);
    let shown = Mutex::new(None::<Instant>);
    let report = |done: usize, total: usize| {
        let mut shown = shown.lock().unwrap();
        if done < total && shown.is_some_and(|at| at.elapsed() < PROGRESS_INTERVAL) {
            return;
        }
        *shown = Some(Instant::now());
        let text = i18n::format(locale, "native.verifying", &[("done", &grouped(done)), ("total", &grouped(total))]);
        progress(Step { text, fraction: Some(done as f32 / total as f32) });
    };
    let stamp = paths.user_data.join(trust::STAMP_FILE);
    let options = trust::Options { resources: &paths.resources, keyring, stamp: &stamp, progress: &report };
    let started = Instant::now();
    match trust::ensure(&options, trust::now_ms()) {
        Ok(integrity) => {
            log::info!("Installed resources verified ({}) in {} ms", integrity.verified, started.elapsed().as_millis());
            if integrity.verified == Verified::Full {
                prune_windows_host(paths);
            }
            progress(i18n::text(locale, "native.starting").to_owned().into());
            Ok(Some(integrity))
        }
        Err(error) => {
            log::error!("Installed resources were not accepted: {error}");
            Err(anyhow!(trust_message(locale, &error)))
        }
    }
}

/// What the window says about resources that were not accepted.
pub fn trust_message(locale: Locale, error: &TrustError) -> String {
    let with = |key: &str, name: &str, value: &str| i18n::format(locale, key, &[(name, &value)]);
    match error {
        TrustError::Invalid(detail) => with("native.trustInvalid", "detail", detail),
        TrustError::Encoding => i18n::text(locale, "native.trustEncoding").to_owned(),
        TrustError::Signature => i18n::text(locale, "native.trustSignature").to_owned(),
        TrustError::DuplicatePaths => i18n::text(locale, "native.trustDuplicates").to_owned(),
        TrustError::Outside => i18n::text(locale, "native.trustOutside").to_owned(),
        TrustError::Missing(path) => with("native.trustMissing", "path", path),
        TrustError::Damaged(path) => with("native.trustDamaged", "path", path),
        TrustError::Replaced(path) => with("native.trustReplaced", "path", path),
        TrustError::Mismatch(path) => with("native.trustMismatch", "path", path),
        TrustError::Io(detail) => with("native.trustUnreadable", "detail", detail),
    }
}

/// What the window says about a module operation that failed.
pub fn module_message(locale: Locale, error: &ModuleError) -> String {
    let with = |key: &str, name: &str, value: &str| i18n::format(locale, key, &[(name, &value)]);
    match error {
        ModuleError::Unavailable => i18n::text(locale, "native.modulesUnavailable").to_owned(),
        ModuleError::Invalid(detail) => with("native.modulesInvalid", "detail", detail),
        ModuleError::NotOffered(id) => with("native.moduleNotOffered", "id", id),
        ModuleError::Busy => i18n::text(locale, "native.moduleBusy").to_owned(),
        ModuleError::Closing => i18n::text(locale, "native.moduleClosing").to_owned(),
        ModuleError::RemoveBusy => i18n::text(locale, "native.moduleRemoveBusy").to_owned(),
        ModuleError::Archive(detail) => with("native.moduleArchive", "detail", detail),
        ModuleError::Download(error) => download_message(locale, error),
    }
}

/// What the window says about a download that failed.
pub fn download_message(locale: Locale, error: &DownloadError) -> String {
    let with = |key: &str, name: &str, value: &dyn std::fmt::Display| i18n::format(locale, key, &[(name, value)]);
    let text = |key: &str| i18n::text(locale, key).to_owned();
    match error {
        DownloadError::Insecure(url) => with("native.downloadInsecure", "url", url),
        DownloadError::Redirects => text("native.downloadRedirects"),
        DownloadError::Network(detail) => with("native.downloadNetwork", "detail", detail),
        DownloadError::Stalled(seconds) => with("native.downloadStalled", "seconds", seconds),
        DownloadError::Interrupted => text("native.downloadInterrupted"),
        DownloadError::Status(status) => with("native.downloadHttp", "status", status),
        DownloadError::InvalidResume => text("native.downloadResume"),
        DownloadError::InvalidResponse => text("native.downloadResponse"),
        DownloadError::TooLarge => text("native.downloadTooLarge"),
        DownloadError::Mismatch => text("native.downloadMismatch"),
        DownloadError::FileMismatch => text("native.downloadFileMismatch"),
        DownloadError::InvalidCache => text("native.downloadCache"),
        DownloadError::Link(path) => with("native.downloadLink", "path", path),
        DownloadError::Cancelled => text("native.downloadCancelled"),
        DownloadError::Io(detail) => with("native.downloadIo", "detail", detail),
    }
}

/// A count with its thousands separated, as `toLocaleString` writes it in Chinese and in English.
fn grouped(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// The daily recheck of the installed files (`runBackgroundChecks`). A failure that means files were changed or
/// damaged is returned for the window to show; any other is only logged. After either, the next start checks every
/// file again.
pub fn recheck_resources(integrity: &Integrity) -> Option<TrustError> {
    match integrity.recheck(trust::now_ms(), trust::RECHECK_INTERVAL_MS) {
        Ok(Recheck::Skipped) => None,
        Ok(outcome) => {
            log::info!("Installed resources rechecked: {outcome:?}");
            None
        }
        Err(error) => {
            log::warn!("Release resource recheck failed: {error}");
            error.is_integrity().then_some(error)
        }
    }
}

/// Remove what interrupted module installations left and downloads this version does not pin (`modules.clean`), what
/// earlier versions left in the installation (`removeRetiredResources`, `pruneWindowsHost`), keeping what the accepted
/// signed list covers, then run the Host's `prune`. Failures only cost disk space and are logged.
pub fn clean_up(paths: &Paths, integrity: Option<&Integrity>, prune: Option<Prune>, modules: &Modules) {
    // Development resources pin no modules, so nothing is removed there.
    match modules.clean() {
        Ok(()) | Err(ModuleError::Unavailable) => {}
        Err(error) => log::warn!("Component cleanup failed: {error}"),
    }
    if paths.packaged() {
        let signed = integrity.map(|integrity| integrity.manifest.files.iter().map(|file| file.path.to_lowercase()).collect()).unwrap_or_default();
        let removed = maintenance::remove_retired(&paths.install_root, &paths.resources, &signed);
        if !removed.names.is_empty() {
            log::info!("Removed resources that earlier installers left behind: {}", removed.names.join(", "));
        }
        for failure in removed.failed {
            log::warn!("A resource an earlier installer left behind was not removed: {failure}");
        }
        prune_windows_host(paths);
    }
    if let Some(prune) = prune {
        prune();
    }
}

/// Delete the files below `windows-host` that its `runtime.json` does not list: an update overwrites the files it ships
/// but leaves those the new version dropped.
fn prune_windows_host(paths: &Paths) {
    match maintenance::prune_windows_host(&paths.resources, false) {
        Ok(0) => {}
        Ok(bytes) => log::info!("Removed {:.0} MiB of Windows runtime files that earlier versions left behind", bytes as f64 / 1_048_576.0),
        Err(error) => log::warn!("Old Windows runtime files were not removed: {error:#}"),
    }
}

/// The target this launch uses, and its Host once it is ready. When the Linux runtime could not be downloaded and the
/// user chose to run on Windows instead, the Windows Host starts and Windows is recorded for later launches.
pub fn start(
    paths: &Paths,
    events: Arc<CarrierEvents>,
    askpass: Option<&Arc<Askpass>>,
    modules: &Modules,
    ask: &AskRuntime<'_>,
    progress: &(dyn Fn(Step) + Sync),
) -> Result<Started> {
    let preferences = targets::read_preferences(&paths.desktop_settings())?;
    let saved = targets::saved_target(&preferences).unwrap_or_else(|_| ExecutionTarget::windows());
    let prepared = match &saved {
        ExecutionTarget::Windows { .. } => Some((windows_host(paths)?, None)),
        ExecutionTarget::Wsl { .. } => wsl_host(paths, &saved, modules, &events, ask, progress)?,
        ExecutionTarget::Ssh { .. } => {
            let askpass = askpass.context("Rainy could not start the service that answers SSH questions.")?;
            if let Some((host, prune)) = ssh_host(paths, &saved, events.clone(), askpass, modules, ask, progress)? {
                return Ok(Started { target: saved, host, prune });
            }
            None
        }
    };
    let Some((command, prune)) = prepared else {
        progress(i18n::text(saved_locale(&paths.user_data), "native.starting").to_owned().into());
        let host = HostProcess::start(&windows_host(paths)?, events)?;
        let target = ExecutionTarget::windows();
        if let Err(error) = targets::save_target(&paths.desktop_settings(), &target) {
            log::warn!("The execution target was not saved: {error:#}");
        }
        return Ok(Started { target, host, prune: None });
    };
    Ok(Started { host: HostProcess::start(&command, events)?, target: saved, prune })
}

/// Install this version's runtime on the server and start the Host there over SSH (`prepareSsh`, `startSshHost`), with
/// the removal of the runtimes earlier versions installed there. When the server cannot download the base itself, this
/// computer downloads it and uploads it; `None` when the user then chose to run on Windows instead.
fn ssh_host(
    paths: &Paths,
    target: &ExecutionTarget,
    events: Arc<CarrierEvents>,
    askpass: &Arc<Askpass>,
    modules: &Modules,
    ask: &AskRuntime<'_>,
    progress: &(dyn Fn(Step) + Sync),
) -> Result<Option<(HostProcess, Option<Prune>)>> {
    let locale = saved_locale(&paths.user_data);
    let destination = ssh::destination_of(target)?;
    let name = ssh::name(&destination);
    progress(i18n::format(locale, "native.sshConnecting", &[("target", &target.label())]).into());
    let files = runtime_files(paths, modules);
    let pieces = if std::env::var("RAINY_SSH_NO_FETCH").as_deref() == Ok("1") { json!([]) } else { modules.pieces(LINUX_RUNTIME) };
    let fraction = |done: u64, total: u64| (total > 0).then(|| done as f32 / total as f32);
    let report = |step: InstallProgress| {
        progress(match step {
            InstallProgress::Downloading { done, total } => Step {
                text: i18n::format(locale, "native.sshDownloading", &[("done", &megabytes(done)), ("total", &megabytes(total))]),
                fraction: fraction(done, total),
            },
            InstallProgress::Uploading { done, total } => Step {
                text: i18n::format(locale, "native.sshUploading", &[("name", &name), ("done", &megabytes(done)), ("total", &megabytes(total))]),
                fraction: fraction(done, total),
            },
        })
    };
    let log = |line: &str| events.write_log(line);
    // One askpass session per `ssh` run; a refused login is remembered so the next run asks again.
    let installer = |run: &dyn Fn(&[(String, String)]) -> Result<std::result::Result<ServerRuntime, ssh::RunOutput>>| {
        let session = askpass.session();
        match run(session.env())? {
            Ok(answer) => Ok(answer),
            Err(output) => {
                if output.stderr.contains("Permission denied") {
                    session.failed();
                }
                Err(ssh::failure(&destination, output.code, &output.stderr))
            }
        }
    };
    let mut answer = installer(&|env| ssh::fetch_runtime(&destination, &files, &pieces, env, &report, &log))?;
    let mut uploaded = false;
    if let ServerRuntime::NeedsArchive { error } = &answer {
        if let Some(error) = error {
            events.write_log(&format!("{} could not download the Linux runtime itself: {error}", target.label()));
        }
        let base = match development_base(paths) {
            Some(base) => base,
            None => {
                if !download_linux_runtime(modules, locale, ask, progress)? {
                    return Ok(None);
                }
                modules.path(LINUX_RUNTIME)?
            }
        };
        answer = installer(&|env| ssh::receive_runtime(&destination, &files, &base, env, &report, &log))?;
        uploaded = true;
    }
    let (node, host) = ssh::installed_paths(&destination, answer, uploaded)?;
    if uploaded {
        remove_linux_runtime(paths, modules, &events);
    }
    progress(i18n::text(locale, "native.starting").to_owned().into());
    let env = shared_env(target);
    // Another port is chosen when the chosen one was taken here or on the server.
    let mut attempt = 0;
    loop {
        attempt += 1;
        let session = askpass.session();
        events.clear_recent();
        let port = ssh::free_loopback_port()?;
        let command = ssh::host_command(&destination, &node, &host, &env, port, session.env())?;
        match HostProcess::start(&command, events.clone()) {
            Ok(process) => {
                let prune = paths.packaged().then(|| {
                    let (askpass, destination, files, events) = (askpass.clone(), destination.clone(), files.clone(), events.clone());
                    Box::new(move || match ssh::prune_runtimes(&destination, &files, askpass.session().env()) {
                        Ok(removed) if !removed.is_empty() => events.write_log(&format!("Removed {} Linux runtimes that earlier versions installed", removed.len())),
                        Ok(_) => {}
                        Err(error) => events.write_log(&format!("Earlier Linux runtimes were not removed: {error:#}")),
                    }) as Prune
                });
                return Ok(Some((process, prune)));
            }
            Err(error) => {
                let said = events.recent();
                let lower = format!("{said}\n{error:#}").to_ascii_lowercase();
                if attempt < 3 && (lower.contains("forwarding") || lower.contains("address already in use")) {
                    continue;
                }
                if said.contains("Permission denied") {
                    session.failed();
                }
                return Err(if format!("{error:#}").contains("The execution Host stopped") { ssh::failure(&destination, None, &said) } else { error });
            }
        }
    }
}

/// Answers the Host's requests and writes its output to `host.log`.
pub struct CarrierEvents {
    user_data: PathBuf,
    log: Mutex<Option<std::fs::File>>,
    /// The last lines the Host (or `ssh`) wrote, to explain a start that failed.
    recent: Mutex<VecDeque<String>>,
    /// Carries the agent's preview commands to the window, each with the channel for its answer.
    pub preview: Mutex<Option<async_channel::Sender<(Value, std::sync::mpsc::Sender<Value>)>>>,
    key: Mutex<Option<Option<String>>>,
    pub on_exit: Mutex<Option<Box<dyn Fn(Option<i32>) + Send>>>,
}

impl CarrierEvents {
    pub fn new(paths: &Paths) -> Arc<Self> {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(paths.log_path()).ok();
        Arc::new(Self { user_data: paths.user_data.clone(), log: Mutex::new(log), recent: Mutex::default(), preview: Mutex::new(None), key: Mutex::new(None), on_exit: Mutex::new(None) })
    }

    pub fn clear_recent(&self) {
        self.recent.lock().unwrap().clear();
    }

    pub fn recent(&self) -> String {
        Vec::from(self.recent.lock().unwrap().clone()).join("\n")
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

    fn preview(&self, command: Value) -> Value {
        let unavailable = || json!({ "error": "The preview window is unavailable." });
        let Some(sender) = self.preview.lock().unwrap().clone() else { return unavailable() };
        let (reply, answer) = std::sync::mpsc::channel();
        if sender.send_blocking((command, reply)).is_err() {
            return unavailable();
        }
        // A load waits up to 20 s and a settle a few more; past this the window is not answering.
        answer.recv_timeout(std::time::Duration::from_secs(90)).unwrap_or_else(|_| unavailable())
    }

    fn diagnostic(&self, line: &str) {
        self.write_log(line);
        let mut recent = self.recent.lock().unwrap();
        recent.push_back(line.to_owned());
        if recent.len() > 40 {
            recent.pop_front();
        }
    }

    fn exited(&self, code: Option<i32>) {
        self.write_log(&format!("Host exited: {code:?}"));
        if let Some(callback) = self.on_exit.lock().unwrap().as_ref() {
            callback(code);
        }
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn counts_files_as_3x_shows_them() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(9015), "9,015");
        assert_eq!(grouped(1_234_567), "1,234,567");
        let text = i18n::format(Locale::Zh, "native.verifying", &[("done", &grouped(1200)), ("total", &grouped(9015))]);
        assert_eq!(text, "首次启动，正在校验安装文件 1,200 / 9,015");
    }

    #[test]
    fn a_checking_build_runs_only_installed_resources() {
        let dir = tempfile::tempdir().unwrap();
        let host = |resources: &Path| std::fs::create_dir_all(resources.join("windows-host")).unwrap();
        let repo = dir.path().join("Rainy");
        let built = repo.join("native").join("target").join("release");
        let install = dir.path().join("Programs").join("RainyAgent");
        let moved = dir.path().join("Copied").join("RainyAgent");
        let named = dir.path().join("named");
        host(&repo.join("tmp").join("dev-resources"));
        // Anything above the installation, up to the drive, as a planted `C:\tmp\dev-resources` would be.
        host(&dir.path().join("tmp").join("dev-resources"));
        host(&named);

        // Installed: the installation's own, also over RAINY_DEV_RESOURCES.
        host(&install.join("resources"));
        for checks in [true, false] {
            let found = find_resources(&install, None, checks);
            assert_eq!(found, Some((install.join("resources"), true)));
        }
        assert_eq!(find_resources(&install, Some(named.clone()), true), Some((install.join("resources"), true)));
        // Without installed resources a checking build has nothing to run.
        for root in [&built, &moved] {
            assert_eq!(find_resources(root, None, true), None);
            assert_eq!(find_resources(root, Some(named.clone()), true), None);
        }
        host(&built.join("resources"));
        assert_eq!(find_resources(&built, None, true), None);

        // A debug build, or one made without keys, takes development resources, in this order.
        assert_eq!(find_resources(&install, Some(named.clone()), false), Some((named.clone(), false)));
        assert_eq!(find_resources(&built, None, false), Some((built.join("resources"), false)));
        std::fs::remove_dir_all(built.join("resources")).unwrap();
        assert_eq!(find_resources(&built, None, false), Some((repo.join("tmp").join("dev-resources"), false)));
        // Only in the repository it was built in.
        assert_eq!(find_resources(&moved, None, false), None);
    }

    #[test]
    fn explains_module_and_download_failures() {
        let downloads = [
            DownloadError::Insecure("http://example.com/x".into()),
            DownloadError::Redirects,
            DownloadError::Network("WinHTTP 12029".into()),
            DownloadError::Stalled(60),
            DownloadError::Interrupted,
            DownloadError::Status(404),
            DownloadError::InvalidResume,
            DownloadError::InvalidResponse,
            DownloadError::TooLarge,
            DownloadError::Mismatch,
            DownloadError::FileMismatch,
            DownloadError::InvalidCache,
            DownloadError::Link(r"C:\Users\x\AppData".into()),
            DownloadError::Cancelled,
            DownloadError::Io("x".into()),
        ];
        let others = [
            ModuleError::Unavailable,
            ModuleError::Invalid("x".into()),
            ModuleError::NotOffered("strata".into()),
            ModuleError::Busy,
            ModuleError::Closing,
            ModuleError::RemoveBusy,
            ModuleError::Archive("x".into()),
        ];
        // The English messages are the carrier's own, which the log records.
        for error in downloads.into_iter().map(ModuleError::Download).chain(others) {
            assert_eq!(module_message(Locale::En, &error), error.to_string());
            assert!(!module_message(Locale::Zh, &error).starts_with("native."));
        }
        assert_eq!(module_message(Locale::Zh, &ModuleError::Download(DownloadError::Status(404))), "下载失败（HTTP 404），请检查网络后重试");
        assert_eq!(module_message(Locale::Zh, &ModuleError::Busy), "另一个组件正在下载，请稍候");
        let detail = i18n::format(Locale::Zh, "native.linuxDownloadDetail", &[("error", &"下载校验失败，请重试")]);
        assert_eq!(detail, "下载校验失败，请重试\nWSL 和 SSH 远程开发用的 Linux 运行环境底座有变化，需要联网下载一次，已下载的部分会保留。");
        let step = i18n::format(Locale::Zh, "native.linuxDownloading", &[("done", &megabytes(26_000_000)), ("total", &megabytes(52_717_256))]);
        assert_eq!(step, "正在下载 Linux 运行环境 25 / 50 MB");
    }

    #[test]
    fn development_checkouts_stage_the_linux_base() {
        let dir = tempfile::tempdir().unwrap();
        let resources = dir.path().join("resources");
        std::fs::create_dir_all(&resources).unwrap();
        let paths = Paths { user_data: dir.path().join("ud"), resources: resources.clone(), install_root: dir.path().to_path_buf(), installed: false };
        let modules = Modules::new(&resources, &paths.user_data, |_| {});
        // Neither a staged base nor a descriptor: the installer is told about a base that is not there.
        assert_eq!(runtime_files(&paths, &modules).base, None);
        std::fs::write(resources.join("linux-runtime.tar.xz"), b"base").unwrap();
        assert_eq!(runtime_files(&paths, &modules).base, Some(resources.join("linux-runtime.tar.xz")));
    }

    #[test]
    fn explains_resources_that_were_not_accepted() {
        let path = "windows-host/node/node.exe".to_owned();
        assert_eq!(trust_message(Locale::Zh, &TrustError::Missing(path.clone())), "发行资源缺失：windows-host/node/node.exe");
        assert_eq!(trust_message(Locale::En, &TrustError::Mismatch(path.clone())), "A Rainy file failed verification: windows-host/node/node.exe");
        assert_eq!(trust_message(Locale::Zh, &TrustError::Signature), "发行清单签名无效");
        // The English messages are the carrier's own, which the log records.
        for error in [
            TrustError::Invalid("x".into()),
            TrustError::Encoding,
            TrustError::Signature,
            TrustError::DuplicatePaths,
            TrustError::Outside,
            TrustError::Missing(path.clone()),
            TrustError::Damaged(path.clone()),
            TrustError::Replaced(path.clone()),
            TrustError::Mismatch(path.clone()),
            TrustError::Io("x".into()),
        ] {
            assert_eq!(trust_message(Locale::En, &error), error.to_string());
            assert!(!trust_message(Locale::Zh, &error).starts_with("native."));
        }
    }
}
