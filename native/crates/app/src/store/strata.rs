//! Strata in Settings → Tools: Rainy's Strata manager and its status. Nothing is downloaded, started or removed unless
//! the user asks; an update to Strata's newest release installs by itself when no model runs.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;
use rainy_carrier::strata::{Choice, Connection, Manager, Settings, Status};
use serde_json::{Value, json};

use super::Store;

/// The tool's state.
#[derive(Default)]
pub struct Strata {
    pub manager: Option<Arc<Manager>>,
    pub status: Option<Status>,
    /// Connecting the loaded server as the default model.
    pub connecting: bool,
}

impl Store {
    /// The installed resources were accepted: create the manager and look at the port and the disk.
    pub(super) fn strata_ready(&mut self, cx: &mut Context<Self>) {
        if self.strata.manager.is_some() {
            return;
        }
        // One pending notice is enough: the status is read when it is handled, so a burst of progress is one repaint.
        let (changed_tx, changed) = async_channel::bounded::<()>(1);
        let manager = Manager::new(&self.paths.user_data, move || {
            let _ = changed_tx.try_send(());
        });
        manager.watch();
        self.strata.status = Some(manager.status());
        self.strata.manager = Some(manager.clone());
        let reader = manager.clone();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            while changed.recv().await.is_ok() {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let status = reader.status();
                if this
                    .update(cx, |store, cx| {
                        store.strata.status = Some(status);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.background_spawn(async move { manager.refresh() }).detach();
    }

    /// Look at the port and the disk again, and read the catalog when it is not known yet.
    pub fn refresh_strata(&mut self, cx: &mut Context<Self>) {
        let Some(manager) = self.strata.manager.clone() else { return };
        cx.background_spawn(async move {
            manager.refresh();
            if manager.status().installed.is_some() && manager.status().catalog.is_none() {
                manager.load_catalog();
            }
        })
        .detach();
    }

    /// Run one request of the manager off the interface thread; a refusal comes back to show.
    pub fn strata_do(&mut self, cx: &mut Context<Self>, run: impl FnOnce(&Arc<Manager>) -> anyhow::Result<()> + Send + 'static) -> Task<Result<(), String>> {
        let Some(manager) = self.strata.manager.clone() else { return Task::ready(Err(String::new())) };
        cx.background_spawn(async move { run(&manager).map_err(|error| format!("{error:#}")) })
    }

    /// Set up model files the person already has, where they are.
    pub fn use_local_strata_model(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, move |manager| manager.use_local(&path))
    }

    pub fn use_strata_legacy(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, |manager| manager.use_legacy())
    }

    pub fn install_strata(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, |manager| manager.install())
    }

    pub fn update_strata(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, |manager| manager.update())
    }

    pub fn check_strata(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, |manager| manager.check_update())
    }

    pub fn uninstall_strata(&mut self, models: bool, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, move |manager| manager.uninstall(models))
    }

    pub fn save_strata(&mut self, settings: Settings, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, move |manager| manager.save(settings).map(|_| ()))
    }

    pub fn download_strata_model(&mut self, choice: Choice, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, move |manager| manager.download(choice))
    }

    pub fn remove_strata_model(&mut self, choice: Choice, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, move |manager| manager.remove_model(choice))
    }

    pub fn cancel_strata(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, |manager| {
            manager.cancel();
            Ok(())
        })
    }

    pub fn start_strata(&mut self, choice: Choice, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, move |manager| manager.start(choice))
    }

    pub fn stop_strata(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, |manager| manager.stop())
    }

    pub fn load_strata_catalog(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, |manager| {
            manager.load_catalog();
            Ok(())
        })
    }

    pub fn remove_strata_legacy(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.strata_do(cx, |manager| manager.remove_legacy())
    }

    /// Have the selected Host check the loaded server and save it as the default model (`models.connectStrata`).
    pub fn connect_strata(&mut self, cx: &mut Context<Self>) -> Task<Result<Value, String>> {
        let Some(manager) = self.strata.manager.clone() else { return Task::ready(Err(String::new())) };
        if self.strata.connecting {
            return Task::ready(Err(String::new()));
        }
        self.strata.connecting = true;
        cx.notify();
        let locale = self.locale();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let connection: Result<Connection, String> = cx.background_spawn(async move { manager.connection() }).await.map_err(|error| format!("{error:#}"));
            let result = match connection {
                Ok(connection) => match this.update(cx, |store, _| store.call("models.connectStrata", json!({ "baseURL": connection.base_url }))) {
                    Ok(call) => call.await.map_err(|error| crate::i18n::error_text(locale, &error)),
                    Err(_) => Err(String::new()),
                },
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |store, cx| {
                store.strata.connecting = false;
                cx.notify();
            });
            result
        })
    }
}
