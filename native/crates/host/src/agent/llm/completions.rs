//! OpenAI Chat Completions (`POST {base}/chat/completions`), as pi-ai sends it to OpenAI-compatible servers: DeepSeek,
//! Qwen, local servers such as Strata, and others.

use std::collections::HashMap;

use rainy_protocol::chat::{AssistantBlock, AssistantMessage, ResultContent};
use serde_json::{Map, Value, json};

use super::json::parse_arguments;
use super::{Event, HttpRequest, Level, Message, Model, Part, Reader, Request, UserContent, preview_arguments, replay, usage};

/// What a server is, by its provider id and address (pi-ai `detectCompat`).
struct Compat {
    store: bool,
    reasoning_effort: bool,
    thinking_format: String,
    /// DeepSeek wants `reasoning_content` on every assistant message of a reasoning model.
    reasoning_on_assistant: bool,
    openrouter: bool,
}

fn compat(model: &Model) -> Compat {
    let (provider, url) = (model.provider.as_str(), model.base_url.as_str());
    let lower = url.to_lowercase();
    let zai = provider == "zai" || provider == "zai-coding-cn" || url.contains("api.z.ai") || url.contains("open.bigmodel.cn");
    let together = provider == "together" || url.contains("api.together.ai") || url.contains("api.together.xyz");
    let moonshot = provider == "moonshotai" || provider == "moonshotai-cn" || url.contains("api.moonshot.");
    let openrouter = provider == "openrouter" || url.contains("openrouter.ai");
    let cloudflare = provider == "cloudflare-workers-ai" || url.contains("api.cloudflare.com");
    let gateway = provider == "cloudflare-ai-gateway" || url.contains("gateway.ai.cloudflare.com");
    let nvidia = provider == "nvidia" || url.contains("integrate.api.nvidia.com");
    let ling = provider == "ant-ling" || url.contains("api.ant-ling.com");
    let cerebras = provider == "cerebras" || url.contains("cerebras.ai");
    let deepseek = provider == "deepseek" || lower.contains("deepseek.com");
    let grok = provider == "xai" || url.contains("api.x.ai");
    let opencode = provider == "opencode" || url.contains("opencode.ai");
    let nonstandard = nvidia || cerebras || grok || together || url.contains("chutes.ai") || deepseek || zai || moonshot || opencode || cloudflare || gateway || ling;
    let detected = if deepseek {
        "deepseek"
    } else if zai {
        "zai"
    } else if together {
        "together"
    } else if openrouter {
        "openrouter"
    } else {
        "openai"
    };
    Compat {
        store: !nonstandard,
        reasoning_effort: !grok && !zai && !moonshot && !together && !gateway && !nvidia && !ling,
        thinking_format: model.thinking_format.clone().unwrap_or_else(|| detected.into()),
        reasoning_on_assistant: deepseek,
        openrouter,
    }
}

/// A Responses API tool call id (`call|item`) in the form Chat Completions accepts.
fn normalize_id(id: &str, model: &Model) -> String {
    if let Some((call, item)) = id.split_once('|') {
        let clean = |text: &str| text.chars().map(|char| if char.is_ascii_alphanumeric() || char == '_' || char == '-' { char } else { '_' }).collect::<String>();
        let (call, item) = (clean(call), clean(item));
        let combined = if item.is_empty() { call.clone() } else { format!("{call}_{item}") };
        if combined.len() <= 40 {
            return combined;
        }
        let hash = &crate::text::sha256_hex(id)[..8];
        return format!("{}_{hash}", &call[..call.len().min(40 - hash.len() - 1).max(1)]);
    }
    if model.provider == "openai" && id.len() > 40 { id[..40].to_owned() } else { id.to_owned() }
}

/// The conversation as Chat Completions messages.
fn messages(model: &Model, request: &Request<'_>, compat: &Compat) -> Vec<Value> {
    let mut params = Vec::new();
    if !request.system.is_empty() {
        params.push(json!({ "role": "system", "content": request.system }));
    }
    let replayed = replay(request.messages, model, &|id, _| normalize_id(id, model));
    let mut index = 0;
    while index < replayed.len() {
        match &replayed[index] {
            Message::User { content: UserContent::Text(text), .. } => params.push(json!({ "role": "user", "content": text })),
            Message::User { content: UserContent::Parts(parts), .. } => {
                let content: Vec<Value> = parts
                    .iter()
                    .filter(|part| !matches!(part, Part::Text(text) if text.is_empty()))
                    .map(|part| match part {
                        Part::Text(text) => json!({ "type": "text", "text": text }),
                        Part::Image { data, mime_type } => json!({ "type": "image_url", "image_url": { "url": format!("data:{mime_type};base64,{data}") } }),
                    })
                    .collect();
                if !content.is_empty() {
                    params.push(json!({ "role": "user", "content": content }));
                }
            }
            Message::Assistant(reply) => {
                if let Some(message) = assistant(model, reply, compat) {
                    params.push(message);
                }
            }
            Message::ToolResult(_) => {
                let mut images = Vec::new();
                while let Some(Message::ToolResult(result)) = replayed.get(index) {
                    let text = super::text_parts(&result.content);
                    let has_images = result.content.iter().any(|part| matches!(part, ResultContent::Image { .. }));
                    let content = if !text.is_empty() {
                        text
                    } else if has_images {
                        "(see attached image)".into()
                    } else {
                        "(no tool output)".into()
                    };
                    params.push(json!({ "role": "tool", "content": content, "tool_call_id": result.tool_call_id }));
                    if model.images {
                        for part in &result.content {
                            if let ResultContent::Image { data, mime_type } = part {
                                images.push(json!({ "type": "image_url", "image_url": { "url": format!("data:{mime_type};base64,{data}") } }));
                            }
                        }
                    }
                    index += 1;
                }
                if !images.is_empty() {
                    let mut content = vec![json!({ "type": "text", "text": "Attached image(s) from tool result:" })];
                    content.extend(images);
                    params.push(json!({ "role": "user", "content": content }));
                }
                continue;
            }
        }
        index += 1;
    }
    params
}

fn assistant(model: &Model, reply: &AssistantMessage, compat: &Compat) -> Option<Value> {
    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), Value::Null);
    let text: String = reply.content.iter().filter_map(|block| if let AssistantBlock::Text { text, .. } = block { (!text.trim().is_empty()).then_some(text.as_str()) } else { None }).collect();
    if !text.is_empty() {
        message.insert("content".into(), json!(text));
    }
    let thinking: Vec<(&str, Option<&str>)> = reply
        .content
        .iter()
        .filter_map(|block| if let AssistantBlock::Thinking { thinking, thinking_signature, .. } = block { (!thinking.trim().is_empty()).then_some((thinking.as_str(), thinking_signature.as_deref())) } else { None })
        .collect();
    // The reasoning goes back in the field it came in.
    if let Some((_, Some(field))) = thinking.first()
        && matches!(*field, "reasoning" | "reasoning_content" | "reasoning_text")
    {
        message.insert((*field).into(), json!(thinking.iter().map(|(text, _)| *text).collect::<Vec<_>>().join("\n")));
    }
    let calls: Vec<Value> = reply
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantBlock::ToolCall { id, name, arguments, .. } => Some(json!({ "id": id, "type": "function", "function": { "name": name, "arguments": Value::Object(arguments.clone()).to_string() } })),
            _ => None,
        })
        .collect();
    let has_calls = !calls.is_empty();
    if has_calls {
        message.insert("tool_calls".into(), Value::Array(calls));
    }
    if compat.reasoning_on_assistant && model.reasoning && !message.contains_key("reasoning_content") {
        message.insert("reasoning_content".into(), json!(""));
    }
    (!text.is_empty() || has_calls).then_some(Value::Object(message))
}

fn tools(request: &Request<'_>) -> Vec<Value> {
    request.tools.iter().map(|tool| json!({ "type": "function", "function": { "name": tool.name, "description": tool.description, "parameters": tool.parameters } })).collect()
}

/// The request and its reply reader.
pub(crate) fn request(model: &Model, request: &Request<'_>, api_key: Option<&str>, _user_agent: &str) -> (HttpRequest, Box<dyn Reader>) {
    let compat = compat(model);
    let mut body = Map::new();
    body.insert("model".into(), json!(model.id));
    body.insert("messages".into(), Value::Array(messages(model, request, &compat)));
    body.insert("stream".into(), json!(true));
    if model.base_url.contains("api.openai.com")
        && !request.no_cache
        && let Some(session) = request.session_id
    {
        body.insert("prompt_cache_key".into(), json!(session.chars().take(64).collect::<String>()));
    }
    body.insert("stream_options".into(), json!({ "include_usage": true }));
    if compat.store {
        body.insert("store".into(), json!(false));
    }
    let field = model.max_tokens_field.clone().unwrap_or_else(|| "max_completion_tokens".into());
    body.insert(field, json!(super::max_tokens(model, request)));
    if !request.tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools(request)));
    } else if request.messages.iter().any(|message| matches!(message, Message::ToolResult(_)) || matches!(message, Message::Assistant(reply) if reply.content.iter().any(|block| matches!(block, AssistantBlock::ToolCall { .. })))) {
        // Proxies in front of Anthropic want the field whenever the conversation used tools.
        body.insert("tools".into(), json!([]));
    }
    if request.no_tool_calls && !request.tools.is_empty() {
        body.insert("tool_choice".into(), json!("none"));
    }
    if model.reasoning {
        thinking(model, request.reasoning.filter(|level| *level != Level::Off), &compat, &mut body);
    }
    let mut headers = Vec::new();
    if let Some(key) = api_key.filter(|key| !key.is_empty()) {
        headers.push(("authorization".to_owned(), format!("Bearer {key}")));
    }
    if compat.openrouter
        && let Some(session) = request.session_id
    {
        headers.push(("x-session-id".to_owned(), session.to_owned()));
    }
    let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    (HttpRequest { url, headers, body: Value::Object(body) }, Box::new(CompletionsReader::default()))
}

/// Reasoning parameters in the server's dialect.
fn thinking(model: &Model, effort: Option<Level>, compat: &Compat, body: &mut Map<String, Value>) {
    let named = |level: Level| json!(model.levels.name(level.as_str()).unwrap_or(level.as_str()));
    let off = model.levels.name("off").map(str::to_owned);
    match compat.thinking_format.as_str() {
        "deepseek" => {
            if effort.is_some() {
                body.insert("thinking".into(), json!({ "type": "enabled" }));
            } else if model.levels.has_off() {
                body.insert("thinking".into(), json!({ "type": "disabled" }));
            }
            if let Some(level) = effort.filter(|_| compat.reasoning_effort) {
                body.insert("reasoning_effort".into(), named(level));
            }
        }
        "qwen" => {
            body.insert("enable_thinking".into(), json!(effort.is_some()));
            if let Some(level) = effort.filter(|_| compat.reasoning_effort) {
                body.insert("reasoning_effort".into(), named(level));
            }
        }
        "zai" => {
            body.insert("thinking".into(), if effort.is_some() { json!({ "type": "enabled", "clear_thinking": false }) } else { json!({ "type": "disabled" }) });
        }
        "together" => {
            body.insert("reasoning".into(), json!({ "enabled": effort.is_some() }));
        }
        "openrouter" => match effort {
            Some(level) => {
                body.insert("reasoning".into(), json!({ "effort": named(level) }));
            }
            None if model.levels.has_off() => {
                body.insert("reasoning".into(), json!({ "effort": off.unwrap_or_else(|| "none".into()) }));
            }
            None => {}
        },
        _ => match effort {
            Some(level) if compat.reasoning_effort => {
                body.insert("reasoning_effort".into(), named(level));
            }
            None if compat.reasoning_effort => {
                if let Some(Some(off)) = model.levels.entry("off") {
                    body.insert("reasoning_effort".into(), json!(off));
                }
            }
            _ => {}
        },
    }
}

#[derive(Default)]
struct CompletionsReader {
    text: Option<usize>,
    thinking: Option<usize>,
    /// Tool call blocks by the stream's index and by id, with their arguments as written so far.
    by_index: HashMap<u64, usize>,
    by_id: HashMap<String, usize>,
    arguments: HashMap<usize, String>,
    finished: bool,
}

fn stop_reason(reason: &str) -> (&'static str, Option<String>) {
    match reason {
        "stop" | "end" => ("stop", None),
        "length" => ("length", None),
        "function_call" | "tool_calls" => ("toolUse", None),
        other => ("error", Some(format!("Provider finish_reason: {other}"))),
    }
}

fn chunk_usage(raw: &Value) -> rainy_protocol::chat::Usage {
    let number = |value: Option<&Value>| value.and_then(Value::as_u64).unwrap_or(0);
    let prompt = number(raw.get("prompt_tokens"));
    let read = raw
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .or_else(|| raw.get("prompt_cache_hit_tokens").and_then(Value::as_u64))
        .or_else(|| raw.get("cached_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let write = number(raw.pointer("/prompt_tokens_details/cache_write_tokens"));
    let reasoning = number(raw.pointer("/completion_tokens_details/reasoning_tokens"));
    usage(prompt.saturating_sub(read + write), number(raw.get("completion_tokens")), read, write, Some(reasoning))
}

impl Reader for CompletionsReader {
    fn event(&mut self, _name: &str, data: &str, output: &mut AssistantMessage, emit: &mut dyn FnMut(Event, &AssistantMessage)) -> Result<(), String> {
        if data.trim() == "[DONE]" {
            return Ok(());
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else { return Ok(()) };
        if let Some(error) = chunk.get("error").filter(|error| !error.is_null()) {
            let message = error.get("message").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| error.to_string());
            return Err(message);
        }
        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            output.rest.entry("responseId").or_insert_with(|| json!(id));
        }
        if let Some(raw) = chunk.get("usage").filter(|usage| usage.is_object()) {
            output.usage = chunk_usage(raw);
        }
        let Some(choice) = chunk.get("choices").and_then(Value::as_array).and_then(|choices| choices.first()) else { return Ok(()) };
        if chunk.get("usage").is_none_or(Value::is_null)
            && let Some(raw) = choice.get("usage").filter(|usage| usage.is_object())
        {
            output.usage = chunk_usage(raw);
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            let (stop, error) = stop_reason(reason);
            output.stop_reason = stop.into();
            output.rest.insert("rawStopReason".into(), json!(reason));
            if error.is_some() {
                output.error_message = error;
            }
            self.finished = true;
        }
        let Some(delta) = choice.get("delta") else { return Ok(()) };
        if let Some(text) = delta.get("content").and_then(Value::as_str).filter(|text| !text.is_empty()) {
            let index = match self.text {
                Some(index) => index,
                None => {
                    output.content.push(AssistantBlock::Text { text: String::new(), text_signature: None, rest: Map::new() });
                    let index = output.content.len() - 1;
                    self.text = Some(index);
                    emit(Event::BlockStarted(index), output);
                    index
                }
            };
            if let AssistantBlock::Text { text: held, .. } = &mut output.content[index] {
                held.push_str(text);
            }
            emit(Event::Updated, output);
        }
        if let Some((field, text)) = ["reasoning_content", "reasoning", "reasoning_text"].iter().find_map(|field| delta.get(*field).and_then(Value::as_str).filter(|text| !text.is_empty()).map(|text| (*field, text))) {
            let index = match self.thinking {
                Some(index) => index,
                None => {
                    output.content.push(AssistantBlock::Thinking { thinking: String::new(), thinking_signature: Some(field.into()), redacted: None, rest: Map::new() });
                    let index = output.content.len() - 1;
                    self.thinking = Some(index);
                    emit(Event::BlockStarted(index), output);
                    index
                }
            };
            if let AssistantBlock::Thinking { thinking, .. } = &mut output.content[index] {
                thinking.push_str(text);
            }
            emit(Event::Updated, output);
        }
        for call in delta.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
            let stream_index = call.get("index").and_then(Value::as_u64);
            let id = call.get("id").and_then(Value::as_str).filter(|id| !id.is_empty());
            let name = call.pointer("/function/name").and_then(Value::as_str).unwrap_or_default();
            let known = stream_index.and_then(|at| self.by_index.get(&at).copied()).or_else(|| id.and_then(|id| self.by_id.get(id).copied()));
            let index = match known {
                Some(index) => index,
                None => {
                    output.content.push(AssistantBlock::ToolCall { id: id.unwrap_or_default().into(), name: name.into(), arguments: Map::new(), rest: Map::new() });
                    let index = output.content.len() - 1;
                    self.arguments.insert(index, String::new());
                    emit(Event::BlockStarted(index), output);
                    index
                }
            };
            if let Some(at) = stream_index {
                self.by_index.entry(at).or_insert(index);
            }
            if let Some(id) = id {
                self.by_id.insert(id.to_owned(), index);
            }
            let partial = self.arguments.entry(index).or_default();
            if let Some(more) = call.pointer("/function/arguments").and_then(Value::as_str) {
                partial.push_str(more);
            }
            if let AssistantBlock::ToolCall { id: held_id, name: held_name, arguments, .. } = &mut output.content[index] {
                if held_id.is_empty()
                    && let Some(id) = id
                {
                    *held_id = id.into();
                }
                if held_name.is_empty() && !name.is_empty() {
                    *held_name = name.into();
                }
                *arguments = preview_arguments(partial);
            }
            emit(Event::Updated, output);
        }
        Ok(())
    }

    fn finish(&mut self, output: &mut AssistantMessage, emit: &mut dyn FnMut(Event, &AssistantMessage)) -> Result<(), String> {
        for index in 0..output.content.len() {
            if let AssistantBlock::ToolCall { arguments, .. } = &mut output.content[index] {
                *arguments = parse_arguments(self.arguments.get(&index).map_or("", String::as_str));
            }
            emit(Event::BlockEnded(index), output);
        }
        if output.stop_reason == "error" {
            return Err(output.error_message.clone().unwrap_or_else(|| "Provider returned an error stop reason".into()));
        }
        if !self.finished || output.stop_reason == "pending" {
            return Err("Stream ended without finish_reason".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{Api, LevelMap, ToolSpec};

    fn model(provider: &str, url: &str) -> Model {
        Model {
            api: Api::Completions,
            id: "m".into(),
            provider: provider.into(),
            base_url: url.into(),
            reasoning: true,
            images: false,
            context_window: 128_000,
            max_tokens: 8000,
            levels: LevelMap::EFFORTS,
            cost: Default::default(),
            claude: None,
            max_tokens_field: Some("max_tokens".into()),
            thinking_format: None,
        }
    }

    #[test]
    fn builds_deepseek_requests() {
        let model = model("deepseek", "https://api.deepseek.com");
        let mut reply = crate::agent::llm::empty_reply(&model);
        reply.stop_reason = "toolUse".into();
        reply.content = vec![
            AssistantBlock::Thinking { thinking: "plan".into(), thinking_signature: Some("reasoning_content".into()), redacted: None, rest: Map::new() },
            AssistantBlock::ToolCall { id: "c1".into(), name: "read".into(), arguments: json!({ "path": "a" }).as_object().unwrap().clone(), rest: Map::new() },
        ];
        let messages = vec![Message::user("hi"), Message::Assistant(reply)];
        let tools = vec![ToolSpec { name: "read".into(), description: "Read".into(), parameters: json!({ "type": "object" }) }];
        let request = Request { system: "sys", messages: &messages, tools: &tools, reasoning: Some(Level::High), max_tokens: None, session_id: Some("s"), unmark_last_user: false, no_cache: false, no_tool_calls: false };
        let (http, _) = super::request(&model, &request, Some("k"), "ua");
        let body = &http.body;
        assert_eq!(http.url, "https://api.deepseek.com/chat/completions");
        assert_eq!(body["thinking"], json!({ "type": "enabled" }));
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("store").is_none());
        assert_eq!(body["messages"][0], json!({ "role": "system", "content": "sys" }));
        assert_eq!(body["messages"][2]["reasoning_content"], "plan");
        assert_eq!(body["messages"][2]["tool_calls"][0]["function"]["arguments"], r#"{"path":"a"}"#);
        // The unanswered call gets a failed result.
        assert_eq!(body["messages"][3]["content"], "No result provided");
    }

    #[test]
    fn reads_a_stream() {
        let model = model("local", "http://127.0.0.1:8080/v1");
        let mut output = crate::agent::llm::empty_reply(&model);
        let mut reader = CompletionsReader::default();
        let mut events = Vec::new();
        let mut emit = |event: Event, _: &AssistantMessage| events.push(event);
        for data in [
            r#"{"id":"x","choices":[{"delta":{"reasoning_content":"think"}}]}"#,
            r#"{"choices":[{"delta":{"content":"Hi"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":60}}}"#,
            "[DONE]",
        ] {
            reader.event("", data, &mut output, &mut emit).unwrap();
        }
        reader.finish(&mut output, &mut emit).unwrap();
        assert_eq!(output.stop_reason, "toolUse");
        assert_eq!((output.usage.input, output.usage.cache_read, output.usage.output), (40.0, 60.0, 20.0));
        assert!(matches!(&output.content[0], AssistantBlock::Thinking { thinking, .. } if thinking == "think"));
        assert!(matches!(&output.content[2], AssistantBlock::ToolCall { arguments, .. } if arguments["path"] == "a.txt"));
        assert!(events.contains(&Event::BlockStarted(0)) && events.contains(&Event::BlockEnded(2)));
    }
}
