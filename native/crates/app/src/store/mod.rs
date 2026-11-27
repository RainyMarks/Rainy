//! Application state kept current from the Host: connection, preferences, models, chat list and open chats.

pub mod chat;
pub mod diagnostics;
pub mod execution;
pub mod ide;
pub mod language;
pub mod modules;
pub mod shell;
pub mod strata;
pub mod update;

mod lifecycle;
mod reconnect;
mod sessions;
mod teams;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use gpui_kit::*;
use rainy_carrier::askpass::{Askpass, Question};
use rainy_carrier::ssh_tunnel::{PortForwardRule, TunnelManager, TunnelState, TunnelStatus};
use rainy_carrier::targets::SshHostProfile;
use rainy_carrier::trust::{Integrity, TrustError};
use rainy_carrier::{ConnectionState, ExecutionTarget, HostProcess, RpcClient};
use rainy_protocol::{
    AppInfo, ExternalAgentStatus, ExternalBackendRef, ExternalTeam, HostMetrics, ModelSelection, ModelsStatus, RpcError, SessionSummary, UiPreferences,
};
use serde_json::Value;

use crate::boot::{self, Paths};
use crate::i18n::Locale;
use chat::ChatView;
use sessions::PendingChatEvent;

/// Where startup stands.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    /// The step under way and, when known, how much of it is done (0 to 1).
    Starting(SharedString, Option<f32>),
    Ready,
    Failed(SharedString),
}

/// One chat the interface follows.
pub struct ChatModel {
    pub id: String,
    pub view: Option<ChatView>,
    pub error: Option<SharedString>,
    /// The whole chat was asked for; later reloads (after a reconnect) keep loading all of it.
    pub whole: bool,
    /// A load of the whole chat is under way.
    pub loading_whole: bool,
    /// Bumped each time a load replaces `view`, so views can tell a new snapshot from an update of the old one.
    pub loaded: u64,
    pub(super) load: u64,
    /// Real-time events received while `view` is still `None` (initial `sessions.get` in flight), replayed on arrival.
    pending_events: Vec<PendingChatEvent>,
}

impl ChatModel {
    pub fn busy(&self) -> bool {
        self.view.as_ref().is_some_and(ChatView::busy)
    }
}

/// Silent SSH reconnect backoff state (`1s -> 3s -> 8s`, 3 attempts before alerting).
#[derive(Debug, Clone, PartialEq)]
pub enum SshAutoReconnect {
    Idle,
    Waiting {
        attempt: u8,
        max: u8,
        retry_at: Instant,
        last_error: Option<String>,
    },
    Attempting {
        attempt: u8,
        max: u8,
    },
    Exhausted {
        last_error: String,
    },
}

/// Backoff delays (in seconds) between automatic SSH reconnect attempts.
pub(super) const SSH_RECONNECT_BACKOFFS_SECS: [u64; 3] = [1, 3, 8];

/// What changed, for views that only care about part of the state.
#[derive(Clone)]
pub enum StoreEvent {
    PrefsChanged,
    SummariesChanged,
    SelectionChanged,
    /// The active Host was replaced or reconnected in place; every subsystem (IDE, LSP, Execution, WebPane) refreshes.
    HostReconnected(RpcClient),
    /// A Host event other views follow (`ide.hint`, `memory.changed`, …).
    Host { name: String, data: Value },
    /// The Host ended while the interface was using it; the exit code, if it had one.
    HostExited(Option<i32>),
    /// The daily recheck found installed files changed or damaged.
    ResourcesDamaged(TrustError),
    /// Something about an update to show.
    Update(update::UpdateNotice),
    /// Windows DPAPI credential storage recovered from corruption or could not protect keys.
    CredentialAlert(boot::CredentialAlert),
    /// The team an external agent runs for this chat changed (`external.team`).
    TeamChanged(String),
}

/// A question `ssh` asks, waiting for the window to show it; dropping `reply` cancels it.
pub struct AskpassRequest {
    pub question: Question,
    pub reply: mpsc::Sender<Option<String>>,
}

/// How long quitting waits for the editor's unsaved text to be stored.
pub(super) const QUIT_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a component operation may take to stop at its next chunk.
const MODULES_TIMEOUT: Duration = Duration::from_secs(10);

/// What still has to stop when Rainy quits, restarts or hands over to an installer, taken out of the store
/// ([`Store::shutdown`]) so the waiting happens off the interface thread.
pub struct Shutdown {
    pub(super) host: Option<Arc<HostProcess>>,
    pub(super) modules: Arc<rainy_carrier::modules::Modules>,
    pub(super) strata: Option<Arc<rainy_carrier::strata::Manager>>,
    pub(super) tunnels: Arc<TunnelManager>,
}

impl Shutdown {
    /// Stop the Host (asked first, killed after its grace period), active SSH port-forwarding tunnels, and the Strata
    /// server Rainy started at the same time, and wait for a component operation to stop at its next chunk; whether
    /// none still runs. Blocks for up to about ten seconds.
    pub fn finish(self) -> bool {
        let stop = |name: &str, run: Box<dyn FnOnce() + Send>| std::thread::Builder::new().name(name.to_owned()).spawn(run).ok();
        let tunnels = stop("stop-tunnels", Box::new(move || self.tunnels.stop_all()));
        let host = self.host.and_then(|host| stop("stop-host", Box::new(move || host.stop())));
        // The model server Rainy started does not outlive it; a server it did not start is left running.
        let strata = self.strata.and_then(|strata| stop("stop-strata", Box::new(move || strata.close())));
        let idle = self.modules.wait_idle(MODULES_TIMEOUT);
        for thread in [tunnels, host, strata].into_iter().flatten() {
            let _ = thread.join();
        }
        idle
    }
}

/// Hide every window of Rainy at once, before what quitting waits for. Asynchronous, so it is safe in any update: the
/// windows go when the message loop next runs.
pub fn hide_windows() {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible, SW_HIDE, ShowWindowAsync};
    unsafe extern "system" fn hide(window: HWND, _: LPARAM) -> i32 {
        let mut process = 0;
        // SAFETY: plain queries and an asynchronous show request on a window handle Windows enumerated.
        unsafe {
            GetWindowThreadProcessId(window, &mut process);
            if process == std::process::id() && IsWindowVisible(window) != 0 {
                ShowWindowAsync(window, SW_HIDE);
            }
        }
        1
    }
    // SAFETY: the callback only reads the handles it is given.
    unsafe { EnumWindows(Some(hide), 0) };
}

/// The Linux runtime could not be downloaded during startup: why, and where the user's choice goes. Dropping `reply`
/// quits, as closing 3.x's dialog did.
pub struct RuntimePrompt {
    pub error: String,
    pub reply: mpsc::Sender<boot::RuntimeChoice>,
}

fn start_askpass(questions_tx: async_channel::Sender<AskpassRequest>) -> Option<Arc<Askpass>> {
    // `ssh` starts Rainy.exe itself as its askpass program, which hands the question to this process.
    std::env::current_exe().ok().and_then(|exe| {
        let ask = move |question: Question| {
            let (reply, answer) = mpsc::channel();
            let open = question.open.clone();
            questions_tx.send_blocking(AskpassRequest { question, reply }).ok()?;
            // The `ssh` run may end while the window asks; the window then closes the question (`ui::ssh`).
            loop {
                match answer.recv_timeout(Duration::from_millis(250)) {
                    Ok(answer) => return answer,
                    Err(mpsc::RecvTimeoutError::Timeout) if open.load(std::sync::atomic::Ordering::SeqCst) => {}
                    Err(_) => return None,
                }
            }
        };
        Askpass::start(exe, Arc::new(ask)).inspect_err(|error| log::warn!("SSH questions cannot be answered: {error:#}")).ok()
    })
}

pub struct Store {
    pub paths: Paths,
    pub phase: Phase,
    pub connection: ConnectionState,
    pub rpc: Option<RpcClient>,
    pub(super) host: Option<Arc<HostProcess>>,
    /// The installed resources as startup accepted them, for the daily recheck; `None` when this run checks none.
    pub(super) integrity: Option<Arc<Integrity>>,
    /// The Host's start left this for the background maintenance.
    pub(super) prune: Option<boot::Prune>,
    pub(super) maintenance_scheduled: bool,
    /// Where the Host runs; known once startup has read the saved choice.
    pub target: Option<ExecutionTarget>,
    /// Answers the questions `ssh` asks, in this window; started lazily on demand so local Windows startup spawns no extra listener.
    pub askpass: Option<Arc<Askpass>>,
    askpass_tx: async_channel::Sender<AskpassRequest>,
    /// Questions waiting for the window.
    pub questions: async_channel::Receiver<AskpassRequest>,
    /// Failed Linux runtime downloads waiting for the window to ask what to do.
    pub runtime_prompts: async_channel::Receiver<RuntimePrompt>,
    pub(super) prompt: async_channel::Sender<RuntimePrompt>,
    /// Optional components: PHP in Settings, the Linux runtime base for WSL and SSH.
    pub modules: modules::OptionalModules,
    /// The Strata local model engine, once the modules are read.
    pub strata: strata::Strata,
    pub(super) stopping: bool,
    /// Quitting or restarting gracefully ([`Store::quit`], [`Store::restart`]) is under way.
    pub(super) quitting: bool,
    pub info: Option<AppInfo>,
    pub prefs: UiPreferences,
    pub models: Option<ModelsStatus>,
    /// Detected & configured external CLI agents (`codex`, `antigravity`, custom `acp`).
    pub external_agents: Vec<ExternalAgentStatus>,
    /// External agent chosen for the next new chat (`None` uses built-in models).
    pub pending_external: Option<ExternalBackendRef>,
    /// Model & reasoning selection for the next new external-agent chat.
    pub pending_external_model: Option<ModelSelection>,
    /// The teams external agents run, by chat id, with every board post read so far.
    pub teams: HashMap<String, ExternalTeam>,
    pub(super) team_fetch_seq: HashMap<String, u64>,
    pub(super) team_inflight: HashMap<String, Option<u64>>,
    pub(super) team_used: HashMap<String, u64>,
    pub(super) team_tick: u64,
    pub summaries: Vec<SessionSummary>,
    pub chats: HashMap<String, Entity<ChatModel>>,
    /// Monotonic access counter per cached chat for bounded LRU eviction.
    pub(super) chat_used: HashMap<String, u64>,
    pub(super) chat_tick: u64,
    pub selected: Option<String>,
    /// Counts the changes of `selected`, so work that finishes later (a chat created for a message) can tell whether
    /// the user moved on meanwhile.
    pub selections: u64,
    /// The permission mode picked for the next new chat; `None` starts it in the default from Settings.
    pub pending_permission: Option<String>,
    /// The next new chat of a project works in its own Git worktree.
    pub pending_worktree: bool,
    /// The executor model the next new chat is orchestrated with (`chat.setOrchestrate`).
    pub pending_orchestrate: Option<ModelSelection>,
    /// Chats whose `sessions.read` is on its way.
    pub(super) reading: HashSet<String>,
    /// Explorer's "Open with Rainy" and the file types Rainy is offered for.
    pub shell: shell::Shell,
    /// Rainy's own updates.
    pub updates: update::Updates,
    /// Exporting the diagnostics zip.
    pub diagnostics: diagnostics::Diagnostics,
    /// Credential protection alerts raised before the window subscribed.
    pub(super) pending_credential_alerts: Vec<boot::CredentialAlert>,
    /// Standalone dynamic SSH `-L` port forwarding manager (never interrupts the primary Host).
    pub tunnels: Arc<TunnelManager>,
    /// Last known snapshot of active SSH port-forwarding rules.
    pub tunnel_status: TunnelStatus,
    /// Latest polled remote hardware/cgroup/GPU/disk metrics snapshot.
    pub host_metrics: Option<HostMetrics>,
    /// Non-blocking silent SSH reconnect state (`1s -> 3s -> 8s`).
    pub ssh_reconnect: SshAutoReconnect,
    /// Signal channel to wake the backoff sleep immediately when the user clicks "Retry now".
    pub(super) ssh_retry_now: Option<async_channel::Sender<()>>,
    /// Background task driving the silent SSH auto-reconnect loop.
    pub(super) ssh_reconnect_task: Option<Task<()>>,
    /// Target label currently being swapped to in place.
    pub swapping_target: Option<SharedString>,
    /// True while an in-place target swap or SSH reconnect is under way.
    pub(super) reconnecting: bool,
    /// True after `install_connected_host` until the new RPC socket reports `Open` (cookie ready).
    pub(super) pending_reconnected: bool,
    /// Monotonic generation incremented on every Host start/swap to ignore stale exit callbacks.
    pub(super) host_generation: u64,
    /// Active startup task, replaced rather than pushed to `_tasks`.
    pub(super) startup_task: Option<Task<()>>,
    /// Active manual SSH reconnect or in-place target swap task, replaced rather than pushed to `_tasks`.
    pub(super) active_swap_task: Option<Task<()>>,
    /// Fixed set of long-lived once-per-run background tasks (maintenance, shell, updates).
    pub(super) _tasks: Vec<Task<()>>,
}

impl EventEmitter<StoreEvent> for Store {}

struct GlobalStore(Entity<Store>);
impl Global for GlobalStore {}

impl Store {
    /// The application's store.
    pub fn global(cx: &App) -> Entity<Store> {
        cx.global::<GlobalStore>().0.clone()
    }

    pub fn locale(&self) -> Locale {
        Locale::from_code(&self.prefs.locale)
    }

    /// Lazily start the local SSH askpass listener when SSH is actually used (avoids spinning up a listener thread in local Windows mode).
    pub fn ensure_askpass(&mut self) -> Option<Arc<Askpass>> {
        if let Some(existing) = &self.askpass {
            return Some(existing.clone());
        }
        let started = start_askpass(self.askpass_tx.clone());
        if let Some(ref askpass) = started {
            self.askpass = Some(askpass.clone());
            // Rebuild TunnelManager with askpass attached if no tunnel is running yet.
            if matches!(self.tunnel_status.state, TunnelState::Stopped) {
                self.tunnels = Arc::new(TunnelManager::new(Some(askpass.clone())));
            }
        }
        started
    }

    /// Create the store and start the Host in the background.
    pub fn init(paths: Paths, cx: &mut App) -> Entity<Store> {
        let saved_locale = boot::saved_locale(&paths.user_data);
        rainy_carrier::locale::set_chinese(saved_locale == Locale::Zh);
        let starting: SharedString = crate::i18n::text(saved_locale, "native.starting").into();
        let (questions_tx, questions) = async_channel::unbounded::<AskpassRequest>();
        // The saved target is shown while it starts; the Host runs there unless the saved choice is unreadable.
        let target = rainy_carrier::targets::read_preferences(&paths.desktop_settings())
            .ok()
            .map(|preferences| rainy_carrier::targets::saved_target(&preferences).unwrap_or_else(|_| ExecutionTarget::windows()));
        // Start askpass synchronously when the initial target is SSH so `boot::start` can use it immediately;
        // for local Windows/WSL targets, start it off the main thread so cold start is never blocked while
        // keeping `store.askpass` populated for Settings (`ui/settings/runtime.rs`).
        let initial_is_ssh = matches!(target, Some(ExecutionTarget::Ssh { .. }));
        let askpass = if initial_is_ssh {
            start_askpass(questions_tx.clone())
        } else {
            None
        };
        let shell = shell::Shell::new(&paths);
        let updates = update::Updates::new(&paths);
        let (prompt, runtime_prompts) = async_channel::unbounded::<RuntimePrompt>();
        let (module_tx, module_states) = async_channel::unbounded::<rainy_carrier::modules::State>();
        let manager = Arc::new(rainy_carrier::modules::Modules::new(&paths.resources, &paths.user_data, move |state| {
            let _ = module_tx.try_send(state.clone());
        }));
        let tunnels = Arc::new(TunnelManager::new(askpass.clone()));
        let tunnel_status = TunnelStatus { target: None, rules: Vec::new(), state: TunnelState::Stopped };
        let store = cx.new(|cx| {
            let mut store = Store {
                paths,
                phase: Phase::Starting(starting, None),
                connection: ConnectionState::Connecting,
                rpc: None,
                host: None,
                integrity: None,
                prune: None,
                maintenance_scheduled: false,
                target,
                askpass,
                askpass_tx: questions_tx,
                questions,
                runtime_prompts,
                prompt,
                modules: modules::OptionalModules::new(manager),
                strata: strata::Strata::default(),
                stopping: false,
                quitting: false,
                info: None,
                prefs: UiPreferences::default(),
                models: None,
                external_agents: Vec::new(),
                pending_external: None,
                pending_external_model: None,
                teams: HashMap::new(),
                team_fetch_seq: HashMap::new(),
                team_inflight: HashMap::new(),
                team_used: HashMap::new(),
                team_tick: 0,
                summaries: Vec::new(),
                chats: HashMap::new(),
                chat_used: HashMap::new(),
                chat_tick: 0,
                selected: None,
                selections: 0,
                pending_permission: None,
                pending_worktree: false,
                pending_orchestrate: None,
                reading: HashSet::new(),
                shell,
                updates,
                diagnostics: diagnostics::Diagnostics::default(),
                pending_credential_alerts: Vec::new(),
                tunnels,
                tunnel_status,
                host_metrics: None,
                ssh_reconnect: SshAutoReconnect::Idle,
                ssh_retry_now: None,
                ssh_reconnect_task: None,
                swapping_target: None,
                reconnecting: false,
                pending_reconnected: false,
                host_generation: 0,
                startup_task: None,
                active_swap_task: None,
                _tasks: Vec::new(),
            };
            cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
                while let Ok(state) = module_states.recv().await {
                    if this.update(cx, |store, cx| store.module_state(state, cx)).is_err() {
                        break;
                    }
                }
            })
            .detach();
            store.start(cx);
            if !initial_is_ssh {
                let tx = store.askpass_tx.clone();
                let bg = cx.background_spawn(async move { start_askpass(tx) });
                cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
                    if let Some(started) = bg.await {
                        let _ = this.update(cx, |store, _| {
                            if !store.stopping && !store.quitting && store.askpass.is_none() {
                                store.askpass = Some(started.clone());
                                store.tunnels = Arc::new(TunnelManager::new(Some(started)));
                            }
                        });
                    }
                })
                .detach();
            }
            store
        });
        cx.set_global(GlobalStore(store.clone()));
        store
    }

    /// Drain any credential protection alerts recorded during early startup before the window subscribed.
    pub fn take_credential_alerts(&mut self) -> Vec<boot::CredentialAlert> {
        std::mem::take(&mut self.pending_credential_alerts)
    }

    /// The Host's launch address, which exchanges its token for the session cookie; pages in WebView2 start there.
    pub fn launch_url(&self) -> Option<String> {
        self.host.as_ref().map(|host| host.ready().url.clone())
    }

    /// Whether the active execution target is remote (SSH or WSL).
    pub fn is_remote(&self) -> bool {
        matches!(self.target, Some(ExecutionTarget::Ssh { .. } | ExecutionTarget::Wsl { .. }))
    }

    /// Whether the active execution target is an SSH server.
    pub fn is_ssh(&self) -> bool {
        matches!(self.target, Some(ExecutionTarget::Ssh { .. }))
    }

    /// Research profile configured for the currently active SSH target, if any.
    pub fn current_ssh_profile(&self) -> Option<SshHostProfile> {
        let target = self.target.as_ref()?;
        boot::ssh_profile(&self.paths, target)
    }

    /// Clear any pending external-agent selection so the next new chat uses built-in models.
    pub fn clear_pending_external(&mut self, cx: &mut Context<Self>) {
        if self.pending_external.is_some() || self.pending_external_model.is_some() {
            self.pending_external = None;
            self.pending_external_model = None;
            self.notify_composer(cx);
            cx.notify();
        }
    }

    /// Notify composer pickers subscribed to `StoreEvent` without requiring broad `cx.observe(&store)`.
    pub(crate) fn notify_composer(&self, cx: &mut Context<Self>) {
        cx.emit(StoreEvent::Host { name: "composer.pending".to_owned(), data: Value::Null });
    }

    /// Apply a new set of dynamic `-L` port-forwarding rules for the active SSH target without interrupting the Host.
    pub fn apply_tunnel_rules(
        &mut self,
        rules: Vec<PortForwardRule>,
        cx: &mut Context<Self>,
    ) -> Task<Result<TunnelStatus, String>> {
        let Some(target @ ExecutionTarget::Ssh { .. }) = self.target.clone() else {
            return cx.spawn(async move |_, _| Err("not-ssh".into()));
        };
        let destination = match rainy_carrier::ssh::destination_of(&target) {
            Ok(dest) => dest,
            Err(error) => {
                let msg = format!("{error:#}");
                return cx.spawn(async move |_, _| Err(msg));
            }
        };
        let _ = self.ensure_askpass();
        let tunnels = self.tunnels.clone();
        let profile = self.current_ssh_profile();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let result = cx
                .background_spawn(async move { tunnels.apply_rules(&destination, rules, profile.as_ref()) })
                .await;
            match result {
                Ok(status) => {
                    let _ = this.update(cx, |store, cx| {
                        store.tunnel_status = status.clone();
                        cx.notify();
                    });
                    Ok(status)
                }
                Err(error) => {
                    let msg = format!("{error:#}");
                    let _ = this.update(cx, |store, cx| {
                        let mut st = store.tunnels.status();
                        st.state = TunnelState::Error(msg.clone());
                        store.tunnel_status = st;
                        cx.notify();
                    });
                    Err(msg)
                }
            }
        })
    }

    /// Start any `auto_start` port-forwarding rules configured in the active SSH host's profile.
    pub fn start_auto_tunnels(&mut self, cx: &mut Context<Self>) {
        let Some(profile) = self.current_ssh_profile() else { return };
        let auto_rules: Vec<PortForwardRule> = profile.forwards.into_iter().filter(|rule| rule.auto_start).collect();
        if auto_rules.is_empty() {
            return;
        }
        self.apply_tunnel_rules(auto_rules, cx).detach();
    }

    /// Call a Host method; fails at once while the Host is not connected.
    pub fn call(&self, method: &str, params: Value) -> impl Future<Output = Result<Value, RpcError>> + use<> {
        let call = self.rpc.as_ref().map(|rpc| rpc.call(method, params));
        let disconnected = crate::i18n::text(self.locale(), "app.connectionLost");
        async move {
            match call {
                Some(call) => call.await,
                None => Err(RpcError { code: "disconnected".into(), message: disconnected.into(), data: None }),
            }
        }
    }
}
