//! Interface preferences (`prefs.get`, `prefs.set`, `prefs.changed`).

use serde::{Deserialize, Serialize};

/// Interface preferences kept by the Host.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UiPreferences {
    /// `zh` or `en`.
    pub locale: String,
    /// `system`, `light` or `dark`.
    pub theme: String,
    pub ui_font_size: f32,
    pub code_font_size: f32,
    /// `queue` or `steer`.
    pub busy_enter: String,
    /// `compact`, `standard` or `detailed`.
    pub step_detail: String,
    pub show_usage: bool,
    pub color_scheme: String,
    pub accent: String,
    pub ui_font: String,
    pub code_font: String,
    pub inline_completion: bool,
    pub completion_model: String,
    pub auto_keep_changes: bool,
    pub rain_effects: bool,
    pub web_search: bool,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            locale: "zh".into(),
            theme: "system".into(),
            ui_font_size: 13.0,
            code_font_size: 13.0,
            busy_enter: "queue".into(),
            step_detail: "standard".into(),
            show_usage: false,
            color_scheme: "neutral".into(),
            accent: "auto".into(),
            ui_font: String::new(),
            code_font: String::new(),
            inline_completion: false,
            completion_model: String::new(),
            auto_keep_changes: true,
            rain_effects: true,
            web_search: true,
        }
    }
}
