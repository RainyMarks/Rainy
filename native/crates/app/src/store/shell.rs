//! Explorer integration in Settings → General: the choice saved in desktop.json, written to the registry by the installed
//! Rainy a few seconds after the workbench is ready, when the interface language changes and when a switch is turned
//! (3.x main.ts `syncShellIntegration` and the `rainy:shell-integration*` handlers).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use rainy_carrier::shell::{self, Change, CurrentUser, Language, ShellIntegration};
use rainy_carrier::targets;

use super::Store;
use crate::boot::Paths;
use crate::i18n::Locale;

/// How long after the workbench is ready the registry is first brought up to date (3.x `shellTimer`).
const SYNC_DELAY: Duration = Duration::from_secs(5);

/// The integration as the settings page shows it.
pub struct Shell {
    /// Only an installed release build writes to the registry (3.x `app.isPackaged`): a development build never
    /// registers itself.
    pub available: bool,
    /// The saved choice.
    pub choice: ShellIntegration,
    /// A switch was turned and its change is being written.
    pub busy: bool,
    scheduled: bool,
    /// The interface language last seen; a change rewrites the menu label and the type names.
    seen: Option<Locale>,
    registration: Arc<Registration>,
}

/// What a sync needs, shared with the background threads it runs on.
struct Registration {
    settings: PathBuf,
    executable: String,
    /// The language syncs write; read when one runs, so whichever runs last writes the latest.
    language: Mutex<Language>,
    /// Syncs run one after another.
    queue: Mutex<()>,
}

impl Registration {
    /// Bring the registry to the saved choice and log what changed.
    fn run(&self) -> anyhow::Result<()> {
        let _queue = self.queue.lock().unwrap();
        let language = *self.language.lock().unwrap();
        let synced = shell::sync_saved(&mut CurrentUser, &self.settings, &self.executable, language);
        let state = |on: bool| if on { "on" } else { "off" };
        match &synced {
            Ok(Some(wanted)) => {
                log::info!("Explorer integration updated: context menu {}, file types {}", state(wanted.context_menu), state(wanted.file_types));
            }
            Ok(None) => {}
            Err(error) => log::warn!("Explorer integration was not updated: {error:#}"),
        }
        synced.map(|_| ())
    }
}

impl Shell {
    pub fn new(paths: &Paths) -> Self {
        let settings = paths.desktop_settings();
        let executable = std::env::current_exe().ok().and_then(|exe| exe.to_str().map(str::to_owned));
        let choice = targets::read_preferences(&settings).map(|preferences| shell::preference(&preferences)).unwrap_or_default();
        Self {
            available: paths.packaged() && executable.is_some(),
            choice,
            busy: false,
            scheduled: false,
            seen: None,
            registration: Arc::new(Registration {
                settings,
                executable: executable.unwrap_or_default(),
                language: Mutex::new(Language::Chinese),
                queue: Mutex::new(()),
            }),
        }
    }
}

fn language(locale: Locale) -> Language {
    match locale {
        Locale::Zh => Language::Chinese,
        Locale::En => Language::English,
    }
}

impl Store {
    /// Once, when the workbench is first ready: bring the registry up to date a few seconds later, unless Rainy is
    /// quitting by then. A first start, a moved installation or a changed language rewrites the entries.
    pub(super) fn schedule_shell(&mut self, cx: &mut Context<Self>) {
        if !self.shell.available || self.shell.scheduled {
            return;
        }
        self.shell.scheduled = true;
        let registration = self.shell.registration.clone();
        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            cx.background_executor().timer(SYNC_DELAY).await;
            if this.update(cx, |store, _| store.stopping).unwrap_or(true) {
                return;
            }
            let _ = cx.background_spawn(async move { registration.run() }).await;
        });
        self._tasks.push(task);
    }

    /// Follow the interface language: the menu label and the file type names are rewritten when it changes.
    pub(super) fn follow_shell_language(&mut self, cx: &mut Context<Self>) {
        let locale = self.locale();
        *self.shell.registration.language.lock().unwrap() = language(locale);
        let changed = self.shell.seen.is_some_and(|seen| seen != locale);
        self.shell.seen = Some(locale);
        if changed && self.shell.available {
            let registration = self.shell.registration.clone();
            cx.background_spawn(async move {
                let _ = registration.run();
            })
            .detach();
        }
    }

    /// Save a switch and write it (`rainy:shell-integration-set`). A failure is shown in `window`, and the switch goes
    /// back to what it was, in desktop.json too, so the next start does not apply it unasked; the switches show what is
    /// saved either way.
    pub fn change_shell(&mut self, change: Change, window: AnyWindowHandle, cx: &mut Context<Self>) {
        if !self.shell.available || self.shell.busy {
            return;
        }
        self.shell.busy = true;
        cx.notify();
        let registration = self.shell.registration.clone();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let (written, saved) = cx
                .background_spawn(async move {
                    let written = shell::change_saved(&registration.settings, change, || registration.run());
                    (written, targets::read_preferences(&registration.settings).map(|preferences| shell::preference(&preferences)))
                })
                .await;
            let _ = this.update(cx, |store, cx| {
                store.shell.busy = false;
                if let Ok(choice) = saved {
                    store.shell.choice = choice;
                }
                cx.notify();
            });
            if let Err(error) = written {
                let _ = window.update(cx, |_, window, cx| window.push_notification(Notification::error(format!("{error:#}")), cx));
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;
    use crate::i18n;

    #[test]
    fn a_development_build_never_registers_itself() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("desktop.json"), r#"{"explorerIntegration": {"contextMenu": true}}"#).unwrap();
        let paths = Paths { user_data: folder.path().into(), resources: folder.path().into(), install_root: folder.path().into(), installed: true };
        let shell = Shell::new(&paths);
        assert_eq!(shell.choice, ShellIntegration { context_menu: true, file_types: false });
        assert_eq!(shell.available, !cfg!(debug_assertions));
    }

    #[test]
    fn the_settings_texts_are_the_3_9_1_ones() {
        for key in ["groupSystem", "contextMenuTitle", "contextMenuDescription", "fileTypesTitle", "fileTypesDescription", "defaultAppsTitle", "defaultAppsOpen", "defaultAppsDescription", "shellUnavailable"] {
            let key = format!("native.{key}");
            assert_ne!(i18n::text(Locale::Zh, &key), key);
            assert_ne!(i18n::text(Locale::En, &key), key);
        }
        assert_eq!(i18n::text(Locale::Zh, "native.shellUnavailable"), "只有安装版 Rainy 可以修改这些设置");
        assert_eq!(i18n::text(Locale::En, "native.defaultAppsOpen"), "Open system settings…");
    }
}

/// Open Windows' Default apps settings on Rainy's page (`rainy:default-apps`); a failure is shown in `window`.
pub fn open_default_apps(window: AnyWindowHandle, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(error) = cx.background_spawn(async move { shell::open_default_apps() }).await {
            let _ = window.update(cx, |_, window, cx| window.push_notification(Notification::error(format!("{error:#}")), cx));
        }
    })
    .detach();
}
