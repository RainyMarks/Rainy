//! Appearance: color mode, color scheme, accent, motion and fonts (`AppearanceSection.tsx`).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::json;

use super::{group, heading, row, segmented, set_prefs};
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
        .child(group(cx, Some("settings.groupMotion"), vec![row(
            cx,
            t("rainEffectsTitle"),
            Some(t("rainEffectsDescription")),
            Switch::new("rain-effects").checked(prefs.rain_effects).on_click(|checked: &bool, window, cx| {
                set_prefs(json!({ "rainEffects": *checked }), window, cx);
            }),
        )]))
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
