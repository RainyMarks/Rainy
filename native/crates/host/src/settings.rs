//! `settings.json` and the API keys under the Host home (3.x `settings.ts`). The file format is 3.x's: a 3.x or 4.0 Host
//! reads what this one writes and the other way round.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::credentials::{CredentialFiles, Keys, Seal};
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::store::write_json;

/// Longest global prompt, in characters.
pub const GLOBAL_PROMPT_MAX_CHARS: usize = 4000;
/// Placeholder stored for local models that accept any key.
pub const LOCAL_NO_KEY: &str = "rainy-local-no-key";
/// Remembered approvals per project are capped, so a runaway model cannot grow the file without bound.
const MAX_RULES: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSetup {
    pub provider: String,
    #[serde(rename = "baseURL")]
    pub base_url: String,
    pub model: String,
    pub context_window: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    pub local: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<String>,
    /// Whether requests may carry images; unset, only a model of the Claude catalog takes them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<bool>,
}

/// The model a chat or future chats use; the page reads the same type.
pub use rainy_protocol::models::ModelSelection;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub enabled: bool,
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// Tools the server lists that are allowed; empty allows all.
    pub tools: Vec<String>,
    /// Tools turned off in Settings; a tool the server adds later is on.
    #[serde(default, rename = "disabledTools", skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
}

impl McpServerConfig {
    /// Whether the agent may call a tool of this server, by its own name or as `mcp__<server>__<tool>`.
    pub fn allows(&self, tool: &str, qualified: &str) -> bool {
        let listed = |names: &[String]| names.iter().any(|name| name == tool || name == qualified);
        (self.tools.is_empty() || listed(&self.tools)) && !listed(&self.disabled_tools)
    }

    /// Whether a connection made for `other` serves this config too: the tool switches never need a reconnect.
    pub fn same_connection(&self, other: &Self) -> bool {
        let bare = |config: &Self| Self { tools: Vec::new(), disabled_tools: Vec::new(), ..config.clone() };
        bare(self) == bare(other)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PermissionRules {
    pub commands: Vec<String>,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Security {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<BTreeMap<String, PermissionRules>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxySettings {
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self { mode: "system".into(), url: None }
    }
}

/// The persisted settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsData {
    pub version: u8,
    pub models: Vec<ModelSetup>,
    pub selected: Option<ModelSelection>,
    pub global_prompt: String,
    pub mcp_servers: Vec<McpServerConfig>,
    /// Interface preferences as stored; values this version does not accept are dropped when read.
    pub ui: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workbench: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<Security>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxySettings>,
}

impl Default for SettingsData {
    fn default() -> Self {
        Self { version: 1, models: Vec::new(), selected: None, global_prompt: String::new(), mcp_servers: Vec::new(), ui: Map::new(), workbench: None, security: None, proxy: None }
    }
}

/// Interface defaults for a new installation.
pub fn default_prefs() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "locale": "zh", "theme": "system", "uiFontSize": 14, "codeFontSize": 13, "busyEnter": "queue", "stepDetail": "standard",
        "showUsage": true, "colorScheme": "graphite", "accent": "auto", "uiFont": "", "codeFont": "", "inlineCompletion": false,
        "completionModel": "", "autoKeepChanges": true, "rainEffects": true, "webSearch": true, "markdownFocus": false,
        "markdownTypewriter": false, "markdownImages": "assets"
    }) else {
        unreachable!()
    };
    map
}

fn one_of(value: &Value, allowed: &[&str]) -> bool {
    value.as_str().is_some_and(|text| allowed.contains(&text))
}

fn int_between(value: &Value, min: i64, max: i64) -> bool {
    value.as_i64().is_some_and(|number| (min..=max).contains(&number)) && value.as_f64().is_some_and(|number| number.fract() == 0.0)
}

/// Font families as Settings accepts them: up to four names of letters, digits and a few marks, optionally quoted.
pub fn font_families(value: &str) -> Option<Vec<String>> {
    if value.chars().count() > 160 {
        return None;
    }
    if value.trim().is_empty() {
        return Some(Vec::new());
    }
    let names: Vec<String> = value
        .split(',')
        .map(|name| {
            let name = name.trim();
            let unquoted = ['\'', '"'].iter().find_map(|quote| name.strip_prefix(*quote).and_then(|rest| rest.strip_suffix(*quote))).unwrap_or(name);
            unquoted.trim().to_owned()
        })
        .collect();
    let valid = |name: &String| {
        (1..=64).contains(&name.chars().count())
            && name.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '_' | '+' | '&' | '(' | ')' | '-') || unicode_mark(c))
    };
    (names.len() <= 4 && names.iter().all(valid)).then_some(names)
}

fn unicode_mark(c: char) -> bool {
    // Combining marks, as `\p{M}` accepts.
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

pub fn provider_id(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && text.len() <= 48
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Whether one interface preference holds a value this version accepts.
pub fn valid_pref(key: &str, value: &Value) -> bool {
    const SCHEMES: [&str; 7] = ["graphite", "slate", "midnight", "sand", "forest", "rose", "black"];
    const ACCENTS: [&str; 7] = ["auto", "blue", "violet", "green", "orange", "pink", "teal"];
    match key {
        "locale" => one_of(value, &["zh", "en"]),
        "theme" => one_of(value, &["system", "light", "dark"]),
        "uiFontSize" | "codeFontSize" => int_between(value, 12, 17),
        "busyEnter" => one_of(value, &["queue", "steer"]),
        "stepDetail" => one_of(value, &["compact", "standard", "detailed"]),
        "colorScheme" => one_of(value, &SCHEMES),
        "accent" => one_of(value, &ACCENTS),
        "uiFont" | "codeFont" => value.as_str().is_some_and(|text| font_families(text).is_some()),
        "showUsage" | "inlineCompletion" | "autoKeepChanges" | "rainEffects" | "webSearch" | "markdownFocus" | "markdownTypewriter" => value.is_boolean(),
        "markdownImages" => one_of(value, &["assets", "named", "same"]),
        "completionModel" => value.as_str().is_some_and(|text| text.is_empty() || provider_id(text)),
        _ => false,
    }
}

fn valid_url(text: &str) -> bool {
    url::Url::parse(text).is_ok()
}

fn header_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn header_value(value: &str) -> bool {
    value.bytes().all(|byte| byte == b'\t' || (byte >= 0x20 && byte != 0x7f))
}

pub fn check_model(model: &ModelSetup) -> HostResult<()> {
    let ok = provider_id(&model.provider)
        && valid_url(&model.base_url)
        && !model.model.is_empty()
        && model.context_window >= 4096
        && model.max_tokens.is_none_or(|tokens| tokens > 0)
        && model.api.as_deref().is_none_or(|api| ["openai-completions", "openai-responses", "anthropic-messages"].contains(&api))
        && model.thinking.as_deref().is_none_or(|level| ["off", "low", "high", "max"].contains(&level))
        && model.thinking_format.as_deref().is_none_or(|format| ["openai", "deepseek", "qwen"].contains(&format))
        && model.max_tokens_field.as_deref().is_none_or(|field| ["max_tokens", "max_completion_tokens"].contains(&field));
    if ok { Ok(()) } else { Err(HostError::invalid(l("模型设置无效。", "The model settings are not valid."))) }
}

pub fn check_server(server: &McpServerConfig) -> HostResult<()> {
    let name = (1..=32).contains(&server.name.len()) && server.name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    let transport = match server.transport.as_str() {
        "stdio" => server.command.as_deref().is_some_and(|command| !command.is_empty()),
        "streamable-http" => server.url.as_deref().is_some_and(valid_url),
        _ => false,
    };
    let headers = server.headers.as_ref().is_none_or(|headers| {
        let mut seen = std::collections::HashSet::new();
        headers.iter().all(|(name, value)| header_name(name) && header_value(value) && seen.insert(name.to_lowercase()))
    });
    if !name {
        return Err(HostError::invalid(l("MCP 服务器名称只能包含 1–32 个字母、数字、_ 或 -。", "MCP server names are 1–32 letters, digits, _ or -.")));
    }
    if !transport {
        return Err(HostError::invalid(l("stdio 服务器需要命令，HTTP 服务器需要网址。", "stdio servers need a command; HTTP servers need a URL.")));
    }
    if !headers {
        return Err(HostError::invalid(l("MCP 请求头需要有效且互不相同的名称和值。", "MCP request headers need valid, distinct names and values.")));
    }
    Ok(())
}

fn check_data(data: &SettingsData) -> HostResult<()> {
    for model in &data.models {
        check_model(model)?;
    }
    for server in &data.mcp_servers {
        check_server(server)?;
    }
    if data.global_prompt.chars().count() > GLOBAL_PROMPT_MAX_CHARS {
        return Err(HostError::invalid("The global prompt is too long."));
    }
    if let Some(rules) = data.security.as_ref().and_then(|security| security.rules.as_ref()) {
        for rule in rules.values() {
            let fits = |list: &Vec<String>, max: usize| list.len() <= MAX_RULES && list.iter().all(|item| (1..=max).contains(&item.chars().count()));
            if !fits(&rule.commands, 200) || !fits(&rule.tools, 128) {
                return Err(HostError::invalid("A remembered approval is not valid."));
            }
        }
    }
    Ok(())
}

/// Read `settings.json` as 3.x does: preferences this version does not accept are dropped instead of failing the file.
fn parse_file(raw: Value) -> io::Result<SettingsData> {
    let mut data: SettingsData = serde_json::from_value(raw).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("settings.json: {error}")))?;
    data.ui.retain(|key, value| valid_pref(key, value));
    check_data(&data).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("settings.json: {}", error.message)))?;
    Ok(data)
}

/// What can be kept of a `settings.json` that [`parse_file`] refuses (edited by hand, or written by another version):
/// every part that is valid on its own, so one bad model or server does not take the others, or the Host, down.
fn salvage(raw: Option<Value>) -> SettingsData {
    let Some(Value::Object(raw)) = raw else { return SettingsData::default() };
    let field = |name: &str| raw.get(name).cloned().unwrap_or(Value::Null);
    let each = |name: &str| match field(name) {
        Value::Array(items) => items,
        _ => Vec::new(),
    };
    let mut data = SettingsData {
        models: each("models").into_iter().filter_map(|item| serde_json::from_value(item).ok()).filter(|model| check_model(model).is_ok()).collect(),
        selected: serde_json::from_value(field("selected")).unwrap_or_default(),
        global_prompt: serde_json::from_value::<String>(field("globalPrompt"))
            .ok()
            .filter(|prompt| prompt.chars().count() <= GLOBAL_PROMPT_MAX_CHARS)
            .unwrap_or_default(),
        mcp_servers: each("mcpServers").into_iter().filter_map(|item| serde_json::from_value(item).ok()).filter(|server| check_server(server).is_ok()).collect(),
        ui: serde_json::from_value(field("ui")).unwrap_or_default(),
        workbench: serde_json::from_value(field("workbench")).unwrap_or_default(),
        security: serde_json::from_value(field("security")).unwrap_or_default(),
        proxy: serde_json::from_value(field("proxy")).unwrap_or_default(),
        ..SettingsData::default()
    };
    data.ui.retain(|key, value| valid_pref(key, value));
    if check_data(&data).is_err() {
        data.security = None;
    }
    if check_data(&data).is_err() { SettingsData::default() } else { data }
}

/// Credential name of a provider: the DeepSeek preset uses `DEEPSEEK_API_KEY`, both Claude presets share
/// `ANTHROPIC_API_KEY`, every other provider has its own name. An environment variable of that name overrides it.
pub fn credential_name(provider: &str) -> String {
    match provider {
        "rainy-deepseek" => "DEEPSEEK_API_KEY".into(),
        "rainy-claude" | "rainy-claude-haiku" => "ANTHROPIC_API_KEY".into(),
        _ => format!("RAINY_{}_KEY", provider.replace('-', "_").to_uppercase()),
    }
}

fn credential_label(name: &str) -> String {
    match name {
        "DEEPSEEK_API_KEY" => "DeepSeek".into(),
        "ANTHROPIC_API_KEY" => "Claude".into(),
        _ => name.strip_prefix("RAINY_").and_then(|rest| rest.strip_suffix("_KEY")).map_or_else(|| name.to_owned(), |provider| provider.to_lowercase().replace('_', "-")),
    }
}

struct Inner {
    data: SettingsData,
    keys: Keys,
    credentials: CredentialFiles,
}

/// Settings and credentials of one Host. Every change is written before it is visible.
pub struct Settings {
    file: PathBuf,
    inner: Mutex<Inner>,
}

impl Settings {
    /// Load both files; a missing `settings.json` starts empty and keeps the stored keys.
    pub fn load(home: &Path, seal: Option<Seal>) -> io::Result<Self> {
        let file = home.join("settings.json");
        let mut credentials = CredentialFiles::new(home, seal);
        let keys = credentials.read()?;
        let bytes = match std::fs::read(&file) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let data = match bytes {
            Some(bytes) => {
                let raw = serde_json::from_slice::<Value>(&bytes);
                match raw.as_ref().map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("settings.json: {error}"))).and_then(|raw| parse_file(raw.clone())) {
                    Ok(data) => data,
                    Err(error) => {
                        // The file as it was stays beside the one written now, for the user to look at.
                        let kept = home.join(format!("settings.json.damaged-{}", crate::text::now_ms()));
                        std::fs::write(&kept, &bytes)?;
                        let data = salvage(raw.ok());
                        log::warn!("{error}; kept what is valid and the file as it was in {}", kept.display());
                        write_json(&file, &data)?;
                        data
                    }
                }
            }
            None => {
                let data = SettingsData::default();
                write_json(&file, &data)?;
                data
            }
        };
        Ok(Self { file, inner: Mutex::new(Inner { data, keys, credentials }) })
    }

    pub fn get(&self) -> SettingsData {
        self.inner.lock().unwrap().data.clone()
    }

    pub fn keys_sealed(&self) -> bool {
        self.inner.lock().unwrap().credentials.sealed()
    }

    /// Apply a change and persist it; the change is refused when the result is not valid.
    pub fn update(&self, change: impl FnOnce(&mut SettingsData) -> HostResult<()>) -> HostResult<SettingsData> {
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.data.clone();
        change(&mut next)?;
        next.ui.retain(|key, value| valid_pref(key, value));
        check_data(&next)?;
        write_json(&self.file, &next)?;
        inner.data = next;
        Ok(inner.data.clone())
    }

    /// Interface preferences with defaults filled in.
    pub fn prefs(&self) -> Map<String, Value> {
        let mut prefs = default_prefs();
        prefs.extend(self.inner.lock().unwrap().data.ui.clone());
        prefs
    }

    pub fn locale(&self) -> String {
        self.prefs().get("locale").and_then(Value::as_str).unwrap_or("zh").to_owned()
    }

    /// Update interface preferences; every given field must be valid.
    pub fn set_prefs(&self, change: Map<String, Value>) -> HostResult<Map<String, Value>> {
        if let Some((key, _)) = change.iter().find(|(key, value)| default_prefs().contains_key(*key) && !valid_pref(key, value)) {
            return Err(HostError::invalid(format!("Invalid preference {key}")));
        }
        self.update(|data| {
            data.ui.extend(change);
            Ok(())
        })?;
        Ok(self.prefs())
    }

    pub fn default_permission(&self) -> String {
        self.inner.lock().unwrap().data.security.as_ref().and_then(|security| security.default_mode.clone()).unwrap_or_else(|| "ask".into())
    }

    pub fn set_default_permission(&self, mode: &str) -> HostResult<String> {
        if !["read-only", "ask", "auto"].contains(&mode) {
            return Err(HostError::invalid("Unknown permission mode"));
        }
        self.update(|data| {
            data.security.get_or_insert_with(Security::default).default_mode = Some(mode.to_owned());
            Ok(())
        })?;
        Ok(self.default_permission())
    }

    pub fn permission_rules(&self, workspace_id: &str) -> PermissionRules {
        let inner = self.inner.lock().unwrap();
        inner.data.security.as_ref().and_then(|security| security.rules.as_ref()).and_then(|rules| rules.get(workspace_id)).cloned().unwrap_or_default()
    }

    /// Remember (`present`) or forget an approval for a project: a command prefix or a tool name.
    pub fn set_permission_rule(&self, workspace_id: &str, command: bool, rule: &str, present: bool) -> HostResult<PermissionRules> {
        self.update(|data| {
            let security = data.security.get_or_insert_with(Security::default);
            let all = security.rules.get_or_insert_with(BTreeMap::new);
            let mut current = all.get(workspace_id).cloned().unwrap_or_default();
            let list = if command { &mut current.commands } else { &mut current.tools };
            list.retain(|item| item != rule);
            if present {
                list.push(rule.to_owned());
            }
            let excess = list.len().saturating_sub(MAX_RULES);
            list.drain(..excess);
            if current.commands.is_empty() && current.tools.is_empty() {
                all.remove(workspace_id);
            } else {
                all.insert(workspace_id.to_owned(), current);
            }
            Ok(())
        })?;
        Ok(self.permission_rules(workspace_id))
    }

    /// A provider's API key; an environment variable with the credential's name takes precedence.
    pub fn api_key(&self, provider: &str) -> Option<String> {
        let name = credential_name(provider);
        std::env::var(&name).ok().filter(|value| !value.is_empty()).or_else(|| self.inner.lock().unwrap().keys.get(&name).cloned())
    }

    /// Credential names of stored keys this Host cannot open and no environment variable replaces.
    pub fn unreadable_keys(&self) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        inner.credentials.unreadable_names().into_iter().filter(|name| std::env::var(name).is_err() && !inner.keys.contains_key(name)).collect()
    }

    /// Settings → Models' explanation of keys that could not be opened, in the interface language.
    pub fn unreadable_keys_message(&self) -> Option<String> {
        let names = self.unreadable_keys();
        if names.is_empty() {
            return None;
        }
        let labels: Vec<String> = names.iter().map(|name| credential_label(name)).collect();
        let (zh, en) = (labels.join("、"), labels.join(", "));
        Some(if self.keys_sealed() {
            l(
                &format!("已保存的 {zh} API 密钥无法用当前的 Windows 数据保护密钥解开（例如重置过 Windows 账户密码后），请重新填写。原来的加密内容仍保留在 .credentials.sealed.json 中。"),
                &format!("The saved API keys for {en} cannot be opened with the current Windows data protection key, for example after the Windows account password was reset. Enter them again; the encrypted values stay in .credentials.sealed.json."),
            )
        } else {
            l(
                &format!("Windows 数据保护当前不可用，无法读取加密保存的 {zh} API 密钥。可以稍后重启 Rainy 再试，或重新填写密钥（将不加密保存）。"),
                &format!("Windows data protection is unavailable, so the encrypted API keys for {en} cannot be read. Restart Rainy later, or enter the keys again; they will then be stored unencrypted."),
            )
        })
    }

    /// Providers whose real key is available; the local-model placeholder does not count.
    pub fn providers_with_keys(&self) -> Vec<String> {
        let providers: Vec<String> = self.get().models.into_iter().map(|model| model.provider).collect();
        providers.into_iter().filter(|provider| self.api_key(provider).is_some_and(|key| key != LOCAL_NO_KEY)).collect()
    }

    /// Store (`Some`) or remove a provider's API key.
    pub fn set_api_key(&self, provider: &str, key: Option<&str>) -> HostResult<()> {
        let mut inner = self.inner.lock().unwrap();
        let name = credential_name(provider);
        let mut next = inner.keys.clone();
        match key {
            Some(key) => next.insert(name.clone(), key.to_owned()),
            None => next.remove(&name),
        };
        let removed = if key.is_none() { vec![name] } else { Vec::new() };
        inner.credentials.write(&next, &removed)?;
        inner.keys = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_switches_keep_an_mcp_connection() {
        let server = McpServerConfig { name: "files".into(), enabled: true, transport: "stdio".into(), command: Some("npx".into()), args: None, env: None, url: None, headers: None, tools: Vec::new(), disabled_tools: Vec::new() };
        let switched = McpServerConfig { disabled_tools: vec!["write_file".into()], ..server.clone() };
        assert!(server.same_connection(&switched));
        assert!(!server.same_connection(&McpServerConfig { command: Some("uvx".into()), ..server.clone() }));
    }

    #[test]
    fn reads_a_3x_file_and_drops_unknown_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let text = r#"{"version":1,"models":[{"provider":"rainy-deepseek","baseURL":"https://api.deepseek.com","model":"deepseek-v4","contextWindow":1000000,"local":false,"api":"openai-completions","thinking":"high","thinkingFormat":"deepseek"}],"selected":{"provider":"rainy-deepseek","model":"deepseek-v4","thinking":"high"},"globalPrompt":"","mcpServers":[],"ui":{"locale":"en","colorScheme":"future-scheme","uiFontSize":15},"security":{"defaultMode":"auto"}}"#;
        std::fs::write(dir.path().join("settings.json"), text).unwrap();
        let settings = Settings::load(dir.path(), None).unwrap();
        let prefs = settings.prefs();
        assert_eq!(prefs["locale"], "en");
        assert_eq!(prefs["colorScheme"], "graphite");
        assert_eq!(prefs["uiFontSize"], 15);
        assert_eq!(settings.default_permission(), "auto");
        settings.set_prefs(Map::from_iter([("theme".to_owned(), json!("dark"))])).unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("settings.json")).unwrap()).unwrap();
        assert_eq!(saved["ui"], json!({"locale": "en", "uiFontSize": 15, "theme": "dark"}));
        assert_eq!(saved["models"][0]["baseURL"], "https://api.deepseek.com");
        assert!(settings.set_prefs(Map::from_iter([("uiFontSize".to_owned(), json!(30))])).is_err());
    }

    #[test]
    fn keeps_what_is_valid_of_a_damaged_file() {
        let dir = tempfile::tempdir().unwrap();
        let good = r#"{"provider":"rainy-deepseek","baseURL":"https://api.deepseek.com","model":"deepseek-v4","contextWindow":1000000,"local":false}"#;
        let text = format!(
            r#"{{"version":1,"models":[{good},{{"provider":"bad","baseURL":"nope","model":"","contextWindow":1,"local":false}}],"selected":{{"provider":"rainy-deepseek","model":"deepseek-v4"}},"globalPrompt":"始终用中文","mcpServers":[{{"name":"no command","enabled":true,"transport":"stdio","tools":[]}}],"ui":{{"locale":"en"}}}}"#
        );
        std::fs::write(dir.path().join("settings.json"), &text).unwrap();
        let settings = Settings::load(dir.path(), None).unwrap();
        let data = settings.get();
        assert_eq!(data.models.len(), 1);
        assert_eq!(data.global_prompt, "始终用中文");
        assert!(data.mcp_servers.is_empty());
        assert_eq!(settings.prefs()["locale"], "en");
        let kept: Vec<_> = std::fs::read_dir(dir.path()).unwrap().filter_map(|entry| entry.ok()).filter(|entry| entry.file_name().to_string_lossy().starts_with("settings.json.damaged-")).collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read_to_string(kept[0].path()).unwrap(), text);

        // Not JSON at all: defaults, and the text kept.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("settings.json"), "{ \"models\": [ ,").unwrap();
        assert_eq!(Settings::load(dir.path(), None).unwrap().get(), SettingsData::default());
    }

    #[test]
    fn names_credentials_like_3x() {
        assert_eq!(credential_name("rainy-deepseek"), "DEEPSEEK_API_KEY");
        assert_eq!(credential_name("rainy-claude-haiku"), "ANTHROPIC_API_KEY");
        assert_eq!(credential_name("my-local"), "RAINY_MY_LOCAL_KEY");
        assert_eq!(credential_label("RAINY_MY_LOCAL_KEY"), "my-local");
    }

    #[test]
    fn checks_fonts_like_the_page() {
        assert_eq!(font_families("'Microsoft YaHei', monospace"), Some(vec!["Microsoft YaHei".into(), "monospace".into()]));
        assert_eq!(font_families("霞鹜文楷"), Some(vec!["霞鹜文楷".into()]));
        assert!(font_families("a;b").is_none());
        assert!(font_families("a,b,c,d,e").is_none());
    }

    #[test]
    fn remembers_rules_per_project() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::load(dir.path(), None).unwrap();
        settings.set_permission_rule("w", true, "python3", true).unwrap();
        settings.set_permission_rule("w", false, "mcp__a__b", true).unwrap();
        assert_eq!(settings.permission_rules("w"), PermissionRules { commands: vec!["python3".into()], tools: vec!["mcp__a__b".into()] });
        settings.set_permission_rule("w", true, "python3", false).unwrap();
        settings.set_permission_rule("w", false, "mcp__a__b", false).unwrap();
        assert!(settings.get().security.unwrap().rules.unwrap().is_empty());
    }
}
