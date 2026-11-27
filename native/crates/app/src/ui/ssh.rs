//! The questions `ssh` asks, in Rainy's window (`src/main/ssh-prompt.ts`): trusting a new host key, and typing a
//! password, passphrase or code; and what the window says when the Host ends.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;
use rainy_carrier::ExecutionTarget;
use rainy_carrier::askpass::QuestionKind;

use crate::store::{AskpassRequest, Store};
use crate::ui::{tr, trf};

/// Monotonic tag for the most recently opened SSH askpass dialog so an older request's 300ms poll
/// never closes a newer dialog.
static ACTIVE_ASK_SEQ: AtomicU64 = AtomicU64::new(0);

/// Show one question; the answer goes back to `ssh`, and closing the dialog any other way cancels it. The dialog closes
/// by itself when the `ssh` run that asked has ended.
pub fn ask(request: AskpassRequest, window: &mut Window, cx: &mut App) {
    let seq = ACTIVE_ASK_SEQ.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    // The dialog owns the reply: dropping the dialog without an answer drops the sender, which `ssh` reads as a cancel.
    let reply = Rc::new(RefCell::new(Some(request.reply)));
    let dialog_alive = Rc::new(Cell::new(true));
    let (pending, open, poll_alive) = (reply.clone(), request.question.open.clone(), dialog_alive.clone());
    window
        .spawn(cx, async move |cx| {
            while pending.borrow().is_some() && poll_alive.get() {
                cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
                if !open.load(Ordering::SeqCst) && pending.borrow_mut().take().is_some() {
                    if poll_alive.replace(false) && ACTIVE_ASK_SEQ.load(Ordering::SeqCst) == seq {
                        let _ = cx.update(|window, cx| window.close_dialog(cx));
                    }
                    break;
                }
            }
        })
        .detach();
    let send = {
        let dialog_alive = dialog_alive.clone();
        move |answer: Option<String>| {
            dialog_alive.set(false);
            if let Some(reply) = reply.borrow_mut().take() {
                let _ = reply.send(answer);
            }
        }
    };
    let prompt = request.question.prompt.trim().to_owned();
    match request.question.kind {
        QuestionKind::HostKey => {
            let details: Vec<&str> = prompt.lines().filter(|line| !line.contains("continue connecting")).collect();
            let description: SharedString = format!("{}\n\n{}", details.join("\n").trim(), tr(cx, "native.sshFingerprint")).into();
            window.open_alert_dialog(cx, move |alert, _, cx| {
                let (trust, cancel, closed) = (send.clone(), send.clone(), send.clone());
                alert
                    .title(tr(cx, "native.sshFirstTime"))
                    .description(description.clone())
                    .ok_text(tr(cx, "native.sshTrust"))
                    .cancel_text(tr(cx, "chat.cancel"))
                    .show_cancel(true)
                    .on_ok(move |_, _, _| {
                        trust(Some("yes".to_owned()));
                        true
                    })
                    .on_cancel(move |_, _, _| {
                        cancel(None);
                        true
                    })
                    .on_close(move |_, _, _| closed(None))
            });
        }
        QuestionKind::Secret => {
            let input = cx.new(|cx| InputState::new(window, cx).masked(true));
            let field = input.clone();
            window.open_dialog(cx, move |dialog, _, cx| {
                let (on_ok, on_click, on_cancel, closed) = (send.clone(), send.clone(), send.clone(), send.clone());
                let (ok_field, click_field) = (field.clone(), field.clone());
                dialog
                    .title(tr(cx, "native.sshVerify"))
                    .w(px(460.))
                    .child(
                        v_flex()
                            .gap_3()
                            .child(
                                div()
                                    .id("ssh-ask-prompt")
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .max_h(px(120.))
                                    .overflow_y_scroll()
                                    .child(prompt.clone()),
                            )
                            .child(Input::new(&field)),
                    )
                    .on_ok(move |_, _, cx| {
                        on_ok(Some(ok_field.read(cx).value().to_string()));
                        true
                    })
                    .on_cancel(move |_, _, _| {
                        on_cancel(None);
                        true
                    })
                    .on_close(move |_, _, _| closed(None))
                    .footer(
                        DialogFooter::new()
                            .child(Button::new("ssh-cancel").outline().label(tr(cx, "chat.cancel")).on_click(|_, window, cx| window.close_dialog(cx)))
                            .child(Button::new("ssh-continue").primary().label(tr(cx, "native.continue")).on_click(move |_, window, cx| {
                                on_click(Some(click_field.read(cx).value().to_string()));
                                window.close_dialog(cx);
                            })),
                    )
            });
            window.defer(cx, move |window, cx| {
                window.defer(cx, move |window, cx| input.update(cx, |state, cx| state.focus(window, cx)));
            });
        }
    }
}

/// The Host ended while Rainy used it: SSH runs silent backoff reconnect first (`1s -> 3s -> 8s`) without a blocking
/// modal; local/WSL Hosts show the restart alert immediately.
pub fn host_exited(store: Entity<Store>, code: Option<i32>, window: &mut Window, cx: &mut App) {
    let target = store.read(cx).target.clone();
    if matches!(target, Some(ExecutionTarget::Ssh { .. })) {
        let handle = window.window_handle();
        store.update(cx, |store, cx| store.start_ssh_auto_reconnect(handle, cx));
        return;
    }
    let reason = match code {
        Some(code) => trf(cx, "native.exitCode", &[("code", &code)]),
        None => tr(cx, "native.processKilled"),
    };
    let title = tr(cx, "native.hostStopped");
    let description = trf(cx, "native.hostStoppedDetail", &[("reason", &reason)]);
    let action = tr(cx, "native.restart");
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let store = store.clone();
        alert
            .title(title.clone())
            .description(description.clone())
            .ok_text(action.clone())
            .cancel_text(tr(cx, "native.later"))
            .show_cancel(true)
            .on_ok(move |_, _, cx| {
                store.update(cx, |store, cx| store.restart(cx));
                true
            })
    });
}

/// Shown after silent SSH backoff reconnect is exhausted or rejected by authentication: lets the user retry,
/// hot-swap back to local Windows, or dismiss and keep reading the window.
pub fn host_exited_exhausted(window: &mut Window, cx: &mut App, detail: String) {
    let store = Store::global(cx);
    let target_label = store
        .read(cx)
        .target
        .as_ref()
        .map(|target| target.label().to_owned())
        .unwrap_or_default();
    let title = trf(cx, "native.sshLost", &[("target", &target_label)]);
    let body = trf(cx, "ssh.reconnectExhaustedDetail", &[("error", &detail)]);
    window.open_dialog(cx, move |dialog, _, cx| {
        let retry_store = store.clone();
        let local_store = store.clone();
        dialog
            .title(title.clone())
            .w(px(460.))
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().text_color(cx.theme().foreground).child(tr(cx, "ssh.reconnectExhausted")))
                    .child(
                        div()
                            .id("ssh-exhausted-detail")
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .max_h(px(180.))
                            .overflow_y_scroll()
                            .child(body.clone()),
                    ),
            )
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("ssh-exhausted-later")
                            .outline()
                            .label(tr(cx, "native.later"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("ssh-exhausted-local")
                            .outline()
                            .label(tr(cx, "ssh.switchLocal"))
                            .on_click(move |_, window, cx| {
                                let handle = window.window_handle();
                                window.close_dialog(cx);
                                local_store.update(cx, |store, cx| {
                                    store.swap_target_in_place(ExecutionTarget::windows(), handle, cx);
                                });
                            }),
                    )
                    .child(
                        Button::new("ssh-exhausted-retry")
                            .primary()
                            .label(tr(cx, "ssh.retryNow"))
                            .on_click(move |_, window, cx| {
                                let handle = window.window_handle();
                                window.close_dialog(cx);
                                retry_store.update(cx, |store, cx| store.reconnect_ssh(handle, cx));
                            }),
                    ),
            )
    });
}
