//! Models, app information and permissions.

pub mod reasoning;

use serde::{Deserialize, Serialize};

/// The model a chat or future chats use.
///
/// `thinking` names any [`reasoning::Level`]. It is stored the way 3.x and 4.0–4.2 keep reading it: `thinking` with
/// the nearest of the names they know (off, low, high, max) and `thinkingLevel` with the level itself when that
/// differs ([`reasoning::store_level`], [`reasoning::read_level`]).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(from = "StoredSelection", into = "StoredSelection")]
pub struct ModelSelection {
    pub provider: String,
    pub model: String,
    pub thinking: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredSelection {
    provider: String,
    model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking_level: Option<String>,
}

impl From<StoredSelection> for ModelSelection {
    fn from(stored: StoredSelection) -> Self {
        Self { provider: stored.provider, model: stored.model, thinking: reasoning::read_level(stored.thinking, stored.thinking_level) }
    }
}

impl From<ModelSelection> for StoredSelection {
    fn from(selection: ModelSelection) -> Self {
        let (thinking, thinking_level) = reasoning::store_level(selection.thinking.as_deref());
        Self { provider: selection.provider, model: selection.model, thinking, thinking_level }
    }
}

/// Saved model profile; never carries a credential. `thinking` is the level new chats start with, stored like
/// [`ModelSelection::thinking`].
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(from = "StoredSetup", into = "StoredSetup")]
pub struct ModelSetup {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub context_window: f64,
    pub max_tokens: Option<f64>,
    pub local: bool,
    pub api: Option<String>,
    pub thinking: Option<String>,
    /// `openai`, `deepseek` or `qwen`, for the OpenAI Chat Completions protocol.
    pub thinking_format: Option<String>,
    /// `max_tokens` or `max_completion_tokens`.
    pub max_tokens_field: Option<String>,
    /// Whether requests may carry images. `models.status` always reports the value in effect; a saved profile
    /// without it takes images only for a model of the Claude catalog.
    pub images: Option<bool>,
}

impl ModelSetup {
    /// What this profile's model offers for reasoning.
    pub fn reasoning(&self) -> reasoning::Reasoning {
        reasoning::reasoning(&reasoning::Profile {
            api: reasoning::Api::of(self.api.as_deref(), self.local),
            base_url: &self.base_url,
            provider: &self.provider,
            model: &self.model,
            thinking_format: self.thinking_format.as_deref(),
            local: self.local,
            reasoning: self.thinking.is_some(),
        })
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredSetup {
    provider: String,
    #[serde(rename = "baseURL")]
    base_url: String,
    model: String,
    context_window: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_tokens: Option<f64>,
    local: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_tokens_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    images: Option<bool>,
}

impl From<StoredSetup> for ModelSetup {
    fn from(stored: StoredSetup) -> Self {
        Self {
            provider: stored.provider,
            base_url: stored.base_url,
            model: stored.model,
            context_window: stored.context_window,
            max_tokens: stored.max_tokens,
            local: stored.local,
            api: stored.api,
            thinking: reasoning::read_level(stored.thinking, stored.thinking_level),
            thinking_format: stored.thinking_format,
            max_tokens_field: stored.max_tokens_field,
            images: stored.images,
        }
    }
}

impl From<ModelSetup> for StoredSetup {
    fn from(setup: ModelSetup) -> Self {
        let (thinking, thinking_level) = reasoning::store_level(setup.thinking.as_deref());
        Self {
            provider: setup.provider,
            base_url: setup.base_url,
            model: setup.model,
            context_window: setup.context_window,
            max_tokens: setup.max_tokens,
            local: setup.local,
            api: setup.api,
            thinking,
            thinking_level,
            thinking_format: setup.thinking_format,
            max_tokens_field: setup.max_tokens_field,
            images: setup.images,
        }
    }
}

/// A built-in preset offered by Settings → Models.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ModelPreset {
    pub name: String,
    pub model: ModelSetup,
}

/// Model settings shown by Settings → Models and the composer's model control. Each model's reasoning levels come
/// from its profile ([`ModelSetup::reasoning`]).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelsStatus {
    pub models: Vec<ModelSetup>,
    pub credentials: Vec<String>,
    pub selected: Option<ModelSelection>,
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

impl ModelsStatus {
    /// The saved profile of a provider id; one is kept per provider.
    pub fn model(&self, provider: &str) -> Option<&ModelSetup> {
        self.models.iter().find(|model| model.provider == provider)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn older_versions_read_the_nearest_level() {
        let selection = ModelSelection { provider: "p".into(), model: "m".into(), thinking: Some("xhigh".into()) };
        let stored = serde_json::to_value(&selection).unwrap();
        assert_eq!(stored, json!({ "provider": "p", "model": "m", "thinking": "high", "thinkingLevel": "xhigh" }));
        assert_eq!(serde_json::from_value::<ModelSelection>(stored).unwrap(), selection);
        // 3.x and 4.0–4.2 drop `thinkingLevel` when they write the selection again, so their level is the one read.
        let rewritten = json!({ "provider": "p", "model": "m", "thinking": "low", "thinkingLevel": "xhigh" });
        assert_eq!(serde_json::from_value::<ModelSelection>(rewritten).unwrap().thinking.as_deref(), Some("low"));
        let legacy = ModelSelection { thinking: Some("max".into()), ..selection };
        assert_eq!(serde_json::to_value(&legacy).unwrap(), json!({ "provider": "p", "model": "m", "thinking": "max" }));
        let setup: ModelSetup = serde_json::from_value(json!({
            "provider": "p", "baseURL": "https://h", "model": "gpt-5.5", "contextWindow": 1000, "local": false, "thinking": "high", "thinkingLevel": "medium"
        }))
        .unwrap();
        assert_eq!(setup.thinking.as_deref(), Some("medium"));
        let written = serde_json::to_value(&setup).unwrap();
        assert_eq!((&written["thinking"], &written["thinkingLevel"], &written["baseURL"]), (&json!("high"), &json!("medium"), &json!("https://h")));
        assert!(written.get("maxTokens").is_none());
    }

    #[test]
    fn a_flattened_selection_keeps_its_level() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Params {
            session_id: String,
            #[serde(flatten)]
            selection: ModelSelection,
        }
        let params: Params = serde_json::from_value(json!({ "sessionId": "s", "provider": "p", "model": "m", "thinking": "high", "thinkingLevel": "xhigh" })).unwrap();
        assert_eq!((params.session_id.as_str(), params.selection.thinking.as_deref()), ("s", Some("xhigh")));
    }
}
