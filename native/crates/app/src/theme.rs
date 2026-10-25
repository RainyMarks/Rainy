//! Rainy's themes. `assets/rainy-theme.json` (made by `native/scripts/make-theme.py`) gives the syntax colors and
//! everything Rainy does not set; the surfaces, text and accent come from the chosen color scheme and accent
//! (`assets/schemes.json`, exported from `src/renderer/theme/schemes.css` by `native/scripts/export-schemes.py`).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

use gpui_kit::component::button::ButtonCustomVariant;
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeConfig, ThemeMode};
use gpui_kit::{App, Hsla, SharedString, Window, px};
use rainy_protocol::UiPreferences;
use serde_json::Value;

const THEMES: &str = include_str!("../assets/rainy-theme.json");
const SCHEMES: &str = include_str!("../assets/schemes.json");
const UI_FONT: &str = "Segoe UI";
const CODE_FONT: &str = "Cascadia Mono";

type Ramp = HashMap<String, String>;

struct Palettes {
    schemes: HashMap<String, Ramp>,
    accents: HashMap<String, Ramp>,
    light: Value,
    dark: Value,
}

fn palettes() -> &'static Palettes {
    static PALETTES: OnceLock<Palettes> = OnceLock::new();
    PALETTES.get_or_init(|| {
        let ramps: HashMap<String, HashMap<String, Ramp>> = serde_json::from_str(SCHEMES).expect("valid schemes.json");
        let themes: Value = serde_json::from_str(THEMES).expect("valid rainy-theme.json");
        let find = |mode: &str| {
            themes["themes"].as_array().and_then(|list| list.iter().find(|theme| theme["mode"] == mode)).cloned().expect("theme")
        };
        Palettes {
            schemes: ramps["schemes"].clone(),
            accents: ramps["accents"].clone(),
            light: find("light"),
            dark: find("dark"),
        }
    })
}

/// The accent a scheme pairs with when the preference is `auto` (`SCHEME_ACCENTS` in `src/shared/appearance.ts`).
pub fn scheme_accent(scheme: &str) -> &'static str {
    match scheme {
        "midnight" => "violet",
        "sand" => "orange",
        "forest" => "green",
        "rose" => "pink",
        _ => "blue",
    }
}

/// Surface, text and accent colors for one mode.
fn colors(dark: bool, gray: &Ramp, accent: &Ramp) -> Vec<(&'static str, String)> {
    let g = |step: &str| gray.get(step).cloned().unwrap_or_else(|| "#808080".into());
    let a = |step: &str| accent.get(step).cloned().unwrap_or_else(|| "#4176e6".into());
    if dark {
        vec![
            ("background", g("925")), ("foreground", g("100")), ("caret", g("100")), ("border", "#ffffff17".into()),
            ("muted.background", g("825")), ("muted.foreground", g("500")),
            ("accent.background", "#ffffff0f".into()), ("accent.foreground", g("100")),
            ("popover.background", g("850")), ("popover.foreground", g("100")),
            ("primary.background", a("500")), ("primary.hover.background", a("450")), ("primary.active.background", a("600")),
            ("secondary.background", g("825")), ("secondary.hover.background", g("800")), ("secondary.active.background", g("775")),
            ("secondary.foreground", g("100")), ("ring", a("400")), ("selection.background", a("800")),
            ("link.foreground", a("400")), ("link.hover.foreground", a("300")), ("link.active.foreground", a("400")),
            ("list.background", g("875")), ("list.even.background", g("875")), ("list.head.background", g("875")),
            ("sidebar.background", g("875")), ("sidebar.foreground", g("100")), ("sidebar.accent.foreground", g("100")),
            ("sidebar.primary.background", a("500")),
            ("title_bar.background", g("875")), ("status_bar.background", g("875")),
            ("tab.background", g("875")), ("tab.active.background", g("925")), ("tab.foreground", g("500")),
            ("tab.active.foreground", g("100")), ("tab_bar.background", g("875")), ("tab_bar.segmented.background", g("825")),
            ("progress_bar.background", a("400")), ("drag_border", a("400")), ("info.background", a("500")),
            ("window.border", g("800")), ("group_box.background", g("850")), ("group_box.foreground", g("100")),
            ("table.background", g("875")), ("switch.background", g("750")),
        ]
    } else {
        vec![
            ("background", g("00")), ("foreground", g("900")), ("caret", g("900")), ("border", "#0000001a".into()),
            ("muted.background", g("75")), ("muted.foreground", g("600")),
            ("accent.background", "#0000000a".into()), ("accent.foreground", g("900")),
            ("popover.background", g("00")), ("popover.foreground", g("900")),
            ("primary.background", a("500")), ("primary.hover.background", a("450")), ("primary.active.background", a("600")),
            ("secondary.background", g("75")), ("secondary.hover.background", g("100")), ("secondary.active.background", g("150")),
            ("secondary.foreground", g("900")), ("ring", a("500")), ("selection.background", a("200")),
            ("link.foreground", a("500")), ("link.hover.foreground", a("600")), ("link.active.foreground", a("500")),
            ("list.background", g("00")), ("list.even.background", g("25")), ("list.head.background", g("25")),
            ("sidebar.background", g("40")), ("sidebar.foreground", g("900")), ("sidebar.accent.foreground", g("900")),
            ("sidebar.primary.background", a("500")),
            ("title_bar.background", g("40")), ("status_bar.background", g("40")),
            ("tab.background", g("40")), ("tab.active.background", g("00")), ("tab.foreground", g("600")),
            ("tab.active.foreground", g("900")), ("tab_bar.background", g("40")), ("tab_bar.segmented.background", g("100")),
            ("progress_bar.background", a("500")), ("drag_border", a("500")), ("info.background", a("500")),
            ("window.border", g("200")), ("group_box.background", g("25")), ("group_box.foreground", g("900")),
            ("table.background", g("00")), ("switch.background", g("200")),
        ]
    }
}

/// The light and dark theme for a scheme and accent preference.
pub fn themes_for(scheme: &str, accent: &str) -> (Rc<ThemeConfig>, Rc<ThemeConfig>) {
    let palettes = palettes();
    let gray = palettes.schemes.get(scheme).or_else(|| palettes.schemes.get("graphite")).expect("graphite");
    let accent = if accent == "auto" { scheme_accent(scheme) } else { accent };
    let accent = palettes.accents.get(accent).or_else(|| palettes.accents.get("blue")).expect("blue");
    let build = |base: &Value, dark: bool| {
        let mut theme = base.clone();
        for (key, value) in colors(dark, gray, accent) {
            theme["colors"][key] = Value::String(value);
        }
        Rc::new(serde_json::from_value::<ThemeConfig>(theme).expect("valid theme"))
    };
    (build(&palettes.light, false), build(&palettes.dark, true))
}

/// Text in the warning color. The theme's amber is a fill color: as text on a light surface it is 2:1, so light mode
/// darkens it to about 5:1.
pub fn warning_text(cx: &App) -> Hsla {
    let warning = cx.theme().warning;
    if cx.theme().mode.is_dark() { warning } else { Hsla { l: warning.l.min(0.30), ..warning } }
}

/// A selected ghost button (the side tabs, the pane toggles): GPUI Kit's own selected background is about 1.2:1
/// against the bar, so a selected one shows the accent color on a tinted background, in both modes.
pub fn selected_button(cx: &App) -> ButtonCustomVariant {
    let theme = cx.theme();
    let dark = theme.mode.is_dark();
    let foreground = if dark { theme.link } else { theme.primary_active };
    let tint = |opacity: f32| theme.primary.opacity(if dark { opacity * 1.8 } else { opacity });
    ButtonCustomVariant::new(cx).color(tint(0.12)).foreground(foreground).hover(tint(0.18)).active(tint(0.24))
}

/// The first family of a comma-separated font preference, or the default.
fn family(preference: &str, default: &str) -> SharedString {
    preference
        .split(',')
        .map(|name| name.trim().trim_matches(['"', '\'']))
        .find(|name| !name.is_empty())
        .unwrap_or(default)
        .to_owned()
        .into()
}

/// Apply the theme, color scheme, accent, font and font size preferences.
pub fn apply(prefs: &UiPreferences, window: Option<&mut Window>, cx: &mut App) {
    let mode = match prefs.theme.as_str() {
        "light" => ThemeMode::Light,
        "dark" => ThemeMode::Dark,
        _ => window.as_ref().map(|window| window.appearance()).unwrap_or_else(|| cx.window_appearance()).into(),
    };
    let (light, dark) = themes_for(&prefs.color_scheme, &prefs.accent);
    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
    });
    Theme::change(mode, window, cx);
    Theme::update(cx, |theme| {
        theme.font_family = family(&prefs.ui_font, UI_FONT);
        theme.mono_font_family = family(&prefs.code_font, CODE_FONT);
        theme.font_size = px(prefs.ui_font_size.clamp(11.0, 20.0));
        theme.mono_font_size = px(prefs.code_font_size.clamp(10.0, 24.0));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_first_font_family() {
        assert_eq!(family("", UI_FONT), "Segoe UI");
        assert_eq!(family("'LXGW WenKai', serif", UI_FONT), "LXGW WenKai");
    }

    #[test]
    fn builds_every_scheme_and_accent() {
        for scheme in ["graphite", "slate", "midnight", "sand", "forest", "rose", "black", "unknown"] {
            for accent in ["auto", "blue", "violet", "green", "orange", "pink", "teal"] {
                let (light, dark) = themes_for(scheme, accent);
                assert!(!light.mode.is_dark() && dark.mode.is_dark());
            }
        }
        assert_eq!(scheme_accent("forest"), "green");
    }
}
