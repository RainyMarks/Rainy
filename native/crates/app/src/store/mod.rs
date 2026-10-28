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

use gpui_kit::*;
use rainy_carrier::askpass::{Askpass, Question};
use rainy_carrier::trust::{Integrity, TrustError};
use rainy_carrier::{ConnectionState, HostProcess, RpcClient, RpcEvent};
use rainy_protocol::{
    AppInfo, ModelsStatus, RpcError, SessionEntryEvent, SessionSnapshot, SessionStateEvent, SessionSummary,
    SessionToolEvent, UiPreferences, parse_stream_event,
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

/// What changed, for views that only care about part of the state.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreEvent {
    PrefsChanged,
    SummariesChanged,
    SelectionChanged,
    /// A Host event other views follow (`ide.hint`, `memory.changed`, …).
    Host { name: String, data: Value },
    /// The Host ended while the interface was using it; the exit code, if it had one.
    HostExited(Option<i32>),
    /// The daily recheck found installed files changed or damaged.
    ResourcesDamaged(TrustError),
    /// Something about an update to show.
    Update(update::UpdateNotice),
}

/// A question `ssh` asks, waiting for the window to show it; dropping `reply` cancels it.
pub struct AskpassRequest {
    pub question: Question,
    pub reply: mpsc::Sender<Option<String>>,
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
    pub info: Option<AppInfo>,
    pub prefs: UiPreferences,
    pub models: Option<ModelsStatus>,
    pub summaries: Vec<SessionSummary>,
    pub chats: HashMap<String, Entity<ChatModel>>,
    pub selected: Option<String>,
    /// Counts the changes of `selected`, so work that finishes later (a chat created for a message) can tell whether
    /// the user moved on meanwhile.
    pub selections: u64,
    /// The permission mode picked for the next new chat; `None` starts it in the default from Settings.
    pub pending_permission: Option<String>,
    /// Explorer's "Open with Rainy" and the file types Rainy is offered for.
    pub shell: shell::Shell,
    /// Rainy's own updates.
    pub updates: update::Updates,
    /// Exporting the diagnostics zip.
    pub diagnostics: diagnostics::Diagnostics,
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
                questions_tx.send_blocking(AskpassRequest { question, reply }).ok()?;
                answer.recv().ok().flatten()
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
                info: None,
                prefs: UiPreferences::default(),
                models: None,
                summaries: Vec::new(),
                chats: HashMap::new(),
                selected: None,
                selections: 0,
                pending_permission: None,
                shell,
                updates,
                diagnostics: diagnostics::Diagnostics::default(),
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

    fn start(&mut self, cx: &mut Context<Self>) {
        let paths = self.paths.clone();
        let events = CarrierEvents::new(&paths);
        let (progress_tx, progress_rx) = async_channel::unbounded::<boot::Step>();
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
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            if let Ok(code) = exit_rx.recv().await {
                cx.update(|_| crate::web::preview::close());
                let _ = this.update(cx, |store, cx| {
                    if !store.stopping && store.host.is_some() {
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
                    let _ = this.update(cx, |store, cx| {
                        store.phase = Phase::Failed(format!("{error:#}").into());
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |store, cx| {
                store.host = Some(host);
                store.target = Some(target);
                store.rpc = Some(rpc);
                store.integrity = integrity.map(Arc::new);
                store.prune = prune;
                cx.notify();
            });
            while let Ok(event) = events.recv().await {
                if this.update(cx, |store, cx| store.handle(event, cx)).is_err() {
                    break;
                }
            }
        });
        self._tasks.push(task);
    }

    /// The Host's launch address, which exchanges its token for the session cookie; pages in WebView2 start there.
    pub fn launch_url(&self) -> Option<String> {
        self.host.as_ref().map(|host| host.ready().url.clone())
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

    /// Stop the Host; called when the application quits.
    pub fn shutdown(&mut self) {
        self.stopping = true;
        // A download stops; what it received is kept for the next attempt.
        self.modules.manager.close();
        self.updates.cancel();
        if let Some(rpc) = self.rpc.take() {
            rpc.close();
        }
        if let Some(host) = self.host.take() {
            host.stop();
        }
    }

    fn handle(&mut self, event: RpcEvent, cx: &mut Context<Self>) {
        match event {
            RpcEvent::State(state) => {
                let reopened = state == ConnectionState::Open;
                self.connection = state;
                if reopened {
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
        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let info = rpc.request::<AppInfo>("app.info", Value::Null).await;
            let prefs = rpc.request::<UiPreferences>("prefs.get", Value::Null).await;
            let models = rpc.request::<ModelsStatus>("models.status", Value::Null).await;
            let summaries = rpc.request::<Vec<SessionSummary>>("sessions.list", json!({})).await;
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
            let _ = this.update(cx, |store, cx| {
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

    /// The chat with this id, loading it the first time.
    pub fn chat(&mut self, id: &str, cx: &mut Context<Self>) -> Entity<ChatModel> {
        if let Some(chat) = self.chats.get(id) {
            return chat.clone();
        }
        let chat = cx.new(|_| ChatModel { id: id.to_owned(), view: None, error: None, whole: false, loading_whole: false, loaded: 0, load: 0 });
        self.chats.insert(id.to_owned(), chat.clone());
        self.load_chat(id, false, cx);
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
        }
        if self.selected != id {
            self.selections += 1;
        }
        self.selected = id;
        cx.emit(StoreEvent::SelectionChanged);
        cx.notify();
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

    /// Start Rainy again with this run's switches, then quit. The new process waits for this one to exit before it
    /// takes the single-instance lock, and opens none of the paths this run was launched with.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        if let Ok(exe) = std::env::current_exe() {
            self.shutdown();
            let arguments = rainy_carrier::launch::relaunch_arguments(&std::env::args_os().collect::<Vec<_>>(), std::process::id());
            if let Err(error) = std::process::Command::new(exe).args(arguments).spawn() {
                log::warn!("Rainy could not start again: {error}");
            }
        }
        cx.quit();
    }

    /// Remember `target` and restart into it; refused while the Host is busy.
    pub fn switch_target(&mut self, target: ExecutionTarget, window: AnyWindowHandle, cx: &mut Context<Self>) {
        let settings = self.paths.desktop_settings();
        let host = self.host.clone();
        let locale = self.locale();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let busy = match host.clone() {
                Some(host) => cx.background_spawn(async move { host.inspect_activity("freeze").unwrap_or(false) }).await,
                None => false,
            };
            if busy {
                if let Some(host) = host {
                    cx.background_spawn(async move { host.inspect_activity("resume") }).detach();
                }
                let text = crate::i18n::text(locale, "native.busyBeforeSwitch");
                let _ = window.update(cx, |_, window, cx| {
                    use gpui_kit::component::WindowExt as _;
                    window.push_notification(gpui_kit::component::notification::Notification::warning(text), cx)
                });
                return;
            }
            if let Err(error) = rainy_carrier::targets::save_target(&settings, &target) {
                log::warn!("The execution target was not saved: {error:#}");
            }
            let _ = this.update(cx, |store, cx| store.restart(cx));
        })
        .detach();
    }

    /// Start a chat in a project, or outside any project, in the mode picked for it (`pending_permission`, used up
    /// once the chat exists); resolves with its id. The caller selects it, unless `selections` moved on meanwhile: a
    /// user who went to another chat stays there.
    pub fn create_chat(&mut self, workspace_id: Option<String>, cx: &mut Context<Self>) -> Task<Result<String, RpcError>> {
        let mut params = json!({ "workspaceId": workspace_id });
        if let Some(mode) = &self.pending_permission {
            params["permission"] = json!(mode);
        }
        let request = self.call("sessions.create", params);
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let summary = serde_json::from_value::<SessionSummary>(request.await?)
                .map_err(|error| RpcError { code: "invalid-response".into(), message: error.to_string(), data: None })?;
            let id = summary.id.clone();
            let _ = this.update(cx, |store, cx| {
                store.pending_permission = None;
                let mut next: Vec<SessionSummary> = store.summaries.iter().filter(|s| s.id != summary.id).cloned().collect();
                next.push(summary);
                store.set_summaries(next, cx);
                cx.notify();
            });
            Ok(id)
        })
    }
}
