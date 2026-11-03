//! Appearance: color mode, color scheme, accent and fonts (`AppearanceSection.tsx`; 3.x's rain effects have no
//! counterpart here, so their switch is gone while the stored `rainEffects` stays).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::json;

use super::{group, heading, row, segmented, set_prefs};
use crate::fonts::{self, CHOICES, FontLibrary, Use};
use crate::i18n::Locale;
use crate::store::Store;
use crate::theme::{scheme_accent, themes_for};
use crate::ui::tr;

const SCHEMES: [(&str, &str); 7] = [
    ("graphite", "schemeGraphite"),
    ("slate", "schemeSlate"),
    ("midnight", "schemeMidnight"),
    ("sand", "schemeSand"),
    ("forest", "schemeForest"),
    ("rose", "schemeRose"),
    ("black", "schemeBlack"),
];
const ACCENTS: [(&str, &str); 7] = [
    ("auto", "accentAuto"),
    ("blue", "accentBlue"),
    ("violet", "accentViolet"),
    ("green", "accentGreen"),
    ("orange", "accentOrange"),
    ("pink", "accentPink"),
    ("teal", "accentTeal"),
];
/// `FONT_SIZE_RANGE` in the renderer.
const FONT_SIZES: std::ops::RangeInclusive<f32> = 11.0..=20.0;

/// A font menu: Windows' own fonts, then open-source ones downloaded when chosen (`fonts.rs`).
fn font_row(used: Use, preference: &str, cx: &App) -> AnyElement {
    let t = |key: &str| tr(cx, &format!("settings.{key}"));
    let locale = Store::global(cx).read(cx).locale();
    let name = |choice: &fonts::Choice| if locale == Locale::Zh { choice.zh } else { choice.en };
    let library = cx.global::<FontLibrary>();
    let current = fonts::first_family(preference).to_owned();
    let downloading = library.downloading.as_ref();
    let label: SharedString = match downloading.filter(|download| download.used == used) {
        Some(download) => {
            let shown = fonts::choice(download.family, used).map_or(download.family, name);
            format!("{shown} · {}", i18n_format(cx, "settings.fontDownloading", &[("percent", &download.percent())])).into()
        }
        None => fonts::choice(&current, used).map_or_else(|| current.clone(), |choice| name(choice).to_owned()).into(),
    };
    let items: Vec<(SharedString, &'static fonts::Choice, bool)> = CHOICES
        .iter()
        .filter(|choice| choice.used == used && (choice.file.is_some() || library.available(choice)))
        .map(|choice| {
            let ready = library.available(choice);
            let text = match &choice.file {
                Some(file) if !ready => format!("{} · {}", name(choice), i18n_format(cx, "settings.fontDownload", &[("size", &fonts::megabytes(file.bytes))])),
                _ => name(choice).to_owned(),
            };
            (text.into(), choice, ready)
        })
        .collect();
    let picker = Button::new(ElementId::Name(format!("font-{}", used.pref()).into()))
        .outline()
        .small()
        .label(label)
        .dropdown_caret(true)
        .disabled(downloading.is_some())
        .dropdown_menu(move |mut menu, _, _| {
            for (text, choice, ready) in &items {
                let (choice, ready) = (*choice, *ready);
                menu = menu.item(PopupMenuItem::new(text.clone()).checked(current == choice.family).on_click(move |_, window, cx| {
                    if ready {
                        set_prefs(json!({ used.pref(): choice.family }), window, cx);
                    } else {
                        fonts::download_font(choice, window, cx);
                    }
                }));
            }
            menu
        });
    let (title, description) = match used {
        Use::Ui => ("uiFontTitle", "uiFontDescription"),
        Use::Code => ("codeFontTitle", "codeFontDescription"),
    };
    let description = match &library.failed {
        Some((failed, error)) if *failed == used => i18n_format(cx, "settings.fontFailed", &[("error", error)]).into(),
        _ => t(description),
    };
    row(cx, t(title), Some(description), picker)
}

fn i18n_format(cx: &App, key: &str, vars: &[(&str, &dyn std::fmt::Display)]) -> String {
    crate::i18n::format(Store::global(cx).read(cx).locale(), key, vars)
}

fn color(hex: &str) -> Hsla {
    let value = u32::from_str_radix(hex.trim_start_matches('#').get(..6).unwrap_or("808080"), 16).unwrap_or(0x808080);
    rgb(value).into()
}

/// A small picture of a scheme in the current mode: side bar, main area and an accent mark.
fn swatch(scheme: &'static str, label: SharedString, selected: bool, dark: bool, accent: &str, cx: &App) -> impl IntoElement {
    let (light, night) = themes_for(scheme, accent);
    let config = if dark { night } else { light };
    let pick = |key: &str| config_color(&config, key);
    let theme = cx.theme();
    v_flex()
        .id(ElementId::Name(format!("scheme-{scheme}").into()))
        .gap_1()
        .items_center()
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            if !selected {
                set_prefs(json!({ "colorScheme": scheme }), window, cx);
            }
        })
        .child(
            h_flex()
                .w(px(76.))
                .h(px(48.))
                .rounded(theme.radius)
                .overflow_hidden()
                .border_2()
                .border_color(if selected { theme.ring } else { theme.border })
                .child(v_flex().w(px(22.)).h_full().p_1().gap_1().bg(pick("sidebar.background")).child(div().h(px(3.)).w_full().rounded_sm().bg(pick("muted.foreground"))))
                .child(
                    v_flex()
                        .flex_1()
                        .h_full()
                        .p_1()
                        .gap_1()
                        .bg(pick("background"))
                        .child(div().h(px(3.)).w(px(30.)).rounded_sm().bg(pick("muted.foreground")))
                        .child(div().h(px(10.)).w(px(24.)).rounded_sm().bg(pick("muted.background")))
                        .child(div().h(px(4.)).w(px(14.)).rounded_sm().bg(pick("primary.background"))),
                ),
        )
        .child(div().text_xs().child(label))
}

fn config_color(config: &gpui_kit::component::ThemeConfig, key: &str) -> Hsla {
    let value = serde_json::to_value(&config.colors).ok();
    let text = value.as_ref().and_then(|colors| colors.get(key)).and_then(|value| value.as_str()).unwrap_or("#808080");
    color(text)
}

pub fn render(store: &Entity<Store>, _: &mut Window, cx: &mut App) -> impl IntoElement + use<> {
    let prefs = store.read(cx).prefs.clone();
    let dark = cx.theme().is_dark();
    let t = |key: &str| tr(cx, &format!("settings.{key}"));
    let accent_colors: Vec<(&'static str, Hsla)> = ACCENTS
        .iter()
        .map(|(id, _)| {
            let accent = if *id == "auto" { scheme_accent(&prefs.color_scheme) } else { id };
            let (light, night) = themes_for(&prefs.color_scheme, accent);
            (*id, config_color(if dark { &night } else { &light }, "primary.background"))
        })
        .collect();
    let theme = cx.theme();
    let sizes = [("uiFontSize", prefs.ui_font_size, "fontSizeTitle", "fontSizeDescription"), ("codeFontSize", prefs.code_font_size, "codeFontSizeTitle", "codeFontSizeDescription")];
    v_flex()
        .gap_4()
        .child(heading(cx, "settings.navAppearance", "settings.appearanceLead"))
        .child(group(cx, Some("settings.groupColors"), vec![
            row(
                cx,
                t("colorModeTitle"),
                Some(t("colorModeDescription")),
                segmented(
                    "color-mode",
                    vec![("light", t("appearanceLight")), ("dark", t("appearanceDark")), ("system", t("appearanceSystem"))],
                    &prefs.theme,
                    |value, window, cx| set_prefs(json!({ "theme": value }), window, cx),
                ),
            ),
            v_flex()
                .px_4()
                .py_3()
                .gap_3()
                .child(v_flex().gap_0p5().child(div().text_sm().child(t("colorSchemeTitle"))).child(div().text_xs().text_color(theme.muted_foreground).child(t("colorSchemeDescription"))))
                .child(h_flex().gap_3().flex_wrap().children(SCHEMES.iter().map(|(id, key)| {
                    swatch(id, t(key), prefs.color_scheme == *id, dark, &prefs.accent, cx)
                })))
                .into_any_element(),
            row(
                cx,
                t("accentTitle"),
                Some(t("accentDescription")),
                h_flex().gap_2().items_center().children(accent_colors.into_iter().map(|(id, fill)| {
                    let selected = prefs.accent == id;
                    let label = t(ACCENTS.iter().find(|(value, _)| *value == id).map_or("accentAuto", |(_, key)| key));
                    div()
                        .id(ElementId::Name(format!("accent-{id}").into()))
                        .size(px(22.))
                        .rounded_full()
                        .bg(fill)
                        .border_2()
                        .border_color(if selected { theme.foreground } else { theme.transparent })
                        .cursor_pointer()
                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx))
                        .on_click(move |_, window, cx| {
                            if !selected {
                                set_prefs(json!({ "accent": id }), window, cx);
                            }
                        })
                })),
            ),
        ]))
        .child(group(
            cx,
            Some("settings.groupFonts"),
            sizes
                .into_iter()
                .map(|(field, value, title, description)| {
                    let stepper = h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            Button::new(ElementId::Name(format!("{field}-down").into()))
                                .outline()
                                .xsmall()
                                .icon(IconName::Minus)
                                .disabled(value <= *FONT_SIZES.start())
                                .on_click(move |_, window, cx| set_prefs(json!({ field: value - 1.0 }), window, cx)),
                        )
                        .child(div().w(px(32.)).text_center().text_sm().child(format!("{value}")))
                        .child(
                            Button::new(ElementId::Name(format!("{field}-up").into()))
                                .outline()
                                .xsmall()
                                .icon(IconName::Plus)
                                .disabled(value >= *FONT_SIZES.end())
                                .on_click(move |_, window, cx| set_prefs(json!({ field: value + 1.0 }), window, cx)),
                        )
                        .child(div().text_xs().text_color(theme.muted_foreground).child(t("fontSizeUnit")));
                    row(cx, t(title), Some(t(description)), stepper)
                })
                .chain([font_row(Use::Ui, &prefs.ui_font, cx), font_row(Use::Code, &prefs.code_font, cx)])
                .chain(std::iter::once(
                    v_flex()
                        .px_4()
                        .py_3()
                        .gap_2()
                        .child(div().child(t("fontPreviewText")))
                        .child(div().font_family(theme.mono_font_family.clone()).text_size(theme.mono_font_size).child(t("fontPreviewCode")))
                        .into_any_element(),
                ))
                .collect(),
        ))
        .child(h_flex().justify_end().child(
            Button::new("appearance-reset").ghost().small().label(t("appearanceReset")).on_click(|_, window, cx| {
                set_prefs(
                    json!({ "theme": "system", "colorScheme": "graphite", "accent": "auto", "rainEffects": true, "uiFont": "", "codeFont": "", "uiFontSize": 14, "codeFontSize": 13 }),
                    window,
                    cx,
                );
            }),
        ))
        .when(false, |this| this)
}
