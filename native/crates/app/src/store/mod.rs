//! Application state kept current from the Host: connection, preferences, models, chat list and open chats.

pub mod chat;
pub mod execution;
pub mod ide;

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::*;
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
    Starting(SharedString),
    Ready,
    Failed(SharedString),
}

/// One chat the interface follows.
pub struct ChatModel {
    pub id: String,
    pub view: Option<ChatView>,
    pub error: Option<SharedString>,
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
}

pub struct Store {
    pub paths: Paths,
    pub phase: Phase,
    pub connection: ConnectionState,
    pub rpc: Option<RpcClient>,
    host: Option<Arc<HostProcess>>,
    /// Where the Host runs; known once startup has read the saved choice.
    pub target: Option<ExecutionTarget>,
    pub info: Option<AppInfo>,
    pub prefs: UiPreferences,
    pub models: Option<ModelsStatus>,
    pub summaries: Vec<SessionSummary>,
    pub chats: HashMap<String, Entity<ChatModel>>,
    pub selected: Option<String>,
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
        let store = cx.new(|cx| {
            let mut store = Store {
                paths,
                phase: Phase::Starting("正在启动执行后端…".into()),
                connection: ConnectionState::Connecting,
                rpc: None,
                host: None,
                target: None,
                info: None,
                prefs: UiPreferences::default(),
                models: None,
                summaries: Vec::new(),
                chats: HashMap::new(),
                selected: None,
                _tasks: Vec::new(),
            };
            store.start(cx);
            store
        });
        cx.set_global(GlobalStore(store.clone()));
        store
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let paths = self.paths.clone();
        let events = CarrierEvents::new(&paths);
        let (progress_tx, progress_rx) = async_channel::unbounded::<String>();
        let started = cx.background_spawn(async move {
            let (target, command) = boot::prepare(&paths, &|step| {
                let _ = progress_tx.try_send(step);
            })?;
            let host = HostProcess::start(&command, events)?;
            anyhow::Ok((target, Arc::new(host)))
        });
        // Startup steps (starting WSL, installing the runtime) are shown while they run.
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            while let Ok(step) = progress_rx.recv().await {
                let _ = this.update(cx, |store, cx| {
                    if matches!(store.phase, Phase::Starting(_)) {
                        store.phase = Phase::Starting(step.into());
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let (target, host) = match started.await {
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

    /// Stop the Host; called when the application quits.
    pub fn shutdown(&mut self) {
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
                    eprintln!("{method} failed: {} {}", error.code, error.message);
                }
            }
            let _ = this.update(cx, |store, cx| {
                match (&info, &prefs) {
                    (Ok(_), Ok(_)) => store.phase = Phase::Ready,
                    (Err(error), _) | (_, Err(error)) => store.phase = Phase::Failed(error.message.clone().into()),
                }
                if let Ok(info) = info {
                    store.info = Some(info);
                }
                if let Ok(prefs) = prefs {
                    store.prefs = prefs;
                    cx.emit(StoreEvent::PrefsChanged);
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
        self._tasks.push(task);
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
                        cx.emit(StoreEvent::SelectionChanged);
                    }
                    cx.emit(StoreEvent::SummariesChanged);
                    cx.notify();
                }
            }
            "prefs.changed" => {
                if let Ok(prefs) = serde_json::from_value::<UiPreferences>(data) {
                    self.prefs = prefs;
                    cx.emit(StoreEvent::PrefsChanged);
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
        let chat = cx.new(|_| ChatModel { id: id.to_owned(), view: None, error: None, load: 0 });
        self.chats.insert(id.to_owned(), chat.clone());
        self.load_chat(id, false, cx);
        chat
    }

    /// Load (or reload) a chat; a newer load replaces an older one that finishes later.
    pub fn load_chat(&mut self, id: &str, whole: bool, cx: &mut Context<Self>) {
        let (Some(rpc), Some(chat)) = (self.rpc.clone(), self.chats.get(id).cloned()) else { return };
        let load = chat.update(cx, |chat, _| {
            chat.load += 1;
            chat.load
        });
        let mut params = json!({ "sessionId": id });
        if !whole {
            params["limit"] = json!(INITIAL_ENTRIES);
        }
        let request = rpc.request::<SessionSnapshot>("sessions.get", params);
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            let result = request.await;
            let _ = chat.update(cx, |chat, cx| {
                if chat.load != load {
                    return;
                }
                match result {
                    Ok(snapshot) => {
                        chat.view = Some(ChatView::new(snapshot, whole));
                        chat.error = None;
                    }
                    Err(error) => chat.error = Some(error.message.into()),
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
        self.selected = id;
        cx.emit(StoreEvent::SelectionChanged);
        cx.notify();
    }

    /// Call a Host method; fails at once while the Host is not connected.
    pub fn call(&self, method: &str, params: Value) -> impl Future<Output = Result<Value, RpcError>> + use<> {
        let call = self.rpc.as_ref().map(|rpc| rpc.call(method, params));
        async move {
            match call {
                Some(call) => call.await,
                None => Err(RpcError { code: "disconnected".into(), message: "The execution backend is not connected.".into(), data: None }),
            }
        }
    }

    /// Start Rainy again with the same arguments, then quit this instance.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        if let Ok(exe) = std::env::current_exe() {
            self.shutdown();
            let _ = std::process::Command::new(exe).args(std::env::args_os().skip(1)).spawn();
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

    /// Create a chat in no project and select it; resolves with its id.
    /// Start a chat in a project, or outside any project.
    pub fn create_chat(&mut self, workspace_id: Option<String>, cx: &mut Context<Self>) -> Task<Option<String>> {
        let request = self.call("sessions.create", json!({ "workspaceId": workspace_id }));
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let summary = serde_json::from_value::<SessionSummary>(request.await.ok()?).ok()?;
            let id = summary.id.clone();
            this.update(cx, |store, cx| {
                let mut next: Vec<SessionSummary> = store.summaries.iter().filter(|s| s.id != summary.id).cloned().collect();
                next.push(summary);
                store.set_summaries(next, cx);
                store.select(Some(id.clone()), cx);
            })
            .ok()?;
            Some(id)
        })
    }
}
