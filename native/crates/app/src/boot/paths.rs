//! User-data and installed-resource discovery, integrity verification and retired-file maintenance.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context as _, Result, anyhow, bail};
use rainy_carrier::modules::{ModuleError, Modules};
use rainy_carrier::trust::{self, Integrity, Recheck, TrustError, Verified};
use rainy_carrier::{launch, maintenance, targets};
use serde_json::Value;

use super::messages::{grouped, trust_message};
use super::targets::Prune;
use super::{PROGRESS_INTERVAL, Step};
use crate::i18n::{self, Locale};

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

pub(super) fn desktop_settings(user_data: &Path) -> PathBuf {
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
pub(super) fn find_resources(install_root: &Path, development: Option<PathBuf>, installed_only: bool) -> Option<(PathBuf, bool)> {
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

/// The language of messages shown before the Host answers, as the last run recorded it.
pub fn saved_locale(user_data: &Path) -> Locale {
    let preferences = targets::read_preferences(&desktop_settings(user_data)).unwrap_or_default();
    Locale::from_code(preferences.get("locale").and_then(Value::as_str).unwrap_or("zh"))
}

/// Check the installed resources against their signed list before anything from them runs (`ensureReleaseIntegrity`),
/// showing the progress of a full check.
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
        let mut shown = shown.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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

/// The daily recheck of the installed files (`runBackgroundChecks`). A failure that means files were changed or
/// damaged is returned for the window to show; any other is only logged.
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
