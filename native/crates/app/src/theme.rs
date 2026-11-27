//! Rainy's themes. `assets/rainy-theme.json` (made by `native/scripts/make-theme.py`) gives the syntax colors and
//! everything Rainy does not set; the surfaces, text and accent come from the chosen color scheme and accent
//! (`assets/schemes.json`, exported from `src/renderer/theme/schemes.css` by `native/scripts/export-schemes.py`).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use gpui_kit::component::button::ButtonCustomVariant;
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeConfig, ThemeMode};
use gpui_kit::{App, Global, Hsla, SharedString, Window, px};
use rainy_protocol::UiPreferences;
use serde_json::Value;

#[derive(Clone, PartialEq)]
struct AppliedTheme {
    mode: ThemeMode,
    color_scheme: String,
    accent: String,
    ui_font: String,
    code_font: String,
    ui_font_size: f32,
    code_font_size: f32,
}

impl Global for AppliedTheme {}

type ThemePairCache = Mutex<HashMap<(String, String), (ThemeConfig, ThemeConfig)>>;

/// Cache of resolved `(light, dark)` themes keyed by `(scheme, accent)` so repeated lookups do not re-parse JSON.
fn theme_cache() -> &'static ThemePairCache {
    static CACHE: OnceLock<ThemePairCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

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
            // The selected chat, file, row or section is tinted with the accent, as selected buttons are.
            ("list.active.background", format!("{}38", a("500"))),
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
            ("list.active.background", format!("{}1f", a("500"))),
        ]
    }
}

/// The light and dark theme for a scheme and accent preference.
pub fn themes_for(scheme: &str, accent: &str) -> (Rc<ThemeConfig>, Rc<ThemeConfig>) {
    let palettes = palettes();
    let scheme_key = if palettes.schemes.contains_key(scheme) { scheme } else { "graphite" };
    let resolved_accent = if accent == "auto" { scheme_accent(scheme) } else { accent };
    let accent_key = if palettes.accents.contains_key(resolved_accent) { resolved_accent } else { "blue" };
    let key = (scheme_key.to_owned(), accent_key.to_owned());
    let mut cache = theme_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (light, dark) = cache.entry(key).or_insert_with(|| {
        let gray = &palettes.schemes[scheme_key];
        let accent = &palettes.accents[accent_key];
        let build = |base: &Value, dark: bool| {
            let mut theme = base.clone();
            for (key, value) in colors(dark, gray, accent) {
                theme["colors"][key] = Value::String(value);
            }
            serde_json::from_value::<ThemeConfig>(theme).expect("valid theme")
        };
        (build(&palettes.light, false), build(&palettes.dark, true))
    });
    (Rc::new(light.clone()), Rc::new(dark.clone()))
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
/// Skips rebuilding themes when none of the theme-affecting preferences changed (for example, toggling notifications).
pub fn apply(prefs: &UiPreferences, window: Option<&mut Window>, cx: &mut App) {
    let mode = match prefs.theme.as_str() {
        "light" => ThemeMode::Light,
        "dark" => ThemeMode::Dark,
        _ => window.as_ref().map(|window| window.appearance()).unwrap_or_else(|| cx.window_appearance()).into(),
    };
    let next = AppliedTheme {
        mode,
        color_scheme: prefs.color_scheme.clone(),
        accent: prefs.accent.clone(),
        ui_font: prefs.ui_font.clone(),
        code_font: prefs.code_font.clone(),
        ui_font_size: prefs.ui_font_size.clamp(11.0, 20.0),
        code_font_size: prefs.code_font_size.clamp(10.0, 24.0),
    };
    if cx.try_global::<AppliedTheme>().is_some_and(|applied| *applied == next) {
        return;
    }
    let palette_changed = cx
        .try_global::<AppliedTheme>()
        .is_none_or(|applied| applied.color_scheme != next.color_scheme || applied.accent != next.accent);
    if palette_changed {
        let (light, dark) = themes_for(&next.color_scheme, &next.accent);
        Theme::update(cx, |theme| {
            theme.light_theme = light;
            theme.dark_theme = dark;
        });
    }
    let mode_changed = cx.try_global::<AppliedTheme>().is_none_or(|applied| applied.mode != next.mode);
    if palette_changed || mode_changed {
        Theme::change(mode, window, cx);
    }
    // A downloaded font is loaded before the theme names it.
    crate::fonts::load_chosen(prefs, cx);
    Theme::update(cx, |theme| {
        theme.font_family = family(&next.ui_font, UI_FONT);
        theme.mono_font_family = family(&next.code_font, CODE_FONT);
        theme.font_size = px(next.ui_font_size);
        theme.mono_font_size = px(next.code_font_size);
    });
    cx.set_global(next);
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

    #[test]
    fn a_selected_row_is_tinted_with_the_accent() {
        let palettes = palettes();
        let (gray, violet) = (&palettes.schemes["graphite"], &palettes.accents["violet"]);
        let selected = |dark: bool| colors(dark, gray, violet).into_iter().find(|(key, _)| *key == "list.active.background").map(|(_, value)| value);
        assert_eq!(selected(false).as_deref(), Some("#8462dc1f"));
        assert_eq!(selected(true).as_deref(), Some("#8462dc38"));
    }
}
