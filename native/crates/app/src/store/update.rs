//! Updates (3.x `updates.ts` and its hooks in `main.ts`): one silent check after the workbench is ready, manual checks
//! from Settings → General, the download, the offer to restart and install, and the saved shutdown that hands over to
//! the installer. Every step goes to rainy.log.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::*;
use rainy_carrier::HostProcess;
use rainy_carrier::download::DownloadError;
use rainy_carrier::update::{self, Prepared, Source, UpdateError, Version};

use super::{Phase, Store, StoreEvent};
use crate::boot::{self, Paths};
use crate::i18n::{self, Locale};
use crate::store::ide::Ide;

/// How long after the workbench is ready the automatic check runs.
const CHECK_DELAY: Duration = Duration::from_secs(10);
/// How long saving the drafts may take before the user is asked (3.x `rainy:flush-workbench`).
const FLUSH_TIMEOUT: Duration = Duration::from_secs(65);
/// How long a component operation may take to stop at its next chunk.
const MODULES_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the updater stands (3.x `RainyUpdateState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatePhase {
    Idle,
    Checking,
    Current,
    Unavailable,
    Downloading { version: String, percent: u8 },
    Ready { version: String },
    Installing { version: String },
    Failed,
}

/// What the window says about an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateNotice {
    /// A manual check found nothing newer.
    Current,
    /// A manual check in a build that does not update itself.
    Unavailable,
    /// A manual check, a download or an installation failed: why.
    Failed(String),
    /// The installer of this version is ready: restart and install, or later.
    Ready(String),
    /// Work is running, so the update waits.
    Busy,
    /// The Host was stopped for the update, but the installer did not start: why. Rainy starts again.
    Restart(String),
}

pub struct Updates {
    /// An installed release build (3.x `app.isPackaged`), or a build given a test feed.
    pub available: bool,
    pub phase: UpdatePhase,
    source: Source,
    cache: Option<PathBuf>,
    current: Version,
    /// The downloaded installer, verified, waiting for the user.
    prepared: Option<Prepared>,
    running: bool,
    /// The running check reports its outcome.
    manual: bool,
    scheduled: bool,
    cancel: Arc<AtomicBool>,
}

impl Updates {
    pub fn new(paths: &Paths) -> Self {
        let source = Source::from_env();
        let cache = update::cache_dir(&source, &paths.user_data);
        let updates = Self::with(paths.packaged(), source, cache);
        if updates.source.overridden() {
            let cache = updates.cache.as_deref().map_or_else(|| "none".into(), |cache| cache.display().to_string());
            log::info!("Updates come from {} (RAINY_UPDATE_FEED), installers go to {cache}", updates.source.feed);
        }
        updates
    }

    fn with(packaged: bool, source: Source, cache: Option<PathBuf>) -> Self {
        Self {
            available: packaged || source.overridden(),
            phase: UpdatePhase::Idle,
            source,
            cache,
            current: update::parse_version(env!("CARGO_PKG_VERSION")).expect("Rainy's version is a semantic version"),
            prepared: None,
            running: false,
            manual: false,
            scheduled: false,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A check, a download or an installation is under way.
    pub fn busy(&self) -> bool {
        self.running || matches!(self.phase, UpdatePhase::Installing { .. })
    }

    /// Stop a check or a download; what arrived is kept for the next start.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// What a check found.
enum Outcome {
    Current,
    Prepared(Prepared),
}

/// Whether installing may start.
enum Gate {
    Busy,
    Free,
    /// Only if the Host says it is idle.
    Host(Arc<HostProcess>),
}

/// How an installation that did not hand over ended.
enum Installed {
    /// Rainy keeps running with the update still prepared; what to tell the user, if anything.
    Kept(Option<UpdateNotice>),
    /// The installer no longer matches its release and is not offered again.
    Discarded(UpdateNotice),
    /// The Host was stopped but the installer did not start: why.
    Stopped(String),
}

impl Store {
    /// Once, when the workbench is first ready: the automatic check a few seconds later.
    pub(super) fn schedule_update_check(&mut self, cx: &mut Context<Self>) {
        if !self.updates.available || self.updates.scheduled {
            return;
        }
        self.updates.scheduled = true;
        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            cx.background_executor().timer(CHECK_DELAY).await;
            let _ = this.update(cx, |store, cx| store.check_updates(false, cx));
        });
        self._tasks.push(task);
    }

    /// Check for a newer release and download it (3.x `RainyUpdates.check`). Only a manual check reports that nothing
    /// was found or that it failed; a manual check during another joins it, and one after a download offers the
    /// prepared update again without the network.
    pub fn check_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.stopping {
            return;
        }
        let updates = &mut self.updates;
        if updates.running {
            updates.manual |= manual;
            return;
        }
        if matches!(updates.phase, UpdatePhase::Installing { .. }) {
            return;
        }
        if let Some(prepared) = &updates.prepared {
            if manual {
                cx.emit(StoreEvent::Update(UpdateNotice::Ready(prepared.version.to_string())));
            }
            return;
        }
        if !updates.available {
            updates.phase = UpdatePhase::Unavailable;
            cx.notify();
            if manual {
                cx.emit(StoreEvent::Update(UpdateNotice::Unavailable));
            }
            return;
        }
        log::info!("Update check ({}), running {}", if manual { "manual" } else { "automatic" }, updates.current);
        updates.running = true;
        updates.manual = manual;
        updates.phase = UpdatePhase::Checking;
        cx.notify();
        let (source, cache, current, cancel) = (updates.source.clone(), updates.cache.clone(), updates.current.clone(), updates.cancel.clone());
        let (progress_tx, progress_rx) = async_channel::unbounded::<(String, u8)>();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            while let Ok((version, percent)) = progress_rx.recv().await {
                let _ = this.update(cx, |store, cx| {
                    if store.updates.running {
                        store.updates.phase = UpdatePhase::Downloading { version, percent };
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        let work = cx.background_spawn(async move { prepare(&source, cache, &current, &cancel, &progress_tx) });
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let result = work.await;
            let _ = this.update(cx, |store, cx| store.update_checked(result, cx));
        })
        .detach();
    }

    fn update_checked(&mut self, result: Result<Outcome, (&'static str, UpdateError)>, cx: &mut Context<Self>) {
        let manual = std::mem::take(&mut self.updates.manual);
        self.updates.running = false;
        if self.stopping {
            return;
        }
        let notice = match result {
            Ok(Outcome::Current) => {
                self.updates.phase = UpdatePhase::Current;
                manual.then_some(UpdateNotice::Current)
            }
            Ok(Outcome::Prepared(prepared)) => {
                let version = prepared.version.to_string();
                self.updates.phase = UpdatePhase::Ready { version: version.clone() };
                self.updates.prepared = Some(prepared);
                // Offered after automatic checks too, as 3.x did.
                Some(UpdateNotice::Ready(version))
            }
            Err((operation, error)) => {
                log::warn!("Update {operation} failed: {error}");
                self.updates.phase = UpdatePhase::Failed;
                manual.then(|| UpdateNotice::Failed(update_message(self.locale(), &error)))
            }
        };
        cx.notify();
        if let Some(notice) = notice {
            cx.emit(StoreEvent::Update(notice));
        }
    }

    /// Startup, a component download or work in the Host keeps the update waiting (3.x `prepare`), and so does a Host
    /// that has ended: nothing stops what it started, which may run from the installation folder. A Host that never
    /// started does not, so an installation whose Host cannot start can still update itself.
    fn install_gate(&self) -> Gate {
        if matches!(self.phase, Phase::Starting(..)) || self.modules.busy || self.modules.running() {
            return Gate::Busy;
        }
        match &self.host {
            Some(host) if host.has_exited() => Gate::Busy,
            Some(host) => Gate::Host(host.clone()),
            None => Gate::Free,
        }
    }

    /// Restart into the prepared update (3.x `restartWithInstall`), refused while work is running. The Host stops
    /// taking work, the drafts are saved, the installer is checked once more, the window placement is saved, the Host
    /// and the module manager stop, and the installer starts as Rainy exits. When a step before the Host stops fails or
    /// the user cancels, Rainy keeps running with the Host resumed.
    pub fn install_update(&mut self, window: AnyWindowHandle, cx: &mut Context<Self>) {
        let Some(prepared) = self.updates.prepared.clone() else { return };
        if self.stopping || self.updates.busy() {
            return;
        }
        let version = prepared.version.to_string();
        log::info!("Update {version}: restart and install");
        self.updates.phase = UpdatePhase::Installing { version: version.clone() };
        cx.notify();
        let (gate, cancel, locale) = (self.install_gate(), self.updates.cancel.clone(), self.locale());
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let installed = install(&this, window, prepared, gate, &cancel, locale, cx).await;
            let _ = this.update(cx, |store, cx| {
                let notice = match installed {
                    Installed::Kept(notice) => {
                        store.updates.phase = UpdatePhase::Ready { version };
                        notice
                    }
                    Installed::Discarded(notice) => {
                        store.updates.prepared = None;
                        store.updates.phase = UpdatePhase::Failed;
                        Some(notice)
                    }
                    Installed::Stopped(detail) => Some(UpdateNotice::Restart(detail)),
                };
                cx.notify();
                if let Some(notice) = notice {
                    cx.emit(StoreEvent::Update(notice));
                }
            });
        })
        .detach();
    }
}

/// Check, then download what is newer; without the network, an installer an earlier start downloaded is offered.
fn prepare(
    source: &Source,
    cache: Option<PathBuf>,
    current: &Version,
    cancel: &Arc<AtomicBool>,
    progress: &async_channel::Sender<(String, u8)>,
) -> Result<Outcome, (&'static str, UpdateError)> {
    let options = source.options(cancel.clone());
    let cache = cache.ok_or_else(|| ("check", UpdateError::Io("LOCALAPPDATA is not set".into())))?;
    let installer = match update::check(source, current, crate::release::keyring(), &options) {
        Ok(None) => return Ok(Outcome::Current),
        Ok(Some(installer)) => installer,
        Err(UpdateError::Download(error)) if error != DownloadError::Cancelled => match update::pending(&cache, current, cancel) {
            Some(prepared) => {
                log::info!("Update check failed ({error}); offering {}, downloaded earlier", prepared.version);
                return Ok(Outcome::Prepared(prepared));
            }
            None => return Err(("check", UpdateError::Download(error))),
        },
        Err(error) => return Err(("check", error)),
    };
    let version = installer.version.to_string();
    let shown = Cell::new(None);
    let report = |done: u64, total: u64| {
        let percent = (done.saturating_mul(100) / total.max(1)).min(100) as u8;
        if shown.replace(Some(percent)) != Some(percent) {
            let _ = progress.try_send((version.clone(), percent));
        }
    };
    report(0, installer.expected.bytes);
    update::download(&installer, &cache, &options, &report).map(Outcome::Prepared).map_err(|error| ("download", error))
}

/// The saved shutdown and the handover; returns only when Rainy does not exit.
async fn install(
    this: &WeakEntity<Store>,
    window: AnyWindowHandle,
    prepared: Prepared,
    gate: Gate,
    cancel: &Arc<AtomicBool>,
    locale: Locale,
    cx: &mut AsyncApp,
) -> Installed {
    let version = prepared.version.clone();
    let host = match gate {
        Gate::Busy => {
            log::info!("Update {version}: not installed while Rainy starts, a component downloads or the Host has ended");
            return Installed::Kept(Some(UpdateNotice::Busy));
        }
        Gate::Free => None,
        Gate::Host(host) => {
            let asked = host.clone();
            match cx.background_spawn(async move { asked.inspect_activity("freeze") }).await {
                Ok(false) => Some(host),
                Ok(true) => {
                    log::info!("Update {version}: not installed while work runs in the Host");
                    resume(Some(host), cx);
                    return Installed::Kept(Some(UpdateNotice::Busy));
                }
                Err(error) => {
                    log::warn!("Update {version}: the Host did not say whether work is running: {error:#}");
                    resume(Some(host), cx);
                    return Installed::Kept(Some(UpdateNotice::Failed(format!("{error:#}"))));
                }
            }
        }
    };
    log::info!("Update {version}: the Host takes no new work; saving the drafts");
    loop {
        let flush = cx.update(|cx| Ide::global(cx).update(cx, |ide, cx| ide.flush_now(cx)));
        let detail = match flush.with_timeout(FLUSH_TIMEOUT, cx.background_executor()).await {
            Ok(Ok(())) => break,
            Ok(Err(error)) => Some(error),
            Err(_) => None,
        };
        log::warn!("Update {version}: the drafts were not saved: {}", detail.as_deref().unwrap_or("no answer in time"));
        let asked = window.update(cx, |_, window, cx| crate::ui::update::retry_save(detail, window, cx));
        let retry = match asked {
            Ok(answer) => answer.recv().await.unwrap_or(false),
            Err(_) => false,
        };
        if !retry {
            log::info!("Update {version}: not installed, the drafts were not saved");
            resume(host, cx);
            return Installed::Kept(None);
        }
    }
    log::info!("Update {version}: the drafts and the editor state are saved; checking the installer");
    let (checked, flag) = (prepared.clone(), cancel.clone());
    match cx.background_spawn(async move { checked.verify(&flag) }).await {
        Ok(true) => {}
        verified => {
            let detail = match verified {
                Err(error) => update_message(locale, &error),
                Ok(_) => boot::download_message(locale, &DownloadError::FileMismatch),
            };
            log::warn!("Update {version}: {} no longer matches its release: {detail}", prepared.path.display());
            resume(host, cx);
            return Installed::Discarded(UpdateNotice::Failed(detail));
        }
    }
    // Nothing of Rainy may keep a file in the installation folder open once the installer runs.
    log::info!("Update {version}: saving the window placement and stopping the Host and the module manager");
    let modules = cx.update(|cx| {
        crate::placement::save(cx);
        crate::web::preview::close();
        this.update(cx, |store, _| {
            store.shutdown();
            store.modules.manager.clone()
        })
        .ok()
    });
    let Some(modules) = modules else { return Installed::Kept(None) };
    // As 3.x, nothing is installed when the cleanup failed; Rainy starts again and offers the update again.
    if !cx.background_spawn(async move { modules.wait_idle(MODULES_TIMEOUT) }).await {
        log::warn!("Update {version}: a component operation had not stopped; the installer is not started");
        return Installed::Stopped(i18n::text(locale, "native.updateCleanupFailed").to_owned());
    }
    log::info!("Update {version}: starting {}", update::command_line(&prepared.path).to_string_lossy());
    match update::launch(&prepared.path) {
        Ok(process) => {
            log::info!("Update {version}: the installer runs (process {process}); Rainy exits");
            std::process::exit(0);
        }
        Err(error) => {
            log::error!("Update {version}: the installer did not start: {error}");
            Installed::Stopped(i18n::format(locale, "native.updateLaunchFailed", &[("detail", &error)]))
        }
    }
}

/// Let the Host take work again after an installation that did not go ahead.
fn resume(host: Option<Arc<HostProcess>>, cx: &mut AsyncApp) {
    let Some(host) = host else { return };
    cx.background_spawn(async move {
        if let Err(error) = host.inspect_activity("resume") {
            log::warn!("The Host did not resume after the update was not installed: {error:#}");
        }
    })
    .detach();
}

/// What the window says about a failed check or download.
pub fn update_message(locale: Locale, error: &UpdateError) -> String {
    match error {
        UpdateError::Download(error) => boot::download_message(locale, error),
        UpdateError::Invalid(detail) => i18n::format(locale, "native.updateInvalid", &[("detail", detail)]),
        UpdateError::WebInstaller => i18n::text(locale, "native.updateWebInstaller").to_owned(),
        UpdateError::Signature => i18n::text(locale, "native.updateSignature").to_owned(),
        UpdateError::Io(detail) => i18n::format(locale, "native.downloadIo", &[("detail", detail)]),
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn only_installed_releases_and_test_feeds_update() {
        let cache = Some(PathBuf::from("C:/cache"));
        assert!(!Updates::with(false, Source::github(), cache.clone()).available);
        assert!(Updates::with(true, Source::github(), cache.clone()).available);
        assert!(Updates::with(false, Source::at("http://127.0.0.1:9/latest.yml"), cache).available);
        let updates = Updates::with(true, Source::github(), None);
        assert_eq!(updates.current.to_string(), env!("CARGO_PKG_VERSION"));
        assert!(!updates.busy());
    }

    #[test]
    fn the_dialog_texts_are_the_3x_ones() {
        let zh = |key: &str| i18n::text(Locale::Zh, key);
        let en = |key: &str| i18n::text(Locale::En, key);
        assert_eq!(zh("native.updateTitle"), "Rainy 更新");
        assert_eq!(zh("native.updateCurrent"), "已是最新版本：");
        assert_eq!(zh("native.updateUnavailable"), "自动更新仅用于已安装的发行版。");
        assert_eq!(zh("native.updateFailed"), "更新未完成，应用和已有数据保持可用。请稍后通过“检查更新”重试。");
        assert_eq!(zh("native.updateReady"), "更新已下载：");
        assert_eq!(zh("native.updateRestartDetail"), "重启前会保存草稿并关闭执行环境。选择“稍后”可继续工作，之后通过“检查更新”安装。");
        assert_eq!((zh("native.updateLater"), zh("native.updateInstall")), ("稍后", "重启并安装"));
        assert_eq!(zh("native.updateBusy"), "请先结束运行中的任务、程序、调试、终端及环境准备，再安装更新。");
        assert_eq!(zh("native.checkUpdates"), "检查更新…");
        assert_eq!(en("native.updateTitle"), "Rainy updates");
        assert_eq!(en("native.updateCurrent"), "You are up to date:");
        assert_eq!(en("native.updateReady"), "Update downloaded:");
        assert_eq!((en("native.updateLater"), en("native.updateInstall")), ("Later", "Restart and install"));
        assert_eq!(en("native.updateBusy"), "Finish running tasks, programs, debugging, terminals, and environment setup before installing the update.");
        assert_eq!(en("native.checkUpdates"), "Check for updates…");
    }

    #[test]
    fn explains_failures_in_the_interface_language() {
        assert_eq!(update_message(Locale::Zh, &UpdateError::Signature), "更新信息的签名无效");
        assert_eq!(update_message(Locale::Zh, &UpdateError::Invalid("version is missing".into())), "更新信息无效：version is missing");
        assert_eq!(update_message(Locale::Zh, &UpdateError::Download(DownloadError::Status(404))), "下载失败（HTTP 404），请检查网络后重试");
        assert_eq!(update_message(Locale::En, &UpdateError::WebInstaller), "The update information describes a web installer, which Rainy does not use.");
        assert_eq!(i18n::text(Locale::Zh, "native.updateCleanupFailed"), "环境组件的操作未能及时停止，安装程序没有启动。");
        assert_eq!(i18n::text(Locale::En, "native.updateCleanupFailed"), "An environment component operation did not stop in time, so the installer was not started.");
    }
}
