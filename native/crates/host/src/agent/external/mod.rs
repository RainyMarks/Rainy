//! External CLI agent harness (`codex app-server --stdio` and ACP servers such as `agy acp`).
//!
//! Runs entirely inside `rainy-host` over child process stdio JSON-RPC with zero extra runtime
//! dependencies, so Windows, WSL and SSH targets all support external agents uniformly.

pub mod acp;
pub mod codex;
pub mod jsonrpc;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rainy_protocol::chat::{
    ExternalAgentConfig, ExternalAgentStatus, ExternalBackendRef, ExternalModelInfo, ExternalStatusResponse, PendingApproval, UsageSummary,
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
    UsageUpdated(UsageSummary),
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
    cache: Mutex<HashMap<String, CachedProbe>>,
    codex_pool: AsyncMutex<HashMap<ProcessKey, Arc<CodexProcess>>>,
    acp_pool: AsyncMutex<HashMap<String, (ProcessKey, Arc<AcpProcess>)>>,
    stop_reaper: CancellationToken,
}

impl ExternalManager {
    pub fn new(settings: Arc<Settings>, windows: bool) -> Arc<Self> {
        let manager = Arc::new(Self {
            settings,
            windows,
            cache: Mutex::new(HashMap::new()),
            codex_pool: AsyncMutex::new(HashMap::new()),
            acp_pool: AsyncMutex::new(HashMap::new()),
            stop_reaper: CancellationToken::new(),
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
                        (
                            resolved.is_some(),
                            resolved.map(|p| p.to_string_lossy().into_owned()),
                            None,
                            Vec::new(),
                            None,
                        )
                    }
                };
                ExternalAgentStatus {
                    label: display_label(&cfg),
                    install_hint: install_hint(&cfg.kind, self.windows),
                    login_hint: login_hint(&cfg.kind, self.windows),
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
                        "外部代理标识只能包含 1–32 个小写字母、数字或连字符。",
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
            return Err(HostError::invalid(l("外部代理标识无效。", "Invalid external agent kind.")));
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

    /// Release any per-session external process when a Rainy chat is closed or deleted.
    pub async fn close_session(&self, session_id: &str) {
        let removed = self.acp_pool.lock().await.remove(session_id);
        if let Some((_, process)) = removed {
            process.close().await;
        }
    }

    /// Shut down the reaper and terminate every running external agent subprocess cleanly.
    pub async fn close(&self) {
        self.stop_reaper.cancel();
        let codex_list: Vec<Arc<CodexProcess>> = self.codex_pool.lock().await.drain().map(|(_, p)| p).collect();
        let acp_list: Vec<Arc<AcpProcess>> = self.acp_pool.lock().await.drain().map(|(_, (_, p))| p).collect();
        futures::future::join_all(codex_list.iter().map(|p| p.close())).await;
        futures::future::join_all(acp_list.iter().map(|p| p.close())).await;
    }

    async fn reap_idle(&self) {
        let mut stale_codex = Vec::new();
        {
            let mut pool = self.codex_pool.lock().await;
            pool.retain(|_, proc| {
                if !proc.is_alive() || proc.idle_for() >= IDLE_REAP_AFTER {
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
            pool.retain(|_, (_, proc)| {
                if !proc.is_alive() || proc.idle_for() >= IDLE_REAP_AFTER {
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

    async fn evict_kind(&self, kind: &str) {
        let mut stale_codex = Vec::new();
        {
            let mut pool = self.codex_pool.lock().await;
            pool.retain(|k, proc| {
                if k.kind == kind {
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
                if k.kind == kind {
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

    async fn get_or_spawn_codex(&self, key: ProcessKey, args: &[String]) -> HostResult<Arc<CodexProcess>> {
        if let Some(existing) = self.codex_pool.lock().await.get(&key).filter(|proc| proc.is_alive()).cloned() {
            return Ok(existing);
        }
        // Spawn and complete the handshake outside the pool lock so other workspaces never stall.
        let spawned = CodexProcess::spawn(&key.binary, args, &key.cwd).await?;
        let existing = {
            let mut pool = self.codex_pool.lock().await;
            if let Some(existing) = pool.get(&key).filter(|proc| proc.is_alive()).cloned() {
                Some(existing)
            } else {
                pool.insert(key, spawned.clone());
                None
            }
        };
        if let Some(existing) = existing {
            spawned.close().await;
            return Ok(existing);
        }
        Ok(spawned)
    }

    async fn get_or_spawn_acp(&self, session_id: &str, key: ProcessKey, args: &[String]) -> HostResult<Arc<AcpProcess>> {
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
        self.acp_pool.lock().await.insert(session_id.to_owned(), (key, spawned.clone()));
        Ok(spawned)
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
                "请先在 设置 → 外部 Agent 中配置该 ACP 代理的可执行命令。",
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
        return CachedProbe { installed: false, ..CachedProbe::default() };
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
        match AcpProcess::spawn(&cfg.kind, &binary, &probe_args, cwd).await {
            Ok(proc) => {
                let outcome = tokio::time::timeout(PROBE_MODELS_TIMEOUT, proc.probe_session(cwd, &cancel)).await;
                proc.close().await;
                match outcome {
                    Ok(Ok(meta)) => (meta.models, None),
                    Ok(Err(err)) => (Vec::new(), Some(err.message)),
                    Err(_) => (
                        Vec::new(),
                        Some(l("外部 ACP 代理探测超时，请确认已完成登录。", "External ACP probe timed out; verify CLI login.")),
                    ),
                }
            }
            Err(err) => (Vec::new(), Some(err.message)),
        }
    };

    CachedProbe { installed: true, binary_path, version, models, error }
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
        return Err(HostError::invalid(l("外部代理命令路径无效。", "External agent binary path is invalid.")));
    }
    Ok(())
}

fn check_args_slice(args: &[String]) -> HostResult<()> {
    if args.len() > 32 || args.iter().any(|a| a.chars().count() > 512 || a.contains('\0')) {
        return Err(HostError::invalid(l("外部代理启动参数无效。", "External agent arguments are invalid.")));
    }
    Ok(())
}

/// Validate a persisted [`ExternalAgentConfig`].
pub fn check_external_agent(config: &ExternalAgentConfig) -> HostResult<()> {
    if !is_valid_kind(&config.kind) {
        return Err(HostError::invalid(l(
            "外部代理标识只能包含 1–32 个小写字母、数字或连字符。",
            "External agent kind must be 1–32 lowercase letters, digits or hyphens.",
        )));
    }
    if let Some(bin) = &config.binary {
        check_binary_str(bin)?;
    }
    check_args_slice(&config.args)?;
    if config.kind != "codex" && config.kind != "antigravity" && config.binary.as_deref().map(str::trim).unwrap_or("").is_empty() {
        return Err(HostError::invalid(l("自定义 ACP 代理必须指定可执行命令。", "A custom ACP agent must specify a binary command.")));
    }
    Ok(())
}

/// Validate an [`ExternalBackendRef`] passed to `sessions.create`.
pub fn check_backend_ref(backend: &ExternalBackendRef) -> HostResult<ExternalBackendRef> {
    let kind = backend.kind.trim().to_ascii_lowercase();
    if !is_valid_kind(&kind) {
        return Err(HostError::invalid(l("外部代理类型无效。", "Invalid external backend kind.")));
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
}
