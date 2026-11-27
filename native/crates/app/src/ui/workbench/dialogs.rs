//! Workbench-level dialogs and target-switching menus (credential alerts, resource recheck, runtime download failure, target hot-swap).

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use rainy_carrier::trust::TrustError;
use rainy_carrier::{ExecutionTarget, ssh, targets};

use crate::boot::RuntimeChoice;
use crate::store::{RuntimePrompt, Store};
use crate::ui::settings::Section;
use crate::ui::{tr, trf};

use super::WorkbenchHandle;

/// The execution environments to switch to, grouped into Local/WSL and SSH remote hosts.
/// Choosing a target hot-swaps in place without restarting the application.
pub(super) fn target_menu(mut menu: PopupMenu, store: Entity<Store>, current: Option<String>, cx: &App) -> PopupMenu {
    let mut local_choices = vec![ExecutionTarget::windows()];
    local_choices.extend(targets::wsl_distributions());
    menu = menu.label(tr(cx, "ssh.menuLocalGroup"));
    for choice in local_choices {
        let checked = current.as_deref() == Some(choice.id());
        let store = store.clone();
        menu = menu.item(PopupMenuItem::new(choice.label().to_owned()).checked(checked).on_click(move |_, window, cx| {
            if !checked {
                confirm_switch(store.clone(), choice.clone(), window, cx);
            }
        }));
    }
    let prefs = targets::read_preferences(&store.read(cx).paths.desktop_settings()).unwrap_or_default();
    let ssh_hosts = ssh::hosts(&prefs);
    let summaries = ssh::config_host_summaries();
    menu = menu.separator().label(tr(cx, "ssh.menuSshGroup"));
    for host in ssh_hosts {
        let choice = host.target;
        let checked = current.as_deref() == Some(choice.id());
        let alias = match &choice {
            ExecutionTarget::Ssh { destination, .. } => destination.as_str(),
            _ => choice.label(),
        };
        let detail = summaries.iter().find(|s| s.alias == alias).and_then(|s| {
            let mut parts = Vec::new();
            if let Some(hn) = &s.host_name {
                if let Some(user) = &s.user {
                    parts.push(format!("{user}@{hn}"));
                } else {
                    parts.push(hn.clone());
                }
            }
            if let Some(port) = s.port {
                parts.push(format!(":{port}"));
            }
            (!parts.is_empty()).then(|| parts.join(""))
        });
        let label = match detail {
            Some(detail) if detail != choice.label() => format!("{}  ({detail})", choice.label()),
            _ => choice.label().to_owned(),
        };
        let store = store.clone();
        menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(move |_, window, cx| {
            if !checked {
                confirm_switch(store.clone(), choice.clone(), window, cx);
            }
        }));
    }
    menu.separator().item(PopupMenuItem::new(tr(cx, "ssh.menuManageSsh")).on_click(|_, window, cx| {
        if let Some(workbench) = cx.try_global::<WorkbenchHandle>().and_then(|handle| handle.0.upgrade()) {
            workbench.update(cx, |workbench, cx| workbench.open_settings(Section::Runtime, window, cx));
        }
    }))
}

/// Explicit user-visible alert when DPAPI credential protection recovered from corruption or failed.
pub(super) fn credential_alert(alert: &crate::boot::CredentialAlert, window: &mut Window, cx: &mut App) {
    let (title, description) = match alert {
        crate::boot::CredentialAlert::Warning(message) => (tr(cx, "native.credentialWarningTitle"), SharedString::from(message.clone())),
        crate::boot::CredentialAlert::Error(detail) => (
            tr(cx, "native.credentialErrorTitle"),
            trf(cx, "native.credentialErrorDetail", &[("detail", detail)]),
        ),
    };
    window.open_alert_dialog(cx, move |alert, _, cx| {
        alert.title(title.clone()).description(description.clone()).ok_text(tr(cx, "native.ok"))
    });
}

/// The daily recheck found installed files changed or damaged. Rainy keeps running; the next start checks every file again.
pub(super) fn resources_damaged(error: &TrustError, window: &mut Window, cx: &mut App) {
    let message = crate::boot::trust_message(crate::ui::locale(cx), error);
    let description = trf(cx, "native.resourcesRecheck", &[("message", &message)]);
    window.open_alert_dialog(cx, move |alert, _, cx| alert.title(tr(cx, "native.resourcesChanged")).description(description.clone()).ok_text(tr(cx, "native.ok")));
}

/// The Linux runtime could not be downloaded during startup: retry, run on Windows instead, or quit.
pub(super) fn runtime_download_failed(prompt: RuntimePrompt, window: &mut Window, cx: &mut App) {
    let reply = Rc::new(RefCell::new(Some(prompt.reply)));
    let answer = move |choice: RuntimeChoice| {
        if let Some(reply) = reply.borrow_mut().take() {
            let _ = reply.send(choice);
        }
    };
    let description = trf(cx, "native.linuxDownloadDetail", &[("error", &prompt.error)]);
    window.open_dialog(cx, move |dialog, window, cx| {
        let (retry, windows, quit, closed) = (answer.clone(), answer.clone(), answer.clone(), answer.clone());
        let room = (f32::from(window.viewport_size().height) - 260.).clamp(120., 420.);
        dialog
            .title(tr(cx, "native.linuxDownloadFailed"))
            .w(px(520.))
            .child(
                div()
                    .id("runtime-download-error")
                    .max_h(px(room))
                    .overflow_y_scroll()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(description.clone()),
            )
            .on_close(move |_, _, _| closed(RuntimeChoice::Quit))
            .overlay_closable(false)
            .footer(
                DialogFooter::new()
                    .child(Button::new("runtime-quit").outline().label(tr(cx, "native.quit")).on_click(move |_, window, cx| {
                        quit(RuntimeChoice::Quit);
                        window.close_dialog(cx);
                    }))
                    .child(Button::new("runtime-windows").outline().label(tr(cx, "native.useWindows")).on_click(move |_, window, cx| {
                        windows(RuntimeChoice::Windows);
                        window.close_dialog(cx);
                    }))
                    .child(Button::new("runtime-retry").primary().label(tr(cx, "native.retry")).on_click(move |_, window, cx| {
                        retry(RuntimeChoice::Retry);
                        window.close_dialog(cx);
                    })),
            )
    });
}

/// Ask, then hot-swap the execution environment in place without restarting.
pub(crate) fn confirm_switch(store: Entity<Store>, target: ExecutionTarget, window: &mut Window, cx: &mut App) {
    let label = target.label().to_owned();
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let (store, target) = (store.clone(), target.clone());
        alert
            .title(tr(cx, "native.switchTarget"))
            .description(trf(cx, "ssh.swapConfirm", &[("target", &label)]))
            .ok_text(tr(cx, "native.switch"))
            .cancel_text(tr(cx, "chat.cancel"))
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                let handle = window.window_handle();
                store.update(cx, |store, cx| store.switch_target(target.clone(), handle, cx));
                true
            })
    });
}
