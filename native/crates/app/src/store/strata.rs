//! The Strata local model card in Settings → Models (3.x `strata-controller.ts` and the `rainy:strata-*` handlers): the
//! engine manager of the optional `strata` module, its status, and the user's explicit operations. Nothing starts a
//! model unless the user asks.

use std::sync::Arc;

use gpui_kit::*;
use rainy_carrier::strata::{Connection, Manager, Settings, Status};
use serde_json::{Value, json};

use super::Store;

/// The card's state.
#[derive(Default)]
pub struct Strata {
    pub manager: Option<Arc<Manager>>,
    pub status: Option<Status>,
    /// Reading the status.
    pub loading: bool,
    /// The operation in progress: `save`, `start`, `stop` or `connect`.
    pub pending: Option<&'static str>,
    /// Why the status could not be read.
    pub error: Option<String>,
}

impl Store {
    /// The modules were read: create the engine manager over the `strata` module's folder.
    pub(super) fn strata_ready(&mut self, cx: &mut Context<Self>) {
        if self.strata.manager.is_some() {
            return;
        }
        let root = match self.modules.manager.path("strata") {
            Ok(root) => root,
            Err(error) => {
                // The card says why instead of waiting for a manager that never comes.
                self.strata.error = Some(crate::boot::module_message(self.locale(), &error));
                cx.notify();
                return;
            }
        };
        let (changed_tx, changed) = async_channel::unbounded::<()>();
        let manager = Manager::new(root, &self.paths.user_data, move || {
            let _ = changed_tx.try_send(());
        });
        self.strata.manager = Some(manager);
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            while changed.recv().await.is_ok() {
                if this.update(cx, |store, cx| store.refresh_strata(false, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Read the status; `probe` also asks the server on the port.
    pub fn refresh_strata(&mut self, probe: bool, cx: &mut Context<Self>) {
        let Some(manager) = self.strata.manager.clone() else { return };
        self.strata.loading = true;
        cx.notify();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let status = cx.background_spawn(async move { manager.status(probe) }).await;
            let _ = this.update(cx, |store, cx| {
                store.strata.loading = false;
                match status {
                    Ok(status) => {
                        store.strata.status = Some(status);
                        store.strata.error = None;
                    }
                    Err(error) => store.strata.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Run one operation off the interface thread; the new status replaces the shown one, and failures come back to the
    /// caller to show.
    fn strata_operation(&mut self, name: &'static str, cx: &mut Context<Self>, run: impl FnOnce(Arc<Manager>) -> anyhow::Result<Status> + Send + 'static) -> Task<Result<Status, String>> {
        let Some(manager) = self.strata.manager.clone() else { return Task::ready(Err(String::new())) };
        if self.strata.pending.is_some() {
            return Task::ready(Err(String::new()));
        }
        self.strata.pending = Some(name);
        cx.notify();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let result = cx.background_spawn(async move { run(manager) }).await.map_err(|error| error.to_string());
            let _ = this.update(cx, |store, cx| {
                store.strata.pending = None;
                if let Ok(status) = &result {
                    store.strata.status = Some(status.clone());
                }
                cx.notify();
            });
            result
        })
    }

    pub fn save_strata(&mut self, settings: Settings, cx: &mut Context<Self>) -> Task<Result<Status, String>> {
        self.strata_operation("save", cx, move |manager| manager.save(settings))
    }

    pub fn start_strata(&mut self, cx: &mut Context<Self>) -> Task<Result<Status, String>> {
        self.strata_operation("start", cx, |manager| manager.start())
    }

    pub fn stop_strata(&mut self, cx: &mut Context<Self>) -> Task<Result<Status, String>> {
        self.strata_operation("stop", cx, |manager| manager.stop())
    }

    /// Have the selected Host check the loaded server and save it as the default model (`models.connectStrata`).
    pub fn connect_strata(&mut self, cx: &mut Context<Self>) -> Task<Result<Value, String>> {
        let Some(manager) = self.strata.manager.clone() else { return Task::ready(Err(String::new())) };
        if self.strata.pending.is_some() {
            return Task::ready(Err(String::new()));
        }
        self.strata.pending = Some("connect");
        cx.notify();
        let locale = self.locale();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let connection: Result<Connection, String> = cx.background_spawn(async move { manager.connection() }).await.map_err(|error| error.to_string());
            let call = match connection {
                Ok(connection) => this.update(cx, |store, _| store.call("models.connectStrata", json!({ "baseURL": connection.base_url }))).ok(),
                Err(error) => {
                    let _ = this.update(cx, |store, cx| {
                        store.strata.pending = None;
                        cx.notify();
                    });
                    return Err(error);
                }
            };
            let result = match call {
                Some(call) => call.await.map_err(|error| crate::i18n::error_text(locale, &error)),
                None => Err(String::new()),
            };
            let _ = this.update(cx, |store, cx| {
                store.strata.pending = None;
                cx.notify();
            });
            result
        })
    }

    /// Stop the engine Rainy started, for quitting.
    pub fn strata_manager(&self) -> Option<Arc<Manager>> {
        self.strata.manager.clone()
    }
}
