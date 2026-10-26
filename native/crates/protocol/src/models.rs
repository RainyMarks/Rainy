//! Models, app information and permissions.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The model a chat or future chats use.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelSelection {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

/// Saved model profile; never carries a credential.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelSetup {
    pub provider: String,
    #[serde(rename = "baseURL")]
    pub base_url: String,
    pub model: String,
    pub context_window: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    pub local: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// `openai`, `deepseek` or `qwen`, for the OpenAI Chat Completions protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<String>,
    /// `max_tokens` or `max_completion_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<String>,
    /// Whether requests may carry images. `models.status` always reports the value in effect; a saved profile
    /// without it takes images only for a model of the Claude catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<bool>,
}

/// A built-in preset offered by Settings → Models.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ModelPreset {
    pub name: String,
    pub model: ModelSetup,
}

/// Model settings shown by Settings → Models and the composer's model picker.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelsStatus {
    pub models: Vec<ModelSetup>,
    pub credentials: Vec<String>,
    pub selected: Option<ModelSelection>,
    #[serde(default)]
    pub thinking_levels: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub presets: Vec<ModelPreset>,
    /// Whether stored API keys are encrypted with Windows data protection.
    #[serde(default)]
    pub keys_encrypted: Option<bool>,
    /// Which stored keys this Windows account can no longer decrypt.
    #[serde(default)]
    pub keys_unreadable: Option<String>,
    #[serde(default)]
    pub global_prompt: Option<GlobalPrompt>,
}

/// The prompt added to every request of every chat.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GlobalPrompt {
    pub text: String,
    pub max_chars: usize,
}

/// `app.info`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    /// `Windows`, `WSL` or `SSH`.
    pub environment: String,
    pub execution_target_id: String,
    pub platform: String,
    pub home: String,
    /// The Node.js version the Host runs on; Hosts before 4.0 do not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

/// A registered project directory.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub workspace_id: String,
    pub path: String,
    pub title: String,
}
