//! Saved model profiles, presets, discovery, the connection probe and Strata (3.x `models.ts`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rainy_protocol::chat::{AssistantBlock, AssistantMessage};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::budget::resolve_budget;
use super::llm::{self, Level, Llm, Message, Model, Request, ToolSpec};
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::network::Network;
use crate::settings::{GLOBAL_PROMPT_MAX_CHARS, LOCAL_NO_KEY, ModelSelection, ModelSetup, Settings, provider_id};

fn preset(provider: &str, base_url: &str, model: &str, context_window: u64, max_tokens: u64, api: &str, thinking: &str, thinking_format: Option<&str>) -> ModelSetup {
    ModelSetup {
        provider: provider.into(),
        base_url: base_url.into(),
        model: model.into(),
        context_window,
        max_tokens: Some(max_tokens),
        local: false,
        api: Some(api.into()),
        thinking: Some(thinking.into()),
        thinking_format: thinking_format.map(str::to_owned),
        max_tokens_field: None,
        images: None,
    }
}

/// Official DeepSeek API with Rainy's context default; it takes images, which Settings can turn off.
pub fn deepseek_flash() -> ModelSetup {
    ModelSetup {
        images: Some(true),
        ..preset("rainy-deepseek", "https://api.deepseek.com", "deepseek-flash", 1_000_000, 393_216, "openai-responses", "max", Some("deepseek"))
    }
}

/// The presets Settings → Models offers.
pub fn presets() -> Vec<(&'static str, ModelSetup)> {
    vec![
        ("DeepSeek V4.1 Flash", deepseek_flash()),
        // A 200K window keeps long chats affordable.
        ("Claude Opus 5.5", preset("rainy-claude", "https://api.anthropic.com", "claude-opus-5-5", 200_000, 64_000, "anthropic-messages", "high", None)),
        // A 100K window keeps every request in Haiku's lower price tier.
        ("Claude Haiku 5.5", preset("rainy-claude-haiku", "https://api.anthropic.com", "claude-haiku-5-5", 100_000, 16_000, "anthropic-messages", "high", None)),
    ]
}

/// Both Claude presets share one key.
fn credential_shared(left: &str, right: &str) -> bool {
    let claude = ["rainy-claude", "rainy-claude-haiku"];
    claude.contains(&left) && claude.contains(&right)
}

/// A profile as `models.status` shows it: with the image setting in effect, so a form shows what requests do.
fn with_images(setup: &ModelSetup) -> ModelSetup {
    ModelSetup { images: Some(Model::from_setup(setup, 0).images), ..setup.clone() }
}

/// The levels a profile accepts, in escalation order.
pub fn thinking_levels(setup: &ModelSetup) -> Vec<Level> {
    if setup.api.as_deref() == Some("anthropic-messages")
        && let Some(claude) = llm::catalog::claude(&setup.model)
    {
        return Level::ALL.into_iter().filter(|level| claude.levels.offers(level.as_str())).collect();
    }
    if setup.thinking.is_none() { vec![Level::Off] } else { Level::ALL.to_vec() }
}

/// The level a request uses; a model that cannot turn reasoning off thinks a little instead.
pub fn effective_thinking(setup: &ModelSetup, requested: Option<&str>) -> Option<Level> {
    let requested = Level::parse(requested.or(setup.thinking.as_deref())?)?;
    let levels = thinking_levels(setup);
    if levels.contains(&requested) {
        return Some(requested);
    }
    if requested == Level::Off && levels.contains(&Level::Low) { Some(Level::Low) } else { levels.last().copied() }
}

/// A model resolved for one request.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub setup: ModelSetup,
    pub model: Model,
    pub thinking: Option<Level>,
}

impl Resolved {
    /// The selection that names this model.
    pub fn selection(&self) -> ModelSelection {
        ModelSelection { provider: self.setup.provider.clone(), model: self.setup.model.clone(), thinking: self.thinking.map(|level| level.as_str().to_owned()) }
    }
}

fn string<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input.get(field).and_then(Value::as_str)
}

/// Connection fields, checked without echoing a credential in an error.
struct Connection {
    provider: String,
    base_url: String,
    api: String,
    api_key: Option<String>,
}

fn parse_connection(input: &Value) -> HostResult<Connection> {
    let invalid = |zh: &str, en: &str| HostError::invalid(l(zh, en));
    if !input.is_object() {
        return Err(invalid("模型设置必须是对象。", "Model settings must be an object."));
    }
    let provider = string(input, "provider").filter(|provider| provider_id(provider)).ok_or_else(|| invalid("供应商 ID 只允许小写字母、数字和连字符。", "The provider ID may contain only lowercase letters, digits and hyphens."))?;
    let base = string(input, "baseURL").ok_or_else(|| invalid("请填写服务地址。", "Enter the service address."))?;
    let url = url::Url::parse(base).map_err(|_| invalid("服务地址必须是有效的 HTTP(S) Base URL。", "The service address must be a valid HTTP(S) base URL."))?;
    if !["http", "https"].contains(&url.scheme()) || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("服务地址必须是 HTTP(S) Base URL，不能包含密钥或查询参数。", "The service address must be an HTTP(S) base URL without credentials or query parameters."));
    }
    let api = string(input, "api").filter(|api| llm::Api::parse(api).is_some()).ok_or_else(|| invalid("不支持的 API 协议。", "This API protocol is not supported."))?;
    let api_key = match input.get("apiKey") {
        None | Some(Value::Null) => None,
        Some(Value::String(key)) => Some(key.clone()),
        Some(_) => return Err(invalid("密钥格式无效。", "The key format is invalid.")),
    };
    Ok(Connection { provider: provider.into(), base_url: url.as_str().trim_end_matches('/').into(), api: api.into(), api_key })
}

/// A model form: the profile, its typed key, and the provider id it was saved under before.
struct SetupInput {
    setup: ModelSetup,
    api_key: Option<String>,
    previous_provider: Option<String>,
}

fn parse_setup(input: &Value) -> HostResult<SetupInput> {
    let invalid = |zh: &str, en: &str| HostError::invalid(l(zh, en));
    if !input.is_object() {
        return Err(invalid("模型设置必须是对象。", "Model settings must be an object."));
    }
    let model = string(input, "model").map(str::trim).filter(|model| !model.is_empty()).ok_or_else(|| invalid("请填写服务地址和模型 ID。", "Enter the service address and the model ID."))?;
    let (Some(context_window), Some(local)) = (input.get("contextWindow").and_then(Value::as_u64), input.get("local").and_then(Value::as_bool)) else {
        return Err(invalid("请填写实际上下文长度并选择本地或 API 模型。", "Enter the actual context length and choose a local or API model."));
    };
    let max_tokens = match input.get("maxTokens") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_u64().ok_or_else(|| invalid("输出上限必须是数字。", "The output limit must be a number."))?),
    };
    resolve_budget(context_window, max_tokens)?;
    let mut with_api = input.clone();
    if input.get("api").is_none_or(Value::is_null) {
        with_api["api"] = json!(if local { "openai-completions" } else { "openai-responses" });
    }
    let connection = parse_connection(&with_api)?;
    let thinking = string(input, "thinking").map(str::to_owned);
    if input.get("thinking").is_some_and(|value| !value.is_null()) && thinking.as_deref().and_then(Level::parse).is_none() {
        return Err(invalid("推理档位无效。", "The reasoning level is invalid."));
    }
    let thinking_format = string(input, "thinkingFormat").map(str::to_owned);
    if input.get("thinkingFormat").is_some_and(|value| !value.is_null()) && !thinking_format.as_deref().is_some_and(|format| ["openai", "deepseek", "qwen"].contains(&format)) {
        return Err(invalid("推理协议无效。", "The reasoning format is invalid."));
    }
    let max_tokens_field = string(input, "maxTokensField").map(str::to_owned);
    if input.get("maxTokensField").is_some_and(|value| !value.is_null()) && !max_tokens_field.as_deref().is_some_and(|field| ["max_tokens", "max_completion_tokens"].contains(&field)) {
        return Err(invalid("输出参数格式无效。", "The output limit field is invalid."));
    }
    let images = match input.get("images") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(images)) => Some(*images),
        Some(_) => return Err(invalid("图片输入设置无效。", "The image input setting is invalid.")),
    };
    let previous_provider = string(input, "previousProvider").filter(|provider| provider_id(provider)).map(str::to_owned);
    Ok(SetupInput {
        setup: ModelSetup {
            provider: connection.provider,
            base_url: connection.base_url,
            model: model.into(),
            context_window,
            max_tokens,
            local,
            api: Some(connection.api),
            thinking,
            thinking_format,
            max_tokens_field,
            images,
        },
        api_key: connection.api_key,
        previous_provider,
    })
}

type Listener = Box<dyn Fn(&Value) + Send + Sync>;

/// Model settings of one Host.
pub struct Models {
    settings: Arc<Settings>,
    network: Arc<Network>,
    pub llm: Arc<Llm>,
    listeners: Mutex<Vec<Listener>>,
}

impl Models {
    pub fn new(settings: Arc<Settings>, network: Arc<Network>, llm: Arc<Llm>) -> Self {
        Self { settings, network, llm, listeners: Mutex::default() }
    }

    pub fn on_change(&self, listener: impl Fn(&Value) + Send + Sync + 'static) {
        self.listeners.lock().unwrap().push(Box::new(listener));
    }

    fn publish(&self) {
        let status = self.status();
        for listener in self.listeners.lock().unwrap().iter() {
            listener(&status);
        }
    }

    /// Saved models, keys present, selection, presets and the global prompt.
    pub fn status(&self) -> Value {
        let data = self.settings.get();
        let presets = presets();
        let mut providers: Vec<String> = Vec::new();
        for provider in data.models.iter().map(|model| model.provider.clone()).chain(presets.iter().map(|(_, model)| model.provider.clone())) {
            if !providers.contains(&provider) {
                providers.push(provider);
            }
        }
        // Presets count too: a preset that shares a stored key saves without one.
        let credentials: Vec<String> = providers.into_iter().filter(|provider| self.settings.api_key(provider).is_some_and(|key| key != LOCAL_NO_KEY)).collect();
        let levels: serde_json::Map<String, Value> =
            data.models.iter().map(|model| (model.provider.clone(), json!(thinking_levels(model).iter().map(|level| level.as_str()).collect::<Vec<_>>()))).collect();
        let models: Vec<ModelSetup> = data.models.iter().map(with_images).collect();
        let mut status = json!({
            "models": models,
            "credentials": credentials,
            "keysEncrypted": self.settings.keys_sealed(),
            "selected": data.selected,
            "thinkingLevels": levels,
            "presets": presets.iter().map(|(name, model)| json!({ "name": name, "model": with_images(model) })).collect::<Vec<_>>(),
            "globalPrompt": { "text": data.global_prompt, "maxChars": GLOBAL_PROMPT_MAX_CHARS },
        });
        if let Some(message) = self.settings.unreadable_keys_message() {
            status["keysUnreadable"] = json!(message);
        }
        status
    }

    /// A selection resolved to a streamable model; `None` uses the default; nothing when no model is saved.
    pub fn resolve(&self, selection: Option<&ModelSelection>) -> Option<Resolved> {
        let data = self.settings.get();
        let target = selection.cloned().or(data.selected.clone());
        let setup = match &target {
            None => data.models.first()?.clone(),
            Some(target) => data.models.iter().find(|model| model.provider == target.provider && model.model == target.model)?.clone(),
        };
        let requested = target.as_ref().filter(|target| target.provider == setup.provider && target.model == setup.model).and_then(|target| target.thinking.clone()).or_else(|| setup.thinking.clone());
        let thinking = effective_thinking(&setup, requested.as_deref());
        let output = resolve_budget(setup.context_window, setup.max_tokens).map(|budget| budget.output_tokens).unwrap_or(4096);
        Some(Resolved { model: Model::from_setup(&setup, output), setup, thinking })
    }

    /// Save a profile and make it the default; one profile is kept per provider id.
    pub fn configure(&self, input: &Value) -> HostResult<ModelSelection> {
        let SetupInput { setup, api_key, previous_provider } = parse_setup(input)?;
        let data = self.settings.get();
        // Editing a profile under a new provider id moves it; the old id's key comes along unless a new one is typed.
        let renamed = previous_provider.filter(|previous| *previous != setup.provider && data.models.iter().any(|model| model.provider == *previous));
        let carried = renamed.as_deref().and_then(|renamed| self.settings.api_key(renamed));
        let stored = self.settings.api_key(&setup.provider).or(carried);
        let typed = api_key.as_deref().map(str::trim).filter(|key| !key.is_empty());
        // A local model's placeholder key is no key for an API service.
        if typed.is_none() && !setup.local && stored.as_deref().is_none_or(|key| key == LOCAL_NO_KEY) {
            return Err(HostError::new("missing-key", l("请填写 API 密钥。API 模型需要密钥才能保存。", "Enter the API key. An API model cannot be saved without one.")));
        }
        if let Some(key) = typed {
            self.settings.set_api_key(&setup.provider, Some(key))?;
        } else if self.settings.api_key(&setup.provider).is_none() {
            self.settings.set_api_key(&setup.provider, Some(stored.as_deref().unwrap_or(LOCAL_NO_KEY)))?;
        }
        let level = effective_thinking(&setup, None);
        // An empty output limit stays automatic, so a later context change resizes it.
        let saved = ModelSetup { thinking: level.map(|level| level.as_str().to_owned()), ..setup };
        let previous = data.models.iter().find(|model| model.provider == saved.provider && model.model == saved.model);
        // Re-saving the default with an unchanged form level keeps the level picked in the list.
        let kept = match (&data.selected, previous) {
            (Some(current), Some(previous)) if previous.thinking == saved.thinking && current.provider == saved.provider && current.model == saved.model && current.thinking.is_some() => {
                effective_thinking(&saved, current.thinking.as_deref())
            }
            _ => level,
        };
        let selection = ModelSelection { provider: saved.provider.clone(), model: saved.model.clone(), thinking: kept.filter(|level| *level != Level::Off).map(|level| level.as_str().to_owned()) };
        let chosen = selection.clone();
        self.settings.update(|next| {
            next.models.retain(|model| model.provider != saved.provider && Some(&model.provider) != renamed.as_ref());
            next.models.push(saved);
            next.selected = Some(chosen);
            Ok(())
        })?;
        if let Some(renamed) = &renamed
            && !self.settings.get().models.iter().any(|model| credential_shared(&model.provider, renamed))
        {
            self.settings.set_api_key(renamed, None)?;
        }
        self.publish();
        Ok(selection)
    }

    /// Delete a provider's profile and key.
    pub fn remove(&self, provider: &str) -> HostResult<Value> {
        self.settings.update(|data| {
            data.models.retain(|model| model.provider != provider);
            if data.selected.as_ref().is_some_and(|selected| selected.provider == provider) {
                data.selected = data.models.first().map(|next| ModelSelection { provider: next.provider.clone(), model: next.model.clone(), thinking: None });
            }
            Ok(())
        })?;
        if !self.settings.get().models.iter().any(|model| model.provider != provider && credential_shared(&model.provider, provider)) {
            self.settings.set_api_key(provider, None)?;
        }
        self.publish();
        Ok(self.status())
    }

    /// Choose the model future chats start with.
    pub fn select(&self, selection: &ModelSelection) -> HostResult<Value> {
        let setup = self
            .settings
            .get()
            .models
            .into_iter()
            .find(|model| model.provider == selection.provider && model.model == selection.model)
            .ok_or_else(|| HostError::new("not-found", l("模型尚未配置。", "This model is not configured.")))?;
        let level = effective_thinking(&setup, selection.thinking.as_deref());
        self.settings.update(|data| {
            data.selected = Some(ModelSelection { provider: setup.provider.clone(), model: setup.model.clone(), thinking: level.map(|level| level.as_str().to_owned()) });
            Ok(())
        })?;
        self.publish();
        Ok(self.status())
    }

    /// Save the text added to every system prompt; empty removes it.
    pub fn set_global_prompt(&self, text: &str) -> HostResult<Value> {
        let trimmed = text.trim();
        if trimmed.chars().count() > GLOBAL_PROMPT_MAX_CHARS {
            return Err(HostError::new(
                "too-large",
                l(&format!("全局提示词最多 {GLOBAL_PROMPT_MAX_CHARS} 个字符。"), &format!("The global prompt can be at most {GLOBAL_PROMPT_MAX_CHARS} characters.")),
            ));
        }
        self.settings.update(|data| {
            data.global_prompt = trimmed.to_owned();
            Ok(())
        })?;
        self.publish();
        Ok(self.status())
    }

    /// The models an endpoint offers; nothing is saved.
    pub async fn discover(&self, input: &Value) -> HostResult<Value> {
        let connection = parse_connection(input)?;
        let key = connection.api_key.as_deref().map(str::trim).filter(|key| !key.is_empty()).map(str::to_owned).or_else(|| self.settings.api_key(&connection.provider));
        let base = connection.base_url.trim_end_matches('/');
        let anthropic = connection.api == "anthropic-messages";
        let url = if anthropic { format!("{}/v1/models?limit=1000", base.strip_suffix("/v1").unwrap_or(base)) } else { format!("{base}/models") };
        let failed = |message: String| HostError::new("discovery-failed", message);
        let parsed = url::Url::parse(&url).map_err(|error| failed(error.to_string()))?;
        let mut request = self.network.client(&parsed).await.get(&url).timeout(Duration::from_secs(10)).header("accept", "application/json");
        if anthropic {
            request = request.header("anthropic-version", "2023-06-01");
            if let Some(key) = &key {
                request = request.header("x-api-key", key);
            }
        } else if let Some(key) = &key {
            request = request.header("authorization", format!("Bearer {key}"));
        }
        let response = request.send().await.map_err(|error| {
            let error = crate::agent::llm::transport_error(&error);
            failed(l(&format!("无法连接 {url}：{error}"), &format!("Could not connect to {url}: {error}")))
        })?;
        if response.status().is_redirection() || !response.status().is_success() {
            let status = response.status().as_u16();
            return Err(failed(l(&format!("模型列表请求失败（HTTP {status}）。"), &format!("The model list request failed (HTTP {status})."))));
        }
        let text = response.text().await.map_err(|error| failed(error.to_string()))?;
        if text.len() > 8 * 1024 * 1024 {
            return Err(failed(l("模型列表过大。", "The model list is too large.")));
        }
        let body: Value = serde_json::from_str(&text).map_err(|error| failed(error.to_string()))?;
        let listed: Vec<Value> = match (&body["data"], &body["models"]) {
            (Value::Array(data), _) => data.clone(),
            (_, Value::Array(models)) => models.clone(),
            (_, Value::Object(models)) => models
                .iter()
                .map(|(id, entry)| {
                    let mut entry = if entry.is_object() { entry.clone() } else { json!({}) };
                    entry["id"] = json!(id);
                    entry
                })
                .collect(),
            _ => return Err(failed(l("该服务的模型列表格式无法识别，请手动填写模型 ID。", "This service lists its models in an unknown format. Enter the model ID yourself."))),
        };
        Ok(Value::Array(
            listed
                .iter()
                .filter_map(|entry| {
                    let id = entry["id"].as_str().filter(|id| !id.is_empty())?;
                    let window = [&entry["contextWindow"], &entry["context_window"], &entry["context_length"], &entry["max_input_tokens"], &entry["limit"]["context"]]
                        .into_iter()
                        .find_map(|value| value.as_u64().filter(|window| *window > 0 && *window < (1 << 53)));
                    let mut item = json!({ "id": id });
                    if let Some(window) = window {
                        item["contextWindow"] = json!(window);
                    }
                    Some(item)
                })
                .collect(),
        ))
    }

    /// Check a saved profile with one plain reply and one tool call.
    pub async fn probe(&self, input: &Value) -> HostResult<Value> {
        let SetupInput { setup: form, .. } = parse_setup(input)?;
        let saved = self.settings.get().models.into_iter().find(|model| model.provider == form.provider && model.model == form.model);
        let output = |setup: &ModelSetup| resolve_budget(setup.context_window, setup.max_tokens).map(|budget| budget.output_tokens).ok();
        let same = saved.as_ref().is_some_and(|saved| {
            saved.base_url == form.base_url
                && saved.api == form.api
                && saved.context_window == form.context_window
                && output(saved) == output(&form)
                && saved.thinking.as_deref() == effective_thinking(&form, None).map(Level::as_str)
                && (form.api.as_deref() != Some("openai-completions")
                    || (saved.thinking_format.as_deref().unwrap_or("openai") == form.thinking_format.as_deref().unwrap_or("openai")
                        && saved.max_tokens_field.as_deref().unwrap_or("max_tokens") == form.max_tokens_field.as_deref().unwrap_or("max_tokens")))
        });
        let Some(saved) = saved.filter(|_| same) else {
            return Err(HostError::new("not-saved", l("请先保存当前模型设置，再验证这份已保存的配置。", "Save the current model settings first, then check the saved configuration.")));
        };
        let resolved = self.resolve(Some(&ModelSelection { provider: saved.provider.clone(), model: saved.model.clone(), thinking: saved.thinking.clone() })).ok_or_else(|| HostError::other("model"))?;
        let reply = self.complete(&resolved, "Connection diagnostic. Reply only with Rainy connected.", &[], Duration::from_secs(180), Completion::default()).await;
        if reply.stop_reason == "error" || reply.stop_reason == "aborted" {
            return Err(HostError::new("probe-failed", reply.error_message.unwrap_or_else(|| l("连接失败。", "The connection failed."))));
        }
        let mut text: String = llm::reply_text(&reply).chars().take(300).collect();
        let tools = [ToolSpec {
            name: "rainy_probe".into(),
            description: "Harmless connection diagnostic; returns the supplied integer.".into(),
            parameters: json!({ "type": "object", "properties": { "value": { "type": "integer" } }, "required": ["value"] }),
        }];
        let tool_reply = self.complete(&resolved, "Connection diagnostic. Call rainy_probe with value 7 exactly once. Do not access files or run commands.", &tools, Duration::from_secs(180), Completion::default()).await;
        let mut tool_call = false;
        if tool_reply.stop_reason == "error" || tool_reply.stop_reason == "aborted" {
            let reason: String = tool_reply.error_message.unwrap_or_else(|| l("未通过", "failed")).chars().take(200).collect();
            text.push_str(&l(&format!("\n工具诊断：{reason}"), &format!("\nTool check: {reason}")));
        } else {
            tool_call = tool_reply.content.iter().any(|block| matches!(block, AssistantBlock::ToolCall { name, .. } if name == "rainy_probe"));
        }
        Ok(json!({ "stream": !text.is_empty(), "toolCall": tool_call, "text": text }))
    }

    /// Save a loaded local Strata server as the default model, after this Host reached it itself.
    pub async fn connect_strata(&self, input: &Value) -> HostResult<ModelSelection> {
        let base = string(input, "baseURL").ok_or_else(|| HostError::invalid(l("Strata 连接需要本机服务地址。", "Connecting to Strata needs the local service address.")))?;
        let health = crate::strata::inspect(&self.network, base).await.map_err(|detail| {
            HostError::other(l(
                &format!("当前执行环境无法连接已加载的 Strata：{detail} Windows 本地模型请使用 Windows 执行环境；不会切换到云端。"),
                &format!("This execution environment cannot reach the loaded Strata model: {detail} Use the Windows execution environment for local Windows models; Rainy does not switch to a cloud model."),
            ))
        })?;
        let expected = input.get("expectedContextWindow");
        if let Some(expected) = expected
            && expected.as_u64() != Some(health.context_window)
        {
            return Err(HostError::other(l("Strata 的实际上下文已改变，请刷新后重新连接。", "The Strata context length changed. Refresh and connect again.")));
        }
        if input.get("maxTokens").is_some() && (!input["maxTokens"].is_u64() || expected.is_none()) {
            return Err(HostError::other(l("调整输出上限需要确认当前 Strata 上下文。", "Changing the output limit requires confirming the current Strata context length.")));
        }
        let previous = self.settings.get().models.into_iter().find(|model| model.provider == "rainy-strata");
        let max_tokens = input["maxTokens"].as_u64().or_else(|| previous.as_ref().and_then(|model| model.max_tokens));
        let mut form = json!({
            "provider": "rainy-strata", "baseURL": health.base_url, "model": health.model, "contextWindow": health.context_window, "local": true,
            "api": "openai-completions", "thinkingFormat": "openai", "maxTokensField": "max_tokens",
            "thinking": previous.and_then(|model| model.thinking).unwrap_or_else(|| "off".into()),
        });
        if let Some(max_tokens) = max_tokens {
            form["maxTokens"] = json!(max_tokens);
        }
        self.configure(&form)
    }

    /// One request outside a chat (probe, compaction, memory, titles, commit messages); failures are recorded in the
    /// reply's `stopReason`.
    pub async fn complete(&self, resolved: &Resolved, prompt: &str, tools: &[ToolSpec], timeout: Duration, options: Completion) -> AssistantMessage {
        let messages = [Message::user(prompt)];
        let request = Request {
            system: options.system.as_deref().unwrap_or(""),
            messages: &messages,
            tools,
            reasoning: options.thinking.or(resolved.thinking),
            max_tokens: options.max_tokens,
            session_id: None,
            unmark_last_user: false,
            no_cache: false,
            no_tool_calls: false,
        };
        let cancel = options.cancel.clone().unwrap_or_default();
        let key = self.settings.api_key(&resolved.setup.provider);
        let mut ignore = |_: llm::Event, _: &AssistantMessage| {};
        let timed = tokio::time::timeout(timeout, self.llm.stream(&resolved.model, &request, key, &cancel, &mut ignore)).await;
        match timed {
            Ok(reply) => reply,
            Err(_) => {
                let mut reply = llm::empty_reply(&resolved.model);
                reply.stop_reason = "aborted".into();
                reply.error_message = Some(l("请求超时。", "The request timed out."));
                reply
            }
        }
    }

    /// The API key of a provider.
    pub fn api_key(&self, provider: &str) -> Option<String> {
        self.settings.api_key(provider)
    }
}

/// Options of [`Models::complete`].
#[derive(Default)]
pub struct Completion {
    pub system: Option<String>,
    pub max_tokens: Option<u64>,
    pub thinking: Option<Level>,
    pub cancel: Option<CancellationToken>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_levels_like_3x() {
        let presets = presets();
        let opus = &presets[1].1;
        // Claude Opus 5.5 cannot turn reasoning off; it thinks a little instead.
        assert_eq!(thinking_levels(opus), vec![Level::Low, Level::High, Level::Max]);
        assert_eq!(effective_thinking(opus, Some("off")), Some(Level::Low));
        let mut plain = presets[0].1.clone();
        plain.thinking = None;
        assert_eq!(effective_thinking(&plain, Some("high")), Some(Level::Off));
        assert_eq!(thinking_levels(&presets[0].1), Level::ALL.to_vec());
    }

    #[test]
    fn checks_forms() {
        let form = json!({ "provider": "my-ai", "baseURL": "https://api.example.com/v1/", "model": " m ", "contextWindow": 128000, "local": false, "apiKey": "k" });
        let input = parse_setup(&form).unwrap();
        assert_eq!((input.setup.base_url.as_str(), input.setup.model.as_str(), input.setup.api.as_deref()), ("https://api.example.com/v1", "m", Some("openai-responses")));
        assert!(parse_setup(&json!({ "provider": "x", "baseURL": "https://u:p@h", "model": "m", "contextWindow": 128000, "local": false })).is_err());
        assert!(parse_setup(&json!({ "provider": "x", "baseURL": "https://h", "model": "m", "contextWindow": 8192, "maxTokens": 8000, "local": false })).is_err());
    }

    #[test]
    fn image_input_follows_the_profile() {
        let presets = presets();
        let (deepseek, opus) = (&presets[0].1, &presets[1].1);
        // Unset keeps the 3.x rule: only the Claude catalog takes images. The DeepSeek preset says so itself.
        assert!(Model::from_setup(opus, 0).images && opus.images.is_none());
        assert_eq!((deepseek.images, Model::from_setup(deepseek, 0).images), (Some(true), true));
        let text_only = ModelSetup { images: Some(false), ..opus.clone() };
        assert!(!Model::from_setup(&text_only, 0).images);
        let plain = ModelSetup { images: None, ..deepseek.clone() };
        assert_eq!((with_images(&plain).images, with_images(opus).images), (Some(false), Some(true)));
        // Forms carry a bool or nothing; an old file without the field reads as unset.
        let form = json!({ "provider": "my-ai", "baseURL": "https://h", "model": "m", "contextWindow": 128000, "local": false, "images": true });
        assert_eq!(parse_setup(&form).unwrap().setup.images, Some(true));
        let with = |images: Value| {
            let mut form = form.clone();
            form["images"] = images;
            parse_setup(&form).map(|input| input.setup.images)
        };
        assert_eq!(with(Value::Null).ok(), Some(None));
        assert!(with(json!("yes")).is_err());
        let old: ModelSetup = serde_json::from_value(json!({ "provider": "p", "baseURL": "https://h", "model": "m", "contextWindow": 1000, "local": false })).unwrap();
        assert_eq!(old.images, None);
        assert!(serde_json::to_value(&old).unwrap().get("images").is_none());
    }
}
