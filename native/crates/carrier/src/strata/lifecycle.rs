//! Strata lifecycle manager: installation, updates, model management, serving, and interruptible MCP session handling.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use super::catalog::Catalog;
use super::control;
use super::env::{self, Folders};
use super::health;
use super::mcp;
use super::process::{self, Failure, Terminal};
use super::storage::{folder_bytes, free_gb, models_in, read_json, remembered_data_dir, remove_folder, same_folder, write_record};
use super::types::{Choice, Connection, Engine, FoundModel, Inner, Installed, Job, JobKind, Phase, Server, ServerHealth, Settings, Status, Update};
use crate::locale::localized;

/// How long the management server stays open without calls.
const IDLE_SESSION: Duration = Duration::from_secs(10 * 60);
/// Between checks of Strata's update feed.
const UPDATE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// After Rainy starts, before the first check.
const FIRST_CHECK: Duration = Duration::from_secs(45);
/// A model load takes 1 to 3 minutes; an engine or helper files may have to arrive first.
const START_TIMEOUT: Duration = Duration::from_secs(3 * 60 * 60);
const SETTINGS_FILE: &str = "rainy.json";
const RECORD_FILE: &str = "rainy-strata.json";

/// The one owner of Rainy's Strata.
pub struct Manager {
    folders: Folders,
    /// The Strata folder of the person's own installation (`%LOCALAPPDATA%\Strata`), looked in when Rainy has none.
    own: Option<PathBuf>,
    /// Rainy's user data folder, for the files earlier versions left.
    user_data: PathBuf,
    temp: Mutex<Option<PathBuf>>,
    pub(super) inner: Mutex<Inner>,
    /// Serializes RPC calls against the single stdio management server without blocking `end_session()`/`close()`.
    session_call: Mutex<()>,
    /// Idle management server handle; held only briefly while checking out or returning a session.
    session: Mutex<Option<mcp::Session>>,
    /// Concurrent kill handle for the active management server so `close()`/`end_session()` wakes blocked calls immediately.
    session_abort: Mutex<Option<mcp::AbortHandle>>,
    changed: Arc<dyn Fn() + Send + Sync>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_secs()).unwrap_or(0)
}

/// A data folder for messages; none means the person's own Strata's folder.
fn shown(folder: &Option<PathBuf>) -> String {
    folder.as_ref().map_or_else(|| localized("Strata 的模型文件夹", "Strata's model folder"), |folder| folder.display().to_string())
}

/// The model `strata models import --plan` found in `picked`, and the folder to set it up from; why not, in Strata's
/// plain words.
pub(super) fn local_choice(answer: &Value, picked: &str) -> Result<(Choice, PathBuf)> {
    let plan = answer.get("plan");
    let candidate = plan.and_then(|plan| plan.get("candidate_selection")).filter(|candidate| candidate.is_object());
    let family = candidate.and_then(|candidate| candidate.get("family")).and_then(Value::as_str).filter(|family| !family.is_empty());
    let model = candidate.and_then(|candidate| candidate.get("model")).and_then(Value::as_str).filter(|model| !model.is_empty());
    let (Some(family), Some(model)) = (family, model) else {
        let issues: Vec<String> = plan
            .and_then(|plan| plan.get("plain_issues"))
            .and_then(Value::as_array)
            .map(|issues| issues.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        let detail = if issues.is_empty() { process::reported_error(answer).unwrap_or_default() } else { issues.join(" ") };
        bail!(localized(
            &format!("Strata 没能认出 {picked} 里的模型文件。{detail}"),
            &format!("Strata did not recognize the model files in {picked}. {detail}")
        ));
    };
    let folder = plan.and_then(|plan| plan.get("model_dir")).and_then(Value::as_str).filter(|folder| !folder.is_empty()).unwrap_or(picked);
    Ok((Choice { family: family.to_owned(), model: model.to_owned() }, PathBuf::from(folder)))
}

/// Whether the error is a cancellation.
fn cancelled(error: &anyhow::Error) -> bool {
    matches!(error.downcast_ref::<Failure>(), Some(Failure::Cancelled)) || error.to_string() == "cancelled"
}

impl Manager {
    /// Read the settings and what is installed; nothing starts until asked. `changed` runs whenever the status changed.
    pub fn new(user_data: &Path, changed: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Self::with_own(user_data, control::own_folder(), changed)
    }

    /// [`Self::new`], looking for the person's own Strata in `own`.
    pub fn with_own(user_data: &Path, own: Option<PathBuf>, changed: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        let folders = Folders::new(user_data);
        let settings = read_json::<Settings>(&folders.root.join(SETTINGS_FILE)).unwrap_or_default();
        let manager = Arc::new(Self {
            folders,
            own,
            user_data: user_data.to_path_buf(),
            temp: Mutex::new(None),
            inner: Mutex::new(Inner { settings, ..Inner::default() }),
            session_call: Mutex::new(()),
            session: Mutex::new(None),
            session_abort: Mutex::new(None),
            changed: Arc::new(changed),
        });
        let installed = manager.read_installed();
        manager.inner.lock().unwrap().installed = installed;
        manager
    }

    pub fn folders(&self) -> &Folders {
        &self.folders
    }

    fn notify(&self) {
        (self.changed)();
    }

    /// The record of the last `--version` and engine reads, beside what is on disk now.
    fn read_installed(&self) -> Option<Installed> {
        let (control, external) = self.control()?;
        let external = external.then(|| control.root.to_string_lossy().into_owned());
        let record = read_json::<Installed>(&self.folders.root.join(RECORD_FILE)).filter(|record| record.external == external);
        Some(match record {
            Some(record) if record.control == control.version => record,
            Some(record) => Installed { control: control.version, ..record },
            None => Installed { strata: None, control: control.version, engine: None, external },
        })
    }

    /// The Strata to run: the one Rainy downloaded, else one the person installed themselves (`true`).
    fn control(&self) -> Option<(control::Control, bool)> {
        control::active(&self.folders)
            .map(|control| (control, false))
            .or_else(|| self.own.as_deref().and_then(control::own_install_in).map(|control| (control, true)))
    }

    /// Rainy runs a Strata the person installed themselves.
    fn external(&self) -> bool {
        self.control().is_some_and(|(_, external)| external)
    }

    /// The folder of the person's own Strata whose run configs its app and Terminal share.
    fn own_install_folder(&self) -> Result<PathBuf> {
        let folder = self.own.as_ref().ok_or_else(|| anyhow!("LOCALAPPDATA is not set"))?.join("install");
        fs::create_dir_all(&folder)?;
        Ok(folder)
    }

    /// The temporary folder of Strata's processes, found once.
    fn temporary(&self) -> PathBuf {
        let mut temp = self.temp.lock().unwrap();
        temp.get_or_insert_with(|| {
            let folder = env::temporary_folder(&self.folders.temp);
            log::info!("Strata's temporary folder: {}", folder.display());
            folder
        })
        .clone()
    }

    /// `external`: the person's own Strata, run with their own folders.
    fn terminal_for(&self, control: control::Control, external: bool) -> Terminal {
        let source = self.inner.lock().unwrap().settings.effective_source();
        let environment = if external { env::own_environment(&self.temporary(), source) } else { env::environment(&self.folders, &self.temporary(), source) };
        Terminal { python: control.python(), root: control.root, environment, log: self.folders.logs.join("terminal.log") }
    }

    fn terminal(&self) -> Result<Terminal> {
        let (control, external) = self.control().ok_or_else(|| anyhow!(localized("请先下载 Strata。", "Download Strata first.")))?;
        Ok(self.terminal_for(control, external))
    }

    /// Where model downloads go; none when the person's own Strata keeps its own folder and they chose none here.
    fn data_dir(&self) -> Option<PathBuf> {
        let (chosen, automatic) = {
            let inner = self.inner.lock().unwrap();
            (inner.settings.data_dir.clone().map(PathBuf::from), inner.automatic_data.clone())
        };
        if chosen.is_some() {
            return chosen;
        }
        if self.external() {
            return None;
        }
        Some(automatic.unwrap_or_else(|| {
            let folder = self.automatic_data_dir();
            self.inner.lock().unwrap().automatic_data = Some(folder.clone());
            folder
        }))
    }

    /// Lightweight data-folder resolution for `status()` on the UI thread: never enumerates all fixed drives or queries
    /// `GetDiskFreeSpaceExW` across volumes; full cross-drive discovery happens in background `refresh()` or downloads.
    fn quick_data_dir(&self, inner: &Inner) -> Option<PathBuf> {
        if let Some(chosen) = &inner.settings.data_dir {
            return Some(PathBuf::from(chosen));
        }
        if inner.installed.as_ref().is_some_and(|installed| installed.external.is_some()) {
            return None;
        }
        if let Some(cached) = &inner.automatic_data {
            return Some(cached.clone());
        }
        if inner.installed.is_none() {
            return Some(self.folders.root.join("Strata-data"));
        }
        if let Some(remembered) = remembered_data_dir(&self.folders.roaming) {
            return Some(remembered);
        }
        let local = self.folders.root.join("Strata-data");
        if !models_in(&local).is_empty() {
            return Some(local);
        }
        Some(local)
    }

    /// The data folder when none is chosen. It stays where the models are: the folder Strata's setup remembers from the
    /// last download, else a `Strata-data` folder that holds model files, and only on a computer without any the
    /// default (the roomiest drive), which the first download then keeps ([`Self::keep_data_dir`]).
    fn automatic_data_dir(&self) -> PathBuf {
        if let Some(remembered) = remembered_data_dir(&self.folders.roaming) {
            return remembered;
        }
        self.data_candidates()
            .into_iter()
            .find(|folder| !models_in(folder).is_empty())
            .unwrap_or_else(|| env::default_data_dir(&self.folders))
    }

    /// Every data folder model files of Rainy's Strata may be in: the chosen one, the one Strata remembers, Strata's
    /// default beside its install folder, the one inside the tool and `<drive>:\Strata-data` on every fixed drive.
    fn data_candidates(&self) -> Vec<PathBuf> {
        let chosen = self.inner.lock().unwrap().settings.data_dir.clone().map(PathBuf::from);
        let mut folders: Vec<PathBuf> = chosen.into_iter().chain(remembered_data_dir(&self.folders.roaming)).collect();
        folders.extend(self.folders.install.parent().map(|strata| strata.join("Strata-data")));
        folders.push(self.folders.root.join("Strata-data"));
        folders.extend(env::fixed_drives().into_iter().map(|drive| PathBuf::from(format!("{drive}:\\Strata-data"))));
        let mut unique: Vec<PathBuf> = Vec::new();
        for folder in folders {
            if !unique.iter().any(|known| same_folder(known, &folder)) {
                unique.push(folder);
            }
        }
        unique
    }

    /// Model files in the candidate data folders that Strata has no run config for, one entry per model: the
    /// complete copy, else the largest.
    fn find_models(&self) -> Vec<FoundModel> {
        if self.external() || self.control().is_none() {
            return Vec::new();
        }
        let configured: Vec<String> = fs::read_dir(&self.folders.install)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| {
                        let name = entry.file_name().to_string_lossy().to_lowercase();
                        name.strip_prefix("strata-")
                            .and_then(|rest| rest.strip_suffix(".json"))
                            .filter(|tag| !tag.ends_with(".shared-settings"))
                            .map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut found: Vec<FoundModel> = Vec::new();
        for folder in self.data_candidates() {
            for (id, complete, bytes) in models_in(&folder) {
                if configured.contains(&id) {
                    continue;
                }
                let candidate = FoundModel { id, data_dir: folder.to_string_lossy().into_owned(), complete, bytes };
                match found.iter_mut().find(|known| known.id == candidate.id) {
                    Some(known) if (candidate.complete, candidate.bytes) > (known.complete, known.bytes) => *known = candidate,
                    Some(_) => {}
                    None => found.push(candidate),
                }
            }
        }
        found
    }

    /// Save the data folder downloads use now, so that it does not change under the models.
    pub(super) fn keep_data_dir(&self) {
        if self.external() {
            return;
        }
        let Some(folder) = self.data_dir() else { return };
        let settings = {
            let mut inner = self.inner.lock().unwrap();
            if inner.settings.data_dir.is_some() {
                return;
            }
            inner.settings.data_dir = Some(folder.to_string_lossy().into_owned());
            inner.settings.clone()
        };
        if let Ok(text) = serde_json::to_string_pretty(&settings) {
            let _ = write_record(&self.folders.root.join(SETTINGS_FILE), &(text + "\n"));
        }
    }

    pub fn status(&self) -> Status {
        let inner = self.inner.lock().unwrap();
        let data_dir = self.quick_data_dir(&inner);
        let mut server = inner.server.clone();
        server.elapsed = inner.started.map(|started| started.elapsed().as_secs()).unwrap_or(0);
        Status {
            installed: inner.installed.clone(),
            job: inner.job.clone(),
            server,
            catalog: inner.catalog.clone(),
            catalog_loading: inner.catalog_loading,
            catalog_error: inner.catalog_error.clone(),
            update: inner.update.clone(),
            checking: inner.checking,
            settings: Settings { source: inner.settings.effective_source(), ..inner.settings.clone() },
            data_dir: data_dir.map(|folder| folder.to_string_lossy().into_owned()).unwrap_or_default(),
            data_free_gb: inner.data_free_gb,
            error: inner.error.clone(),
            notice: inner.notice.clone(),
            legacy_bytes: inner.legacy_bytes,
            found: inner.found.clone(),
        }
    }

    /// Look at the port, the files on disk and the free space (call it off the interface thread).
    /// Computes automatic data folder off-lock before atomically swapping it in, and avoids probing `127.0.0.1:8080`
    /// or scanning all drives during cold startup when Strata is not installed.
    pub fn refresh(&self) {
        let (port, owned, phase, chosen) = {
            let inner = self.inner.lock().unwrap();
            (inner.settings.port, inner.owned, inner.server.phase, inner.settings.data_dir.clone().map(PathBuf::from))
        };
        let installed = self.read_installed();
        let should_probe = installed.is_some() || owned || phase != Phase::Stopped;
        let health = if should_probe { health::read(port).ok() } else { None };
        let is_external = installed.as_ref().is_some_and(|item| item.external.is_some());
        let computed_automatic = if chosen.is_none() && !is_external && installed.is_some() {
            Some(self.automatic_data_dir())
        } else {
            None
        };
        let target_dir = if chosen.is_some() {
            chosen
        } else if is_external {
            None
        } else {
            computed_automatic.clone()
        };
        let free = target_dir.as_deref().and_then(free_gb);
        let legacy_modules = self.user_data.join("modules").join("strata");
        let legacy_models = self.user_data.join("strata");
        let legacy = folder_bytes(&legacy_modules).saturating_add(folder_bytes(&legacy_models));
        let found = if installed.is_some() && !is_external { self.find_models() } else { Vec::new() };
        {
            let mut inner = self.inner.lock().unwrap();
            inner.installed = installed;
            if let Some(auto_dir) = computed_automatic {
                inner.automatic_data = Some(auto_dir);
            } else if is_external {
                inner.automatic_data = None;
            }
            inner.found = found;
            inner.data_free_gb = free;
            inner.legacy_bytes = legacy;
            match inner.server.phase {
                Phase::Starting | Phase::Stopping => inner.server.health = health.as_ref().map(ServerHealth::from),
                _ => {
                    inner.server.phase = match (&health, inner.owned) {
                        (Some(_), true) => Phase::Running,
                        (Some(_), false) => Phase::External,
                        (None, _) => {
                            inner.owned = false;
                            Phase::Stopped
                        }
                    };
                    inner.server.health = health.as_ref().map(ServerHealth::from);
                }
            }
        }
        self.notify();
    }

    fn assert_open(&self) -> Result<()> {
        if self.inner.lock().unwrap().closed {
            bail!(localized("Rainy 正在关闭，无法修改 Strata。", "Rainy is closing and cannot change Strata."));
        }
        Ok(())
    }

    /// Change and save the settings; a new download source applies to the next call of the management server.
    pub fn save(&self, settings: Settings) -> Result<Status> {
        if settings.port < 1024 || settings.context.is_some_and(|context| !(4096..=2_097_152).contains(&context)) {
            bail!(localized("请检查端口和上下文长度。", "Check the port and the context length."));
        }
        let (source_changed, folder_changed) = {
            let mut inner = self.inner.lock().unwrap();
            let changed = (inner.settings.effective_source() != settings.effective_source(), inner.settings.data_dir != settings.data_dir);
            inner.settings = settings.clone();
            changed
        };
        write_record(&self.folders.root.join(SETTINGS_FILE), &(serde_json::to_string_pretty(&settings)? + "\n"))?;
        if source_changed {
            self.end_session();
        }
        if folder_changed {
            // Model files in the new folder show at once, as files to use rather than to download.
            let found = self.find_models();
            let free = self.data_dir().and_then(|folder| free_gb(&folder));
            let mut inner = self.inner.lock().unwrap();
            inner.found = found;
            inner.data_free_gb = free;
        }
        self.notify();
        Ok(self.status())
    }

    /// Start `job` on a thread, unless another runs. Rolls back job state if thread spawning fails.
    fn begin(
        self: &Arc<Self>,
        kind: JobKind,
        step: &str,
        cancellable: bool,
        run: impl FnOnce(&Arc<Self>, &Arc<AtomicBool>) -> Result<Option<String>> + Send + 'static,
    ) -> Result<()> {
        self.assert_open()?;
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.job.is_some() {
                bail!(localized("Strata 正在进行另一项操作，请等它完成。", "Strata is busy with another operation. Wait until it finishes."));
            }
            inner.job = Some(Job { kind, step: step.to_owned(), done: 0, total: 0, detail: None, cancellable });
            inner.cancel = Some(cancel.clone());
            inner.error = None;
            inner.notice = None;
        }
        self.notify();
        let this = self.clone();
        let spawned = std::thread::Builder::new().name("strata-job".into()).spawn(move || {
            let result = run(&this, &cancel);
            {
                let mut inner = this.inner.lock().unwrap();
                inner.job = None;
                inner.cancel = None;
                match result {
                    Ok(notice) => inner.notice = notice,
                    Err(error) if cancelled(&error) || cancel.load(Ordering::SeqCst) => {}
                    Err(error) => {
                        log::warn!("Strata: {error:#}");
                        inner.error = Some(format!("{error:#}"));
                    }
                }
            }
            this.refresh();
            this.after_job();
        });
        if let Err(error) = spawned {
            let mut inner = self.inner.lock().unwrap();
            inner.job = None;
            inner.cancel = None;
            drop(inner);
            self.notify();
            return Err(error.into());
        }
        Ok(())
    }

    fn progress(&self, step: &str, done: u64, total: u64, detail: Option<String>) {
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(job) = &mut inner.job {
                job.step = step.to_owned();
                job.done = done;
                job.total = total;
                job.detail = detail;
            }
        }
        self.notify();
    }

    /// Stop the job that can be stopped; a model download stops where it is and resumes next time.
    pub fn cancel(self: &Arc<Self>) {
        let (cancel, download) = {
            let inner = self.inner.lock().unwrap();
            (inner.cancel.clone(), matches!(inner.job.as_ref().map(|job| &job.kind), Some(JobKind::Download { .. })))
        };
        if let Some(cancel) = cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        if download {
            let this = self.clone();
            let _ = std::thread::Builder::new().name("strata-cancel".into()).spawn(move || {
                if let Err(error) = this.call("strata_install", json!({ "cancel": true }), Duration::from_secs(30)) {
                    log::warn!("Strata: the download did not stop cleanly: {error:#}");
                }
            });
        }
    }

    // ---------------------------------------------------------------------------------------------- install, update

    /// Download the newest Strata Terminal and the engine for this computer.
    pub fn install(self: &Arc<Self>) -> Result<()> {
        self.begin(JobKind::Install, "terminal", true, |this, cancel| {
            let offer = control::latest(cancel.clone())?;
            let installed = control::install(
                &this.folders,
                &offer,
                cancel.clone(),
                &|done, total| this.progress(if total == 0 { "verify" } else { "terminal" }, done, total, None),
                &|candidate| this.terminal_for(candidate, false),
            )?;
            control::prune(&this.folders, &installed);
            this.update_components(cancel, false)?;
            Ok(Some(localized("Strata 已安装。", "Strata is installed.")))
        })
    }

    /// `strata update`: the engine for this computer and a newer Terminal, from the signed feed. `only_engine` leaves
    /// the Terminal as it is.
    fn update_components(self: &Arc<Self>, cancel: &Arc<AtomicBool>, only_engine: bool) -> Result<()> {
        let terminal = self.terminal()?;
        self.end_session();
        let mut args = vec!["update", "--output", "jsonl"];
        if only_engine {
            args.push("--no-control");
        }
        let mut answer: Option<Value> = None;
        let mut step = String::from("engine");
        let code = terminal
            .run(&args, Duration::from_secs(4 * 60 * 60), cancel, &mut |line| {
                let Ok(event) = serde_json::from_str::<Value>(line) else { return };
                let phase = event.get("phase").and_then(Value::as_str).unwrap_or_default();
                let component = event.get("component").and_then(Value::as_str).unwrap_or("engine");
                match event.get("event").and_then(Value::as_str) {
                    Some("progress") => {
                        let done = event.get("done").and_then(Value::as_u64).unwrap_or(0);
                        let total = event.get("total").and_then(Value::as_u64).unwrap_or(0);
                        step = if component == "control" { "control".into() } else { "engine".into() };
                        self.progress(&step, done, total, event.get("label").and_then(Value::as_str).map(str::to_owned));
                    }
                    Some("phase") => {
                        step = match (component, phase) {
                            ("control", _) => "control",
                            (_, "download") => "engine",
                            (_, "verify") => "engine-verify",
                            (_, "test") => "engine-test",
                            (_, "switch" | "done") => "engine-switch",
                            _ => "feed",
                        }
                        .into();
                        self.progress(&step, 0, 0, event.get("message").and_then(Value::as_str).map(str::to_owned));
                    }
                    _ if event.get("schema_version").is_some() && event.get("event").is_none() => answer = Some(event),
                    _ => {}
                }
            })
            .map_err(|error| anyhow!(error))?;
        let answer = answer.ok_or_else(|| anyhow!(Failure::Failed(code, terminal.tail())))?;
        if let Some(message) = process::reported_error(&answer) {
            bail!(localized(&format!("Strata 更新失败：{message}"), &format!("The Strata update failed: {message}")));
        }
        if let Some(failed) = answer
            .get("results")
            .and_then(Value::as_array)
            .and_then(|results| results.iter().find(|result| result.get("status").and_then(Value::as_str) == Some("failed")))
        {
            let message = failed.get("error").and_then(Value::as_str).unwrap_or_default();
            bail!(localized(&format!("Strata 更新失败：{message}"), &format!("The Strata update failed: {message}")));
        }
        self.record_versions(cancel);
        Ok(())
    }

    /// Read Strata's version and the engine it would start, for the interface.
    fn record_versions(&self, cancel: &AtomicBool) {
        let Ok(terminal) = self.terminal() else { return };
        let Some((control, external)) = self.control() else { return };
        let mut version = None;
        let _ = terminal.run(&["--version"], Duration::from_secs(300), cancel, &mut |line| {
            // strata 0.2.7 (build 0.1.53; python 3.12.15 at ...)
            if let Some(rest) = line.strip_prefix("strata ") {
                version = rest.split_whitespace().next().map(str::to_owned);
            }
        });
        let engine = terminal.json(&["components", "engine", "--output", "json"], Duration::from_secs(120), cancel).ok().and_then(|answer| {
            let engine = answer.get("engine")?;
            Some(Engine {
                version: engine.get("version")?.as_str()?.to_owned(),
                backend: engine.get("backend").and_then(Value::as_str).unwrap_or_default().to_owned(),
                variant: engine.get("variant").and_then(Value::as_str).unwrap_or_default().to_owned(),
            })
        });
        let installed = Installed { strata: version, control: control.version, engine, external: external.then(|| control.root.to_string_lossy().into_owned()) };
        if let Ok(text) = serde_json::to_string_pretty(&installed) {
            let _ = write_record(&self.folders.root.join(RECORD_FILE), &(text + "\n"));
        }
        self.inner.lock().unwrap().installed = Some(installed);
    }

    /// Read Strata's signed update feed: what is newer than what is installed.
    pub fn check_update(self: &Arc<Self>) -> Result<()> {
        {
            let mut inner = self.inner.lock().unwrap();
            // The person's own Strata updates itself, through its app or `strata update`.
            if inner.checking || inner.installed.as_ref().is_none_or(|installed| installed.external.is_some()) || inner.closed {
                return Ok(());
            }
            inner.checking = true;
        }
        self.notify();
        let this = self.clone();
        let spawned = std::thread::Builder::new().name("strata-check".into()).spawn(move || {
            let result = this.read_feed();
            {
                let mut inner = this.inner.lock().unwrap();
                inner.checking = false;
                match result {
                    Ok(update) => inner.update = Some(update),
                    Err(error) => log::warn!("Strata: the update check failed: {error:#}"),
                }
            }
            this.notify();
            this.after_job();
        });
        if let Err(error) = spawned {
            self.inner.lock().unwrap().checking = false;
            self.notify();
            return Err(error.into());
        }
        Ok(())
    }

    fn read_feed(&self) -> Result<Update> {
        let terminal = self.terminal()?;
        let answer = terminal.json(&["update", "--check", "--output", "json"], Duration::from_secs(300), &AtomicBool::new(false)).map_err(|error| anyhow!(error))?;
        let answer = process::accepted(answer)?;
        let engine = answer.get("engine");
        let engine_status = engine.and_then(|engine| engine.get("status")).and_then(Value::as_str).unwrap_or_default();
        let offered = |value: Option<&Value>| -> Option<String> {
            let offered = value?.get("offered")?;
            offered.get("version").and_then(Value::as_str).or_else(|| offered.as_str()).map(str::to_owned)
        };
        let control = answer.get("control");
        let control_status = control.and_then(|control| control.get("status")).and_then(Value::as_str).unwrap_or_default();
        Ok(Update {
            engine: matches!(engine_status, "install" | "available").then(|| offered(engine)).flatten(),
            control: (control_status == "available").then(|| offered(control)).flatten(),
            no_engine: matches!(engine_status, "wrong-make" | "none-for-this-pc").then(|| {
                engine
                    .and_then(|engine| engine.get("note").or_else(|| engine.get("reasons").and_then(|reasons| reasons.get(0))))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            }),
            checked: now(),
        })
    }

    /// Install what the last check found newer; refused while a model Rainy started serves.
    pub fn update(self: &Arc<Self>) -> Result<()> {
        if self.external() {
            bail!(localized("这是你自己安装的 Strata，请用它自带的更新功能更新。", "This is a Strata you installed yourself; update it with its own updater."));
        }
        if matches!(self.inner.lock().unwrap().server.phase, Phase::Starting | Phase::Running | Phase::Stopping) {
            bail!(localized("请先停止正在运行的模型，再更新 Strata。", "Stop the running model before updating Strata."));
        }
        self.begin(JobKind::Update, "feed", true, |this, cancel| {
            let before = this.inner.lock().unwrap().installed.clone();
            this.update_components(cancel, false)?;
            this.inner.lock().unwrap().update = None;
            let after = this.inner.lock().unwrap().installed.clone();
            Ok((before != after).then(|| {
                let version = after.and_then(|installed| installed.strata).unwrap_or_default();
                localized(&format!("Strata 已更新到 {version}。"), &format!("Strata was updated to {version}."))
            }))
        })
    }

    /// After a job or a check: install an update that is waiting, when nothing runs and the person lets it.
    fn after_job(self: &Arc<Self>) {
        let ready = {
            let mut inner = self.inner.lock().unwrap();
            let idle = inner.job.is_none() && !inner.checking && !inner.closed && matches!(inner.server.phase, Phase::Stopped | Phase::External);
            let wanted = inner.settings.auto_update
                && inner.installed.as_ref().is_some_and(|installed| installed.external.is_none())
                && inner.update.as_ref().is_some_and(Update::available);
            inner.update_waiting = wanted && !idle;
            wanted && idle
        };
        if ready && let Err(error) = self.update() {
            log::warn!("Strata: the automatic update did not start: {error:#}");
        }
    }

    /// For as long as the manager lives: check for updates a while after Rainy starts and every few hours after, and end
    /// the management server once it has been idle for a while.
    pub fn watch(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let _ = std::thread::Builder::new().name("strata-watch".into()).spawn(move || {
            let mut next_check = Instant::now() + FIRST_CHECK;
            loop {
                std::thread::sleep(Duration::from_secs(15));
                let Some(this) = weak.upgrade() else { return };
                if this.inner.lock().unwrap().closed {
                    return;
                }
                this.tidy();
                if Instant::now() >= next_check {
                    next_check = Instant::now() + UPDATE_INTERVAL;
                    let _ = this.check_update();
                }
            }
        });
    }

    // ---------------------------------------------------------------------------------------------- uninstall

    /// Stop everything and remove the tool; `models` removes the downloaded models too.
    pub fn uninstall(self: &Arc<Self>, models: bool) -> Result<()> {
        if self.external() {
            bail!(localized("这是你自己安装的 Strata，Rainy 不会卸载它。", "This is a Strata you installed yourself; Rainy does not remove it."));
        }
        self.begin(JobKind::Uninstall, "stop", false, move |this, _| {
            this.stop_server_now(Duration::from_secs(120));
            if models {
                this.progress("remove", 0, 0, None);
                let catalog = match this.inner.lock().unwrap().catalog.clone() {
                    Some(catalog) => Some(catalog),
                    None => this.call("strata_models", json!({}), Duration::from_secs(300)).ok().map(|answer| Catalog::parse(&answer)),
                };
                let installed: Vec<String> = catalog.map(|catalog| catalog.installed().into_iter().map(|(_, size)| size.id.clone()).collect()).unwrap_or_default();
                for id in installed {
                    if let Err(error) = this.call("strata_remove", json!({ "model": id, "confirm": true }), Duration::from_secs(30 * 60)) {
                        log::warn!("Strata: {id} was not removed: {error:#}");
                    }
                }
            }
            this.end_session();
            this.progress("remove", 0, 0, None);
            let data = this.data_dir().unwrap_or_else(|| env::default_data_dir(&this.folders));
            remove_folder(&this.folders.root)?;
            // A data folder outside the tool goes too when it holds nothing any more.
            if models && !data.starts_with(&this.folders.root) {
                let _ = fs::remove_dir(data.join("models"));
                let _ = fs::remove_dir(&data);
            }
            let mut inner = this.inner.lock().unwrap();
            inner.installed = None;
            inner.catalog = None;
            inner.update = None;
            inner.automatic_data = None;
            inner.settings = Settings::default();
            Ok(Some(localized("Strata 已卸载。", "Strata was removed.")))
        })
    }

    /// Set up the models Rainy 3.x to 4.2 downloaded into its own model cache, where they are.
    pub fn use_legacy(self: &Arc<Self>) -> Result<()> {
        self.use_local(&self.user_data.join("strata"))
    }

    /// Remove the Strata files Rainy 3.x to 4.2 kept: its 0.1.39 module and its model cache.
    pub fn remove_legacy(&self) -> Result<()> {
        remove_folder(&self.user_data.join("modules").join("strata"))?;
        remove_folder(&self.user_data.join("strata"))?;
        self.inner.lock().unwrap().legacy_bytes = 0;
        self.notify();
        Ok(())
    }

    // ---------------------------------------------------------------------------------------------- management server

    /// Call a tool of the management server, starting it when needed.
    /// The `self.session` mutex is held only while checking out / returning the session, never across the IPC wait,
    /// so `close()`, `end_session()`, and `tidy()` remain responsive during 5-30 minute operations.
    fn call(&self, tool: &str, arguments: Value, timeout: Duration) -> Result<Value> {
        let terminal = self.terminal()?;
        let _call_guard = self.session_call.lock().unwrap();
        if self.inner.lock().unwrap().closed {
            bail!(localized("Rainy 正在关闭，无法修改 Strata。", "Rainy is closing and cannot change Strata."));
        }
        let mut session = self.session.lock().unwrap().take();
        if session.as_ref().is_some_and(|existing| !existing.alive() || existing.root != terminal.root) {
            *self.session_abort.lock().unwrap() = None;
            if let Some(old) = session.take() {
                old.stop();
            }
        }
        let mut session = match session {
            Some(existing) => existing,
            None => {
                let install = if self.external() { self.own_install_folder()? } else { self.folders.install.clone() };
                let log = self.folders.logs.join("management.log");
                match mcp::Session::start_with_hook(&terminal, &install, &log, |handle| {
                    *self.session_abort.lock().unwrap() = Some(handle);
                }) {
                    Ok(started) => started,
                    Err(error) => {
                        *self.session_abort.lock().unwrap() = None;
                        return Err(error);
                    }
                }
            }
        };
        let abort_handle = session.abort_handle();
        *self.session_abort.lock().unwrap() = Some(abort_handle.clone());
        let result = session.call(tool, arguments, timeout);
        if self.inner.lock().unwrap().closed || abort_handle.is_aborted() || !session.alive() {
            *self.session_abort.lock().unwrap() = None;
            session.stop();
        } else {
            *self.session.lock().unwrap() = Some(session);
        }
        result
    }

    /// Terminate any idle or in-flight management server immediately without waiting on long-running IPC calls.
    fn end_session(&self) {
        let abort = self.session_abort.lock().unwrap().take();
        if let Some(handle) = abort {
            handle.abort();
        }
        if let Some(idle) = self.session.lock().unwrap().take() {
            idle.stop();
        }
    }

    /// End the management server after a while without calls, unless a download needs it.
    pub fn tidy(&self) {
        if self.inner.lock().unwrap().job.is_some() {
            return;
        }
        let expired = {
            let mut slot = self.session.lock().unwrap();
            if slot.as_ref().is_some_and(|session| session.last_used.elapsed() > IDLE_SESSION) {
                *self.session_abort.lock().unwrap() = None;
                slot.take()
            } else {
                None
            }
        };
        if let Some(old) = expired {
            old.stop();
        }
    }

    /// Read Strata's catalog with what fits this computer (half a minute the first time).
    pub fn load_catalog(self: &Arc<Self>) {
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.catalog_loading || inner.installed.is_none() {
                return;
            }
            inner.catalog_loading = true;
            inner.catalog_error = None;
        }
        self.notify();
        let this = self.clone();
        if let Err(error) = std::thread::Builder::new().name("strata-catalog".into()).spawn(move || {
            let result = this.call("strata_models", json!({}), Duration::from_secs(300));
            {
                let mut inner = this.inner.lock().unwrap();
                inner.catalog_loading = false;
                match result {
                    Ok(answer) => inner.catalog = Some(Catalog::parse(&answer)),
                    Err(error) => inner.catalog_error = Some(format!("{error:#}")),
                }
            }
            this.notify();
        }) {
            let mut inner = self.inner.lock().unwrap();
            inner.catalog_loading = false;
            inner.catalog_error = Some(error.to_string());
            drop(inner);
            self.notify();
        }
    }

    // ---------------------------------------------------------------------------------------------- models

    /// The data folder downloads may go to: Strata's management server accepts a new folder only when its name starts
    /// with `Strata`, so another folder gets a `Strata-data` folder inside.
    /// A folder inside a data folder (its `models` folder, or one model's folder) means that data folder, so that
    /// choosing where the models already are never starts an empty `Strata-data` beside them.
    pub fn data_folder_for(chosen: &Path) -> PathBuf {
        let named = |folder: &Path| folder.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.to_ascii_lowercase().starts_with("strata"));
        if let Some(data) = chosen.ancestors().take(3).find(|folder| named(folder) && folder.join("models").is_dir()) {
            return data.to_path_buf();
        }
        if named(chosen) { chosen.to_path_buf() } else { chosen.join("Strata-data") }
    }

    /// Download a model of the catalog in the background; it resumes where an earlier download stopped, in whichever
    /// data folder that download left its files ([`Status::found`]), and a finished one is only set up.
    pub fn download(self: &Arc<Self>, choice: Choice) -> Result<()> {
        let found = {
            let inner = self.inner.lock().unwrap();
            let id = inner.catalog.as_ref().and_then(|catalog| catalog.size(&choice.family, &choice.model)).map(|(_, size)| size.id.to_lowercase());
            id.and_then(|id| inner.found.iter().find(|found| found.id == id).cloned())
        };
        match found {
            Some(found) => {
                log::info!("Strata: {} {} continues from the files in {}", choice.family, choice.model, found.data_dir);
                self.download_from(choice, None, Some(PathBuf::from(found.data_dir)), found.complete)
            }
            None => self.download_from(choice, None, None, false),
        }
    }

    /// Set up model files the person already has (downloaded by an earlier Rainy, Strata's own app or by hand): Strata
    /// says which model they are (`strata models import --plan`), then checks them where they are and fetches only the
    /// helper files that are missing. Nothing is copied or moved.
    pub fn use_local(self: &Arc<Self>, path: &Path) -> Result<()> {
        self.assert_open()?;
        let terminal = self.terminal()?;
        let picked = path.to_string_lossy().into_owned();
        let answer = terminal.json(&["models", "import", &picked, "--plan", "--output", "json"], Duration::from_secs(600), &AtomicBool::new(false)).map_err(|error| anyhow!(error))?;
        let (choice, folder) = local_choice(&answer, &picked)?;
        log::info!("Strata: {picked} holds {} {}; setting it up from {}", choice.family, choice.model, folder.display());
        self.download_from(choice, Some(folder), None, true)
    }

    /// `local`: model files elsewhere to use where they are; `data`: the data folder to use instead of the usual one,
    /// where an earlier download left this model's files; `present`: the model's files are all here already.
    fn download_from(self: &Arc<Self>, choice: Choice, local: Option<PathBuf>, data: Option<PathBuf>, present: bool) -> Result<()> {
        let kind = JobKind::Download { family: choice.family.clone(), model: choice.model.clone(), local: present };
        self.begin(kind, "model:check", true, move |this, cancel| {
            this.keep_data_dir();
            let data = data.or_else(|| this.data_dir());
            let context = this.inner.lock().unwrap().settings.context;
            let mut arguments = json!({ "family": choice.family, "model": choice.model });
            if let Some(data) = &data {
                arguments["data_dir"] = json!(data.to_string_lossy());
            }
            if let Some(local) = &local {
                arguments["gguf_dir"] = json!(local.to_string_lossy());
            }
            if let Some(context) = context {
                arguments["context"] = json!(context);
            }
            let plan = this.call("strata_install", arguments.clone(), Duration::from_secs(300))?;
            if let Some(short) = plan.pointer("/plan/disk_short").and_then(Value::as_str) {
                let needed = plan.pointer("/plan/disk_needed_gb").and_then(Value::as_f64).unwrap_or(0.0);
                let free = plan.pointer("/plan/disk_free_gb").and_then(Value::as_f64).unwrap_or(0.0);
                bail!(localized(
                    &format!("{} 剩余空间不足：这个模型大约需要 {needed:.0} GB，目前只有 {free:.0} GB。请换一个更大的硬盘位置。（{short}）", shown(&data)),
                    &format!("Not enough free space in {}: the model needs about {needed:.0} GB and {free:.0} GB are free. Choose a folder on a bigger drive. ({short})", shown(&data))
                ));
            }
            arguments["confirm"] = json!(true);
            this.call("strata_install", arguments, Duration::from_secs(300))?;
            let mut misses = 0;
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let answer = match this.call("strata_install", json!({}), Duration::from_secs(120)) {
                    Ok(answer) => answer,
                    Err(error) => {
                        misses += 1;
                        if misses >= 5 {
                            return Err(error);
                        }
                        continue;
                    }
                };
                misses = 0;
                let Some(job) = answer.get("install") else { continue };
                if job.get("running").and_then(Value::as_bool) == Some(true) {
                    let phase = job.get("phase").and_then(Value::as_str).unwrap_or("working");
                    let download = job.get("download");
                    let gb = |key: &str| download.and_then(|download| download.get(key)).and_then(Value::as_f64).map(|gb| (gb * 1e9) as u64).unwrap_or(0);
                    this.progress(&format!("model:{phase}"), gb("done_gb"), gb("total_gb"), job.get("label").and_then(Value::as_str).map(str::to_owned));
                    continue;
                }
                if cancel.load(Ordering::SeqCst) {
                    return Err(anyhow!(Failure::Cancelled));
                }
                return match job.get("result").and_then(Value::as_str) {
                    Some("installed") => {
                        this.inner.lock().unwrap().catalog = None;
                        this.load_catalog();
                        Ok(Some(if present {
                            localized("已有的模型文件已设置好，可以启动了。", "The model files you had are set up and ready to start.")
                        } else {
                            localized("模型已下载完成，可以启动了。", "The model is downloaded and ready to start.")
                        }))
                    }
                    _ => {
                        let error = job.get("error").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| {
                            job.get("last_lines")
                                .and_then(Value::as_array)
                                .map(|lines| lines.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"))
                                .unwrap_or_default()
                        });
                        Err(anyhow!(localized(&format!("模型下载没有完成：{error}"), &format!("The model download did not finish: {error}"))))
                    }
                };
            }
        })
    }

    /// Remove an installed model's own files (its run config, model files and pack; files another model uses stay).
    pub fn remove_model(self: &Arc<Self>, choice: Choice) -> Result<()> {
        {
            let inner = self.inner.lock().unwrap();
            if inner.server.choice.as_ref() == Some(&choice) && matches!(inner.server.phase, Phase::Starting | Phase::Running | Phase::Stopping) {
                bail!(localized("这个模型正在运行，请先停止它。", "This model is running. Stop it first."));
            }
        }
        let id = self
            .inner
            .lock()
            .unwrap()
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.size(&choice.family, &choice.model).map(|(_, size)| size.id.clone()))
            .ok_or_else(|| anyhow!(localized("找不到这个模型。", "This model was not found.")))?;
        self.begin(JobKind::Remove { id: id.clone() }, "remove", false, move |this, _| {
            let answer = this.call("strata_remove", json!({ "model": id, "confirm": true }), Duration::from_secs(30 * 60))?;
            this.inner.lock().unwrap().catalog = None;
            this.load_catalog();
            let failed = answer.get("failed").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
            if failed > 0 {
                bail!(localized(
                    &format!("有 {failed} 个文件没能删除，可能正被其他程序使用。"),
                    &format!("{failed} files could not be removed; another program may be using them.")
                ));
            }
            Ok(Some(localized("模型已删除。", "The model was removed.")))
        })
    }

    // ---------------------------------------------------------------------------------------------- serving

    /// Serve `choice` on the configured port through `strata launch`, which fetches what is missing first.
    pub fn start(self: &Arc<Self>, choice: Choice) -> Result<()> {
        self.assert_open()?;
        let terminal = self.terminal()?;
        let (port, context) = {
            let inner = self.inner.lock().unwrap();
            (inner.settings.port, inner.settings.context)
        };
        if let Ok(found) = health::read(port) {
            let mut inner = self.inner.lock().unwrap();
            if !inner.owned {
                inner.server.phase = Phase::External;
                inner.server.health = Some(ServerHealth::from(&found));
                drop(inner);
                self.notify();
                bail!(localized(
                    &format!("端口 {port} 上已有一个不是 Rainy 启动的 Strata 服务。请先关闭它，或在下面换一个端口。"),
                    &format!("A Strata server that Rainy did not start already uses port {port}. Close it, or choose another port below.")
                ));
            }
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let generation = {
            let mut inner = self.inner.lock().unwrap();
            if matches!(inner.server.phase, Phase::Starting | Phase::Stopping) {
                bail!(localized("Strata 正在启动或停止，请稍候。", "Strata is starting or stopping. Wait a moment."));
            }
            inner.start_generation = inner.start_generation.wrapping_add(1);
            let generation = inner.start_generation;
            inner.server = Server { phase: Phase::Starting, health: None, choice: Some(choice.clone()), elapsed: 0 };
            inner.owned = true;
            inner.started = Some(Instant::now());
            inner.start_cancel = Some(cancel.clone());
            inner.error = None;
            inner.notice = None;
            inner.settings.selected = Some(choice.clone());
            let settings = inner.settings.clone();
            drop(inner);
            let _ = serde_json::to_string_pretty(&settings).map(|text| write_record(&self.folders.root.join(SETTINGS_FILE), &(text + "\n")));
            generation
        };
        self.notify();
        let this = self.clone();
        let spawned = std::thread::Builder::new().name("strata-start".into()).spawn(move || {
            let port_text = port.to_string();
            let context_text = context.map(|context| context.to_string());
            let mut args = vec![
                "launch", "--family", &choice.family, "--model", &choice.model, "--port", &port_text, "--access", "local", "--yes", "--no-tui", "--restart", "--output", "json",
            ];
            if let Some(context) = &context_text {
                args.extend(["--context", context.as_str()]);
            }
            // While it loads, the port says how far it is.
            let watching = Arc::new(AtomicBool::new(true));
            let watcher = {
                let (this, watching) = (this.clone(), watching.clone());
                std::thread::Builder::new().name("strata-start-watch".into()).spawn(move || {
                    while watching.load(Ordering::SeqCst) {
                        let found = health::read(port).ok();
                        {
                            let mut inner = this.inner.lock().unwrap();
                            if inner.start_generation != generation || inner.server.phase != Phase::Starting {
                                break;
                            }
                            inner.server.health = found.as_ref().map(ServerHealth::from);
                        }
                        this.notify();
                        std::thread::sleep(Duration::from_secs(2));
                    }
                })
            };
            let result = terminal.json(&args, START_TIMEOUT, &cancel);
            watching.store(false, Ordering::SeqCst);
            if let Ok(handle) = watcher {
                let _ = handle.join();
            }
            let outcome: Result<()> = match result {
                Ok(answer) => match process::reported_error(&answer) {
                    Some(message) => Err(anyhow!(message)),
                    None if answer.get("serving").and_then(Value::as_bool) == Some(false) => {
                        Err(anyhow!(answer.get("message").and_then(Value::as_str).unwrap_or_default().to_owned()))
                    }
                    None => Ok(()),
                },
                Err(error) => Err(anyhow!(error)),
            };
            let stopped_by_person = cancel.load(Ordering::SeqCst);
            let still_current = {
                let mut inner = this.inner.lock().unwrap();
                if inner.start_generation != generation {
                    false
                } else {
                    inner.start_cancel = None;
                    match &outcome {
                        Ok(()) => {
                            inner.server.phase = Phase::Running;
                            inner.started = None;
                        }
                        Err(error) => {
                            inner.server.phase = Phase::Stopped;
                            inner.started = None;
                            if !stopped_by_person {
                                inner.error = Some(localized(&format!("Strata 没能启动：{error:#}"), &format!("Strata did not start: {error:#}")));
                            }
                        }
                    }
                    true
                }
            };
            if still_current && outcome.is_err() {
                // What it started half-way does not stay; guarded by `generation` so a newer start is never killed.
                let _ = terminal.run(&["launch", "--stop"], Duration::from_secs(120), &AtomicBool::new(false), &mut |_| {});
                let mut inner = this.inner.lock().unwrap();
                if inner.start_generation == generation {
                    inner.owned = false;
                }
            }
            if still_current {
                this.refresh();
                this.after_job();
            }
        });
        if let Err(error) = spawned {
            let mut inner = self.inner.lock().unwrap();
            if inner.start_generation == generation {
                inner.server = Server::default();
                inner.owned = false;
                inner.started = None;
                inner.start_cancel = None;
            }
            drop(inner);
            self.notify();
            return Err(error.into());
        }
        Ok(())
    }

    /// Stop the server Rainy started, waiting up to `limit` for it (off the interface thread); what is left then ends
    /// with Rainy's process.
    fn stop_server_now(&self, limit: Duration) {
        let (owned, cancel) = {
            let mut inner = self.inner.lock().unwrap();
            if !inner.owned && inner.server.phase != Phase::Starting {
                return;
            }
            inner.start_generation = inner.start_generation.wrapping_add(1);
            inner.server.phase = Phase::Stopping;
            (inner.owned, inner.start_cancel.clone())
        };
        self.notify();
        if let Some(cancel) = cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        if owned && let Ok(terminal) = self.terminal() {
            let _ = terminal.run(&["launch", "--stop"], limit, &AtomicBool::new(false), &mut |_| {});
        }
        let mut inner = self.inner.lock().unwrap();
        inner.owned = false;
        inner.server = Server::default();
        inner.started = None;
        inner.start_cancel = None;
    }

    /// Stop the server Rainy started; a server it did not start is left alone.
    pub fn stop(self: &Arc<Self>) -> Result<()> {
        let this = self.clone();
        std::thread::Builder::new().name("strata-stop".into()).spawn(move || {
            this.stop_server_now(Duration::from_secs(120));
            this.refresh();
            this.after_job();
        })?;
        Ok(())
    }

    /// The loaded server, for the selected Host to verify and save as a model.
    pub fn connection(&self) -> Result<Connection> {
        self.assert_open()?;
        let port = self.inner.lock().unwrap().settings.port;
        let health = health::read(port)?;
        if !health.loaded {
            bail!(localized("Strata 模型尚未加载完成。", "The Strata model has not finished loading."));
        }
        if health.authentication_required {
            bail!(localized(
                "此服务需要 API 密钥，请在现有模型设置中配置连接。",
                "This server needs an API key. Set up the connection in your existing model settings."
            ));
        }
        Ok(Connection { base_url: health.base_url, model: health.model, context_window: health.context_window })
    }

    /// Refuse further changes and stop what Rainy started, for quitting.
    pub fn close(&self) {
        let cancel = {
            let mut inner = self.inner.lock().unwrap();
            inner.closed = true;
            inner.cancel.clone()
        };
        if let Some(cancel) = cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        // Interrupt any long-running management call immediately before waiting on server shutdown.
        self.end_session();
        // Quitting does not wait long: what is still running ends with Rainy's process.
        self.stop_server_now(Duration::from_secs(10));
    }
}
