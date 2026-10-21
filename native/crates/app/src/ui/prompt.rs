//! A dialog asking for one line of text, as the 3.x `PromptDialog`.

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::*;

use crate::ui::tr;

/// Ask for text; `submit` gets it trimmed and non-empty, and the dialog closes.
pub fn prompt_text(
    title: SharedString,
    initial: String,
    ok: SharedString,
    submit: impl Fn(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
    // Submitting returns whether the dialog may close: an empty field keeps it open.
    let submit: Rc<dyn Fn(&mut Window, &mut App) -> bool> = {
        let input = input.clone();
        Rc::new(move |window: &mut Window, cx: &mut App| {
            let text = input.read(cx).value().trim().to_owned();
            if text.is_empty() {
                return false;
            }
            submit(text, window, cx);
            true
        })
    };
    let field = input.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let (on_ok, on_click) = (submit.clone(), submit.clone());
        dialog
            .title(title.clone())
            .w(px(440.))
            .child(Input::new(&field))
            .on_ok(move |_, window, cx| on_ok(window, cx))
            .footer(
                DialogFooter::new()
                    .child(Button::new("prompt-cancel").outline().label(tr(cx, "ide.ideCancel")).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("prompt-ok").primary().label(ok.clone()).on_click(move |_, window, cx| {
                        if on_click(window, cx) {
                            window.close_dialog(cx);
                        }
                    })),
            )
    });
    // The dialog moves focus into itself when it opens; the field takes it once the dialog is on screen.
    window.defer(cx, move |window, cx| {
        window.defer(cx, move |window, cx| {
            input.update(cx, |state, cx| {
                state.focus(window, cx);
                state.select_all(window, cx);
            });
        });
    });
}
