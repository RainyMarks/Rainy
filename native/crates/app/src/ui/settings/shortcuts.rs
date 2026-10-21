//! The shortcut list (`ShortcutsSection` in `GeneralSection.tsx`): the keys this interface binds.

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;

use super::{group, heading};
use crate::ui::tr;

/// Keys and what they do; only shortcuts this interface implements are listed.
const SHORTCUTS: &[(&[&str], &str)] = &[
    (&["Ctrl", "Alt", "P"], "settings.shortcutQuickOpen"),
    (&["Ctrl", "Shift", "F"], "settings.shortcutSearchFiles"),
    (&["Ctrl", "Shift", "H"], "settings.shortcutReplaceFiles"),
    (&["Ctrl", "Shift", "G"], "settings.shortcutSourceControl"),
    (&["Ctrl", "Shift", "E"], "settings.shortcutExplorer"),
    (&["Ctrl", ","], "settings.shortcutSettings"),
    (&["Ctrl", "Alt", "N"], "settings.shortcutNewChat"),
    (&["Ctrl", "Alt", "O"], "settings.shortcutOpenFolder"),
    (&["Ctrl", "`"], "settings.shortcutTerminal"),
    (&["Ctrl", "S"], "ide.ideSave"),
    (&["Ctrl", "W"], "ide.ideClose"),
    (&["Enter"], "settings.shortcutSend"),
    (&["Shift", "Enter"], "settings.shortcutNewline"),
    (&["Ctrl", "Enter"], "settings.shortcutAlternate"),
];

pub fn render(cx: &mut App) -> impl IntoElement + use<> {
    let theme = cx.theme().clone();
    let rows: Vec<AnyElement> = SHORTCUTS
        .iter()
        .map(|(keys, label)| {
            h_flex()
                .px_4()
                .py_2()
                .justify_between()
                .items_center()
                .child(div().text_sm().child(tr(cx, label)))
                .child(h_flex().gap_1().children(keys.iter().map(|key| {
                    div()
                        .px_1p5()
                        .min_w(px(22.))
                        .flex()
                        .justify_center()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .child(*key)
                })))
                .into_any_element()
        })
        .collect();
    v_flex().gap_4().child(heading(cx, "settings.shortcutsTitle", "settings.shortcutsDescription")).child(group(cx, None, rows))
}
