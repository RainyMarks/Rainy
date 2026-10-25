//! Optional components in Settings → Runtime (3.x `OptionalModules.tsx` and the `rainy:modules-*` handlers): which
//! modules this version offers, whether they are installed, and their download, cancellation and removal.

use std::sync::Arc;

use gpui_kit::*;
use rainy_carrier::modules::{ModuleError, Modules, Phase, State, Status};

use super::Store;

/// The modules Settings offers.
pub const OFFERED: [&str; 2] = ["strata", "php"];

/// The modules as the settings page shows them.
pub struct OptionalModules {
    pub manager: Arc<Modules>,
    /// The installed resources were accepted, so the descriptor may be read.
    ready: bool,
    /// The offered modules once read, or why they cannot be.
    pub statuses: Option<Result<Vec<Status>, ModuleError>>,
    /// The latest operation.
    pub state: State,
    /// A download or removal started on the page has not finished.
    pub busy: bool,
}

impl OptionalModules {
    pub fn new(manager: Arc<Modules>) -> Self {
        Self { manager, ready: false, statuses: None, state: State::default(), busy: false }
    }

    /// Whether a module is downloading or being unpacked.
    pub fn running(&self) -> bool {
        matches!(self.state.phase, Phase::Downloading | Phase::Installing)
    }
}

impl Store {
    /// The installed resources were accepted: the modules can be listed.
    pub(super) fn modules_ready(&mut self, cx: &mut Context<Self>) {
        self.modules.ready = true;
        self.refresh_modules(cx);
        self.strata_ready(cx);
    }

    /// A module operation published its state.
    pub(super) fn module_state(&mut self, state: State, cx: &mut Context<Self>) {
        let complete = state.phase == Phase::Complete;
        self.modules.state = state;
        if complete {
            self.refresh_modules(cx);
            // The engine may have been installed or removed.
            self.refresh_strata(false, cx);
        }
        cx.notify();
    }

    /// Read which offered modules are installed.
    pub fn refresh_modules(&mut self, cx: &mut Context<Self>) {
        if !self.modules.ready {
            return;
        }
        let manager = self.modules.manager.clone();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let statuses = cx
                .background_spawn(async move { manager.status().map(|list| list.into_iter().filter(|status| OFFERED.contains(&status.id.as_str())).collect()) })
                .await;
            let _ = this.update(cx, |store, cx| {
                store.modules.statuses = Some(statuses);
                cx.notify();
            });
        })
        .detach();
    }

    /// Download, verify and unpack a module; its progress arrives as published states.
    pub fn install_module(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let manager = self.modules.manager.clone();
        self.run_module(id, cx, move || manager.install(id, &|_, _| {}));
    }

    /// Delete an installed module.
    pub fn remove_module(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let manager = self.modules.manager.clone();
        // The engine Rainy started runs from the module's folder, which cannot be removed while it does (3.x stopped
        // it first too).
        let strata = if id == "strata" { self.strata.manager.clone() } else { None };
        self.run_module(id, cx, move || {
            if let Some(strata) = strata
                && let Err(error) = strata.stop()
            {
                log::warn!("Strata was not stopped before its removal: {error:#}");
            }
            manager.remove(id)
        });
    }

    /// Stop the running download; what was downloaded is kept for the next attempt.
    pub fn cancel_module(&mut self) {
        self.modules.manager.cancel();
    }

    fn run_module(&mut self, id: &'static str, cx: &mut Context<Self>, operation: impl FnOnce() -> Result<(), ModuleError> + Send + 'static) {
        if !self.modules.ready || self.modules.busy {
            return;
        }
        self.modules.busy = true;
        cx.notify();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let result = cx.background_spawn(async move { operation() }).await;
            let _ = this.update(cx, |store, cx| {
                store.modules.busy = false;
                // Also failures the operation does not publish: a removal, or another download running.
                if let Err(error) = result
                    && !error.cancelled()
                {
                    log::warn!("The component {id} was not changed: {error}");
                    store.modules.state = State { phase: Phase::Error, module: Some(id.to_owned()), error: Some(error), ..State::default() };
                }
                store.refresh_modules(cx);
                if id == "strata" {
                    store.refresh_strata(false, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}
