//! Appearance: color mode, color scheme, accent and fonts (`AppearanceSection.tsx`; 3.x's rain effects have no
//! counterpart here, so their switch is gone while the stored `rainEffects` stays).

use std::collections::HashMap;
use std::sync::OnceLock;

use gpui_kit::base::{Transition, transition};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use serde_json::json;

use super::widgets::TRANSITION_RING;
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
/// Match `theme::apply` clamps: UI font 11..=20, code font 10..=24.
const UI_FONT_SIZES: std::ops::RangeInclusive<f32> = 11.0..=20.0;
const CODE_FONT_SIZES: std::ops::RangeInclusive<f32> = 10.0..=24.0;

#[derive(Clone, Copy)]
struct SwatchColors {
    sidebar: Hsla,
    muted_fg: Hsla,
    background: Hsla,
    muted_bg: Hsla,
    primary: Hsla,
}

fn color(hex: &str) -> Hsla {
    let value = u32::from_str_radix(hex.trim_start_matches('#').get(..6).unwrap_or("808080"), 16).unwrap_or(0x808080);
    rgb(value).into()
}

fn extract_swatch(config: &gpui_kit::component::ThemeConfig) -> SwatchColors {
    let value = serde_json::to_value(&config.colors).ok();
    let pick = |key: &str| {
        let text = value.as_ref().and_then(|colors| colors.get(key)).and_then(|value| value.as_str()).unwrap_or("#808080");
        color(text)
    };
    SwatchColors {
        sidebar: pick("sidebar.background"),
        muted_fg: pick("muted.foreground"),
        background: pick("background"),
        muted_bg: pick("muted.background"),
        primary: pick("primary.background"),
    }
}

/// Precompute every (scheme, resolved_accent, dark) swatch once so Appearance renders with zero JSON serialization.
fn swatch_cache() -> &'static HashMap<(&'static str, &'static str, bool), SwatchColors> {
    static CACHE: OnceLock<HashMap<(&'static str, &'static str, bool), SwatchColors>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut map = HashMap::with_capacity(SCHEMES.len() * (ACCENTS.len() - 1) * 2);
        for (scheme, _) in SCHEMES {
            for (accent, _) in ACCENTS.iter().filter(|(id, _)| *id != "auto") {
                let (light, dark) = themes_for(scheme, accent);
                map.insert((scheme, *accent, false), extract_swatch(&light));
                map.insert((scheme, *accent, true), extract_swatch(&dark));
            }
        }
        map
    })
}

fn cached_swatch(scheme: &str, accent: &str, dark: bool) -> SwatchColors {
    let resolved_scheme = SCHEMES.iter().find(|(id, _)| *id == scheme).map_or("graphite", |(id, _)| *id);
    let resolved_accent = if accent == "auto" {
        scheme_accent(resolved_scheme)
    } else {
        ACCENTS.iter().find(|(id, _)| *id == accent && *id != "auto").map_or("blue", |(id, _)| *id)
    };
    swatch_cache().get(&(resolved_scheme, resolved_accent, dark)).copied().unwrap_or_else(|| {
        let (light, dark_theme) = themes_for(resolved_scheme, resolved_accent);
        extract_swatch(if dark { &dark_theme } else { &light })
    })
}

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

/// A small picture of a scheme in the current mode: side bar, main area and an accent mark.
fn swatch(scheme: &'static str, label: SharedString, selected: bool, dark: bool, accent: &str, window: &mut Window, cx: &mut App) -> impl IntoElement {
    let colors = cached_swatch(scheme, accent, dark);
    let (radius, target_border) = {
        let theme = cx.theme();
        (theme.radius, if selected { theme.ring } else { theme.border })
    };
    let border = transition(format!("scheme-ring-{scheme}"), target_border, Transition::new(TRANSITION_RING), window, cx);
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
                .rounded(radius)
                .overflow_hidden()
                .border_2()
                .border_color(border)
                .child(v_flex().w(px(22.)).h_full().p_1().gap_1().bg(colors.sidebar).child(div().h(px(3.)).w_full().rounded_sm().bg(colors.muted_fg)))
                .child(
                    v_flex()
                        .flex_1()
                        .h_full()
                        .p_1()
                        .gap_1()
                        .bg(colors.background)
                        .child(div().h(px(3.)).w(px(30.)).rounded_sm().bg(colors.muted_fg))
                        .child(div().h(px(10.)).w(px(24.)).rounded_sm().bg(colors.muted_bg))
                        .child(div().h(px(4.)).w(px(14.)).rounded_sm().bg(colors.primary)),
                ),
        )
        .child(div().text_xs().child(label))
}

fn t(cx: &App, key: &str) -> SharedString {
    tr(cx, &format!("settings.{key}"))
}

pub fn render(store: &Entity<Store>, window: &mut Window, cx: &mut App) -> impl IntoElement + use<> {
    let prefs = store.read(cx).prefs.clone();
    let dark = cx.theme().is_dark();
    let accent_colors: Vec<(&'static str, &'static str, Hsla)> = ACCENTS
        .iter()
        .map(|(id, key)| (*id, *key, cached_swatch(&prefs.color_scheme, id, dark).primary))
        .collect();
    let (foreground, transparent, muted_foreground, mono_font_family, mono_font_size) = {
        let theme = cx.theme();
        (theme.foreground, theme.transparent, theme.muted_foreground, theme.mono_font_family.clone(), theme.mono_font_size)
    };
    let sizes = [
        ("uiFontSize", prefs.ui_font_size, UI_FONT_SIZES, "fontSizeTitle", "fontSizeDescription"),
        ("codeFontSize", prefs.code_font_size, CODE_FONT_SIZES, "codeFontSizeTitle", "codeFontSizeDescription"),
    ];
    let color_mode_items = vec![("light", t(cx, "appearanceLight")), ("dark", t(cx, "appearanceDark")), ("system", t(cx, "appearanceSystem"))];
    let color_mode_seg = segmented(
        "color-mode",
        color_mode_items,
        &prefs.theme,
        window,
        cx,
        |value, window, cx| set_prefs(json!({ "theme": value }), window, cx),
    );
    let scheme_swatches: Vec<AnyElement> = SCHEMES
        .iter()
        .map(|(id, key)| {
            let label = t(cx, key);
            swatch(id, label, prefs.color_scheme == *id, dark, &prefs.accent, window, cx).into_any_element()
        })
        .collect();
    let accent_dots: Vec<AnyElement> = accent_colors
        .into_iter()
        .map(|(id, key, fill)| {
            let selected = prefs.accent == id;
            let label = t(cx, key);
            let ring = transition(
                format!("accent-ring-{id}"),
                if selected { foreground } else { transparent },
                Transition::new(TRANSITION_RING),
                window,
                cx,
            );
            div()
                .id(ElementId::Name(format!("accent-{id}").into()))
                .size(px(22.))
                .rounded_full()
                .bg(fill)
                .border_2()
                .border_color(ring)
                .cursor_pointer()
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx))
                .on_click(move |_, window, cx| {
                    if !selected {
                        set_prefs(json!({ "accent": id }), window, cx);
                    }
                })
                .into_any_element()
        })
        .collect();
    v_flex()
        .gap_4()
        .child(heading(cx, "settings.navAppearance", "settings.appearanceLead"))
        .child(group(cx, Some("settings.groupColors"), vec![
            row(cx, t(cx, "colorModeTitle"), Some(t(cx, "colorModeDescription")), color_mode_seg),
            v_flex()
                .px_4()
                .py_3()
                .gap_3()
                .child(v_flex().gap_0p5().child(div().text_sm().child(t(cx, "colorSchemeTitle"))).child(div().text_xs().text_color(muted_foreground).child(t(cx, "colorSchemeDescription"))))
                .child(h_flex().gap_3().flex_wrap().children(scheme_swatches))
                .into_any_element(),
            row(cx, t(cx, "accentTitle"), Some(t(cx, "accentDescription")), h_flex().gap_2().items_center().children(accent_dots)),
        ]))
        .child(group(cx, Some("settings.groupEffects"), vec![row(
            cx,
            t(cx, "opticsTitle"),
            Some(t(cx, "opticsDescription")),
            Switch::new("optics").checked(prefs.optics).on_click(|checked: &bool, window, cx| {
                set_prefs(json!({ "optics": *checked }), window, cx);
            }),
        )]))
        .child(group(
            cx,
            Some("settings.groupFonts"),
            sizes
                .into_iter()
                .map(|(field, value, range, title, description)| {
                    let min = *range.start();
                    let max = *range.end();
                    let stepper = h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            Button::new(ElementId::Name(format!("{field}-down").into()))
                                .outline()
                                .xsmall()
                                .icon(IconName::Minus)
                                .disabled(value <= min)
                                .on_click(move |_, window, cx| set_prefs(json!({ field: (value - 1.0).max(min) }), window, cx)),
                        )
                        .child(div().w(px(32.)).text_center().text_sm().child(format!("{value}")))
                        .child(
                            Button::new(ElementId::Name(format!("{field}-up").into()))
                                .outline()
                                .xsmall()
                                .icon(IconName::Plus)
                                .disabled(value >= max)
                                .on_click(move |_, window, cx| set_prefs(json!({ field: (value + 1.0).min(max) }), window, cx)),
                        )
                        .child(div().text_xs().text_color(muted_foreground).child(t(cx, "fontSizeUnit")));
                    row(cx, t(cx, title), Some(t(cx, description)), stepper)
                })
                .chain([font_row(Use::Ui, &prefs.ui_font, cx), font_row(Use::Code, &prefs.code_font, cx)])
                .chain(std::iter::once(
                    v_flex()
                        .px_4()
                        .py_3()
                        .gap_2()
                        .child(div().child(t(cx, "fontPreviewText")))
                        .child(div().font_family(mono_font_family).text_size(mono_font_size).child(t(cx, "fontPreviewCode")))
                        .into_any_element(),
                ))
                .collect(),
        ))
        .child(h_flex().justify_end().child(
            Button::new("appearance-reset").ghost().small().label(t(cx, "appearanceReset")).on_click(|_, window, cx| {
                set_prefs(
                    json!({ "theme": "system", "colorScheme": "graphite", "accent": "auto", "rainEffects": true, "optics": false, "uiFont": "", "codeFont": "", "uiFontSize": 14, "codeFontSize": 13 }),
                    window,
                    cx,
                );
            }),
        ))
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn swatch_cache_covers_every_preset_and_aligns_font_ranges() {
        assert_eq!(UI_FONT_SIZES, 11.0..=20.0);
        assert_eq!(CODE_FONT_SIZES, 10.0..=24.0);
        let cache = swatch_cache();
        assert_eq!(cache.len(), SCHEMES.len() * (ACCENTS.len() - 1) * 2);
        // Unknown scheme/accent falls back cleanly without panicking.
        let light = cached_swatch("unknown", "auto", false);
        let dark = cached_swatch("graphite", "blue", true);
        assert_ne!(light.background, dark.background);
    }
}
