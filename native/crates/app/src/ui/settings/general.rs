//! General: language, chat preferences, the Markdown editor, system integration, diagnostics export and version
//! (`GeneralSection.tsx`).

use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Sizable as _, v_flex};
use gpui_kit::*;
use rainy_carrier::shell::Change;
use rainy_protocol::UiPreferences;
use serde_json::json;

use super::{group, heading, row, segmented, set_prefs};
use crate::store::update::UpdatePhase;
use crate::store::{Store, shell};
use crate::ui::{tr, trf};

pub fn render(store: &Entity<Store>, _: &mut Window, cx: &mut App) -> impl IntoElement + use<> {
    let prefs = store.read(cx).prefs.clone();
    let t = |key: &str| tr(cx, &format!("settings.{key}"));
    v_flex()
        .gap_4()
        .child(heading(cx, "settings.navGeneral", "settings.generalLead"))
        .child(group(cx, None, vec![row(
            cx,
            t("languageTitle"),
            None,
            segmented("locale", vec![("zh", t("languageZh")), ("en", t("languageEn"))], &prefs.locale, |value, window, cx| {
                set_prefs(json!({ "locale": value }), window, cx);
            }),
        )]))
        .child(group(cx, Some("settings.groupChat"), vec![
            row(
                cx,
                t("busyEnterTitle"),
                Some(t("busyEnterDescription")),
                segmented("busy-enter", vec![("queue", t("busyEnterQueue")), ("steer", t("busyEnterSteer"))], &prefs.busy_enter, |value, window, cx| {
                    set_prefs(json!({ "busyEnter": value }), window, cx);
                }),
            ),
            row(
                cx,
                t("stepDetailTitle"),
                Some(t("stepDetailDescription")),
                segmented(
                    "step-detail",
                    vec![("compact", t("stepDetailCompact")), ("standard", t("stepDetailStandard")), ("detailed", t("stepDetailDetailed"))],
                    &prefs.step_detail,
                    |value, window, cx| set_prefs(json!({ "stepDetail": value }), window, cx),
                ),
            ),
            row(
                cx,
                t("usageTitle"),
                Some(t("usageDescription")),
                Switch::new("show-usage").checked(prefs.show_usage).on_click(|checked: &bool, window, cx| {
                    set_prefs(json!({ "showUsage": *checked }), window, cx);
                }),
            ),
        ]))
        .child(markdown(&prefs, cx))
        .child(system_integration(store, cx))
        .child(about(store, cx))
}

/// The Markdown editor: where pasted images go (read by the editor page), and Typora's focus and typewriter modes.
fn markdown(prefs: &UiPreferences, cx: &App) -> AnyElement {
    let folders = [("assets", "settings.imageFolderAssets"), ("named", "settings.imageFolderNamed"), ("same", "settings.imageFolderSame")];
    let current = prefs.markdown_images.clone();
    let label = folders.iter().find(|(id, _)| *id == current).map_or("settings.imageFolderAssets", |(_, key)| *key);
    let items: Vec<(&'static str, SharedString)> = folders.iter().map(|(id, key)| (*id, tr(cx, key))).collect();
    let picker = Button::new("markdown-images").outline().small().label(tr(cx, label)).dropdown_caret(true).dropdown_menu(move |mut menu, _, _| {
        for (id, text) in &items {
            let id = *id;
            menu = menu.item(PopupMenuItem::new(text.clone()).checked(current == id).on_click(move |_, window, cx| {
                set_prefs(json!({ "markdownImages": id }), window, cx);
            }));
        }
        menu
    });
    group(cx, Some("settings.groupMarkdown"), vec![
        row(cx, tr(cx, "settings.imageFolderTitle"), Some(tr(cx, "settings.imageFolderDescription")), picker),
        row(
            cx,
            tr(cx, "settings.focusModeTitle"),
            Some(tr(cx, "settings.focusModeDescription")),
            Switch::new("markdown-focus").checked(prefs.markdown_focus).on_click(|checked: &bool, window, cx| {
                set_prefs(json!({ "markdownFocus": *checked }), window, cx);
            }),
        ),
        row(
            cx,
            tr(cx, "settings.typewriterTitle"),
            Some(tr(cx, "settings.typewriterDescription")),
            Switch::new("markdown-typewriter").checked(prefs.markdown_typewriter).on_click(|checked: &bool, window, cx| {
                set_prefs(json!({ "markdownTypewriter": *checked }), window, cx);
            }),
        ),
    ])
    .into_any_element()
}

/// Exporting diagnostics; the version, and checking for updates next to it (3.x had it in the Help menu): the
/// download's progress, and the update waiting to be installed, which the button offers again.
fn about(store: &Entity<Store>, cx: &App) -> AnyElement {
    let updates = &store.read(cx).updates;
    let status: Option<SharedString> = match &updates.phase {
        UpdatePhase::Downloading { version, percent } => Some(trf(cx, "native.updateDownloading", &[("version", version), ("percent", percent)])),
        UpdatePhase::Ready { version } | UpdatePhase::Installing { version } => Some(format!("{} {version}", tr(cx, "native.updateReady")).into()),
        _ => None,
    };
    let checking = updates.busy();
    let automatic = updates.automatic;
    let exporting = store.read(cx).diagnostics.exporting;
    group(cx, Some("settings.groupAbout"), vec![
        row(
            cx,
            tr(cx, "settings.diagnosticsTitle"),
            Some(tr(cx, "settings.diagnosticsDescription")),
            Button::new("export-diagnostics")
                .outline()
                .small()
                .label(tr(cx, if exporting { "settings.diagnosticsExporting" } else { "settings.diagnosticsExport" }))
                .disabled(exporting)
                .on_click(|_, window, cx| {
                    let handle = window.window_handle();
                    Store::global(cx).update(cx, |store, cx| store.export_diagnostics(handle, cx));
                }),
        ),
        row(
            cx,
            // Rainy's own version, which the updater compares and reports.
            trf(cx, "settings.currentVersion", &[("version", &env!("CARGO_PKG_VERSION"))]),
            status,
            Button::new("check-updates")
                .outline()
                .small()
                .label(tr(cx, "native.checkUpdates"))
                .loading(checking)
                .disabled(checking)
                .on_click(|_, _, cx| Store::global(cx).update(cx, |store, cx| store.check_updates(true, cx))),
        ),
        row(
            cx,
            tr(cx, "native.autoUpdateTitle"),
            Some(tr(cx, "native.autoUpdateDescription")),
            Switch::new("auto-update").checked(automatic).on_click(|checked: &bool, _, cx| {
                let on = *checked;
                Store::global(cx).update(cx, |store, cx| store.set_auto_update(on, cx));
            }),
        ),
    ])
    .into_any_element()
}

/// Explorer's "Open with Rainy", the file types Rainy is offered for, and the way to Windows' Default apps. Only an
/// installed Rainy can change them.
fn system_integration(store: &Entity<Store>, cx: &App) -> AnyElement {
    let state = &store.read(cx).shell;
    let (available, busy, choice) = (state.available, state.busy, state.choice);
    let unavailable = (!available).then(|| tr(cx, "native.shellUnavailable"));
    let description = |key: &str| Some(unavailable.clone().unwrap_or_else(|| tr(cx, key)));
    let switch = |id: &'static str, checked: bool, change: fn(bool) -> Change| {
        Switch::new(id).checked(checked).disabled(!available || busy).on_click(move |checked: &bool, window, cx| {
            let handle = window.window_handle();
            Store::global(cx).update(cx, |store, cx| store.change_shell(change(*checked), handle, cx));
        })
    };
    group(cx, Some("native.groupSystem"), vec![
        row(
            cx,
            tr(cx, "native.contextMenuTitle"),
            description("native.contextMenuDescription"),
            switch("shell-context-menu", choice.context_menu, |on| Change { context_menu: Some(on), ..Change::default() }),
        ),
        row(
            cx,
            tr(cx, "native.fileTypesTitle"),
            description("native.fileTypesDescription"),
            switch("shell-file-types", choice.file_types, |on| Change { file_types: Some(on), ..Change::default() }),
        ),
        row(
            cx,
            tr(cx, "native.defaultAppsTitle"),
            Some(tr(cx, "native.defaultAppsDescription")),
            Button::new("shell-default-apps")
                .outline()
                .small()
                .label(tr(cx, "native.defaultAppsOpen"))
                .disabled(!available || !choice.file_types || busy)
                .on_click(|_, window, cx| shell::open_default_apps(window.window_handle(), cx)),
        ),
    ])
    .into_any_element()
}
