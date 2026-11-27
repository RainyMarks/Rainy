//! External CLI agent harness (`codex app-server --stdio` and ACP servers such as `agy acp`).
//!
//! Runs entirely inside `rainy-host` over child process stdio JSON-RPC with zero extra runtime
//! dependencies, so Windows, WSL and SSH targets all support external agents uniformly.

pub mod acp;
pub mod codex;
pub mod common;
pub mod jsonrpc;
pub mod quota;
pub mod team;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rainy_protocol::chat::{
    ExternalAgentConfig, ExternalAgentStatus, ExternalBackendRef, ExternalCommand, ExternalModelInfo, ExternalQuota, ExternalStatusResponse, PendingApproval,
    UsageSummary,
};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use self::acp::{AcpProcess, AcpTurnControl};
use self::codex::{CodexProcess, CodexTurnControl};
use self::jsonrpc::{external_env, resolve_cli};
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::settings::Settings;

/// Idle subprocesses with no active turn are reaped after this duration.
const IDLE_REAP_AFTER: Duration = Duration::from_secs(10 * 60);
const REAP_INTERVAL: Duration = Duration::from_secs(60);
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_MODELS_TIMEOUT: Duration = Duration::from_secs(15);
/// Usage limits read this recently are not read again unless asked to.
const QUOTA_FRESH_MS: i64 = 60_000;
/// `agy -p /usage` asks Google's servers; it answers within seconds.
const QUOTA_TIMEOUT: Duration = Duration::from_secs(40);
/// `agy models` asks Google's servers too.
const AGY_MODELS_TIMEOUT: Duration = Duration::from_secs(30);
/// Models that could not be listed are not asked for again sooner.
const MODELS_RETRY_MS: i64 = 60_000;

/// User decision answering an external agent's approval/permission prompt.
#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    Allow,
    AllowSession,
    Deny,
}

/// Parameters for one conversational turn sent to an external agent backend.
#[derive(Debug, Clone)]
pub struct TurnRequest {
    pub backend: ExternalBackendRef,
    pub cwd: PathBuf,
    pub thread_id: Option<String>,
    pub prompt: String,
    pub image_paths: Vec<PathBuf>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub permission: String,
}

/// Normalized streaming & lifecycle event produced by any external agent driver.
#[derive(Debug)]
pub enum ExternalEvent {
    ThreadReady { thread_id: String },
    TextDelta(String),
    ThinkingDelta(String),
    ToolStarted { id: String, name: String, args: serde_json::Value },
    ToolProgress { id: String, name: String, partial: String },
    ToolCompleted {
        id: String,
        name: String,
        output: String,
        is_error: bool,
        details: Option<serde_json::Value>,
    },
    ApprovalRequested {
        approval: PendingApproval,
        respond: Arc<Mutex<Option<oneshot::Sender<ApprovalDecision>>>>,
    },
    DiffUpdated { diff: String },
    /// The agents the backend runs for this chat changed (Codex sub-agents); `session_id` is left empty.
    TeamUpdated(rainy_protocol::chat::ExternalTeam),
    /// The slash commands the agent offers (ACP `available_commands_update`).
    CommandsUpdated(Vec<ExternalCommand>),
    UsageUpdated(UsageSummary),
    /// The models the agent offers, as a session it opened listed them (ACP `session/new` and `session/resume`).
    ModelsListed(Vec<ExternalModelInfo>),
    /// The account's usage limits, as the agent reported them during the turn (Codex `account/rateLimits/updated`).
    QuotaUpdated(ExternalQuota),
    Notice { code: String, text: String },
    TurnFinished {
        stop_reason: String,
        error: Option<String>,
        usage: Option<UsageSummary>,
        model: Option<String>,
    },
}

/// Control handle for an in-flight external turn.
#[derive(Clone)]
pub struct ExternalTurnHandle {
    inner: TurnControl,
}

#[derive(Clone)]
enum TurnControl {
    Codex(Arc<CodexTurnControl>),
    Acp(Arc<AcpTurnControl>),
}

impl ExternalTurnHandle {
    /// Deliver a live steering message if supported by the backend (`Ok(false)` means queue instead).
    pub async fn steer(&self, text: &str) -> HostResult<bool> {
        match &self.inner {
            TurnControl::Codex(ctrl) => ctrl.steer(text).await,
            TurnControl::Acp(_) => Ok(false),
        }
    }

    /// Politely cancel the in-flight turn on the backend.
    pub async fn interrupt(&self) {
        match &self.inner {
            TurnControl::Codex(ctrl) => ctrl.interrupt().await,
            TurnControl::Acp(ctrl) => ctrl.cancel_prompt(),
        }
    }

    /// Synchronize a mid-turn permission mode change when supported by the backend.
    pub async fn update_permission(&self, mode: &str) {
        if let TurnControl::Acp(ctrl) = &self.inner {
            ctrl.update_permission(mode).await;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProcessKey {
    kind: String,
    binary: PathBuf,
    cwd: PathBuf,
}

#[derive(Clone, Debug, Default)]
struct CachedProbe {
    installed: bool,
    binary_path: Option<String>,
    version: Option<String>,
    models: Vec<ExternalModelInfo>,
    error: Option<String>,
}

/// Manages external CLI agent discovery, probing, configuration and pooled subprocess lifecycles.
pub struct ExternalManager {
    settings: Arc<Settings>,
    windows: bool,
    closed: AtomicBool,
    cache: Mutex<HashMap<String, CachedProbe>>,
    codex_pool: AsyncMutex<HashMap<ProcessKey, Arc<CodexProcess>>>,
    acp_pool: AsyncMutex<HashMap<String, (ProcessKey, Arc<AcpProcess>)>>,
    stop_reaper: CancellationToken,
    /// The slash commands each kind of agent last listed.
    commands: Mutex<HashMap<String, Vec<ExternalCommand>>>,
    /// The models each kind of agent last listed, kept in `external-models.json` so the model menu has them after
    /// a restart without starting the agent (a probe over ACP leaves an empty conversation in Antigravity's history).
    known_models: Mutex<HashMap<String, Vec<ExternalModelInfo>>>,
    models_write_lock: Mutex<()>,
    models_file: PathBuf,
    /// When each kind's models were last listed for want of any known ones.
    models_asked: Mutex<HashMap<String, i64>>,
    /// Each kind's usage limits as last read, and the kinds being read now.
    quotas: Mutex<HashMap<String, ExternalQuota>>,
    quota_reading: Mutex<HashSet<String>>,
}

struct QuotaReadingGuard<'a> {
    set: &'a Mutex<HashSet<String>>,
    kind: String,
}

impl Drop for QuotaReadingGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut reading) = self.set.lock() {
            reading.remove(&self.kind);
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct KnownModels {
    version: u8,
    models: std::collections::BTreeMap<String, Vec<ExternalModelInfo>>,
}

impl ExternalManager {
    pub fn new(settings: Arc<Settings>, windows: bool) -> Arc<Self> {
        let models_file = settings.home().join("external-models.json");
        let known_models = match crate::store::read_json::<KnownModels>(&models_file) {
            Ok(Some(file)) => file.models.into_iter().collect(),
            Ok(None) => HashMap::new(),
            Err(error) => {
                log::warn!("{error}; the external agents' models are listed again by the next probe or chat");
                HashMap::new()
            }
        };
        let manager = Arc::new(Self {
            settings,
            windows,
            closed: AtomicBool::new(false),
            cache: Mutex::new(HashMap::new()),
            codex_pool: AsyncMutex::new(HashMap::new()),
            acp_pool: AsyncMutex::new(HashMap::new()),
            stop_reaper: CancellationToken::new(),
            commands: Mutex::new(HashMap::new()),
            known_models: Mutex::new(known_models),
            models_write_lock: Mutex::new(()),
            models_file,
            models_asked: Mutex::new(HashMap::new()),
            quotas: Mutex::new(HashMap::new()),
            quota_reading: Mutex::new(HashSet::new()),
        });
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let weak = Arc::downgrade(&manager);
            let stop = manager.stop_reaper.clone();
            handle.spawn(async move {
                loop {
                    tokio::select! {
                        () = tokio::time::sleep(REAP_INTERVAL) => {
                            let Some(mgr) = weak.upgrade() else { break };
                            mgr.reap_idle().await;
                        }
                        () = stop.cancelled() => break,
                    }
                }
            });
        }
        manager
    }

    /// Return current status of all built-in (`codex`, `antigravity`) and user-configured ACP backends.
    /// Does not spawn external subprocesses; performs a fast PATH lookup if no probe cache exists yet.
    pub fn status(&self) -> ExternalStatusResponse {
        let configs = self.effective_configs();
        let cache = self.cache.lock().map(|g| g.clone()).unwrap_or_default();
        let env = external_env();
        let agents = configs
            .into_iter()
            .map(|cfg| {
                let cached = cache.get(&cfg.kind);
                let (installed, binary_path, version, models, error) = match cached {
                    Some(hit) => (hit.installed, hit.binary_path.clone(), hit.version.clone(), hit.models.clone(), hit.error.clone()),
                    None => {
                        let cmd = command_name_for(&cfg);
                        let resolved = resolve_cli(&cmd, &env);
                        let missing = (resolved.is_none() && !cmd.is_empty()).then(|| jsonrpc::explain_missing(&cmd, &env));
                        (
                            resolved.is_some(),
                            resolved.map(|p| p.to_string_lossy().into_owned()),
                            None,
                            self.known_models.lock().ok().and_then(|known| known.get(&cfg.kind).cloned()).unwrap_or_default(),
                            missing,
                        )
                    }
                };
                ExternalAgentStatus {
                    label: display_label(&cfg),
                    install_hint: install_hint(&cfg.kind, self.windows),
                    login_hint: login_hint(&cfg.kind, self.windows),
                    commands: self.commands.lock().ok().and_then(|known| known.get(&cfg.kind).cloned()).unwrap_or_default(),
                    quota: self.quotas.lock().ok().and_then(|known| known.get(&cfg.kind).cloned()),
                    kind: cfg.kind,
                    enabled: cfg.enabled,
                    installed,
                    binary: cfg.binary,
                    args: cfg.args,
                    binary_path,
                    version,
                    models,
                    default_model: cfg.default_model,
                    default_thinking: cfg.default_thinking,
                    error,
                }
            })
            .collect();
        ExternalStatusResponse { agents }
    }

    /// Actively probe one or all external backends (binary resolution, `--version`, and model catalog).
    pub async fn probe(
        &self,
        kind: Option<&str>,
        binary_override: Option<&str>,
        args_override: Option<&[String]>,
        cwd: &Path,
    ) -> HostResult<ExternalStatusResponse> {
        let wanted = match kind.map(str::trim).filter(|k| !k.is_empty()) {
            Some(raw) => {
                let normalized = raw.to_ascii_lowercase();
                if !is_valid_kind(&normalized) {
                    return Err(HostError::invalid(l(
                        "外部 Agent 标识只能包含 1–32 个小写字母、数字或连字符。",
                        "External agent kind must be 1–32 lowercase letters, digits or hyphens.",
                    )));
                }
                Some(normalized)
            }
            None => None,
        };
        if let Some(bin) = binary_override {
            check_binary_str(bin)?;
        }
        if let Some(args) = args_override {
            check_args_slice(args)?;
        }
        let mut targets = self.effective_configs();
        if let Some(wanted) = &wanted {
            targets.retain(|cfg| cfg.kind == *wanted);
            if targets.is_empty() {
                let custom = ExternalAgentConfig {
                    kind: wanted.clone(),
                    enabled: true,
                    binary: binary_override.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned),
                    args: args_override.map(|a| a.to_vec()).unwrap_or_default(),
                    label: None,
                    default_model: None,
                    default_thinking: None,
                };
                check_external_agent(&custom)?;
                targets.push(custom);
            }
        }
        let env = external_env();
        for mut cfg in targets {
            if wanted.is_some() {
                if let Some(bin) = binary_override {
                    let trimmed = bin.trim();
                    cfg.binary = (!trimmed.is_empty()).then(|| trimmed.to_owned());
                }
                if let Some(args) = args_override {
                    cfg.args = args.to_vec();
                }
                check_external_agent(&cfg)?;
            }
            let result = probe_one(&cfg, &env, cwd).await;
            self.remember_models(&cfg.kind, result.models.clone());
            if let Ok(mut cache) = self.cache.lock() {
                cache.insert(cfg.kind.clone(), result);
            }
        }
        Ok(self.status())
    }

    /// Persist configuration for one external agent backend (`remove: true` removes a custom ACP entry).
    pub async fn configure(&self, mut config: ExternalAgentConfig, remove: bool) -> HostResult<ExternalStatusResponse> {
        config.kind = config.kind.trim().to_ascii_lowercase();
        if !remove {
            check_external_agent(&config)?;
        } else if !is_valid_kind(&config.kind) {
            return Err(HostError::invalid(l("外部 Agent 标识无效。", "Invalid external agent kind.")));
        }
        let target_kind = config.kind.clone();
        self.settings.update(|data| {
            data.external_agents.retain(|item| item.kind != target_kind);
            if !remove {
                data.external_agents.push(config);
            }
            Ok(())
        })?;
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(&target_kind);
        }
        self.evict_kind(&target_kind).await;
        Ok(self.status())
    }

    /// Start one turn on the requested external agent backend, returning a control handle and event stream.
    pub async fn start_turn(
        self: &Arc<Self>,
        session_id: &str,
        req: TurnRequest,
        cancel: CancellationToken,
    ) -> HostResult<(ExternalTurnHandle, mpsc::UnboundedReceiver<ExternalEvent>)> {
        let kind = req.backend.kind.trim().to_ascii_lowercase();
        let (binary, args) = self.resolve_launch(&req.backend)?;
        let key = ProcessKey { kind: kind.clone(), binary: binary.clone(), cwd: req.cwd.clone() };
        let (tx, rx) = mpsc::unbounded_channel();

        if kind == "codex" {
            let process = self.get_or_spawn_codex(key, &args).await?;
            let control = Arc::new(CodexTurnControl::new(process.rpc.clone()));
            let handle = ExternalTurnHandle { inner: TurnControl::Codex(control.clone()) };
            let session_id = session_id.to_owned();
            tokio::spawn(async move {
                if let Err(err) = process.run_turn(&session_id, &req, tx.clone(), control, cancel).await {
                    let _ = tx.send(ExternalEvent::TurnFinished {
                        stop_reason: if err.code == "aborted" { "aborted".into() } else { "error".into() },
                        error: (err.code != "aborted").then_some(err.message),
                        usage: None,
                        model: req.model.clone(),
                    });
                }
            });
            Ok((handle, rx))
        } else {
            let process = self.get_or_spawn_acp(session_id, key, &args).await?;
            let control = Arc::new(AcpTurnControl::new(process.rpc.clone()));
            let handle = ExternalTurnHandle { inner: TurnControl::Acp(control.clone()) };
            tokio::spawn(async move {
                if let Err(err) = process.run_turn(&req, tx.clone(), control, cancel).await {
                    let _ = tx.send(ExternalEvent::TurnFinished {
                        stop_reason: if err.code == "aborted" { "aborted".into() } else { "error".into() },
                        error: (err.code != "aborted").then_some(err.message),
                        usage: None,
                        model: req.model.clone(),
                    });
                }
            });
            Ok((handle, rx))
        }
    }

    /// Keep the models an agent listed; whether they differ from what was known (then they are saved too).
    pub fn remember_models(&self, kind: &str, models: Vec<ExternalModelInfo>) -> bool {
        if models.is_empty() {
            return false;
        }
        // Serialize read-modify-write + disk persistence so concurrent probes never overwrite newer model lists.
        let Ok(_write_guard) = self.models_write_lock.lock() else { return false };
        let snapshot = {
            let Ok(mut known) = self.known_models.lock() else { return false };
            if known.get(kind) == Some(&models) {
                return false;
            }
            known.insert(kind.to_owned(), models.clone());
            KnownModels { version: 1, models: known.iter().map(|(kind, models)| (kind.clone(), models.clone())).collect() }
        };
        if let Ok(mut cache) = self.cache.lock()
            && let Some(hit) = cache.get_mut(kind)
        {
            hit.models = models;
        }
        if let Err(error) = crate::store::write_json(&self.models_file, &snapshot) {
            log::warn!("could not save {}: {error}", self.models_file.display());
        }
        true
    }

    /// List the models of Codex (`model/list`) or Antigravity (`agy models`) while none are known, so the model menu
    /// offers them before the first chat (which lists them again); neither opens a conversation. At most once a
    /// minute per kind. Whether what is known changed.
    pub async fn list_models_if_unknown(&self, kind: &str, cwd: &Path) -> bool {
        if !matches!(kind, "codex" | "antigravity") || self.known_models.lock().map_or(true, |known| known.get(kind).is_some_and(|models| !models.is_empty())) {
            return false;
        }
        {
            let Ok(mut asked) = self.models_asked.lock() else { return false };
            let now = quota::now_ms();
            if asked.get(kind).is_some_and(|at| now - at < MODELS_RETRY_MS) {
                return false;
            }
            asked.insert(kind.to_owned(), now);
        }
        let Some(cfg) = self.effective_configs().into_iter().find(|cfg| cfg.kind == kind && cfg.enabled) else { return false };
        let env = external_env();
        let Some(binary) = resolve_cli(&command_name_for(&cfg), &env) else { return false };
        let listed = if kind == "codex" {
            let args = if cfg.args.is_empty() { default_args(kind) } else { cfg.args.clone() };
            self.list_codex_models(&binary, &args, cwd).await
        } else {
            list_agy_models(&binary, cwd, &env).await
        };
        match listed {
            Ok(models) => self.remember_models(kind, models),
            Err(error) => {
                log::info!("{kind}: {error}");
                false
            }
        }
    }

    /// The models a Codex already running for a chat lists, else one started for this.
    async fn list_codex_models(&self, binary: &Path, args: &[String], cwd: &Path) -> Result<Vec<ExternalModelInfo>, String> {
        let running = self.codex_pool.lock().await.values().find(|process| process.is_alive()).cloned();
        let started = running.is_none();
        let process = match running {
            Some(process) => process,
            None => CodexProcess::spawn(binary, args, cwd).await.map_err(|error| error.message)?,
        };
        let cancel = CancellationToken::new();
        let listed = tokio::time::timeout(PROBE_MODELS_TIMEOUT, process.list_models(&cancel)).await;
        if started {
            process.close().await;
        }
        match listed {
            Ok(Ok(models)) if !models.is_empty() => Ok(models),
            Ok(Ok(_)) => Err(l("Codex 没有列出模型。", "Codex listed no models.")),
            Ok(Err(error)) => Err(error.message),
            Err(_) => Err(l("Codex 获取模型列表超时。", "Codex's model list timed out.")),
        }
    }

    pub fn remember_commands(&self, kind: &str, commands: Vec<ExternalCommand>) {
        if let Ok(mut known) = self.commands.lock() {
            known.insert(kind.to_owned(), commands);
        }
    }

    /// Keep usage limits an agent of `kind` reported; whether they differ from those known (ignoring when read).
    pub fn note_quota(&self, kind: &str, quota: ExternalQuota) -> bool {
        let Ok(mut known) = self.quotas.lock() else { return false };
        let same = known.get(kind).is_some_and(|old| old.plan == quota.plan && old.windows == quota.windows && old.error == quota.error);
        known.insert(kind.to_owned(), quota);
        !same
    }

    /// Read the usage limits of `kind` again, unless they were read within the last minute and `force` is off: Codex
    /// answers `account/rateLimits/read`, Antigravity prints them for `agy -p /usage` (neither opens a conversation).
    /// Whether what is known changed.
    pub async fn refresh_quota(&self, kind: &str, force: bool, cwd: &Path) -> bool {
        if !matches!(kind, "codex" | "antigravity") {
            return false;
        }
        let fresh = self.quotas.lock().ok().and_then(|known| known.get(kind).map(|quota| quota::now_ms() - quota.fetched_at < QUOTA_FRESH_MS)).unwrap_or(false);
        if fresh && !force {
            return false;
        }
        {
            let Ok(mut reading) = self.quota_reading.lock() else { return false };
            if !reading.insert(kind.to_owned()) {
                return false;
            }
        }
        let _guard = QuotaReadingGuard { set: &self.quota_reading, kind: kind.to_owned() };
        let read = self.read_quota(kind, cwd).await;
        drop(_guard);
        let quota = read.unwrap_or_else(|error| {
            // Keep the last limits read, marked with why they could not be read now.
            let mut last = self.quotas.lock().ok().and_then(|known| known.get(kind).cloned()).unwrap_or_default();
            last.fetched_at = quota::now_ms();
            last.error = Some(error);
            last
        });
        self.note_quota(kind, quota)
    }

    async fn read_quota(&self, kind: &str, cwd: &Path) -> Result<ExternalQuota, String> {
        let cfg = self.effective_configs().into_iter().find(|cfg| cfg.kind == kind).ok_or_else(|| l("没有配置这个外部 Agent。", "This external agent is not configured."))?;
        let env = external_env();
        let command = command_name_for(&cfg);
        let binary = resolve_cli(&command, &env).ok_or_else(|| jsonrpc::explain_missing(&command, &env))?;
        if kind == "codex" {
            let args = if cfg.args.is_empty() { default_args(kind) } else { cfg.args.clone() };
            // A Codex already running for a chat answers as well as a new one, and sooner.
            let running = self.codex_pool.lock().await.values().find(|process| process.is_alive()).cloned();
            let started = running.is_none();
            let process = match running {
                Some(process) => process,
                None => CodexProcess::spawn(&binary, &args, cwd).await.map_err(|error| error.message)?,
            };
            let cancel = CancellationToken::new();
            let read = tokio::time::timeout(QUOTA_TIMEOUT, process.read_rate_limits(&cancel)).await;
            if started {
                process.close().await;
            }
            return match read {
                Ok(Ok(value)) => quota::codex(&value).ok_or_else(|| l("Codex 没有返回额度信息。", "Codex sent no usage limits.")),
                Ok(Err(error)) => Err(error.message),
                Err(_) => Err(l("读取 Codex 额度超时。", "Reading Codex's usage limits timed out.")),
            };
        }
        let output = crate::process::run(&binary, &["-p", "/usage"], cwd, &env, 64 * 1024, Some(QUOTA_TIMEOUT))
            .await
            .map_err(|error| l(&format!("无法运行 agy 读取额度：{error}"), &format!("Could not run agy to read the usage limits: {error}")))?;
        let text = crate::process::console_text(&output.stdout);
        let errors = crate::process::console_text(&output.stderr);
        quota::agy_usage(&text).ok_or_else(|| {
            let said = text.lines().chain(errors.lines()).map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().chars().take(160).collect::<String>();
            if said.is_empty() {
                l("agy 没有返回额度信息。", "agy printed no usage limits.")
            } else {
                l(&format!("agy 没有返回额度信息：{said}"), &format!("agy printed no usage limits: {said}"))
            }
        })
    }

    /// The latest steps of a Codex sub-agent of the main thread `main`.
    pub async fn codex_activity(&self, main: &str, member: &str, limit: usize) -> Option<Vec<rainy_protocol::chat::TeamActivity>> {
        self.codex_pool.lock().await.values().find_map(|process| process.agent_activity(main, member, limit))
    }

    /// Release any per-session external process and Codex team cache when a Rainy chat is closed or deleted.
    pub async fn close_session(&self, session_id: &str) {
        let removed = self.acp_pool.lock().await.remove(session_id);
        if let Some((_, process)) = removed {
            process.close().await;
        }
        let codex_procs: Vec<Arc<CodexProcess>> = self.codex_pool.lock().await.values().cloned().collect();
        for proc in codex_procs {
            proc.remove_session_team(session_id);
        }
    }

    /// Shut down the reaper and terminate every running external agent subprocess cleanly.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.stop_reaper.cancel();
        let codex_list: Vec<Arc<CodexProcess>> = self.codex_pool.lock().await.drain().map(|(_, p)| p).collect();
        let acp_list: Vec<Arc<AcpProcess>> = self.acp_pool.lock().await.drain().map(|(_, (_, p))| p).collect();
        futures::future::join_all(codex_list.iter().map(|p| p.close())).await;
        futures::future::join_all(acp_list.iter().map(|p| p.close())).await;
    }

    async fn drain_pools(
        &self,
        codex_evict: impl Fn(&ProcessKey, &CodexProcess) -> bool,
        acp_evict: impl Fn(&ProcessKey, &AcpProcess) -> bool,
    ) {
        let mut stale_codex = Vec::new();
        {
            let mut pool = self.codex_pool.lock().await;
            pool.retain(|k, proc| {
                if codex_evict(k, proc) {
                    stale_codex.push(proc.clone());
                    false
                } else {
                    true
                }
            });
        }
        let mut stale_acp = Vec::new();
        {
            let mut pool = self.acp_pool.lock().await;
            pool.retain(|_, (k, proc)| {
                if acp_evict(k, proc) {
                    stale_acp.push(proc.clone());
                    false
                } else {
                    true
                }
            });
        }
        for proc in stale_codex {
            proc.close().await;
        }
        for proc in stale_acp {
            proc.close().await;
        }
    }

    async fn reap_idle(&self) {
        self.drain_pools(
            |_, proc| !proc.is_alive() || proc.idle_for() >= IDLE_REAP_AFTER,
            |_, proc| !proc.is_alive() || proc.idle_for() >= IDLE_REAP_AFTER,
        )
        .await;
    }

    async fn evict_kind(&self, kind: &str) {
        self.drain_pools(|k, _| k.kind == kind, |k, _| k.kind == kind).await;
    }

    async fn get_or_spawn_codex(&self, key: ProcessKey, args: &[String]) -> HostResult<Arc<CodexProcess>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(HostError::other(l("外部 Agent 服务已关闭。", "External agent manager is closed.")));
        }
        if let Some(existing) = self.codex_pool.lock().await.get(&key).filter(|proc| proc.is_alive()).cloned() {
            return Ok(existing);
        }
        // Spawn and complete the handshake outside the pool lock so other workspaces never stall.
        let spawned = CodexProcess::spawn(&key.binary, args, &key.cwd).await?;
        let (selected, to_close) = {
            let mut pool = self.codex_pool.lock().await;
            if self.closed.load(Ordering::Acquire) {
                (None, Some(spawned))
            } else if let Some(existing) = pool.get(&key).filter(|proc| proc.is_alive()).cloned() {
                (Some(existing), Some(spawned))
            } else {
                let replaced = pool.insert(key, spawned.clone());
                (Some(spawned), replaced)
            }
        };
        if let Some(stale) = to_close {
            stale.close().await;
        }
        selected.ok_or_else(|| HostError::other(l("外部 Agent 服务已关闭。", "External agent manager is closed.")))
    }

    async fn get_or_spawn_acp(&self, session_id: &str, key: ProcessKey, args: &[String]) -> HostResult<Arc<AcpProcess>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(HostError::other(l("外部 Agent 服务已关闭。", "External agent manager is closed.")));
        }
        let old = {
            let mut pool = self.acp_pool.lock().await;
            match pool.get(session_id) {
                Some((existing_key, existing_proc)) if *existing_key == key && existing_proc.is_alive() => {
                    return Ok(existing_proc.clone());
                }
                _ => pool.remove(session_id).map(|(_, p)| p),
            }
        };
        if let Some(old) = old {
            old.close().await;
        }
        let spawned = AcpProcess::spawn(&key.kind, &key.binary, args, &key.cwd).await?;
        let (selected, to_close) = {
            let mut pool = self.acp_pool.lock().await;
            if self.closed.load(Ordering::Acquire) {
                (None, Some(spawned))
            } else if let Some((existing_key, existing_proc)) = pool.get(session_id)
                && *existing_key == key
                && existing_proc.is_alive()
            {
                (Some(existing_proc.clone()), Some(spawned))
            } else {
                let replaced = pool.insert(session_id.to_owned(), (key, spawned.clone())).map(|(_, p)| p);
                (Some(spawned), replaced)
            }
        };
        if let Some(stale) = to_close {
            stale.close().await;
        }
        selected.ok_or_else(|| HostError::other(l("外部 Agent 服务已关闭。", "External agent manager is closed.")))
    }

    fn effective_configs(&self) -> Vec<ExternalAgentConfig> {
        let stored = self.settings.get().external_agents;
        let find = |kind: &str| stored.iter().find(|c| c.kind == kind).cloned();
        let mut out = vec![
            find("codex").unwrap_or_else(|| default_config("codex")),
            find("antigravity").unwrap_or_else(|| default_config("antigravity")),
        ];
        for cfg in stored {
            if cfg.kind != "codex" && cfg.kind != "antigravity" {
                out.push(cfg);
            }
        }
        out
    }

    fn resolve_launch(&self, backend: &ExternalBackendRef) -> HostResult<(PathBuf, Vec<String>)> {
        let kind = backend.kind.trim().to_ascii_lowercase();
        let configured = self.settings.get().external_agents.into_iter().find(|c| c.kind == kind);
        let cmd = backend
            .binary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| configured.as_ref().and_then(|c| c.binary.as_deref()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned))
            .unwrap_or_else(|| default_command(&kind).to_owned());
        if cmd.is_empty() {
            return Err(HostError::invalid(l(
                "请先在 设置 → 外部 Agent 中配置该 ACP Agent 的可执行命令。",
                "Configure the executable command for this ACP agent in Settings → External Agents first.",
            )));
        }
        let env = external_env();
        let binary = resolve_cli(&cmd, &env).ok_or_else(|| {
            let hint = install_hint(&kind, self.windows).map(|h| format!(" ({h})")).unwrap_or_default();
            HostError::new(
                "external-missing",
                l(
                    &format!("未找到外部 Agent 命令 `{cmd}`。请先在当前执行环境中安装它{hint}。"),
                    &format!("External agent command `{cmd}` was not found. Install it in this execution environment{hint}."),
                ),
            )
        })?;
        let args = if !backend.args.is_empty() {
            backend.args.clone()
        } else if let Some(cfg) = configured.filter(|c| !c.args.is_empty()) {
            cfg.args
        } else {
            default_args(&kind)
        };
        Ok((binary, args))
    }
}

async fn probe_one(cfg: &ExternalAgentConfig, env: &std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>, cwd: &Path) -> CachedProbe {
    let cmd = command_name_for(cfg);
    if cmd.is_empty() {
        return CachedProbe { installed: false, ..CachedProbe::default() };
    }
    let Some(binary) = resolve_cli(&cmd, env) else {
        return CachedProbe { installed: false, error: Some(jsonrpc::explain_missing(&cmd, env)), ..CachedProbe::default() };
    };
    let binary_path = Some(binary.to_string_lossy().into_owned());
    let version = probe_version(&binary, cwd, env).await;
    let cancel = CancellationToken::new();
    let probe_args = if !cfg.args.is_empty() { cfg.args.clone() } else { default_args(&cfg.kind) };

    // `spawn` enforces its own 15s handshake timeout and always calls `rpc.close().await` on failure,
    // so we never drop an in-flight `spawn` future without cleaning up its child process group.
    let (models, error) = if cfg.kind == "codex" {
        match CodexProcess::spawn(&binary, &probe_args, cwd).await {
            Ok(proc) => {
                let outcome = tokio::time::timeout(PROBE_MODELS_TIMEOUT, proc.list_models(&cancel)).await;
                proc.close().await;
                match outcome {
                    Ok(Ok(models)) => (models, None),
                    Ok(Err(err)) => (Vec::new(), Some(err.message)),
                    Err(_) => (
                        Vec::new(),
                        Some(l("Codex 获取模型列表超时，请确认已运行 `codex login` 完成登录。", "Codex model list timed out; verify login with `codex login`.")),
                    ),
                }
            }
            Err(err) => (Vec::new(), Some(err.message)),
        }
    } else {
        // Antigravity lists its models without the empty conversation a probe over ACP leaves in its history.
        let listed = if cfg.kind == "antigravity" { list_agy_models(&binary, cwd, env).await.ok() } else { None };
        match listed {
            Some(models) => (models, None),
            None => match AcpProcess::spawn(&cfg.kind, &binary, &probe_args, cwd).await {
                Ok(proc) => {
                    let outcome = tokio::time::timeout(PROBE_MODELS_TIMEOUT, proc.probe_session(cwd, &cancel)).await;
                    proc.close().await;
                    match outcome {
                        Ok(Ok(meta)) => (meta.models, None),
                        Ok(Err(err)) => (Vec::new(), Some(err.message)),
                        Err(_) => (
                            Vec::new(),
                            Some(l("外部 ACP Agent 探测超时，请确认已完成登录。", "External ACP probe timed out; verify CLI login.")),
                        ),
                    }
                }
                Err(err) => (Vec::new(), Some(err.message)),
            },
        }
    };

    CachedProbe { installed: true, binary_path, version, models, error }
}

/// The models `agy models` lists (it opens no conversation), or what it said instead.
async fn list_agy_models(binary: &Path, cwd: &Path, env: &std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>) -> Result<Vec<ExternalModelInfo>, String> {
    let output = crate::process::run(binary, &["models"], cwd, env, 256 * 1024, Some(AGY_MODELS_TIMEOUT))
        .await
        .map_err(|error| l(&format!("无法运行 agy 列出模型：{error}"), &format!("Could not run agy to list the models: {error}")))?;
    let text = crate::process::console_text(&output.stdout);
    let models = acp::parse_agy_models(&text);
    if !models.is_empty() {
        return Ok(models);
    }
    // The progress line comes first on stderr, an error after it.
    let errors = crate::process::console_text(&output.stderr);
    let said: String = text.lines().map(str::trim).find(|line| !line.is_empty()).or_else(|| errors.lines().map(str::trim).filter(|line| !line.is_empty()).last()).unwrap_or_default().chars().take(160).collect();
    Err(l(&format!("agy 没有列出模型：{said}"), &format!("agy listed no models: {said}")))
}

async fn probe_version(
    binary: &Path,
    cwd: &Path,
    env: &std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>,
) -> Option<String> {
    let out = crate::process::run(binary, &["--version"], cwd, env, 4096, Some(VERSION_TIMEOUT)).await.ok()?;
    let text = if !out.stdout.is_empty() {
        crate::process::console_text(&out.stdout)
    } else {
        crate::process::console_text(&out.stderr)
    };
    text.lines().map(str::trim).find(|line| !line.is_empty()).map(|line| line.chars().take(80).collect())
}

fn default_config(kind: &str) -> ExternalAgentConfig {
    ExternalAgentConfig {
        kind: kind.to_owned(),
        enabled: true,
        binary: None,
        args: default_args(kind),
        label: None,
        default_model: None,
        default_thinking: None,
    }
}

fn default_command(kind: &str) -> &'static str {
    match kind {
        "codex" => "codex",
        "antigravity" => "agy",
        _ => "",
    }
}

fn default_args(kind: &str) -> Vec<String> {
    match kind {
        "antigravity" => vec!["acp".to_owned()],
        _ => Vec::new(),
    }
}

fn command_name_for(cfg: &ExternalAgentConfig) -> String {
    cfg.binary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| default_command(&cfg.kind).to_owned())
}

fn display_label(cfg: &ExternalAgentConfig) -> String {
    if let Some(label) = cfg.label.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return label.to_owned();
    }
    match cfg.kind.as_str() {
        "codex" => "Codex".into(),
        "antigravity" => "Antigravity".into(),
        other => other.to_owned(),
    }
}

fn install_hint(kind: &str, windows: bool) -> Option<String> {
    match kind {
        "codex" => Some("npm i -g @openai/codex".into()),
        "antigravity" if windows => Some("irm https://antigravity.google/cli/install.ps1 | iex".into()),
        "antigravity" => Some("curl -fsSL https://antigravity.google/cli/install.sh | bash".into()),
        _ => None,
    }
}

fn login_hint(kind: &str, windows: bool) -> Option<String> {
    match kind {
        "codex" => Some("codex login".into()),
        "antigravity" if windows => Some("agy acp --login".into()),
        "antigravity" => Some("agy acp --login".into()),
        _ => None,
    }
}

pub fn is_valid_kind(kind: &str) -> bool {
    let mut chars = kind.chars();
    (1..=32).contains(&kind.len())
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn check_binary_str(bin: &str) -> HostResult<()> {
    if bin.chars().count() > 512 || bin.chars().any(|c| c == '\0' || c == '\n' || c == '\r') {
        return Err(HostError::invalid(l("外部 Agent 命令路径无效。", "External agent binary path is invalid.")));
    }
    Ok(())
}

fn check_args_slice(args: &[String]) -> HostResult<()> {
    if args.len() > 32 || args.iter().any(|a| a.chars().count() > 512 || a.contains('\0')) {
        return Err(HostError::invalid(l("外部 Agent 启动参数无效。", "External agent arguments are invalid.")));
    }
    Ok(())
}

/// Validate a persisted [`ExternalAgentConfig`].
pub fn check_external_agent(config: &ExternalAgentConfig) -> HostResult<()> {
    if !is_valid_kind(&config.kind) {
        return Err(HostError::invalid(l(
            "外部 Agent 标识只能包含 1–32 个小写字母、数字或连字符。",
            "External agent kind must be 1–32 lowercase letters, digits or hyphens.",
        )));
    }
    if let Some(bin) = &config.binary {
        check_binary_str(bin)?;
    }
    check_args_slice(&config.args)?;
    if config.kind != "codex" && config.kind != "antigravity" && config.binary.as_deref().map(str::trim).unwrap_or("").is_empty() {
        return Err(HostError::invalid(l("自定义 ACP Agent 必须指定可执行命令。", "A custom ACP agent must specify a binary command.")));
    }
    Ok(())
}

/// Validate an [`ExternalBackendRef`] passed to `sessions.create`.
pub fn check_backend_ref(backend: &ExternalBackendRef) -> HostResult<ExternalBackendRef> {
    let kind = backend.kind.trim().to_ascii_lowercase();
    if !is_valid_kind(&kind) {
        return Err(HostError::invalid(l("外部 Agent 类型无效。", "Invalid external backend kind.")));
    }
    if let Some(bin) = &backend.binary {
        check_binary_str(bin)?;
    }
    check_args_slice(&backend.args)?;
    Ok(ExternalBackendRef {
        kind,
        binary: backend.binary.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned),
        args: backend.args.clone(),
        label: backend.label.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned),
    })
}

/// Canonical provider string stored in [`ModelSelection`] for an external backend (`"external:<kind>"`).
pub fn external_provider_id(kind: &str) -> String {
    format!("external:{kind}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_rejects_invalid_kind_binary_and_args() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Arc::new(Settings::load(dir.path(), None).unwrap());
        let mgr = ExternalManager::new(settings, cfg!(windows));
        assert!(mgr.probe(Some("INVALID KIND!"), None, None, dir.path()).await.is_err());
        assert!(mgr.probe(Some("codex"), Some("bad\nbin"), None, dir.path()).await.is_err());
        let bad_args = vec!["arg\0bad".to_owned()];
        assert!(mgr.probe(Some("codex"), None, Some(&bad_args), dir.path()).await.is_err());
        assert!(mgr.probe(Some("custom-acp"), None, None, dir.path()).await.is_err());
        mgr.close().await;
    }

    /// The model menu lists an agent's models after a restart, from what its last probe or chat listed.
    #[tokio::test]
    async fn listed_models_outlive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Arc::new(Settings::load(dir.path(), None).unwrap());
        let models = vec![ExternalModelInfo {
            id: "gemini-4-neon".into(),
            display_name: Some("Gemini 4 Neon".into()),
            reasoning_efforts: vec!["low".into(), "high".into()],
            default_effort: Some("high".into()),
            is_default: Some(true),
        }];
        let mgr = ExternalManager::new(settings.clone(), cfg!(windows));
        let agy = |mgr: &ExternalManager| mgr.status().agents.into_iter().find(|agent| agent.kind == "antigravity").unwrap();
        assert!(agy(&mgr).models.is_empty());
        assert!(mgr.remember_models("antigravity", models.clone()), "news");
        assert!(!mgr.remember_models("antigravity", models.clone()), "nothing new");
        assert!(!mgr.remember_models("antigravity", Vec::new()), "an empty list forgets nothing");
        assert_eq!(agy(&mgr).models, models);
        mgr.close().await;
        let again = ExternalManager::new(settings, cfg!(windows));
        assert_eq!(agy(&again).models, models);
        again.close().await;
    }

    /// An `agy` that notes each run in `calls.txt` and prints `listed`.
    async fn fake_agy(dir: &Path, listed: &str) -> (Arc<ExternalManager>, PathBuf) {
        let calls = dir.join("calls.txt");
        let output = dir.join("listed.txt");
        std::fs::write(&output, listed).unwrap();
        let script = if cfg!(windows) {
            let script = dir.join("agy.cmd");
            std::fs::write(&script, format!("@echo off\r\necho run>>\"{}\"\r\ntype \"{}\"\r\n", calls.display(), output.display())).unwrap();
            script
        } else {
            let script = dir.join("agy");
            std::fs::write(&script, format!("#!/bin/sh\necho run >> '{}'\ncat '{}'\n", calls.display(), output.display())).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            script
        };
        let settings = Arc::new(Settings::load(dir, None).unwrap());
        let mgr = ExternalManager::new(settings, cfg!(windows));
        let config = ExternalAgentConfig { binary: Some(script.to_string_lossy().into_owned()), ..default_config("antigravity") };
        mgr.configure(config, false).await.unwrap();
        (mgr, calls)
    }

    fn runs(calls: &Path) -> usize {
        std::fs::read_to_string(calls).map(|text| text.lines().count()).unwrap_or(0)
    }

    /// Until Antigravity's models are known, `agy models` lists them (no conversation), at most once a minute.
    #[tokio::test]
    async fn lists_antigravity_models_while_none_are_known() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, calls) = fake_agy(dir.path(), "gemini-4-neon-high\tGemini 4 Neon (High)\ngemini-4-neon-low\tGemini 4 Neon (Low)\n").await;
        assert!(!mgr.list_models_if_unknown("custom-acp", dir.path()).await, "only Codex and Antigravity");
        assert!(mgr.list_models_if_unknown("antigravity", dir.path()).await);
        let agy = mgr.status().agents.into_iter().find(|agent| agent.kind == "antigravity").unwrap();
        assert_eq!(agy.models.len(), 1);
        assert_eq!(agy.models[0].id, "gemini-4-neon");
        assert_eq!(agy.models[0].reasoning_efforts, ["low", "high"]);
        assert!(!mgr.list_models_if_unknown("antigravity", dir.path()).await, "known now");
        assert_eq!(runs(&calls), 1);
        mgr.close().await;

        let dir = tempfile::tempdir().unwrap();
        let (mgr, calls) = fake_agy(dir.path(), "Error: not signed in\n").await;
        assert!(!mgr.list_models_if_unknown("antigravity", dir.path()).await);
        assert!(!mgr.list_models_if_unknown("antigravity", dir.path()).await);
        assert_eq!(runs(&calls), 1, "not asked again within a minute");
        assert!(mgr.status().agents.into_iter().find(|agent| agent.kind == "antigravity").unwrap().models.is_empty());
        mgr.close().await;
    }
}
