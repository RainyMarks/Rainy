//! Rainy's desktop interface: one GPUI window over the Host (`src/host`), which runs on Windows, in WSL or over SSH.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod boot;
mod i18n;
mod store;
mod theme;
mod ui;

use gpui_kit::component::TitleBar;
use gpui_kit::*;

use crate::store::Store;
use crate::ui::workbench::Workbench;

fn main() {
    let paths = match boot::Paths::discover() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("Rainy cannot start: {error:#}");
            std::process::exit(1);
        }
    };
    gpui_kit::application().with_assets(assets::RainyAssets).run(move |cx| {
        gpui_kit::init(cx);
        let store = Store::init(paths, cx);
        let ide = store::ide::Ide::init(store.clone(), cx);
        store::execution::Execution::init(store.clone(), ide, cx);
        ui::ide::bind_keys(cx);
        let prefs = store.read(cx).prefs.clone();
        theme::apply(&prefs, None, cx);

        let mut options = TitleBar::window_options();
        options.window_bounds = Some(WindowBounds::centered(size(px(1380.), px(920.)), cx));
        options.window_min_size = Some(size(px(720.), px(480.)));
        gpui_kit::open_window(options, cx, |window, cx| {
            window.set_window_title("Rainy");
            cx.new(|cx| Workbench::new(store.clone(), window, cx))
        })
        .expect("Rainy could not open its window");

        // The Host stops with the application.
        let quitting = store.clone();
        cx.on_app_quit(move |cx| {
            quitting.update(cx, |store, _| store.shutdown());
            async {}
        })
        .detach();
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
