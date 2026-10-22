//! The questions `ssh` asks, in Rainy's window (`src/main/ssh-prompt.ts`): trusting a new host key, and typing a
//! password, passphrase or code; and what the window says when the Host ends.

use std::cell::RefCell;
use std::rc::Rc;

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

/// Show one question; the answer goes back to `ssh`, and closing the dialog any other way cancels it.
pub fn ask(request: AskpassRequest, window: &mut Window, cx: &mut App) {
    // The dialog owns the reply: dropping the dialog without an answer drops the sender, which `ssh` reads as a cancel.
    let reply = Rc::new(RefCell::new(Some(request.reply)));
    let send = move |answer: Option<String>| {
        if let Some(reply) = reply.borrow_mut().take() {
            let _ = reply.send(answer);
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
                            .child(div().text_sm().text_color(cx.theme().muted_foreground).max_h(px(96.)).overflow_hidden().child(prompt.clone()))
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

/// The Host ended while Rainy used it: an SSH connection can be made again, any other Host restarted.
pub fn host_exited(store: Entity<Store>, code: Option<i32>, window: &mut Window, cx: &mut App) {
    let target = store.read(cx).target.clone();
    let (title, description, action) = match &target {
        Some(target @ ExecutionTarget::Ssh { .. }) => {
            (trf(cx, "native.sshLost", &[("target", &target.label())]), tr(cx, "native.sshLostDetail"), tr(cx, "native.reconnect"))
        }
        _ => {
            let reason = match code {
                Some(code) => trf(cx, "native.exitCode", &[("code", &code)]),
                None => tr(cx, "native.processKilled"),
            };
            (tr(cx, "native.hostStopped"), trf(cx, "native.hostStoppedDetail", &[("reason", &reason)]), tr(cx, "native.restart"))
        }
    };
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
