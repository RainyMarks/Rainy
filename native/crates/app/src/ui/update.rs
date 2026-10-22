//! What the window shows of updates (3.x `notice`, `confirm` and the saved-shutdown dialogs in `main.ts`): the dialogs
//! and the taskbar progress.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _};
use gpui_kit::*;
use raw_window_handle::RawWindowHandle;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{ITaskbarList3, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TaskbarList};

use crate::store::Store;
use crate::store::update::{UpdateNotice, UpdatePhase};
use crate::ui::tr;

/// Show what the updater reports: message boxes for manual checks, the offer to install, the refusal while busy.
pub fn show(notice: &UpdateNotice, window: &mut Window, cx: &mut App) {
    let text = |key: &str| tr(cx, key).to_string();
    match notice {
        UpdateNotice::Current => alert(format!("{} {}", text("native.updateCurrent"), env!("CARGO_PKG_VERSION")), window, cx),
        UpdateNotice::Unavailable => alert(text("native.updateUnavailable"), window, cx),
        UpdateNotice::Busy => alert(text("native.updateBusy"), window, cx),
        UpdateNotice::Failed(detail) => alert(format!("{}\n\n{detail}", text("native.updateFailed")), window, cx),
        UpdateNotice::Ready(version) => offer(version, window, cx),
        UpdateNotice::Restart(detail) => restart(format!("{}\n\n{detail}", text("native.updateFailed")), window, cx),
    }
}

fn alert(description: String, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |alert, _, cx| alert.title(tr(cx, "native.updateTitle")).description(description.clone()).ok_text(tr(cx, "native.ok")));
}

/// 3.x's confirmation: 稍后, the default and what closing means, or 重启并安装.
fn offer(version: &str, window: &mut Window, cx: &mut App) {
    log::info!("Update {version}: offered");
    let description = format!("{} {version}\n\n{}", tr(cx, "native.updateReady"), tr(cx, "native.updateRestartDetail"));
    let version = SharedString::from(version.to_owned());
    window.open_dialog(cx, move |dialog, _, cx| {
        let later = version.clone();
        dialog
            .title(tr(cx, "native.updateTitle"))
            .w(px(480.))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(description.clone()))
            .footer(
                DialogFooter::new()
                    .child(Button::new("update-later").outline().label(tr(cx, "native.updateLater")).on_click(move |_, window, cx| {
                        log::info!("Update {later}: later");
                        window.close_dialog(cx);
                    }))
                    .child(Button::new("update-install").primary().label(tr(cx, "native.updateInstall")).on_click(|_, window, cx| {
                        window.close_dialog(cx);
                        let handle = window.window_handle();
                        Store::global(cx).update(cx, |store, cx| store.install_update(handle, cx));
                    })),
            )
    });
}

/// The installer did not start after the Host was stopped: say so, then start Rainy again.
fn restart(description: String, window: &mut Window, cx: &mut App) {
    let restarted = Rc::new(Cell::new(false));
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let restarted = restarted.clone();
        alert.title(tr(cx, "native.updateTitle")).description(description.clone()).ok_text(tr(cx, "native.restart")).on_close(move |_, _, cx| {
            if !restarted.replace(true) {
                Store::global(cx).update(cx, |store, cx| store.restart(cx));
            }
        })
    });
}

/// Saving the drafts before the update failed (3.x `retrySave`): whether to try again. Closing the dialog keeps Rainy
/// running.
pub fn retry_save(detail: Option<String>, window: &mut Window, cx: &mut App) -> async_channel::Receiver<bool> {
    let (answer, reply) = async_channel::bounded(1);
    let answer = Rc::new(RefCell::new(Some(answer)));
    let send = move |retry: bool| {
        if let Some(answer) = answer.borrow_mut().take() {
            let _ = answer.try_send(retry);
        }
    };
    let description = detail.unwrap_or_else(|| tr(cx, "native.draftsUnsavedDetail").to_string());
    window.open_dialog(cx, move |dialog, _, cx| {
        let (retry, stay, closed) = (send.clone(), send.clone(), send.clone());
        dialog
            .title(tr(cx, "native.draftsUnsaved"))
            .w(px(480.))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(description.clone()))
            .on_close(move |_, _, _| closed(false))
            .overlay_closable(false)
            .footer(
                DialogFooter::new()
                    .child(Button::new("update-stay").outline().label(tr(cx, "native.dontQuit")).on_click(move |_, window, cx| {
                        stay(false);
                        window.close_dialog(cx);
                    }))
                    .child(Button::new("update-retry-save").primary().label(tr(cx, "native.retrySaveQuit")).on_click(move |_, window, cx| {
                        retry(true);
                        window.close_dialog(cx);
                    })),
            )
    });
    reply
}

/// What the taskbar button shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Progress {
    None,
    Indeterminate,
    Percent(u8),
}

thread_local! {
    /// The taskbar, once created, and what it shows.
    static TASKBAR: RefCell<(Option<ITaskbarList3>, Progress)> = const { RefCell::new((None, Progress::None)) };
}

/// Follow the updater on the window's taskbar button (3.x `setProgressBar`): moving while checking, the percentage while
/// downloading, nothing otherwise.
pub fn taskbar(window: &Window, phase: &UpdatePhase) {
    let wanted = match phase {
        UpdatePhase::Checking => Progress::Indeterminate,
        UpdatePhase::Downloading { percent, .. } => Progress::Percent(*percent),
        _ => Progress::None,
    };
    TASKBAR.with(|taskbar| {
        let mut taskbar = taskbar.borrow_mut();
        if taskbar.1 == wanted {
            return;
        }
        let Some(hwnd) = hwnd(window) else { return };
        if taskbar.0.is_none() {
            // SAFETY: COM is initialized on the window's thread (GPUI's OleInitialize); the object stays on this thread.
            let created = unsafe { CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER).and_then(|list| list.HrInit().map(|()| list)) };
            match created {
                Ok(list) => taskbar.0 = Some(list),
                Err(error) => {
                    log::warn!("The taskbar progress is not shown: {error}");
                    taskbar.1 = wanted;
                    return;
                }
            }
        }
        let Some(list) = taskbar.0.as_ref() else { return };
        // SAFETY: a live window handle of this thread.
        let shown = unsafe {
            match wanted {
                Progress::None => list.SetProgressState(hwnd, TBPF_NOPROGRESS),
                Progress::Indeterminate => list.SetProgressState(hwnd, TBPF_INDETERMINATE),
                Progress::Percent(percent) => list.SetProgressState(hwnd, TBPF_NORMAL).and_then(|()| list.SetProgressValue(hwnd, u64::from(percent), 100)),
            }
        };
        if let Err(error) = shown {
            log::warn!("The taskbar progress was not updated: {error}");
        }
        taskbar.1 = wanted;
    });
}

fn hwnd(window: &Window) -> Option<HWND> {
    match raw_window_handle::HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(HWND(handle.hwnd.get() as *mut _)),
        _ => None,
    }
}
