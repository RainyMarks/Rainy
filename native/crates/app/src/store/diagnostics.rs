//! Settings → General → About → Diagnostics (3.x `rainy:export-diagnostics`): a zip of versions, logs and crash dumps,
//! saved where the user chooses and then shown in Explorer. Nothing is uploaded.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use chrono::Utc;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use rainy_carrier::diagnostics::{self, Account};

use super::Store;
use crate::ui::save_dialog::{self, SaveDialog};
use crate::ui::trf;

/// The GPUI this build is made with; a test keeps it in step with Cargo.lock.
const GPUI: &str = "gpui-kit 0.8.0, gpui-pre 0.3.9";
const UNKNOWN: &str = "unknown";

/// Whether an export is under way, which the button shows.
#[derive(Debug, Default)]
pub struct Diagnostics {
    pub exporting: bool,
}

/// What the store knows for `about.txt`, taken when the export starts.
struct Known {
    user_data: PathBuf,
    installed: bool,
    host: Option<String>,
    node: Option<String>,
    target: Option<String>,
    locale: String,
}

impl Store {
    /// Ask where to save the zip (`<Downloads>\Rainy-diagnostics-<UTC yyyyMMddHHmm>.zip`), write it, show it in Explorer
    /// and say where it is in `window`. A failure is shown there too; cancelling does nothing.
    pub fn export_diagnostics(&mut self, window: AnyWindowHandle, cx: &mut Context<Self>) {
        if self.diagnostics.exporting {
            return;
        }
        self.diagnostics.exporting = true;
        cx.notify();
        let known = Known {
            user_data: self.paths.user_data.clone(),
            installed: self.paths.packaged(),
            host: self.info.as_ref().map(|info| info.version.clone()),
            node: self.info.as_ref().and_then(|info| info.node.clone()),
            target: self.target.as_ref().map(|target| target.label().to_owned()),
            locale: self.prefs.locale.clone(),
        };
        let dialog = SaveDialog {
            // Not `tr`: it reads the store, which is being updated here.
            title: crate::i18n::text(self.locale(), "native.diagnosticsSaveTitle").to_owned(),
            folder: diagnostics::downloads_folder(),
            name: diagnostics::default_name(Utc::now()),
            type_name: "Zip".to_owned(),
            extension: "zip".to_owned(),
        };
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            // The window that asked is gone when it cannot be updated; nothing is saved then.
            let chosen = match window.update(cx, |_, window, _| save_dialog::save(window, dialog)) {
                Ok(answer) => answer.recv().await.ok(),
                Err(_) => None,
            };
            let written = match chosen {
                Some(Ok(Some(path))) => Some(cx.background_spawn(async move { export(&path, &known).map(|()| reveal(path)) }).await),
                Some(Ok(None)) | None => None,
                Some(Err(error)) => {
                    log::warn!("The save dialog for diagnostics failed: {error:#}");
                    Some(Err(error))
                }
            };
            let _ = this.update(cx, |store, cx| {
                store.diagnostics.exporting = false;
                cx.notify();
            });
            let Some(written) = written else { return };
            let _ = window.update(cx, |_, window, cx| {
                let notification = match written {
                    Ok(path) => Notification::success(trf(cx, "settings.diagnosticsSaved", &[("path", &path.display())])),
                    Err(error) => Notification::error(format!("{error:#}")),
                };
                window.push_notification(notification, cx);
            });
        })
        .detach();
    }
}

/// `about.txt` (3.x: Rainy, Installed, Electron, Chromium, Node, Windows, Execution target, Interface language, Memory,
/// Exported) with GPUI, WebView2 and the Host in place of Electron and Chromium.
fn about(known: &Known) -> Vec<(String, String)> {
    let or_unknown = |value: &Option<String>| value.clone().unwrap_or_else(|| UNKNOWN.to_owned());
    [
        ("Rainy", env!("CARGO_PKG_VERSION").to_owned()),
        ("Installed", known.installed.to_string()),
        ("GPUI", GPUI.to_owned()),
        ("WebView2", crate::web::webview::runtime_version().unwrap_or_else(|error| error)),
        ("Host", or_unknown(&known.host)),
        ("Node", or_unknown(&known.node)),
        ("Windows", diagnostics::windows_version()),
        ("Execution target", or_unknown(&known.target)),
        ("Interface language", known.locale.clone()),
        ("Memory", diagnostics::memory()),
        ("Exported", Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect()
}

fn export(target: &Path, known: &Known) -> anyhow::Result<()> {
    diagnostics::standard_files(&known.user_data)
        .with_context(|| format!("Saving {}", target.display()))
        .and_then(|files| diagnostics::export(target, &about(known), &files, &Account::current()))
        .inspect_err(|error| log::warn!("Diagnostics were not exported: {error:#}"))?;
    log::info!("Diagnostics exported to {}", target.display());
    Ok(())
}

/// Show the zip selected in Explorer (3.x `shell.showItemInFolder`).
fn reveal(path: PathBuf) -> PathBuf {
    if let Err(error) = diagnostics::reveal(&path) {
        log::warn!("The exported diagnostics could not be shown in Explorer: {error}");
    }
    path
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;
    use crate::i18n::{self, Locale};

    #[test]
    fn names_the_gpui_in_cargo_lock() {
        let lock = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock")).unwrap();
        let version = |name: &str| {
            let entry = format!("name = \"{name}\"\nversion = \"");
            let at = lock.find(&entry).unwrap_or_else(|| panic!("{name} is in Cargo.lock"));
            let rest = &lock[at + entry.len()..];
            rest[..rest.find('"').unwrap()].to_owned()
        };
        assert_eq!(GPUI, format!("gpui-kit {}, gpui-pre {}", version("gpui-kit"), version("gpui-pre")));
    }

    #[test]
    fn writes_about_in_the_3x_order() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("host.log"), "Host started\n").unwrap();
        let known = Known {
            user_data: folder.path().to_owned(),
            installed: false,
            host: Some("4.0.0".to_owned()),
            node: None,
            target: Some("Windows".to_owned()),
            locale: "zh".to_owned(),
        };
        let lines = about(&known);
        let keys: Vec<_> = lines.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, [
            "Rainy", "Installed", "GPUI", "WebView2", "Host", "Node", "Windows", "Execution target", "Interface language", "Memory", "Exported"
        ]);
        let value = |key: &str| lines.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone()).unwrap();
        assert_eq!(value("Rainy"), env!("CARGO_PKG_VERSION"));
        assert_eq!(value("Node"), "unknown");
        let webview = value("WebView2");
        assert!(webview == "not installed" || webview.split('.').all(|part| part.parse::<u32>().is_ok()), "{webview}");
        let target = folder.path().join("Rainy-diagnostics.zip");
        export(&target, &known).unwrap();
        assert!(std::fs::metadata(&target).unwrap().len() > 100);
    }

    #[test]
    fn a_crash_folder_that_cannot_be_read_fails_the_export_like_any_file() {
        let folder = tempfile::tempdir().unwrap();
        let crashpad = folder.path().join("WebView2").join("EBWebView").join("Crashpad");
        std::fs::create_dir_all(&crashpad).unwrap();
        std::fs::write(crashpad.join("reports"), "not a folder").unwrap();
        let known = Known { user_data: folder.path().to_owned(), installed: false, host: None, node: None, target: None, locale: "en".to_owned() };
        let target = folder.path().join("Rainy-diagnostics.zip");
        let error = format!("{:#}", export(&target, &known).unwrap_err());
        assert!(error.starts_with(&format!("Saving {}: Reading {}: ", target.display(), crashpad.join("reports").display())), "{error}");
        assert!(!target.exists());
    }

    #[test]
    fn the_texts_are_the_3x_ones() {
        assert_eq!(i18n::text(Locale::Zh, "settings.diagnosticsTitle"), "诊断信息");
        assert_eq!(i18n::text(Locale::Zh, "settings.diagnosticsExport"), "导出…");
        assert_eq!(i18n::text(Locale::Zh, "settings.diagnosticsExporting"), "正在导出…");
        assert_eq!(i18n::format(Locale::Zh, "settings.diagnosticsSaved", &[("path", &r"C:\x.zip")]), r"诊断信息已保存到 C:\x.zip");
        assert_eq!(i18n::format(Locale::En, "settings.diagnosticsSaved", &[("path", &"x")]), "Diagnostics saved to x");
        assert_eq!(i18n::text(Locale::Zh, "native.diagnosticsSaveTitle"), "导出诊断信息");
        assert_eq!(i18n::text(Locale::En, "native.diagnosticsSaveTitle"), "Export diagnostics");
    }
}
