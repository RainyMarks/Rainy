//! The shortcut list (`ShortcutsSection` in `GeneralSection.tsx`): the keys this interface binds.

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{group, heading};
use crate::ui::tr;

/// Key combinations (one or more ways to press it) and what they do.
type Shortcuts = &'static [(&'static [&'static [&'static str]], &'static str)];

/// Only shortcuts this interface implements are listed, grouped by where they work (`ui::ide::bind_keys`, the
/// composer, the keys a viewer page hands back in `viewers::shortcut_action`, and the rich Markdown editor's own).
const GROUPS: [(Option<&str>, Shortcuts); 5] = [
    (Some("settings.shortcutGroupGeneral"), &[
        (&[&["Ctrl", "Shift", "P"], &["F1"]], "settings.shortcutPalette"),
        (&[&["Ctrl", "P"], &["Ctrl", "Alt", "P"]], "settings.shortcutQuickOpen"),
        (&[&["Ctrl", "Shift", "F"]], "settings.shortcutSearchFiles"),
        (&[&["Ctrl", "Shift", "H"]], "settings.shortcutReplaceFiles"),
        (&[&["Ctrl", "Shift", "E"]], "settings.shortcutExplorer"),
        (&[&["Ctrl", "Shift", "G"]], "settings.shortcutSourceControl"),
        (&[&["Ctrl", "O"], &["Ctrl", "Alt", "O"]], "settings.shortcutOpenFolder"),
        (&[&["Ctrl", "`"]], "ide.ideToggleBottom"),
        (&[&["Ctrl", ","]], "settings.shortcutSettings"),
        (&[&["Ctrl", "S"]], "ide.ideSave"),
        (&[&["Ctrl", "W"]], "ide.ideClose"),
    ]),
    (Some("settings.groupChat"), &[
        (&[&["Ctrl", "Alt", "N"]], "settings.shortcutNewChat"),
        (&[&["Ctrl", "Alt", "K"]], "settings.shortcutSearchChats"),
        (&[&["Enter"]], "settings.shortcutSend"),
        (&[&["Shift", "Enter"]], "settings.shortcutNewline"),
        (&[&["Ctrl", "Enter"]], "settings.shortcutAlternate"),
        (&[&["Esc", "Esc"]], "settings.shortcutStop"),
    ]),
    (Some("settings.shortcutGroupEditor"), &[
        (&[&["Ctrl", "K"]], "settings.shortcutAiEdit"),
        (&[&["Ctrl", "F"]], "ide.ideFind"),
        (&[&["Ctrl", "H"]], "ide.ideReplace"),
        (&[&["Ctrl", "G"]], "ide.ideGoLine"),
        (&[&["Shift", "Alt", "F"]], "ide.ideFormat"),
        (&[&["F12"]], "ide.ideGotoDefinition"),
        (&[&["Shift", "F12"]], "ide.ideFindReferences"),
        (&[&["F2"]], "ide.ideRenameSymbol"),
        (&[&["Ctrl", "Space"]], "settings.shortcutCompletions"),
    ]),
    (Some("settings.shortcutGroupRun"), &[
        (&[&["F5"]], "settings.shortcutDebug"),
        (&[&["Ctrl", "F5"]], "app.cmdRunFile"),
        (&[&["Shift", "F5"]], "settings.shortcutStopRun"),
        (&[&["F9"]], "settings.shortcutBreakpoint"),
        (&[&["F10"]], "ide.ideStepOver"),
        (&[&["F11"]], "ide.ideStepInto"),
        (&[&["Shift", "F11"]], "ide.ideStepOut"),
    ]),
    // The labels name Markdown themselves.
    (None, &[
        (&[&["Ctrl", "/"]], "settings.shortcutMdSource"),
        (&[&["F8"]], "settings.shortcutMdFocus"),
        (&[&["F9"]], "settings.shortcutMdTypewriter"),
        (&[&["Ctrl", "F"], &["Ctrl", "H"]], "settings.shortcutMdFind"),
        (&[&["Ctrl", "0–6"]], "settings.shortcutMdHeading"),
        (&[&["Ctrl", "T"]], "settings.shortcutMdTable"),
        (&[&["Ctrl", "Shift", "K"]], "settings.shortcutMdCode"),
        (&[&["Ctrl", "Shift", "M"]], "settings.shortcutMdMath"),
        (&[&["Ctrl", "Shift", "Q"]], "settings.shortcutMdQuote"),
        (&[&["Ctrl", "Click"]], "settings.shortcutMdLink"),
    ]),
];

pub fn render(cx: &mut App) -> impl IntoElement + use<> {
    let theme = cx.theme().clone();
    let cap = |key: &'static str| {
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
            .child(key)
    };
    let groups: Vec<AnyElement> = GROUPS
        .iter()
        .map(|(title, shortcuts)| {
            let rows = shortcuts
                .iter()
                .map(|(ways, label)| {
                    h_flex()
                        .px_4()
                        .py_2()
                        .gap_4()
                        .justify_between()
                        .items_center()
                        .child(div().flex_1().min_w_0().text_sm().child(tr(cx, label)))
                        .child(h_flex().flex_shrink_0().gap_2().items_center().children(ways.iter().enumerate().map(|(index, keys)| {
                            h_flex()
                                .gap_1()
                                .items_center()
                                .when(index > 0, |this| this.child(div().text_xs().text_color(theme.muted_foreground).child("/")))
                                .children(keys.iter().map(|key| cap(key)))
                        })))
                        .into_any_element()
                })
                .collect();
            group(cx, *title, rows).into_any_element()
        })
        .collect();
    v_flex().gap_4().child(heading(cx, "settings.shortcutsTitle", "settings.shortcutsDescription")).children(groups)
}
