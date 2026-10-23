//! Strata, the local inference engine for Qwen3.8 Flash Next models (3.x `main/strata.ts`, `strata-process.ts`): the
//! optional module holds the engine and its Python server; the weights stay where the user keeps them. Only an
//! explicit start prepares files or allocates a model, a server Rainy did not start is never stopped, and connecting
//! leaves the identity check to the selected Host (`models.connectStrata`).

pub mod health;
pub mod model;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

pub use self::health::Health;
pub use self::model::{Model, ProfileChoice};
use self::model::Resolved;
use crate::locale::localized;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(600);
const POLL: Duration = Duration::from_secs(1);
/// Memory left to the system when the resident budget is automatic.
const RAM_HEADROOM_GIB: f64 = 8.0;
const RUNTIME_FILES: [&str; 7] = [
    "portablepython/python.exe",
    "server/serve/server.py",
    "server/tools/iq_pack.py",
    "server/tools/mtp_rt.py",
    "server/tools/strata_tokenizer.py",
    "server/data/expert-profile.bin",
    "engine/strata.exe",
];
const KV_CACHES: [&str; 3] = ["int8", "q4_0", "k8v4"];

/// Engine allocation and model locations; request reasoning and output limits stay model settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub model_path: String,
    pub mtp_path: String,
    pub context_window: u32,
    pub port: u16,
    pub kv_cache: String,
    #[serde(rename = "vramReserveMiB")]
    pub vram_reserve_mib: u64,
    #[serde(rename = "residentBudgetGiB")]
    pub resident_budget_gib: Option<f64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { model_path: String::new(), mtp_path: String::new(), context_window: 32768, port: 8081, kv_cache: "int8".into(), vram_reserve_mib: 700, resident_budget_gib: None }
    }
}

impl Settings {
    /// The ranges 3.x accepted.
    pub fn check(&self) -> Result<()> {
        let valid = self.model_path.len() <= 32768
            && self.mtp_path.len() <= 32768
            && (8192..=262144).contains(&self.context_window)
            && self.port >= 1024
            && KV_CACHES.contains(&self.kv_cache.as_str())
            && self.resident_budget_gib.is_none_or(|budget| budget.is_finite() && budget > 0.0);
        if valid { Ok(()) } else { Err(anyhow!(localized("请检查上下文长度、端口和内存数值。", "Check the context length, port and memory values."))) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Unconfigured,
    Stopped,
    Preparing,
    Starting,
    Running,
    Stopping,
    /// A Strata server Rainy did not start answers on the port.
    External,
    Error,
}

/// The engine files of the optional module.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RuntimeInfo {
    pub available: bool,
    pub version: Option<String>,
    pub root: String,
    pub missing: Vec<String>,
}

/// The server on the configured port.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub base_url: String,
    pub model: String,
    pub context_window: u64,
    pub loaded: bool,
    /// Rainy started it.
    pub owned: bool,
    pub authentication_required: bool,
}

/// What the settings card shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub settings: Settings,
    pub runtime: RuntimeInfo,
    pub profiles: Vec<ProfileChoice>,
    pub model: Option<Model>,
    pub server: Option<Server>,
    pub progress: Option<String>,
    pub error: Option<String>,
}

/// A loaded, credential-free server for the selected Host to verify and save.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    #[serde(rename = "baseURL")]
    pub base_url: String,
    pub model: String,
    pub context_window: u64,
}

/// A process Rainy started: a preparation tool or the server.
struct Owned {
    child: Mutex<Child>,
}

impl Owned {
    fn finished(&self) -> bool {
        self.child.lock().unwrap().try_wait().ok().flatten().is_some()
    }

    /// Stop the process and its tree, and wait until it has exited.
    fn stop(&self) {
        let mut child = self.child.lock().unwrap();
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        #[cfg(windows)]
        {
            let mut taskkill = Command::new("taskkill.exe");
            taskkill.args(["/PID", &child.id().to_string(), "/T", "/F"]).stdout(Stdio::null()).stderr(Stdio::null());
            hide(&mut taskkill);
            let _ = taskkill.status();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(windows)]
fn hide(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    command.creation_flags(0x0800_0000);
}

#[derive(Default)]
struct Inner {
    initialized: bool,
    settings: Settings,
    model: Option<Resolved>,
    phase: Option<Phase>,
    progress: Option<String>,
    error: Option<String>,
    health: Option<Health>,
    owned: Option<Arc<Owned>>,
    cancel: Option<Arc<AtomicBool>>,
    worker: Option<JoinHandle<()>>,
    closed: bool,
}

/// The one owner of Rainy's Strata engine.
pub struct Manager {
    runtime_root: PathBuf,
    state_root: PathBuf,
    profile_settings: Option<PathBuf>,
    inner: Arc<Mutex<Inner>>,
    /// Keeps save, start and stop in order.
    transaction: Mutex<()>,
    changed: Arc<dyn Fn() + Send + Sync>,
}

/// The environment of Strata's processes: nothing that looks like a credential, no foreign Python setup, offline Hugging Face.
fn child_environment() -> Vec<(String, String)> {
    let blocked = |name: &str| {
        let upper = name.to_ascii_uppercase();
        ["KEY", "SECRET", "TOKEN", "PASSWORD"].iter().any(|word| upper.contains(word))
            || ["PYTHONPATH", "PYTHONHOME", "NODE_OPTIONS", "NODE_PATH"].contains(&upper.as_str())
            || upper.starts_with("STRATA_")
    };
    let mut environment: Vec<(String, String)> = std::env::vars().filter(|(name, _)| !blocked(name)).collect();
    for (name, value) in [("PYTHONDONTWRITEBYTECODE", "1"), ("PYTHONNOUSERSITE", "1"), ("HF_HUB_OFFLINE", "1"), ("TRANSFORMERS_OFFLINE", "1"), ("HF_DATASETS_OFFLINE", "1")] {
        environment.push((name.into(), value.into()));
    }
    environment
}

/// Free physical memory in GiB.
fn available_memory_gib() -> f64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 {
            return status.ullAvailPhys as f64 / f64::from(1u32 << 30);
        }
        0.0
    }
    #[cfg(not(windows))]
    {
        let info = fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let kib = info.lines().find_map(|line| line.strip_prefix("MemAvailable:")).and_then(|rest| rest.split_whitespace().next()?.parse::<f64>().ok()).unwrap_or(0.0);
        kib / f64::from(1u32 << 20)
    }
}

/// Write a private record atomically.
fn write_record(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, text)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

impl Manager {
    /// `runtime_root` is the installed `strata` module; mutable state goes to `<user_data>\strata`. `changed` runs when
    /// a background start or a server exit changes the status. Creating it reads nothing and starts nothing.
    pub fn new(runtime_root: PathBuf, user_data: &Path, changed: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        let profile_settings = std::env::var_os("APPDATA").map(|appdata| PathBuf::from(appdata).join("Strata").join("settings.json"));
        Arc::new(Self { runtime_root, state_root: user_data.join("strata"), profile_settings, inner: Arc::default(), transaction: Mutex::new(()), changed: Arc::new(changed) })
    }

    fn settings_path(&self) -> PathBuf {
        self.state_root.join("settings.json")
    }

    fn initialize(&self) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.initialized {
            return Ok(());
        }
        inner.initialized = true;
        match fs::metadata(self.settings_path()) {
            Ok(info) if info.len() > 65536 => bail!(localized("Strata 设置文件超过 64 KiB。", "The Strata settings file is larger than 64 KiB.")),
            Ok(_) => {
                let settings: Settings = serde_json::from_str(&fs::read_to_string(self.settings_path())?)?;
                settings.check()?;
                inner.settings = settings;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if !inner.settings.model_path.is_empty() {
            match model::resolve(&inner.settings, &self.state_root, false) {
                Ok(resolved) => {
                    inner.error = resolved.as_ref().and_then(|resolved| resolved.missing.first().cloned());
                    inner.model = resolved;
                    inner.phase = Some(Phase::Stopped);
                }
                Err(error) => {
                    inner.phase = Some(Phase::Error);
                    inner.error = Some(error.to_string());
                }
            }
        }
        Ok(())
    }

    fn runtime(&self) -> RuntimeInfo {
        let mut missing: Vec<String> = RUNTIME_FILES.iter().filter(|path| !self.runtime_root.join(path).is_file()).map(|path| (*path).to_owned()).collect();
        if !cfg!(windows) {
            missing.insert(0, "Windows x64 runtime".into());
        }
        let version = fs::read_to_string(self.runtime_root.join("engine/BUILD.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| value.get("version").and_then(|version| version.as_str()).map(str::to_owned));
        RuntimeInfo { available: missing.is_empty(), version, root: self.runtime_root.to_string_lossy().into_owned(), missing }
    }

    fn port(&self) -> u16 {
        self.inner.lock().unwrap().settings.port
    }

    fn busy(inner: &Inner) -> bool {
        inner.worker.as_ref().is_some_and(|worker| !worker.is_finished()) || inner.owned.is_some()
    }

    fn assert_open(&self) -> Result<()> {
        if self.inner.lock().unwrap().closed {
            bail!(localized("Rainy 正在关闭，无法修改 Strata。", "Rainy is closing and cannot change Strata."));
        }
        Ok(())
    }

    /// The public state; `refresh` asks the port's server (up to 3 s, so call it off the interface thread).
    pub fn status(&self, refresh: bool) -> Result<Status> {
        self.initialize()?;
        let phase_now = self.inner.lock().unwrap().phase;
        if refresh && !matches!(phase_now, Some(Phase::Preparing | Phase::Stopping)) {
            let observed = health::read(self.port()).ok();
            self.inner.lock().unwrap().health = observed;
        }
        let profiles = model::discover_profiles(self.profile_settings.as_deref());
        let runtime = self.runtime();
        let inner = self.inner.lock().unwrap();
        let owned = inner.owned.is_some();
        let unconfigured = if inner.settings.model_path.is_empty() { Phase::Unconfigured } else { Phase::Stopped };
        let phase = if !owned && inner.health.is_some() { Phase::External } else { inner.phase.unwrap_or(unconfigured) };
        Ok(Status {
            phase,
            settings: inner.settings.clone(),
            runtime,
            profiles,
            model: inner.model.as_ref().map(|resolved| resolved.model.clone()),
            server: inner.health.as_ref().map(|health| Server {
                base_url: health.base_url.clone(),
                model: health.model.clone(),
                context_window: health.context_window,
                loaded: health.loaded,
                owned,
                authentication_required: health.authentication_required,
            }),
            progress: inner.progress.clone(),
            error: inner.error.clone(),
        })
    }

    /// Save edited settings and inspect the selected files; refused while Rainy's server runs.
    pub fn save(&self, requested: Settings) -> Result<Status> {
        let _transaction = self.transaction.lock().unwrap();
        self.initialize()?;
        self.assert_open()?;
        if Self::busy(&self.inner.lock().unwrap()) {
            bail!(localized("请先停止 Rainy 启动的 Strata，再修改模型和运行参数。", "Stop the Strata server that Rainy started before changing the model or run settings."));
        }
        requested.check()?;
        let changed_model = requested.model_path != self.inner.lock().unwrap().settings.model_path;
        let resolved = model::resolve(&requested, &self.state_root, changed_model)?;
        self.assert_open()?;
        let next = resolved.as_ref().map_or(requested, |resolved| resolved.settings.clone());
        write_record(&self.settings_path(), &(serde_json::to_string_pretty(&next)? + "\n"))?;
        {
            let mut inner = self.inner.lock().unwrap();
            inner.error = resolved.as_ref().and_then(|resolved| resolved.missing.first().cloned());
            inner.phase = Some(if next.model_path.is_empty() { Phase::Unconfigured } else { Phase::Stopped });
            inner.settings = next;
            inner.model = resolved;
            inner.health = None;
        }
        self.status(true)
    }

    /// The resident budget the engine may use, after leaving memory to the system.
    fn resident_budget(&self) -> Result<f64> {
        let usable = (available_memory_gib() - RAM_HEADROOM_GIB).floor();
        if usable < 1.0 {
            bail!(localized(
                "当前可用内存不足，无法在保留系统内存后加载 Strata。请先保存并结束其他占用内存的工作。",
                "There is not enough free memory to load Strata and still leave memory for the system. Save and close other work that uses memory first."
            ));
        }
        let configured = self.inner.lock().unwrap().settings.resident_budget_gib;
        if configured.is_some_and(|budget| budget > usable) {
            bail!(localized(
                &format!("当前内存最多允许约 {usable} GiB 的 Strata 驻留预算。请降低高级设置中的内存预算，或先结束其他工作。"),
                &format!("Free memory allows a Strata resident budget of about {usable} GiB at most. Lower the memory budget in the advanced settings, or close other work first.")
            ));
        }
        Ok(configured.unwrap_or(usable))
    }

    /// Prepare the model files when needed and start the server; returns once the start is admitted.
    pub fn start(self: &Arc<Self>) -> Result<Status> {
        let _transaction = self.transaction.lock().unwrap();
        self.initialize()?;
        self.assert_open()?;
        if Self::busy(&self.inner.lock().unwrap()) {
            return self.status(false);
        }
        let runtime = self.runtime();
        if !runtime.available {
            bail!(localized("当前安装包缺少 Strata 运行文件，请在“设置 → 运行环境”中下载 Strata 本地模型引擎。", "This installation is missing the Strata runtime files. Download the Strata local model engine in Settings → Runtime."));
        }
        let settings = self.inner.lock().unwrap().settings.clone();
        let selected = model::resolve(&settings, &self.state_root, false)?
            .ok_or_else(|| anyhow!(localized("请先选择主模型 GGUF 和配套 MTP 权重。", "Select the main model GGUF and the matching MTP weights first.")))?;
        if !selected.missing.is_empty() {
            bail!(selected.missing.join("\n"));
        }
        if let Ok(health) = health::read(settings.port) {
            if health.model != selected.model.model {
                bail!(localized("所选端口已有另一个 Strata 模型；请更换端口，现有服务保持不变。", "Another Strata model is already using the selected port. Choose a different port; the running server is left unchanged."));
            }
            self.inner.lock().unwrap().health = Some(health);
            return self.status(false);
        }
        self.resident_budget()?;
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut inner = self.inner.lock().unwrap();
            inner.phase = Some(if selected.preparations.is_empty() { Phase::Starting } else { Phase::Preparing });
            inner.error = None;
            inner.model = Some(selected.clone());
            inner.cancel = Some(cancel.clone());
        }
        let this = self.clone();
        let worker = std::thread::Builder::new().name("strata-start".into()).spawn(move || {
            if let Err(error) = this.run(selected, &cancel) {
                let owned = this.inner.lock().unwrap().owned.take();
                if let Some(owned) = owned {
                    owned.stop();
                }
                let mut inner = this.inner.lock().unwrap();
                inner.health = None;
                inner.progress = None;
                if cancel.load(Ordering::SeqCst) {
                    inner.phase = Some(Phase::Stopped);
                } else {
                    inner.phase = Some(Phase::Error);
                    inner.error = Some(error.to_string());
                }
            }
            (this.changed)();
        })?;
        self.inner.lock().unwrap().worker = Some(worker);
        self.status(false)
    }

    fn spawn(&self, args: &[String], log: &str, cancel: &AtomicBool) -> Result<Arc<Owned>> {
        if cancel.load(Ordering::SeqCst) {
            bail!("cancelled");
        }
        self.assert_open()?;
        let logs = self.state_root.join("logs");
        fs::create_dir_all(&logs)?;
        let output = fs::OpenOptions::new().create(true).append(true).open(logs.join(log))?;
        let mut command = Command::new(self.runtime_root.join("portablepython/python.exe"));
        command.args(args).current_dir(self.runtime_root.join("server")).env_clear().envs(child_environment()).stdin(Stdio::null()).stdout(output.try_clone()?).stderr(output);
        #[cfg(windows)]
        hide(&mut command);
        let owned = Arc::new(Owned { child: Mutex::new(command.spawn()?) });
        self.inner.lock().unwrap().owned = Some(owned.clone());
        Ok(owned)
    }

    fn set_progress(&self, phase: Phase, progress: String) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.phase = Some(phase);
            inner.progress = Some(progress);
        }
        (self.changed)();
    }

    fn run(&self, selected: Resolved, cancel: &AtomicBool) -> Result<()> {
        let tools = self.runtime_root.join("server/tools");
        for preparation in &selected.preparations {
            let text = if preparation.mtp {
                localized("正在从本地 MTP 权重准备运行文件…", "Preparing runtime files from the local MTP weights…")
            } else {
                localized("正在从本地 GGUF 准备 Strata 模型文件…", "Preparing Strata model files from the local GGUF…")
            };
            self.set_progress(Phase::Preparing, text);
            fs::create_dir_all(&preparation.directory)?;
            let mut args = vec!["-I".to_owned(), "-B".to_owned(), tools.join(preparation.tool).to_string_lossy().into_owned()];
            args.extend(preparation.args.iter().cloned());
            let child = self.spawn(&args, &format!("prepare-{}.log", unique()), cancel)?;
            let code = loop {
                if cancel.load(Ordering::SeqCst) {
                    child.stop();
                    bail!("cancelled");
                }
                if let Some(status) = child.child.lock().unwrap().try_wait()? {
                    break status;
                }
                std::thread::sleep(Duration::from_millis(250));
            };
            self.inner.lock().unwrap().owned = None;
            if !code.success() {
                bail!(localized(
                    "本地模型准备失败。请确认所有 GGUF 分片及配套 MTP 属于受支持的 Qwen3.8 Flash Next 模型。详细诊断保存在 Rainy 的 strata/logs。",
                    "Preparing the local model failed. Make sure all GGUF shards and the matching MTP belong to a supported Qwen3.8 Flash Next model. Details are saved in the strata/logs folder of Rainy."
                ));
            }
            write_record(&preparation.directory.join(".rainy-complete.json"), "{\"version\":1}\n")?;
        }
        let settings = self.inner.lock().unwrap().settings.clone();
        let ready = model::resolve(&settings, &self.state_root, false)?;
        let complete = ready.as_ref().is_some_and(|ready| {
            ready.preparations.is_empty() && ready.missing.is_empty() && ready.model.pack_path.is_some() && ready.model.tokenizer_path.is_some() && ready.model.mtp_path.is_some()
        });
        let Some(ready) = ready.filter(|_| complete) else {
            bail!(localized("模型文件尚不完整或准备期间发生变化，请检查主模型和配套 MTP 后重试。", "The model files are incomplete or changed during preparation. Check the main model and the matching MTP, then try again."));
        };
        self.inner.lock().unwrap().model = Some(ready.clone());
        let budget = self.resident_budget()?;
        let profile = self.runtime_root.join("server/data/expert-profile.bin").to_string_lossy().into_owned();
        let engine_args: Vec<String> = [
            ("--pack", ready.model.pack_path.clone().unwrap_or_default()),
            ("--native", ready.model.gguf_path.clone()),
            ("--expert-profile", profile),
            ("--expert-cache", "auto".into()),
            ("--prefill", "auto".into()),
            ("--spec", "4".into()),
            ("--spec-min-p", "0.5".into()),
            ("--mtp", ready.model.mtp_path.clone().unwrap_or_default()),
            ("--max-context", settings.context_window.to_string()),
            ("--kv", settings.kv_cache.clone()),
            ("--vram-reserve-mib", settings.vram_reserve_mib.to_string()),
            ("--resident-budget-gib", budget.to_string()),
        ]
        .into_iter()
        .flat_map(|(flag, value)| [flag.to_owned(), value])
        .collect();
        let config = self.state_root.join("run.json");
        let record = serde_json::json!({
            "exe": self.runtime_root.join("engine/strata.exe"), "args": engine_args, "cwd": self.runtime_root.join("server"),
            "tokenizer": ready.model.tokenizer_path, "model_name": ready.model.model,
            "log": self.state_root.join("logs").join(format!("engine-{}.log", unique())),
            "host": "127.0.0.1", "port": settings.port, "open_browser": false, "api_monitor": false,
        });
        write_record(&config, &(serde_json::to_string_pretty(&record)? + "\n"))?;
        self.set_progress(Phase::Starting, localized("正在加载本地 Strata 模型…", "Loading the local Strata model…"));
        let server = self.runtime_root.join("server/serve/server.py");
        let args: Vec<String> = ["-I", "-B", &server.to_string_lossy(), "--engine", "strata", "--config", &config.to_string_lossy(), "--host", "127.0.0.1", "--port", &settings.port.to_string()]
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect();
        let child = self.spawn(&args, &format!("server-{}.log", unique()), cancel)?;
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while Instant::now() < deadline {
            if cancel.load(Ordering::SeqCst) {
                bail!("cancelled");
            }
            if child.finished() {
                bail!(localized(
                    "Strata 服务在就绪前退出。请检查模型文件、显存和端口；诊断保存在 strata/logs。",
                    "The Strata server exited before it was ready. Check the model files, video memory and port. Details are saved in strata/logs."
                ));
            }
            let health = health::read(settings.port).ok();
            if let Some(health) = health.filter(|health| health.loaded) {
                if health.model != ready.model.model || health.authentication_required {
                    bail!(localized("端口上的 Strata 服务与本次启动的模型不一致。", "The Strata server on this port is not running the model that was just started."));
                }
                {
                    let mut inner = self.inner.lock().unwrap();
                    inner.health = Some(health);
                    inner.phase = Some(Phase::Running);
                    inner.progress = None;
                }
                (self.changed)();
                self.watch(child, cancel);
                return Ok(());
            }
            std::thread::sleep(POLL);
        }
        bail!(localized("Strata 启动超时。请检查可用显存、内存及模型文件后重试。", "Strata took too long to start. Check free video memory, memory and the model files, then try again."))
    }

    /// Report a server that exits on its own; its settings and files are kept.
    fn watch(&self, child: Arc<Owned>, cancel: &AtomicBool) {
        while !child.finished() {
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(POLL);
        }
        {
            let mut inner = self.inner.lock().unwrap();
            if !inner.owned.as_ref().is_some_and(|owned| Arc::ptr_eq(owned, &child)) || cancel.load(Ordering::SeqCst) {
                return;
            }
            inner.owned = None;
            inner.health = None;
            inner.phase = Some(Phase::Error);
            inner.error = Some(localized("Strata 服务已退出。模型文件和设置已保留。", "The Strata server exited. Model files and settings are kept."));
        }
        (self.changed)();
    }

    fn stop_owned(&self) {
        let (worker, owned, loaded, port) = {
            let mut inner = self.inner.lock().unwrap();
            if let Some(cancel) = &inner.cancel {
                cancel.store(true, Ordering::SeqCst);
            }
            if Self::busy(&inner) {
                inner.phase = Some(Phase::Stopping);
            }
            (inner.worker.take(), inner.owned.take(), inner.health.as_ref().is_some_and(|health| health.loaded), inner.settings.port)
        };
        (self.changed)();
        if let Some(owned) = owned {
            if loaded && !owned.finished() {
                health::unload(port);
            }
            owned.stop();
        }
        if let Some(worker) = worker {
            let _ = worker.join();
        }
        let mut inner = self.inner.lock().unwrap();
        inner.owned = None;
        inner.cancel = None;
        inner.health = None;
        inner.progress = None;
        inner.phase = Some(if inner.settings.model_path.is_empty() { Phase::Unconfigured } else { Phase::Stopped });
    }

    /// Stop the preparation or server Rainy started and wait for it; another server on the port is left alone.
    pub fn stop(&self) -> Result<Status> {
        let _transaction = self.transaction.lock().unwrap();
        self.initialize()?;
        self.stop_owned();
        self.status(true)
    }

    /// The loaded server, for the selected Host to verify and save as a model.
    pub fn connection(&self) -> Result<Connection> {
        self.initialize()?;
        self.assert_open()?;
        let health = health::read(self.port())?;
        if !health.loaded {
            bail!(localized("Strata 模型尚未加载完成。", "The Strata model has not finished loading."));
        }
        if health.authentication_required {
            bail!(localized("此服务需要 API 密钥，请在现有模型设置中配置连接。", "This server needs an API key. Set up the connection in your existing model settings."));
        }
        Ok(Connection { base_url: health.base_url, model: health.model, context_window: health.context_window })
    }

    /// Refuse further changes and stop what Rainy started, for quitting.
    pub fn close(&self) {
        self.inner.lock().unwrap().closed = true;
        let _transaction = self.transaction.lock().unwrap();
        self.stop_owned();
    }
}

fn unique() -> String {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn settings_keep_the_3x_format_and_ranges() {
        let text = r#"{"modelPath":"","mtpPath":"","contextWindow":32768,"port":8081,"kvCache":"int8","vramReserveMiB":700,"residentBudgetGiB":null}"#;
        let settings: Settings = serde_json::from_str(text).unwrap();
        assert_eq!(settings, Settings::default());
        assert_eq!(serde_json::to_string(&settings).unwrap(), text);
        assert!(Settings { context_window: 4096, ..Settings::default() }.check().is_err());
        assert!(Settings { kv_cache: "fp16".into(), ..Settings::default() }.check().is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"modelPath":"","extra":1}"#).is_err());
    }

    #[test]
    fn reports_a_missing_runtime_and_saves_settings() {
        let dir = std::env::temp_dir().join(format!("rainy-strata-manager-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let manager = Manager::new(dir.join("runtime"), &dir.join("user"), || {});
        let status = manager.status(false).unwrap();
        assert_eq!(status.phase, Phase::Unconfigured);
        assert!(!status.runtime.available && status.runtime.missing.contains(&"engine/strata.exe".to_owned()));
        assert!(manager.start().is_err());
        let saved = manager.save(Settings { port: 18081, ..Settings::default() }).unwrap();
        assert_eq!(saved.settings.port, 18081);
        let reloaded = Manager::new(dir.join("runtime"), &dir.join("user"), || {});
        assert_eq!(reloaded.status(false).unwrap().settings.port, 18081);
        let _ = fs::remove_dir_all(&dir);
    }
}
