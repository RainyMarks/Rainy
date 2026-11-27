//! Interface preferences (`prefs.get`, `prefs.set`, `prefs.changed`).

use serde::{Deserialize, Serialize};

/// Interface preferences kept by the Host.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UiPreferences {
    /// `zh` or `en`.
    #[serde(default = "default_locale")]
    pub locale: String,
    /// `system`, `light` or `dark`.
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_ui_font_size")]
    pub ui_font_size: f32,
    #[serde(default = "default_code_font_size")]
    pub code_font_size: f32,
    /// `queue` or `steer`.
    #[serde(default = "default_busy_enter")]
    pub busy_enter: String,
    /// `compact`, `standard` or `detailed`.
    #[serde(default = "default_step_detail")]
    pub step_detail: String,
    #[serde(default = "default_true")]
    pub show_usage: bool,
    #[serde(default = "default_color_scheme")]
    pub color_scheme: String,
    #[serde(default = "default_accent")]
    pub accent: String,
    #[serde(default)]
    pub ui_font: String,
    #[serde(default)]
    pub code_font: String,
    #[serde(default)]
    pub inline_completion: bool,
    #[serde(default)]
    pub completion_model: String,
    #[serde(default = "default_true")]
    pub auto_keep_changes: bool,
    #[serde(default = "default_true")]
    pub rain_effects: bool,
    #[serde(default = "default_true")]
    pub web_search: bool,
    /// Typora's focus and typewriter modes of the rich Markdown editor.
    #[serde(default)]
    pub markdown_focus: bool,
    #[serde(default)]
    pub markdown_typewriter: bool,
    /// Where images pasted into a Markdown document are saved: `assets`, `named` (`<name>.assets`) or `same`.
    #[serde(default = "default_markdown_images")]
    pub markdown_images: String,
    /// How a full context is freed once old tool outputs are cleared: `recall` (an index of the span, read back with
    /// the recall tool) or `summary` (the model writes a checkpoint). 4.6 also wrote `auto`, read as `recall`.
    #[serde(default = "default_context_strategy")]
    pub context_strategy: String,
    /// Windows notifications when a chat finishes or waits for an approval while Rainy is in the background.
    #[serde(default = "default_true")]
    pub notify: bool,
    /// The optical look: a drifting field of characters and a glow behind a new chat, glass cards, the usage overview
    /// and the reasoning slider. Off unless chosen.
    #[serde(default)]
    pub optics: bool,
}

fn default_true() -> bool {
    true
}

fn default_locale() -> String {
    "zh".into()
}

fn default_theme() -> String {
    "system".into()
}

fn default_ui_font_size() -> f32 {
    14.0
}

fn default_code_font_size() -> f32 {
    13.0
}

fn default_busy_enter() -> String {
    "queue".into()
}

fn default_step_detail() -> String {
    "standard".into()
}

fn default_color_scheme() -> String {
    "graphite".into()
}

fn default_accent() -> String {
    "auto".into()
}

fn default_markdown_images() -> String {
    "assets".into()
}

fn default_context_strategy() -> String {
    "recall".into()
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            locale: default_locale(),
            theme: default_theme(),
            ui_font_size: default_ui_font_size(),
            code_font_size: default_code_font_size(),
            busy_enter: default_busy_enter(),
            step_detail: default_step_detail(),
            show_usage: default_true(),
            color_scheme: default_color_scheme(),
            accent: default_accent(),
            ui_font: String::new(),
            code_font: String::new(),
            inline_completion: false,
            completion_model: String::new(),
            auto_keep_changes: default_true(),
            rain_effects: default_true(),
            web_search: default_true(),
            markdown_focus: false,
            markdown_typewriter: false,
            markdown_images: default_markdown_images(),
            context_strategy: default_context_strategy(),
            notify: default_true(),
            optics: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_json_matches_host_aligned_defaults() {
        let prefs: UiPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs, UiPreferences::default());
        assert_eq!(prefs.locale, "zh");
        assert_eq!(prefs.theme, "system");
        assert_eq!(prefs.ui_font_size, 14.0);
        assert_eq!(prefs.code_font_size, 13.0);
        assert!(prefs.show_usage);
        assert_eq!(prefs.color_scheme, "graphite");
        assert_eq!(prefs.accent, "auto");
        assert!(prefs.auto_keep_changes);
        assert!(prefs.rain_effects);
        assert!(prefs.web_search);
        assert!(prefs.notify);
        assert!(!prefs.optics);
    }

    #[test]
    fn tolerates_missing_any_single_or_legacy_fields() {
        // Legacy 3.x/4.0 payload with only a subset of fields and an unknown future key.
        let legacy: UiPreferences = serde_json::from_value(json!({
            "locale": "en",
            "theme": "dark",
            "uiFontSize": 15,
            "futureFlag": true
        }))
        .unwrap();
        assert_eq!(legacy.locale, "en");
        assert_eq!(legacy.theme, "dark");
        assert_eq!(legacy.ui_font_size, 15.0);
        assert_eq!(legacy.color_scheme, "graphite");
        assert_eq!(legacy.markdown_images, "assets");
        assert_eq!(legacy.context_strategy, "recall");
        assert!(legacy.show_usage);
        assert!(legacy.notify);

        // Removing any single field from a full object must still deserialize cleanly to its default.
        let full = serde_json::to_value(UiPreferences::default()).unwrap();
        let object = full.as_object().unwrap();
        assert_eq!(object.len(), 22);
        for key in object.keys() {
            let mut partial = object.clone();
            partial.remove(key);
            let parsed: UiPreferences = serde_json::from_value(serde_json::Value::Object(partial))
                .unwrap_or_else(|err| panic!("missing `{key}` failed to deserialize: {err}"));
            assert_eq!(parsed, UiPreferences::default(), "missing `{key}` drifted from Default");
        }
    }
}
