//! Rainy's desktop interface: one GPUI window over the Host (`src/host`), which runs on Windows, in WSL or over SSH.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod boot;
mod i18n;
mod launch;
mod placement;
mod release;
mod store;
mod theme;
mod ui;
mod web;

use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::component::TitleBar;
use gpui_kit::*;

use crate::i18n::Locale;
use crate::store::Store;
use crate::ui::workbench::Workbench;

/// Switches the uninstaller passes, in the order they are looked for.
const MAINTENANCE: [&str; 2] = ["--rainy-remove-runtime", "--rainy-unregister-shell"];

/// Until the window is open, a panic means Rainy could not start.
static STARTING: AtomicBool = AtomicBool::new(true);

fn main() {
    // Started by `ssh` to ask for a password or a host key: pass the question to the running Rainy and print the answer.
    if let Some(code) = rainy_carrier::askpass::client() {
        std::process::exit(code);
    }
    if let Some(code) = maintenance(&std::env::args_os().collect::<Vec<_>>()) {
        std::process::exit(code);
    }
    // GPUI's DirectComposition tree is drawn above child windows, which would hide the WebView2 panes; drawing to the
    // window itself keeps them on top of the area they cover.
    // SAFETY: set before any other thread exists.
    unsafe { std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "1") };
    let user_data = match boot::user_data_dir() {
        Ok(user_data) => user_data,
        Err(error) => {
            // The log lives in that folder, so this is not logged.
            eprintln!("Rainy could not start: {error:#}");
            startup_failed(Locale::default(), &error)
        }
    };
    rainy_carrier::logging::init(&user_data.join(rainy_carrier::logging::FILE_NAME));
    let locale = boot::saved_locale(&user_data);
    report_startup_panics(locale);
    let keys = if release::keyring().is_some() { "with release keys" } else { "without release keys" };
    log::info!("Rainy {} starting, {keys}", env!("CARGO_PKG_VERSION"));
    // One Rainy per profile: a later launch, as from Explorer, goes to the window already open and this process exits.
    let argv: Vec<OsString> = std::env::args_os().collect();
    let forwarded = launch::claim(&user_data, &argv);
    let paths = boot::Paths::discover(user_data).unwrap_or_else(|error| startup_failed(locale, &error));
    gpui_kit::application().with_assets(assets::RainyAssets).run(move |cx| {
        // The id 3.x gave its shortcuts and taskbar pins, so the window groups with them.
        cx.set_app_identity("dev.rainy.agent", "Rainy");
        gpui_kit::init(cx);
        let user_data = paths.user_data.clone();
        let store = Store::init(paths, cx);
        let ide = store::ide::Ide::init(store.clone(), cx);
        store::execution::Execution::init(store.clone(), ide, cx);
        launch::init(&argv, cx);
        ui::ide::bind_keys(cx);
        let prefs = store.read(cx).prefs.clone();
        theme::apply(&prefs, None, cx);

        // The Host stops with the application; restart and quit end here without closing the window first.
        let quitting = store.clone();
        cx.on_app_quit(move |cx| {
            placement::save(cx);
            let modules = quitting.update(cx, |store, _| {
                store.shutdown();
                store.modules.manager.clone()
            });
            // An install stops at its next chunk; waiting for it keeps the module folders whole.
            modules.wait_idle(std::time::Duration::from_secs(10));
            async {}
        })
        .detach();

        let mut options = TitleBar::window_options();
        let opening = placement::Opening::new(&user_data, cx);
        opening.apply(&mut options);
        let opened = gpui_kit::open_window(options, cx, |window, cx| {
            window.set_window_title("Rainy");
            placement::track(&user_data, window, cx);
            cx.new(|cx| Workbench::new(store.clone(), window, cx))
        });
        let window = match opened {
            Ok((window, _)) => window,
            Err(error) => {
                report_startup_failure(locale, &format!("Rainy could not open its window: {error:#}"));
                cx.quit();
                return;
            }
        };
        // Shown only now, in its place.
        let _ = window.update(cx, |_, window, _| opening.show(window));
        STARTING.store(false, Ordering::Relaxed);
        launch::listen(forwarded, window, cx);

        cx.on_window_closed(|cx, _| {
            placement::save(cx);
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}

/// The uninstaller's switches, which run without a window or the single-instance lock: the exit code, 0 or 1 after a
/// failure, or `None` for an ordinary start.
fn maintenance(args: &[OsString]) -> Option<i32> {
    let switch = MAINTENANCE.into_iter().find(|name| args.iter().skip(1).any(|arg| arg == name))?;
    let done: anyhow::Result<()> = match switch {
        "--rainy-remove-runtime" => remove_runtime(),
        _ => unregister_shell(),
    };
    if let Err(error) = &done {
        eprintln!("{switch} failed: {error:#}");
    }
    Some(i32::from(done.is_err()))
}

/// The uninstaller removes the installed Windows runtime through Rainy, file by file from its inventory; a build that
/// is not installed removes nothing.
fn remove_runtime() -> anyhow::Result<()> {
    if cfg!(debug_assertions) {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    if let Some(resources) = exe.parent().and_then(rainy_carrier::maintenance::installed_resources) {
        rainy_carrier::maintenance::remove_runtime(&resources)?;
    }
    Ok(())
}

/// The uninstaller removes the Explorer entries and file types through the code that wrote them, keeping the keys
/// desktop.json remembers as the user's own; for a part without that record only Rainy's own keys and values go. Only a
/// release build outside a Cargo build folder removes anything, and it does not need its runtime, so the order of the
/// uninstaller's steps does not matter; entries of another Rainy installation that is still there stay.
fn unregister_shell() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    if cfg!(debug_assertions) || exe.parent().is_none_or(|folder| rainy_carrier::maintenance::checkout(folder).is_some()) {
        return Ok(());
    }
    let executable = exe.to_str().ok_or_else(|| anyhow::anyhow!("The path of Rainy is not valid Unicode: {}", exe.display()))?;
    let settings = boot::user_data_path()?.join("desktop.json");
    rainy_carrier::shell::unregister_saved(&mut rainy_carrier::shell::CurrentUser, &settings, executable)?;
    Ok(())
}

fn startup_failed(locale: Locale, error: &anyhow::Error) -> ! {
    report_startup_failure(locale, &format!("{error:#}"));
    std::process::exit(1);
}

/// Log why Rainy could not start; a release build, which has no console, also says so in a message box.
fn report_startup_failure(locale: Locale, detail: &str) {
    log::error!("Rainy could not start: {detail}");
    if !cfg!(debug_assertions) {
        error_box(i18n::text(locale, "native.startFailed"), detail);
    }
}

/// A release build aborts on a panic; one before the window is open is shown like any other failure to start. The log
/// has it already.
fn report_startup_panics(locale: Locale) {
    if cfg!(debug_assertions) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        if STARTING.load(Ordering::Relaxed) {
            error_box(i18n::text(locale, "native.startFailed"), &rainy_carrier::logging::panic_message(info));
        }
    }));
}

fn error_box(message: &str, detail: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW};
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (text, caption) = (wide(&format!("{message}\n\n{detail}")), wide("Rainy"));
    // SAFETY: both strings end with NUL and outlive the call; no owner window.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_OK | MB_ICONERROR | MB_SETFOREGROUND) };
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    #[test]
    fn keeps_its_own_info_in_the_log() {
        assert!(rainy_carrier::logging::is_rainy(module_path!()));
    }
}
