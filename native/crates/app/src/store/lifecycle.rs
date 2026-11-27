//! Store startup, Host event pump, `on_open` bootstrap, preferences sync, maintenance, quit and restart.

use std::sync::{Arc, mpsc};

use gpui_kit::*;
use rainy_carrier::{ConnectionState, ExecutionTarget, RpcClient, RpcEvent};
use rainy_protocol::{
    AppInfo, ExternalStatusResponse, ModelsStatus, SessionEntryEvent, SessionStateEvent, SessionSummary, SessionToolEvent, UiPreferences, parse_stream_event,
};
use serde_json::{Value, json};

use crate::boot::{self, CarrierEvents};
use crate::i18n::Locale;
use crate::store::sessions::PendingChatEvent;
use crate::store::{QUIT_FLUSH_TIMEOUT, Phase, RuntimePrompt, Shutdown, SshAutoReconnect, Store, StoreEvent, hide_windows, ide, update};

impl Store {
    pub(super) fn wire_carrier_events(&self, cx: &mut Context<Self>) -> (Arc<CarrierEvents>, async_channel::Receiver<Option<i32>>) {
        let events = CarrierEvents::new(&self.paths);
        // A Host that ends after startup is reported once, unless Rainy is quitting.
        let (exit_tx, exit_rx) = async_channel::bounded::<Option<i32>>(1);
        *events.on_exit.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Box::new(move |code| {
            let _ = exit_tx.try_send(code);
        }));
        // The agent's preview commands run in the window, one after another.
        let (preview_tx, preview_rx) = async_channel::unbounded::<(Value, mpsc::Sender<Value>)>();
        *events.preview.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(preview_tx);
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
        *events.credential_alert.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(alert_tx);
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

    pub(super) fn spawn_exit_watcher(&self, generation: u64, exit_rx: async_channel::Receiver<Option<i32>>, cx: &mut Context<Self>) {
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

    pub(super) fn spawn_rpc_pump(&self, generation: u64, events: async_channel::Receiver<RpcEvent>, cx: &mut Context<Self>) {
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

    pub(super) fn start(&mut self, cx: &mut Context<Self>) {
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
                        if store.host_generation == generation {
                            store.phase = Phase::Failed(format!("{error:#}").into());
                            cx.notify();
                        }
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
                        if store.host_generation == generation {
                            store.phase = Phase::Failed(format!("{error:#}").into());
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            let host_fallback = host.clone();
            let rpc_fallback = rpc.clone();
            let installed = this
                .update(cx, |store, cx| {
                    if store.stopping || store.quitting || store.host_generation != generation {
                        return false;
                    }
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
                    true
                })
                .unwrap_or(false);
            if !installed {
                rpc_fallback.close();
                cx.background_spawn(async move { host_fallback.stop(); }).detach();
            }
        });
        self.startup_task = Some(task);
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
        self.startup_task = None;
        self.active_swap_task = None;
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

    /// The socket opened (first time or after a reconnect): load core state concurrently; `external.status` runs
    /// asynchronously in the background so scanning PATH never delays `Phase::Ready`.
    fn on_open(&mut self, cx: &mut Context<Self>) {
        let Some(rpc) = self.rpc.clone() else { return };
        let generation = self.host_generation;
        let rpc_external = rpc.clone();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            match rpc_external.request::<ExternalStatusResponse>("external.status", json!({})).await {
                Ok(external) => {
                    let payload = serde_json::to_value(&external).unwrap_or(Value::Null);
                    let _ = this.update(cx, |store, cx| {
                        if store.host_generation != generation {
                            return;
                        }
                        store.external_agents = external.agents;
                        cx.emit(StoreEvent::Host { name: "external.changed".to_owned(), data: payload });
                        cx.notify();
                    });
                }
                Err(error) => {
                    log::debug!("external.status unavailable: {} {}", error.code, error.message);
                }
            }
        })
        .detach();

        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let info_task = cx.background_spawn({
                let rpc = rpc.clone();
                async move { rpc.request::<AppInfo>("app.info", Value::Null).await }
            });
            let prefs_task = cx.background_spawn({
                let rpc = rpc.clone();
                async move { rpc.request::<UiPreferences>("prefs.get", Value::Null).await }
            });
            let models_task = cx.background_spawn({
                let rpc = rpc.clone();
                async move { rpc.request::<ModelsStatus>("models.status", Value::Null).await }
            });
            let summaries_task = cx.background_spawn({
                let rpc = rpc.clone();
                async move { rpc.request::<Vec<SessionSummary>>("sessions.list", json!({})).await }
            });
            let (info, prefs, models, summaries) = (info_task.await, prefs_task.await, models_task.await, summaries_task.await);
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
                    cx.emit(StoreEvent::Host { name: "models.changed".to_owned(), data: Value::Null });
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
        })
        .detach();
    }

    fn on_event(&mut self, name: &str, data: Value, cx: &mut Context<Self>) {
        let session = |data: &Value| data.get("sessionId").and_then(Value::as_str).map(str::to_owned);
        match name {
            "session.entry" => {
                if let Ok(event) = serde_json::from_value::<SessionEntryEvent>(data) {
                    let id = event.session_id.clone();
                    self.deliver_chat_event(&id, PendingChatEvent::Entry(event), cx);
                }
            }
            "session.stream" => {
                if let Some((id, update)) = parse_stream_event(&data) {
                    self.deliver_chat_event(&id, PendingChatEvent::Stream(update), cx);
                }
            }
            "session.tool" => {
                if let Ok(event) = serde_json::from_value::<SessionToolEvent>(data) {
                    let id = event.session_id.clone();
                    self.deliver_chat_event(&id, PendingChatEvent::Tool(event), cx);
                }
            }
            "session.state" => {
                if let Ok(event) = serde_json::from_value::<SessionStateEvent>(data) {
                    let id = event.session_id.clone();
                    let status = event.status.clone();
                    let awaiting = !event.approvals.is_empty();
                    self.deliver_chat_event(&id, PendingChatEvent::State(event), cx);
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
                    let id = summary.id.clone();
                    self.deliver_chat_event(&id, PendingChatEvent::Summary(summary.clone()), cx);
                    self.upsert_summary(summary, cx);
                    cx.notify();
                }
            }
            "sessions.removed" => {
                if let Some(id) = session(&data) {
                    self.remove_session(&id, cx);
                }
            }
            "prefs.changed" => {
                if let Ok(prefs) = serde_json::from_value::<UiPreferences>(data) {
                    self.set_prefs(prefs, cx);
                    cx.notify();
                }
            }
            "models.changed" => {
                if let Ok(models) = serde_json::from_value::<ModelsStatus>(data.clone()) {
                    self.models = Some(models);
                    cx.emit(StoreEvent::Host { name: name.to_owned(), data });
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
                    cx.emit(StoreEvent::Host { name: name.to_owned(), data });
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

    /// New interface preferences; the language is also recorded for messages shown before the next start's Host answers.
    pub(super) fn set_prefs(&mut self, prefs: UiPreferences, cx: &mut Context<Self>) {
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
}
