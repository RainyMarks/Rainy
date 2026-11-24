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

use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use gpui_kit::*;
use rainy_carrier::askpass::{Askpass, Question};
use rainy_carrier::ssh_tunnel::{PortForwardRule, TunnelManager, TunnelState, TunnelStatus};
use rainy_carrier::targets::SshHostProfile;
use rainy_carrier::trust::{Integrity, TrustError};
use rainy_carrier::{ConnectionState, HostProcess, RpcClient, RpcEvent};
use rainy_protocol::{
    AppInfo, ExternalAgentStatus, ExternalBackendRef, ExternalStatusResponse, ExternalTeam, HostMetrics, ModelSelection, ModelsStatus, RpcError,
    SessionEntryEvent, SessionSnapshot, SessionStateEvent, SessionSummary, SessionToolEvent, UiPreferences, parse_stream_event,
};
use serde_json::{Value, json};

use crate::boot::{self, CarrierEvents, Paths};
use rainy_carrier::ExecutionTarget;
use crate::i18n::Locale;
use chat::{ChatView, INITIAL_ENTRIES};

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
    load: u64,
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
const SSH_RECONNECT_BACKOFFS_SECS: [u64; 3] = [1, 3, 8];

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
const QUIT_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a component operation may take to stop at its next chunk.
const MODULES_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// What still has to stop when Rainy quits, restarts or hands over to an installer, taken out of the store
/// ([`Store::shutdown`]) so the waiting happens off the interface thread.
pub struct Shutdown {
    host: Option<Arc<HostProcess>>,
    modules: Arc<rainy_carrier::modules::Modules>,
    strata: Option<Arc<rainy_carrier::strata::Manager>>,
    tunnels: Arc<TunnelManager>,
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

pub struct Store {
    pub paths: Paths,
    pub phase: Phase,
    pub connection: ConnectionState,
    pub rpc: Option<RpcClient>,
    host: Option<Arc<HostProcess>>,
    /// The installed resources as startup accepted them, for the daily recheck; `None` when this run checks none.
    integrity: Option<Arc<Integrity>>,
    /// The Host's start left this for the background maintenance.
    prune: Option<boot::Prune>,
    maintenance_scheduled: bool,
    /// Where the Host runs; known once startup has read the saved choice.
    pub target: Option<ExecutionTarget>,
    /// Answers the questions `ssh` asks, in this window.
    pub askpass: Option<Arc<Askpass>>,
    /// Questions waiting for the window.
    pub questions: async_channel::Receiver<AskpassRequest>,
    /// Failed Linux runtime downloads waiting for the window to ask what to do.
    pub runtime_prompts: async_channel::Receiver<RuntimePrompt>,
    prompt: async_channel::Sender<RuntimePrompt>,
    /// Optional components: PHP in Settings, the Linux runtime base for WSL and SSH.
    pub modules: modules::OptionalModules,
    /// The Strata local model engine, once the modules are read.
    pub strata: strata::Strata,
    stopping: bool,
    /// Quitting or restarting gracefully ([`Store::quit`], [`Store::restart`]) is under way.
    quitting: bool,
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
    pub summaries: Vec<SessionSummary>,
    pub chats: HashMap<String, Entity<ChatModel>>,
    /// Monotonic access counter per cached chat for bounded LRU eviction.
    chat_used: HashMap<String, u64>,
    chat_tick: u64,
    pub selected: Option<String>,
    /// Counts the changes of `selected`, so work that finishes later (a chat created for a message) can tell whether
    /// the user moved on meanwhile.
    pub selections: u64,
    /// The permission mode picked for the next new chat; `None` starts it in the default from Settings.
    pub pending_permission: Option<String>,
    /// The next new chat of a project works in its own Git worktree.
    pub pending_worktree: bool,
    /// The executor model the next new chat is orchestrated with (`chat.setOrchestrate`).
    pub pending_orchestrate: Option<rainy_protocol::ModelSelection>,
    /// Chats whose `sessions.read` is on its way.
    reading: std::collections::HashSet<String>,
    /// Explorer's "Open with Rainy" and the file types Rainy is offered for.
    pub shell: shell::Shell,
    /// Rainy's own updates.
    pub updates: update::Updates,
    /// Exporting the diagnostics zip.
    pub diagnostics: diagnostics::Diagnostics,
    /// Credential protection alerts raised before the window subscribed.
    pending_credential_alerts: Vec<boot::CredentialAlert>,
    /// Standalone dynamic SSH `-L` port forwarding manager (never interrupts the primary Host).
    pub tunnels: Arc<TunnelManager>,
    /// Last known snapshot of active SSH port-forwarding rules.
    pub tunnel_status: TunnelStatus,
    /// Latest polled remote hardware/cgroup/GPU/disk metrics snapshot.
    pub host_metrics: Option<HostMetrics>,
    /// Non-blocking silent SSH reconnect state (`1s -> 3s -> 8s`).
    pub ssh_reconnect: SshAutoReconnect,
    /// Signal channel to wake the backoff sleep immediately when the user clicks "Retry now".
    ssh_retry_now: Option<async_channel::Sender<()>>,
    /// Background task driving the silent SSH auto-reconnect loop.
    ssh_reconnect_task: Option<Task<()>>,
    /// Target label currently being swapped to in place.
    pub swapping_target: Option<SharedString>,
    /// True while an in-place target swap or SSH reconnect is under way.
    reconnecting: bool,
    /// True after `install_connected_host` until the new RPC socket reports `Open` (cookie ready).
    pending_reconnected: bool,
    /// Monotonic generation incremented on every Host start/swap to ignore stale exit callbacks.
    host_generation: u64,
    _tasks: Vec<Task<()>>,
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

    /// Create the store and start the Host in the background.
    pub fn init(paths: Paths, cx: &mut App) -> Entity<Store> {
        rainy_carrier::locale::set_chinese(boot::saved_locale(&paths.user_data) == Locale::Zh);
        let starting: SharedString = crate::i18n::text(boot::saved_locale(&paths.user_data), "native.starting").into();
        let (questions_tx, questions) = async_channel::unbounded::<AskpassRequest>();
        // `ssh` starts Rainy.exe itself as its askpass program, which hands the question to this process.
        let askpass = std::env::current_exe().ok().and_then(|exe| {
            let ask = move |question: Question| {
                let (reply, answer) = mpsc::channel();
                let open = question.open.clone();
                questions_tx.send_blocking(AskpassRequest { question, reply }).ok()?;
                // The `ssh` run may end while the window asks; the window then closes the question (`ui::ssh`).
                loop {
                    match answer.recv_timeout(std::time::Duration::from_millis(250)) {
                        Ok(answer) => return answer,
                        Err(mpsc::RecvTimeoutError::Timeout) if open.load(std::sync::atomic::Ordering::SeqCst) => {}
                        Err(_) => return None,
                    }
                }
            };
            Askpass::start(exe, Arc::new(ask)).inspect_err(|error| log::warn!("SSH questions cannot be answered: {error:#}")).ok()
        });
        // The saved target is shown while it starts; the Host runs there unless the saved choice is unreadable.
        let target = rainy_carrier::targets::read_preferences(&paths.desktop_settings())
            .ok()
            .map(|preferences| rainy_carrier::targets::saved_target(&preferences).unwrap_or_else(|_| ExecutionTarget::windows()));
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
                summaries: Vec::new(),
                chats: HashMap::new(),
                chat_used: HashMap::new(),
                chat_tick: 0,
                selected: None,
                selections: 0,
                pending_permission: None,
                pending_worktree: false,
                pending_orchestrate: None,
                reading: Default::default(),
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
            store
        });
        cx.set_global(GlobalStore(store.clone()));
        store
    }

    /// Drain any credential protection alerts recorded during early startup before the window subscribed.
    pub fn take_credential_alerts(&mut self) -> Vec<boot::CredentialAlert> {
        std::mem::take(&mut self.pending_credential_alerts)
    }

    fn wire_carrier_events(&self, cx: &mut Context<Self>) -> (Arc<CarrierEvents>, async_channel::Receiver<Option<i32>>) {
        let events = CarrierEvents::new(&self.paths);
        // A Host that ends after startup is reported once, unless Rainy is quitting.
        let (exit_tx, exit_rx) = async_channel::bounded::<Option<i32>>(1);
        *events.on_exit.lock().unwrap() = Some(Box::new(move |code| {
            let _ = exit_tx.try_send(code);
        }));
        // The agent's preview commands run in the window, one after another.
        let (preview_tx, preview_rx) = async_channel::unbounded::<(Value, mpsc::Sender<Value>)>();
        *events.preview.lock().unwrap() = Some(preview_tx);
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            while let Ok((command, reply)) = preview_rx.recv().await {
                let window = cx.update(|cx| cx.windows().first().copied());
                let answer = match window {
                    Some(window) => crate::web::preview::handle(command, window, cx).await,
                    None => json!({ "error": "The preview window is unavailable." }),
                };
                let _ = reply.send(answer);
            }
        })
        .detach();
        // DPAPI credential warnings/errors surface explicitly in the window rather than being swallowed.
        let (alert_tx, alert_rx) = async_channel::unbounded::<boot::CredentialAlert>();
        *events.credential_alert.lock().unwrap() = Some(alert_tx);
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            while let Ok(alert) = alert_rx.recv().await {
                let _ = this.update(cx, |store, cx| {
                    store.pending_credential_alerts.push(alert.clone());
                    cx.emit(StoreEvent::CredentialAlert(alert));
                    cx.notify();
                });
            }
        })
        .detach();
        (events, exit_rx)
    }

    fn spawn_exit_watcher(&self, generation: u64, exit_rx: async_channel::Receiver<Option<i32>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            if let Ok(code) = exit_rx.recv().await {
                cx.update(|_| crate::web::preview::close());
                let _ = this.update(cx, |store, cx| {
                    if !store.stopping && store.host_generation == generation && store.host.is_some() {
                        // Nothing answers any more: requests fail at once instead of waiting for a reconnect that
                        // never comes, also after "Later" in the dialog.
                        if let Some(rpc) = &store.rpc {
                            rpc.close();
                        }
                        cx.emit(StoreEvent::HostExited(code));
                    }
                });
            }
        })
        .detach();
    }

    fn spawn_rpc_pump(&self, generation: u64, events: async_channel::Receiver<RpcEvent>, cx: &mut Context<Self>) {
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            while let Ok(event) = events.recv().await {
                let active = this
                    .update(cx, |store, cx| {
                        if store.host_generation != generation {
                            return false;
                        }
                        store.handle(event, cx);
                        true
                    })
                    .unwrap_or(false);
                if !active {
                    break;
                }
            }
        })
        .detach();
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.host_generation = self.host_generation.wrapping_add(1);
        let generation = self.host_generation;
        let paths = self.paths.clone();
        let (events, exit_rx) = self.wire_carrier_events(cx);
        let (progress_tx, progress_rx) = async_channel::unbounded::<boot::Step>();
        let askpass = self.askpass.clone();
        let (modules, prompt) = (self.modules.manager.clone(), self.prompt.clone());
        let (accepted_tx, accepted) = async_channel::bounded::<()>(1);
        // Nothing from the resources runs before they are accepted.
        let started = cx.background_spawn(async move {
            let report = |step: boot::Step| {
                let _ = progress_tx.try_send(step);
            };
            let ask = |error: String| {
                let (reply, answer) = mpsc::channel();
                if prompt.send_blocking(RuntimePrompt { error, reply }).is_err() {
                    return boot::RuntimeChoice::Quit;
                }
                answer.recv().unwrap_or(boot::RuntimeChoice::Quit)
            };
            let integrity = boot::verify_resources(&paths, &report)?;
            let _ = accepted_tx.try_send(());
            let started = boot::start(&paths, events, askpass.as_ref(), &modules, &ask, &report)?;
            anyhow::Ok((started.target, Arc::new(started.host), started.prune, integrity))
        });
        // The modules' descriptor is read only once the resources were accepted.
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            if accepted.recv().await.is_ok() {
                let _ = this.update(cx, |store, cx| store.modules_ready(cx));
            }
        })
        .detach();
        self.spawn_exit_watcher(generation, exit_rx, cx);
        // Startup steps (starting WSL, installing the runtime) are shown while they run.
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            while let Ok(step) = progress_rx.recv().await {
                let _ = this.update(cx, |store, cx| {
                    if matches!(store.phase, Phase::Starting(..)) {
                        store.phase = Phase::Starting(step.text.into(), step.fraction);
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let (target, host, prune, integrity) = match started.await {
                Ok(started) => started,
                Err(error) => {
                    let _ = this.update(cx, |store, cx| {
                        store.phase = Phase::Failed(format!("{error:#}").into());
                        cx.notify();
                    });
                    return;
                }
            };
            let connected = RpcClient::connect(host.ready());
            let (rpc, events) = match connected {
                Ok(pair) => pair,
                Err(error) => {
                    cx.background_spawn(async move { host.stop(); }).detach();
                    let _ = this.update(cx, |store, cx| {
                        store.phase = Phase::Failed(format!("{error:#}").into());
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |store, cx| {
                let is_ssh = matches!(target, ExecutionTarget::Ssh { .. });
                store.host = Some(host);
                store.target = Some(target);
                store.rpc = Some(rpc);
                store.integrity = integrity.map(Arc::new);
                store.prune = prune;
                store.spawn_rpc_pump(generation, events, cx);
                if is_ssh {
                    store.start_auto_tunnels(cx);
                }
                cx.notify();
            });
        });
        self._tasks.push(task);
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
            cx.notify();
        }
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
                    let snapshot = this
                        .update(cx, |store, cx| {
                            let mut st = store.tunnels.status();
                            st.state = TunnelState::Error(msg.clone());
                            store.tunnel_status = st.clone();
                            cx.notify();
                            st
                        })
                        .ok();
                    let _ = snapshot;
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

    /// A minute after the workbench is first ready, as 3.x did: the daily recheck of the installed files, then removing
    /// what earlier versions left behind and the Linux runtimes they installed. Nothing starts while Rainy is quitting.
    fn schedule_maintenance(&mut self, cx: &mut Context<Self>) {
        if self.maintenance_scheduled {
            return;
        }
        self.maintenance_scheduled = true;
        let (paths, integrity, prune, modules) = (self.paths.clone(), self.integrity.clone(), self.prune.take(), self.modules.manager.clone());
        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            cx.background_executor().timer(boot::MAINTENANCE_DELAY).await;
            let stopping = |cx: &mut AsyncApp| this.update(cx, |store, _| store.stopping).unwrap_or(true);
            if let Some(integrity) = integrity.clone() {
                if stopping(cx) {
                    return;
                }
                if let Some(error) = cx.background_spawn(async move { boot::recheck_resources(&integrity) }).await {
                    let _ = this.update(cx, |_, cx| cx.emit(StoreEvent::ResourcesDamaged(error)));
                    // The cleanup trusts lists among the damaged files (`runtime.json` names what may stay); the next
                    // start checks every file first.
                    return;
                }
            }
            if stopping(cx) {
                return;
            }
            cx.background_spawn(async move { boot::clean_up(&paths, integrity.as_deref(), prune, &modules) }).await;
        });
        self._tasks.push(task);
    }

    /// Stop taking work and hand over what still has to stop: run [`Shutdown::finish`] off the interface thread. A
    /// second call has nothing left to stop.
    pub fn shutdown(&mut self) -> Shutdown {
        self.stopping = true;
        self.ssh_reconnect_task = None;
        self.ssh_retry_now = None;
        self.ssh_reconnect = SshAutoReconnect::Idle;
        crate::web::preview::close();
        // A download stops; what it received is kept for the next attempt.
        self.modules.manager.close();
        self.updates.cancel();
        if let Some(rpc) = self.rpc.take() {
            rpc.close();
        }
        Shutdown {
            host: self.host.take(),
            modules: self.modules.manager.clone(),
            strata: self.strata.manager.clone(),
            tunnels: self.tunnels.clone(),
        }
    }

    /// Quit as 3.x did: the editor's unsaved text is stored, the Host and the Strata server Rainy started stop, then Rainy
    /// exits. The windows go at once; nothing here blocks the interface thread, and a second request while this runs
    /// does nothing.
    pub fn quit(&mut self, cx: &mut Context<Self>) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        log::info!("Quitting: saving the editor state and stopping the Host");
        crate::placement::save(cx);
        hide_windows();
        let flush = self.rpc.is_some() && self.host.as_ref().is_some_and(|host| !host.has_exited());
        // A downloaded update installs once everything stopped, so the next start is the new version.
        let update = update::on_quit(&self.updates);
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            // Asked here rather than while the store is being updated: saving reads the store (the interface language).
            let flush = flush.then(|| cx.update(|cx| ide::Ide::global(cx).update(cx, |ide, cx| ide.flush_now(cx))));
            if let Some(flush) = flush {
                match flush.with_timeout(QUIT_FLUSH_TIMEOUT, cx.background_executor()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => log::warn!("The editor state was not saved before quitting: {error}"),
                    Err(_) => log::warn!("The editor state was not saved before quitting: no answer in time"),
                }
            }
            if let Ok(shutdown) = this.update(cx, |store, _| store.shutdown()) {
                let stopped = cx.background_spawn(async move { shutdown.finish() }).await;
                match &update {
                    Some(prepared) if stopped => cx.background_spawn({
                        let prepared = prepared.clone();
                        async move { update::install_on_quit(&prepared) }
                    })
                    .await,
                    Some(prepared) => log::warn!("Update {}: not installed on quit, a component operation had not stopped", prepared.version),
                    None => {}
                }
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }

    fn handle(&mut self, event: RpcEvent, cx: &mut Context<Self>) {
        match event {
            RpcEvent::State(state) => {
                let reopened = state == ConnectionState::Open;
                self.connection = state;
                if reopened {
                    if self.pending_reconnected {
                        self.pending_reconnected = false;
                        if let Some(rpc) = self.rpc.clone() {
                            cx.emit(StoreEvent::HostReconnected(rpc));
                        }
                    }
                    self.on_open(cx);
                }
                cx.notify();
            }
            RpcEvent::Event { name, data } => self.on_event(&name, data, cx),
        }
    }

    /// The socket opened (first time or after a reconnect): load everything the interface shows.
    fn on_open(&mut self, cx: &mut Context<Self>) {
        let Some(rpc) = self.rpc.clone() else { return };
        let generation = self.host_generation;
        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let info = rpc.request::<AppInfo>("app.info", Value::Null).await;
            let prefs = rpc.request::<UiPreferences>("prefs.get", Value::Null).await;
            let models = rpc.request::<ModelsStatus>("models.status", Value::Null).await;
            let summaries = rpc.request::<Vec<SessionSummary>>("sessions.list", json!({})).await;
            let external = rpc.request::<ExternalStatusResponse>("external.status", json!({})).await;
            for (method, error) in [
                ("app.info", info.as_ref().err()),
                ("prefs.get", prefs.as_ref().err()),
                ("models.status", models.as_ref().err()),
                ("sessions.list", summaries.as_ref().err()),
            ] {
                if let Some(error) = error {
                    log::warn!("{method} failed: {} {}", error.code, error.message);
                }
            }
            if let Err(error) = &external {
                log::debug!("external.status unavailable: {} {}", error.code, error.message);
            }
            let _ = this.update(cx, |store, cx| {
                if store.host_generation != generation {
                    return;
                }
                match (&info, &prefs) {
                    (Ok(_), Ok(_)) => {
                        store.phase = Phase::Ready;
                        store.schedule_maintenance(cx);
                        store.schedule_shell(cx);
                        store.schedule_update_check(cx);
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        store.phase = Phase::Failed(crate::i18n::error_text(boot::saved_locale(&store.paths.user_data), error).into());
                    }
                }
                if let Ok(info) = info {
                    store.info = Some(info);
                }
                if let Ok(prefs) = prefs {
                    store.set_prefs(prefs, cx);
                }
                if let Ok(models) = models {
                    store.models = Some(models);
                }
                if let Ok(summaries) = summaries {
                    store.set_summaries(summaries, cx);
                }
                if let Ok(external) = external {
                    store.external_agents = external.agents;
                }
                // Chats open before a reconnect may have missed events.
                let open: Vec<String> = store.chats.keys().cloned().collect();
                for id in open {
                    store.load_chat(&id, false, cx);
                }
                cx.notify();
            });
        });
        task.detach();
    }

    fn set_summaries(&mut self, mut summaries: Vec<SessionSummary>, cx: &mut Context<Self>) {
        summaries.sort_by(|a, b| b.updated_at.total_cmp(&a.updated_at));
        self.summaries = summaries;
        cx.emit(StoreEvent::SummariesChanged);
        self.mark_read(cx);
    }

    /// Record that the shown chat's replies were seen once it stops, so the sidebar marks only replies that came while
    /// another chat was shown.
    fn mark_read(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else { return };
        let Some(summary) = self.summaries.iter().find(|summary| summary.id == id) else { return };
        if summary.status != "idle" || summary.read_at.is_some_and(|read| read >= summary.updated_at) || !self.reading.insert(id.clone()) {
            return;
        }
        let call = self.call("sessions.read", json!({ "sessionId": id }));
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let _ = call.await;
            let _ = this.update(cx, |store, _| store.reading.remove(&id));
        })
        .detach();
    }

    /// Chats whose messages hold every word of `query`, newest first, with a line around the match.
    pub fn search_chats(&self, query: &str, cx: &mut Context<Self>) -> Task<Vec<(String, String)>> {
        let call = self.call("sessions.search", json!({ "query": query }));
        cx.spawn(async move |_: WeakEntity<Store>, _: &mut AsyncApp| {
            let Ok(found) = call.await else { return Vec::new() };
            found
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| Some((item["sessionId"].as_str()?.to_owned(), item["snippet"].as_str().unwrap_or_default().to_owned())))
                .collect()
        })
    }

    fn on_event(&mut self, name: &str, data: Value, cx: &mut Context<Self>) {
        let session = |data: &Value| data.get("sessionId").and_then(Value::as_str).map(str::to_owned);
        match name {
            "session.entry" => {
                if let Ok(event) = serde_json::from_value::<SessionEntryEvent>(data) {
                    self.update_chat(&event.session_id.clone(), cx, |view| view.apply_entry(event));
                }
            }
            "session.stream" => {
                if let Some((id, update)) = parse_stream_event(&data) {
                    self.update_chat(&id, cx, |view| view.apply_stream(update));
                }
            }
            "session.tool" => {
                if let Ok(event) = serde_json::from_value::<SessionToolEvent>(data) {
                    self.update_chat(&event.session_id.clone(), cx, |view| view.apply_tool(event));
                }
            }
            "session.state" => {
                if let Ok(event) = serde_json::from_value::<SessionStateEvent>(data) {
                    let id = event.session_id.clone();
                    let status = event.status.clone();
                    let awaiting = !event.approvals.is_empty();
                    self.update_chat(&id, cx, |view| view.apply_state(event));
                    if let Some(summary) = self.summaries.iter_mut().find(|s| s.id == id) {
                        summary.status = status;
                        summary.awaiting_approval = awaiting.then_some(true);
                        cx.emit(StoreEvent::SummariesChanged);
                        cx.notify();
                    }
                }
            }
            "sessions.changed" => {
                if let Ok(summary) = serde_json::from_value::<SessionSummary>(data) {
                    self.update_chat(&summary.id.clone(), cx, |view| {
                        view.snapshot.summary = summary.clone();
                        true
                    });
                    let mut next: Vec<SessionSummary> = self.summaries.iter().filter(|s| s.id != summary.id).cloned().collect();
                    next.push(summary);
                    self.set_summaries(next, cx);
                    cx.notify();
                }
            }
            "sessions.removed" => {
                if let Some(id) = session(&data) {
                    self.summaries.retain(|s| s.id != id);
                    self.chats.remove(&id);
                    self.chat_used.remove(&id);
                    if self.selected.as_deref() == Some(id.as_str()) {
                        self.selected = None;
                        self.selections += 1;
                        cx.emit(StoreEvent::SelectionChanged);
                    }
                    cx.emit(StoreEvent::SummariesChanged);
                    cx.notify();
                }
            }
            "prefs.changed" => {
                if let Ok(prefs) = serde_json::from_value::<UiPreferences>(data) {
                    self.set_prefs(prefs, cx);
                    cx.notify();
                }
            }
            "models.changed" => {
                if let Ok(models) = serde_json::from_value::<ModelsStatus>(data) {
                    self.models = Some(models);
                    cx.notify();
                }
            }
            "external.changed" => {
                if let Ok(status) = serde_json::from_value::<ExternalStatusResponse>(data.clone()) {
                    self.external_agents = status.agents;
                    cx.notify();
                }
                cx.emit(StoreEvent::Host { name: name.to_owned(), data });
            }
            "external.commands" => {
                // The slash commands an external agent offers (Antigravity's /swarm, /teamwork…), for the composer.
                if let Some(kind) = data["kind"].as_str()
                    && let Ok(commands) = serde_json::from_value::<Vec<rainy_protocol::ExternalCommand>>(data["commands"].clone())
                    && let Some(agent) = self.external_agents.iter_mut().find(|agent| agent.kind == kind)
                {
                    agent.commands = commands;
                    cx.notify();
                }
            }
            "external.teamChanged" => {
                // Read only for the chat shown and teams already open; the others are read when shown.
                if let Some(id) = data["sessionId"].as_str()
                    && (self.selected.as_deref() == Some(id) || self.teams.contains_key(id))
                    && self.teams.get(id).is_none_or(|team| data["revision"].as_u64() != Some(team.revision))
                {
                    self.fetch_team(id.to_owned(), cx);
                }
            }
            _ => cx.emit(StoreEvent::Host { name: name.to_owned(), data }),
        }
    }

    fn update_chat(&mut self, id: &str, cx: &mut Context<Self>, change: impl FnOnce(&mut ChatView) -> bool) {
        let Some(chat) = self.chats.get(id) else { return };
        chat.update(cx, |chat, cx| {
            if let Some(view) = chat.view.as_mut()
                && change(view)
            {
                cx.notify();
            }
        });
    }

    /// Maximum number of inactive chats kept loaded in memory simultaneously.
    const MAX_CACHED_CHATS: usize = 24;

    fn touch_chat(&mut self, id: &str) {
        self.chat_tick = self.chat_tick.wrapping_add(1);
        self.chat_used.insert(id.to_owned(), self.chat_tick);
    }

    fn evict_lru_chats(&mut self, keep_id: &str, cx: &App) {
        while self.chats.len() > Self::MAX_CACHED_CHATS {
            let victim = self
                .chats
                .iter()
                .filter(|(id, model)| {
                    if id.as_str() == keep_id || self.selected.as_deref() == Some(id.as_str()) {
                        return false;
                    }
                    let pinned = self.summaries.iter().any(|s| s.id == **id && s.pinned);
                    if pinned {
                        return false;
                    }
                    let active = model.read(cx).view.as_ref().is_some_and(|v| v.busy() || !v.snapshot.approvals.is_empty());
                    !active
                })
                .min_by_key(|(id, _)| self.chat_used.get(id.as_str()).copied().unwrap_or(0))
                .map(|(id, _)| id.clone());
            let Some(victim) = victim else { break };
            self.chats.remove(&victim);
            self.chat_used.remove(&victim);
        }
    }

    /// The chat with this id, loading it the first time.
    pub fn chat(&mut self, id: &str, cx: &mut Context<Self>) -> Entity<ChatModel> {
        self.touch_chat(id);
        if let Some(chat) = self.chats.get(id) {
            return chat.clone();
        }
        let chat = cx.new(|_| ChatModel { id: id.to_owned(), view: None, error: None, whole: false, loading_whole: false, loaded: 0, load: 0 });
        self.chats.insert(id.to_owned(), chat.clone());
        self.load_chat(id, false, cx);
        self.evict_lru_chats(id, cx);
        chat
    }

    /// Load (or reload) a chat, its last `INITIAL_ENTRIES` entries or, once asked for, all of them; a newer load
    /// replaces an older one that finishes later.
    pub fn load_chat(&mut self, id: &str, whole: bool, cx: &mut Context<Self>) {
        let (Some(rpc), Some(chat)) = (self.rpc.clone(), self.chats.get(id).cloned()) else { return };
        let (load, whole) = chat.update(cx, |chat, cx| {
            chat.load += 1;
            chat.whole |= whole;
            chat.loading_whole = chat.whole;
            cx.notify();
            (chat.load, chat.whole)
        });
        let mut params = json!({ "sessionId": id });
        if !whole {
            params["limit"] = json!(INITIAL_ENTRIES);
        }
        let request = rpc.request::<SessionSnapshot>("sessions.get", params);
        let locale = self.locale();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            let result = request.await;
            chat.update(cx, |chat, cx| {
                if chat.load != load {
                    return;
                }
                chat.loading_whole = false;
                match result {
                    Ok(snapshot) => {
                        chat.view = Some(ChatView::new(snapshot, whole));
                        chat.loaded += 1;
                        chat.error = None;
                    }
                    Err(error) => {
                        // A failed whole load leaves the window shown and can be asked for again.
                        let key = if whole && chat.view.is_some() { "chat.loadOlderFailed" } else { "chat.chatLoadFailed" };
                        if chat.view.is_some() {
                            chat.whole = false;
                        }
                        chat.error = Some(crate::i18n::format(locale, key, &[("message", &crate::i18n::error_text(locale, &error))]).into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if let Some(id) = &id {
            self.chat(id, cx);
            // An external agent's chat may run a team (a swarm, teamwork, sub-agents); a chat just made may not be
            // listed yet, and is asked as well.
            if !self.summaries.iter().any(|summary| summary.id == *id && summary.external_backend.is_none()) {
                self.fetch_team(id.clone(), cx);
            }
        }
        if self.selected != id {
            self.selections += 1;
        }
        self.selected = id;
        cx.emit(StoreEvent::SelectionChanged);
        self.mark_read(cx);
        cx.notify();
    }

    /// Call a Host method; fails at once while the Host is not connected.
    /// Whether the chat's external agent runs a team: anyone besides itself, or a board.
    pub fn has_team(&self, id: &str) -> bool {
        self.teams.get(id).is_some_and(|team| team.members.len() > 1 || !team.posts.is_empty())
    }

    /// Read the team of an external agent's chat: the board posts newer than the ones held, everything else anew.
    pub fn fetch_team(&mut self, id: String, cx: &mut Context<Self>) {
        let after = self.teams.get(&id).map(|team| team.last_post);
        let call = self.call("external.team", json!({ "sessionId": id, "after": after }));
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let Ok(value) = call.await else { return };
            let Ok(Some(mut team)) = serde_json::from_value::<Option<ExternalTeam>>(value) else { return };
            let _ = this.update(cx, |store, cx| {
                match store.teams.get_mut(&id) {
                    Some(known) => {
                        let newest = known.posts.last().map(|post| post.seq).unwrap_or(0);
                        // The board was written anew (a new swarm starts at post 1 again): read it whole once more.
                        if team.last_post < newest {
                            store.teams.remove(&id);
                            store.fetch_team(id.clone(), cx);
                            return;
                        }
                        let mut posts = std::mem::take(&mut known.posts);
                        posts.extend(team.posts.drain(..).filter(|post| post.seq > newest));
                        let excess = posts.len().saturating_sub(800);
                        posts.drain(..excess);
                        team.posts = posts;
                        *known = team;
                    }
                    None => {
                        store.teams.insert(id.clone(), team);
                    }
                }
                cx.emit(StoreEvent::TeamChanged(id.clone()));
                cx.notify();
            });
        })
        .detach();
    }

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

    /// New interface preferences; the language is also recorded for messages shown before the next start's Host answers.
    fn set_prefs(&mut self, prefs: UiPreferences, cx: &mut Context<Self>) {
        let changed = prefs.locale != self.prefs.locale || boot::saved_locale(&self.paths.user_data) != Locale::from_code(&prefs.locale);
        self.prefs = prefs;
        rainy_carrier::locale::set_chinese(self.locale() == Locale::Zh);
        self.follow_shell_language(cx);
        if changed {
            let (settings, locale) = (self.paths.desktop_settings(), self.prefs.locale.clone());
            cx.background_spawn(async move {
                if let Err(error) = rainy_carrier::targets::save_locale(&settings, &locale) {
                    log::warn!("The interface language was not recorded: {error:#}");
                }
            })
            .detach();
        }
        cx.emit(StoreEvent::PrefsChanged);
    }

    /// Return true when an SSH failure should alert the user immediately instead of retrying in the background.
    fn is_non_retryable_ssh_error(detail: &str) -> bool {
        let lower = detail.to_ascii_lowercase();
        [
            "permission denied",
            "authentication failed",
            "host key verification failed",
            "bad configuration",
            "cancelled",
            "canceled",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    }

    /// Install a freshly connected Host/RPC pair, refresh all subsystems via `StoreEvent::HostReconnected`,
    /// and restore or auto-start SSH port-forwarding tunnels.
    #[allow(clippy::too_many_arguments)]
    fn install_connected_host(
        &mut self,
        generation: u64,
        target: ExecutionTarget,
        host: Arc<HostProcess>,
        rpc: RpcClient,
        rpc_events: async_channel::Receiver<RpcEvent>,
        exit_rx: async_channel::Receiver<Option<i32>>,
        same_target: bool,
        restore_tunnels: Vec<PortForwardRule>,
        cx: &mut Context<Self>,
    ) {
        self.reconnecting = false;
        self.swapping_target = None;
        self.ssh_reconnect = SshAutoReconnect::Idle;
        self.ssh_retry_now = None;
        self.ssh_reconnect_task = None;
        self.host = Some(host);
        self.target = Some(target.clone());
        self.rpc = Some(rpc);
        self.pending_reconnected = true;
        if !same_target {
            self.summaries.clear();
            self.chats.clear();
            self.chat_used.clear();
            self.reading.clear();
            self.selected = None;
            self.selections += 1;
            self.models = None;
            self.external_agents.clear();
            self.pending_external = None;
            self.pending_external_model = None;
            self.teams.clear();
            self.host_metrics = None;
            self.tunnel_status = TunnelStatus { target: None, rules: Vec::new(), state: TunnelState::Stopped };
            cx.emit(StoreEvent::SummariesChanged);
            cx.emit(StoreEvent::SelectionChanged);
        }
        self.spawn_exit_watcher(generation, exit_rx, cx);
        self.spawn_rpc_pump(generation, rpc_events, cx);
        if matches!(target, ExecutionTarget::Ssh { .. }) {
            if same_target && !restore_tunnels.is_empty() {
                self.apply_tunnel_rules(restore_tunnels, cx).detach();
            } else {
                self.start_auto_tunnels(cx);
            }
        }
        cx.notify();
    }

    /// Cancel any background silent SSH reconnect countdown without touching the active Host.
    pub fn cancel_ssh_auto_reconnect(&mut self, cx: &mut Context<Self>) {
        self.ssh_reconnect_task = None;
        self.ssh_retry_now = None;
        if self.ssh_reconnect != SshAutoReconnect::Idle {
            self.ssh_reconnect = SshAutoReconnect::Idle;
            cx.notify();
        }
    }

    /// Wake a sleeping SSH reconnect countdown immediately, or start a fresh reconnect if idle/exhausted.
    pub fn retry_ssh_now(&mut self, window: AnyWindowHandle, cx: &mut Context<Self>) {
        match &self.ssh_reconnect {
            SshAutoReconnect::Waiting { .. } => {
                if let Some(tx) = &self.ssh_retry_now {
                    let _ = tx.try_send(());
                }
            }
            SshAutoReconnect::Attempting { .. } => {}
            SshAutoReconnect::Idle | SshAutoReconnect::Exhausted { .. } => {
                self.reconnect_ssh(window, cx);
            }
        }
    }

    /// Non-blocking silent SSH reconnect with `1s -> 3s -> 8s` backoff. Alerts the user via dialog only if
    /// interactive auth fails or all 3 attempts are exhausted.
    pub fn start_ssh_auto_reconnect(&mut self, window: AnyWindowHandle, cx: &mut Context<Self>) {
        if self.quitting || self.stopping || self.reconnecting {
            return;
        }
        if matches!(self.ssh_reconnect, SshAutoReconnect::Waiting { .. } | SshAutoReconnect::Attempting { .. }) {
            return;
        }
        let Some(target @ ExecutionTarget::Ssh { .. }) = self.target.clone() else {
            return;
        };
        let Some(askpass) = self.askpass.clone() else {
            self.restart(cx);
            return;
        };
        let saved_tunnels = self.tunnel_status.rules.clone();
        let tunnels = self.tunnels.clone();
        self.tunnel_status = TunnelStatus { target: None, rules: Vec::new(), state: TunnelState::Stopped };
        cx.background_spawn(async move { tunnels.stop_all() }).detach();

        if let Some(rpc) = self.rpc.take() {
            rpc.close();
        }
        let previous = self
            .host
            .take()
            .and_then(|host| host.remote_ssh().map(|remote| (remote.host.clone(), host.ready().pid)));
        self.connection = ConnectionState::Closed;

        let (wake_tx, wake_rx) = async_channel::bounded::<()>(1);
        self.ssh_retry_now = Some(wake_tx);
        let start_generation = self.host_generation;
        let paths = self.paths.clone();
        let modules = self.modules.manager.clone();
        let locale = self.locale();
        let label = target.label().to_owned();

        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let max = SSH_RECONNECT_BACKOFFS_SECS.len() as u8;
            let mut last_error: Option<String> = None;

            for (idx, delay_secs) in SSH_RECONNECT_BACKOFFS_SECS.iter().copied().enumerate() {
                let attempt = (idx + 1) as u8;
                let retry_at = Instant::now() + Duration::from_secs(delay_secs);
                let alive = this
                    .update(cx, |store, cx| {
                        if store.stopping || store.quitting || store.host_generation != start_generation {
                            return false;
                        }
                        store.ssh_reconnect = SshAutoReconnect::Waiting {
                            attempt,
                            max,
                            retry_at,
                            last_error: last_error.clone(),
                        };
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    return;
                }

                // Wait until `retry_at` or an immediate wake signal from "Retry now", ticking so the countdown updates.
                while Instant::now() < retry_at {
                    if wake_rx.try_recv().is_ok() {
                        break;
                    }
                    let remaining = retry_at.saturating_duration_since(Instant::now());
                    cx.background_executor().timer(remaining.min(Duration::from_millis(250))).await;
                    let still_same = this
                        .update(cx, |store, cx| {
                            if store.stopping || store.quitting || store.host_generation != start_generation {
                                return false;
                            }
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !still_same {
                        return;
                    }
                }
                while wake_rx.try_recv().is_ok() {}

                let prepared = this
                    .update(cx, |store, cx| {
                        if store.stopping || store.quitting || store.host_generation != start_generation {
                            return None;
                        }
                        store.reconnecting = true;
                        store.connection = ConnectionState::Connecting;
                        store.ssh_reconnect = SshAutoReconnect::Attempting { attempt, max };
                        cx.notify();
                        Some(store.wire_carrier_events(cx))
                    })
                    .ok()
                    .flatten();
                let Some((events, exit_rx)) = prepared else {
                    return;
                };

                let (paths_c, target_c, askpass_c, modules_c, prev_c) =
                    (paths.clone(), target.clone(), askpass.clone(), modules.clone(), previous.clone());
                let outcome = cx
                    .background_spawn(async move {
                        let prev_ref = prev_c.as_ref().map(|(h, pid)| (h.as_str(), *pid));
                        boot::reconnect_ssh(&paths_c, &target_c, events, &askpass_c, &modules_c, prev_ref)
                    })
                    .await;

                match outcome {
                    Ok(rainy_carrier::ssh::ReconnectOutcome::Connected(host)) => {
                        let host = Arc::new(host);
                        match RpcClient::connect(host.ready()) {
                            Ok((rpc, rpc_events)) => {
                                let installed = this
                                    .update(cx, |store, cx| {
                                        if store.stopping || store.quitting || store.host_generation != start_generation {
                                            return false;
                                        }
                                        store.host_generation = store.host_generation.wrapping_add(1);
                                        let next_gen = store.host_generation;
                                        store.install_connected_host(
                                            next_gen,
                                            target.clone(),
                                            host,
                                            rpc,
                                            rpc_events,
                                            exit_rx,
                                            true,
                                            saved_tunnels.clone(),
                                            cx,
                                        );
                                        true
                                    })
                                    .unwrap_or(false);
                                if installed {
                                    let _ = window.update(cx, |_, window, cx| {
                                        use gpui_kit::component::WindowExt as _;
                                        let text = crate::i18n::format(locale, "native.sshReconnected", &[("target", &label)]);
                                        window.push_notification(
                                            gpui_kit::component::notification::Notification::success(text),
                                            cx,
                                        );
                                    });
                                }
                                return;
                            }
                            Err(error) => {
                                cx.background_spawn(async move { host.stop(); }).detach();
                                last_error = Some(format!("{error:#}"));
                            }
                        }
                    }
                    Ok(rainy_carrier::ssh::ReconnectOutcome::NeedsInstall { .. }) => {
                        last_error = Some(crate::i18n::text(locale, "ssh.needsInstall").to_owned());
                        let _ = this.update(cx, |store, cx| {
                            store.reconnecting = false;
                            store.connection = ConnectionState::Closed;
                            cx.notify();
                        });
                        break;
                    }
                    Ok(rainy_carrier::ssh::ReconnectOutcome::Unsupported(system)) => {
                        last_error = Some(system);
                        let _ = this.update(cx, |store, cx| {
                            store.reconnecting = false;
                            store.connection = ConnectionState::Closed;
                            cx.notify();
                        });
                        break;
                    }
                    Err(error) => {
                        let detail = format!("{error:#}");
                        let stop_now = Self::is_non_retryable_ssh_error(&detail);
                        last_error = Some(detail);
                        let _ = this.update(cx, |store, cx| {
                            store.reconnecting = false;
                            store.connection = ConnectionState::Closed;
                            cx.notify();
                        });
                        if stop_now {
                            break;
                        }
                    }
                }
            }

            let detail = last_error.unwrap_or_else(|| crate::i18n::text(locale, "app.connectionLost").to_owned());
            let should_alert = this
                .update(cx, |store, cx| {
                    if store.stopping || store.quitting || store.host_generation != start_generation {
                        return false;
                    }
                    store.reconnecting = false;
                    store.connection = ConnectionState::Closed;
                    store.ssh_retry_now = None;
                    store.ssh_reconnect = SshAutoReconnect::Exhausted { last_error: detail.clone() };
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if should_alert {
                let _ = window.update(cx, |_, window, cx| {
                    crate::ui::ssh::host_exited_exhausted(window, cx, detail);
                });
            }
        });
        self.ssh_reconnect_task = Some(task);
        cx.notify();
    }

    /// Reconnect the current SSH Host in place without restarting `RainyAgent.exe`.
    /// Falls back to an in-place target start when the server needs a fresh runtime installation.
    pub fn reconnect_ssh(&mut self, window: AnyWindowHandle, cx: &mut Context<Self>) {
        if self.reconnecting || self.quitting {
            return;
        }
        let Some(target @ ExecutionTarget::Ssh { .. }) = self.target.clone() else {
            self.restart(cx);
            return;
        };
        let Some(askpass) = self.askpass.clone() else {
            self.restart(cx);
            return;
        };
        self.cancel_ssh_auto_reconnect(cx);
        self.reconnecting = true;
        self.host_generation = self.host_generation.wrapping_add(1);
        let generation = self.host_generation;
        let locale = self.locale();
        let label = target.label().to_owned();
        let saved_tunnels = self.tunnel_status.rules.clone();
        let tunnels = self.tunnels.clone();
        self.tunnel_status = TunnelStatus { target: None, rules: Vec::new(), state: TunnelState::Stopped };
        cx.background_spawn(async move { tunnels.stop_all() }).detach();

        if let Some(rpc) = self.rpc.take() {
            rpc.close();
        }
        let previous = self
            .host
            .take()
            .and_then(|host| host.remote_ssh().map(|remote| (remote.host.clone(), host.ready().pid)));
        self.connection = ConnectionState::Connecting;
        self.ssh_reconnect = SshAutoReconnect::Attempting { attempt: 1, max: 1 };
        cx.notify();
        let _ = window.update(cx, |_, window, cx| {
            use gpui_kit::component::WindowExt as _;
            let note = crate::i18n::format(locale, "native.sshReconnecting", &[("target", &label)]);
            window.push_notification(gpui_kit::component::notification::Notification::info(note), cx);
        });
        let (events, exit_rx) = self.wire_carrier_events(cx);
        let paths = self.paths.clone();
        let modules = self.modules.manager.clone();
        let reconnecting = cx.background_spawn({
            let target = target.clone();
            async move {
                let prev_ref = previous.as_ref().map(|(h, pid)| (h.as_str(), *pid));
                boot::reconnect_ssh(&paths, &target, events, &askpass, &modules, prev_ref)
            }
        });
        let label_done = label.clone();
        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let outcome = reconnecting.await;
            let host = match outcome {
                Ok(rainy_carrier::ssh::ReconnectOutcome::Connected(host)) => Arc::new(host),
                Ok(rainy_carrier::ssh::ReconnectOutcome::NeedsInstall { .. }) => {
                    let _ = this.update(cx, |store, cx| {
                        store.reconnecting = false;
                        store.ssh_reconnect = SshAutoReconnect::Idle;
                        store.swap_target_in_place(target.clone(), window, cx);
                    });
                    return;
                }
                Ok(rainy_carrier::ssh::ReconnectOutcome::Unsupported(system)) => {
                    let _ = this.update(cx, |store, cx| {
                        store.reconnecting = false;
                        store.connection = ConnectionState::Closed;
                        store.ssh_reconnect = SshAutoReconnect::Exhausted { last_error: system.clone() };
                        cx.notify();
                    });
                    let _ = window.update(cx, |_, window, cx| {
                        use gpui_kit::component::WindowExt as _;
                        let text = crate::i18n::format(locale, "native.sshReconnectFailed", &[("target", &label_done), ("error", &system)]);
                        window.push_notification(gpui_kit::component::notification::Notification::error(text), cx);
                    });
                    return;
                }
                Err(error) => {
                    let detail = format!("{error:#}");
                    let _ = this.update(cx, |store, cx| {
                        store.reconnecting = false;
                        store.connection = ConnectionState::Closed;
                        store.ssh_reconnect = SshAutoReconnect::Exhausted { last_error: detail.clone() };
                        cx.notify();
                    });
                    let _ = window.update(cx, |_, window, cx| {
                        use gpui_kit::component::WindowExt as _;
                        let text = crate::i18n::format(locale, "native.sshReconnectFailed", &[("target", &label_done), ("error", &detail)]);
                        window.push_notification(gpui_kit::component::notification::Notification::error(text), cx);
                    });
                    return;
                }
            };
            let (rpc, rpc_events) = match RpcClient::connect(host.ready()) {
                Ok(pair) => pair,
                Err(error) => {
                    cx.background_spawn(async move { host.stop(); }).detach();
                    let detail = format!("{error:#}");
                    let _ = this.update(cx, |store, cx| {
                        store.reconnecting = false;
                        store.connection = ConnectionState::Closed;
                        store.ssh_reconnect = SshAutoReconnect::Exhausted { last_error: detail.clone() };
                        cx.notify();
                    });
                    let _ = window.update(cx, |_, window, cx| {
                        use gpui_kit::component::WindowExt as _;
                        let text = crate::i18n::format(locale, "native.sshReconnectFailed", &[("target", &label_done), ("error", &detail)]);
                        window.push_notification(gpui_kit::component::notification::Notification::error(text), cx);
                    });
                    return;
                }
            };
            let _ = this.update(cx, |store, cx| {
                if store.host_generation != generation {
                    return;
                }
                store.install_connected_host(generation, target, host, rpc, rpc_events, exit_rx, true, saved_tunnels, cx);
            });
            let _ = window.update(cx, |_, window, cx| {
                use gpui_kit::component::WindowExt as _;
                let text = crate::i18n::format(locale, "native.sshReconnected", &[("target", &label_done)]);
                window.push_notification(gpui_kit::component::notification::Notification::success(text), cx);
            });
        });
        self._tasks.push(task);
    }

    /// Hot-swap the execution target (Windows <-> WSL <-> SSH) in place without restarting `RainyAgent.exe`.
    /// Flushes unsaved editor state, stops old tunnels & Host, starts the new Host, and refreshes every subsystem.
    pub fn swap_target_in_place(&mut self, target: ExecutionTarget, window: AnyWindowHandle, cx: &mut Context<Self>) {
        if self.reconnecting || self.quitting {
            return;
        }
        let same_target = self.target.as_ref() == Some(&target);
        let settings = self.paths.desktop_settings();
        let current_host = self.host.clone();
        let host_alive = current_host.as_ref().is_some_and(|host| !host.has_exited());
        let has_rpc = self.rpc.is_some() && host_alive;
        let locale = self.locale();
        let label = target.label().to_owned();

        self.cancel_ssh_auto_reconnect(cx);
        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            // Check whether the current Host is mid-turn before tearing it down.
            if host_alive && let Some(host) = current_host.clone() {
                let busy = cx.background_spawn({
                    let host = host.clone();
                    async move { host.inspect_activity("freeze").unwrap_or(false) }
                })
                .await;
                if busy {
                    cx.background_spawn(async move { host.inspect_activity("resume") }).detach();
                    let text = crate::i18n::text(locale, "native.busyBeforeSwitch");
                    let _ = window.update(cx, |_, window, cx| {
                        use gpui_kit::component::WindowExt as _;
                        window.push_notification(gpui_kit::component::notification::Notification::warning(text), cx);
                    });
                    return;
                }
            }

            // Flush unsaved IDE state while the old Host is still reachable.
            if has_rpc {
                let flush = cx.update(|cx| ide::Ide::global(cx).update(cx, |ide, cx| ide.flush_now(cx)));
                let _ = flush.with_timeout(QUIT_FLUSH_TIMEOUT, cx.background_executor()).await;
            }

            if let Err(error) = rainy_carrier::targets::save_target(&settings, &target) {
                log::warn!("The execution target was not saved: {error:#}");
            }

            let prepared = this
                .update(cx, |store, cx| {
                    if store.reconnecting || store.quitting {
                        return None;
                    }
                    store.reconnecting = true;
                    store.swapping_target = Some(label.clone().into());
                    store.host_generation = store.host_generation.wrapping_add(1);
                    let generation = store.host_generation;
                    store.connection = ConnectionState::Connecting;
                    store.host_metrics = None;
                    let saved_tunnels = if same_target { store.tunnel_status.rules.clone() } else { Vec::new() };
                    store.tunnel_status = TunnelStatus { target: None, rules: Vec::new(), state: TunnelState::Stopped };
                    if let Some(rpc) = store.rpc.take() {
                        rpc.close();
                    }
                    let old_host = store.host.take();
                    let tunnels = store.tunnels.clone();
                    let (events, exit_rx) = store.wire_carrier_events(cx);
                    let paths = store.paths.clone();
                    let askpass = store.askpass.clone();
                    let modules = store.modules.manager.clone();
                    let prompt = store.prompt.clone();
                    cx.notify();
                    Some((generation, old_host, tunnels, saved_tunnels, events, exit_rx, paths, askpass, modules, prompt))
                })
                .ok()
                .flatten();
            let Some((generation, old_host, tunnels, saved_tunnels, events, exit_rx, paths, askpass, modules, prompt)) = prepared else {
                return;
            };

            let _ = window.update(cx, |_, window, cx| {
                use gpui_kit::component::WindowExt as _;
                let text = crate::i18n::format(locale, "ssh.swapping", &[("target", &label)]);
                window.push_notification(gpui_kit::component::notification::Notification::info(text), cx);
            });

            // Stop old tunnels and old Host off the UI thread before launching the new target.
            cx.background_spawn(async move {
                tunnels.stop_all();
                if let Some(old_host) = old_host {
                    old_host.stop();
                }
            })
            .await;

            let (progress_tx, progress_rx) = async_channel::unbounded::<boot::Step>();
            let label_for_progress = label.clone();
            cx.spawn({
                let this = this.clone();
                async move |cx| {
                    while let Ok(step) = progress_rx.recv().await {
                        let _ = this.update(cx, |store, cx| {
                            if store.host_generation == generation && store.reconnecting {
                                store.swapping_target = Some(format!("{label_for_progress} · {}", step.text).into());
                                cx.notify();
                            }
                        });
                    }
                }
            })
            .detach();

            let target_start = target.clone();
            let started = cx
                .background_spawn(async move {
                    let report = |step: boot::Step| {
                        let _ = progress_tx.try_send(step);
                    };
                    let ask = |error: String| {
                        let (reply, answer) = mpsc::channel();
                        if prompt.send_blocking(RuntimePrompt { error, reply }).is_err() {
                            return boot::RuntimeChoice::Quit;
                        }
                        answer.recv().unwrap_or(boot::RuntimeChoice::Quit)
                    };
                    boot::start_target(&paths, target_start, events, askpass.as_ref(), &modules, &ask, &report)
                        .map(|started| (started.target, Arc::new(started.host)))
                })
                .await;

            let (new_target, host) = match started {
                Ok(pair) => pair,
                Err(error) => {
                    let detail = format!("{error:#}");
                    let _ = this.update(cx, |store, cx| {
                        if store.host_generation != generation {
                            return;
                        }
                        store.reconnecting = false;
                        store.swapping_target = None;
                        store.connection = ConnectionState::Closed;
                        store.target = Some(target.clone());
                        if matches!(target, ExecutionTarget::Ssh { .. }) {
                            store.ssh_reconnect = SshAutoReconnect::Exhausted { last_error: detail.clone() };
                        }
                        cx.notify();
                    });
                    let _ = window.update(cx, |_, window, cx| {
                        use gpui_kit::component::WindowExt as _;
                        let text = crate::i18n::format(locale, "ssh.swapFailed", &[("target", &label), ("error", &detail)]);
                        window.push_notification(gpui_kit::component::notification::Notification::error(text), cx);
                    });
                    return;
                }
            };

            let (rpc, rpc_events) = match RpcClient::connect(host.ready()) {
                Ok(pair) => pair,
                Err(error) => {
                    cx.background_spawn(async move { host.stop(); }).detach();
                    let detail = format!("{error:#}");
                    let _ = this.update(cx, |store, cx| {
                        if store.host_generation != generation {
                            return;
                        }
                        store.reconnecting = false;
                        store.swapping_target = None;
                        store.connection = ConnectionState::Closed;
                        cx.notify();
                    });
                    let _ = window.update(cx, |_, window, cx| {
                        use gpui_kit::component::WindowExt as _;
                        let text = crate::i18n::format(locale, "ssh.swapFailed", &[("target", &label), ("error", &detail)]);
                        window.push_notification(gpui_kit::component::notification::Notification::error(text), cx);
                    });
                    return;
                }
            };

            let _ = this.update(cx, |store, cx| {
                if store.host_generation != generation {
                    return;
                }
                store.install_connected_host(generation, new_target, host, rpc, rpc_events, exit_rx, same_target, saved_tunnels, cx);
            });
            let _ = window.update(cx, |_, window, cx| {
                use gpui_kit::component::WindowExt as _;
                let text = crate::i18n::format(locale, "ssh.swapped", &[("target", &label)]);
                window.push_notification(gpui_kit::component::notification::Notification::success(text), cx);
            });
        });
        self._tasks.push(task);
    }

    /// Start Rainy again with this run's switches, then quit. The window goes at once and the Host stops off the
    /// interface thread. The new process waits for this one to exit before it takes the single-instance lock, and opens
    /// none of the paths this run was launched with.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        crate::placement::save(cx);
        hide_windows();
        let shutdown = self.shutdown();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            cx.background_spawn(async move { shutdown.finish() }).await;
            if let Ok(exe) = std::env::current_exe() {
                let arguments = rainy_carrier::launch::relaunch_arguments(&std::env::args_os().collect::<Vec<_>>(), std::process::id());
                if let Err(error) = std::process::Command::new(exe).args(arguments).spawn() {
                    log::warn!("Rainy could not start again: {error}");
                }
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }

    /// Remember `target` and hot-swap into it in place; refused while the Host is busy.
    pub fn switch_target(&mut self, target: ExecutionTarget, window: AnyWindowHandle, cx: &mut Context<Self>) {
        self.swap_target_in_place(target, window, cx);
    }

    /// Start a chat in a project, or outside any project, in the mode picked for it (`pending_permission`, used up
    /// once the chat exists); resolves with its id. The caller selects it, unless `selections` moved on meanwhile: a
    /// user who went to another chat stays there.
    pub fn create_chat(&mut self, workspace_id: Option<String>, cx: &mut Context<Self>) -> Task<Result<String, RpcError>> {
        let mut params = json!({ "workspaceId": workspace_id });
        if let Some(mode) = &self.pending_permission {
            params["permission"] = json!(mode);
        }
        if self.pending_worktree && workspace_id.is_some() {
            params["worktree"] = json!(true);
        }
        let external = self.pending_external.clone();
        let external_model = self.pending_external_model.clone();
        if let Some(backend) = &external
            && let Ok(val) = serde_json::to_value(backend)
        {
            params["externalBackend"] = val;
        }
        if external.is_some()
            && let Some(model) = &external_model
            && let Ok(val) = serde_json::to_value(model)
        {
            params["model"] = val;
        }
        let request = self.call("sessions.create", params);
        // Orchestration applies only to built-in model chats.
        let orchestrate = if external.is_some() { None } else { self.pending_orchestrate.clone() };
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let mut summary = serde_json::from_value::<SessionSummary>(request.await?)
                .map_err(|error| RpcError { code: "invalid-response".into(), message: error.to_string(), data: None })?;
            let id = summary.id.clone();
            // Orchestration picked before the first message is set before that message goes out.
            if let Some(executor) = orchestrate
                && let Ok(call) = this.update(cx, |store, _| store.call("chat.setOrchestrate", json!({ "sessionId": id, "executor": executor })))
                && call.await.is_ok()
            {
                summary.orchestrate = Some(executor);
            }
            let _ = this.update(cx, |store, cx| {
                store.pending_permission = None;
                store.pending_worktree = false;
                store.pending_orchestrate = None;
                store.pending_external = None;
                store.pending_external_model = None;
                let mut next: Vec<SessionSummary> = store.summaries.iter().filter(|s| s.id != summary.id).cloned().collect();
                next.push(summary);
                store.set_summaries(next, cx);
                cx.notify();
            });
            Ok(id)
        })
    }
}
