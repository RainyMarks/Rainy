//! Anthropic Messages (`POST {base}/v1/messages`) with the hour-long prompt cache: breakpoints on the system prompt, the
//! last tool, the last user message and the user message that ended the previous request (the last part derived from
//! the DeepSeek Harness pi-ai adapter, MIT, see LICENSE.upstream).

use std::collections::HashMap;

use rainy_protocol::chat::{AssistantBlock, AssistantMessage, ResultContent};
use serde_json::{Map, Value, json};

use super::json::parse_arguments;
use super::{Event, HttpRequest, Level, Message, Model, Part, Reader, Request, UserContent, preview_arguments, replay};

const INTERLEAVED_THINKING: &str = "interleaved-thinking-2025-05-14";
const OUTPUT_CONFIG: &str = "mid-conversation-output-config-2026-07-01";
const THINKING_BINDING: &str = "thinking-binding-controls-2026-08-01";
/// Breakpoints Anthropic allows in one request.
const MAX_BREAKPOINTS: usize = 4;
/// Thinking tokens of a budget-thinking model, by level.
const BUDGETS: [(Level, u64); 3] = [(Level::Low, 2048), (Level::High, 16_384), (Level::Max, 16_384)];
/// Tokens always left for the answer when a thinking budget shares the response ceiling.
const MIN_ANSWER_TOKENS: u64 = 1024;

fn cache_control() -> Value {
    json!({ "type": "ephemeral", "ttl": "1h" })
}

fn normalize_id(id: &str) -> String {
    id.chars().map(|char| if char.is_ascii_alphanumeric() || char == '_' || char == '-' { char } else { '_' }).take(64).collect()
}

/// The effort of an adaptive-thinking model for a level.
fn effort(model: &Model, level: Level) -> &'static str {
    let named = model.levels.0.iter().find(|(name, _)| *name == level.as_str()).and_then(|(_, value)| *value);
    named.unwrap_or(match level {
        Level::Low => "low",
        _ => "high",
    })
}

/// A tool result's content: one string when it is all text, blocks when it holds images.
fn result_blocks(content: &[ResultContent]) -> Value {
    if !content.iter().any(|part| matches!(part, ResultContent::Image { .. })) {
        return json!(super::text_parts(content));
    }
    let mut blocks: Vec<Value> = content
        .iter()
        .map(|part| match part {
            ResultContent::Text { text } => json!({ "type": "text", "text": text }),
            ResultContent::Image { data, mime_type } => json!({ "type": "image", "source": { "type": "base64", "media_type": mime_type, "data": data } }),
        })
        .collect();
    if !content.iter().any(|part| matches!(part, ResultContent::Text { .. })) {
        blocks.insert(0, json!({ "type": "text", "text": "(see attached image)" }));
    }
    Value::Array(blocks)
}

/// The conversation as Anthropic messages, each reply's effort before it for a model that replays them.
fn messages(model: &Model, request: &Request<'_>, active_effort: &str) -> Vec<Value> {
    let effort_messages = model.claude.is_some_and(|claude| claude.effort_messages);
    let mut params: Vec<Value> = Vec::new();
    let mut efforts: HashMap<usize, String> = HashMap::new();
    let replayed = replay(request.messages, model, &|id, _| normalize_id(id));
    let mut index = 0;
    while index < replayed.len() {
        match &replayed[index] {
            Message::User { content: UserContent::Text(text), .. } => {
                if !text.trim().is_empty() {
                    params.push(json!({ "role": "user", "content": [{ "type": "text", "text": text }] }));
                }
            }
            Message::User { content: UserContent::Parts(parts), .. } => {
                let blocks: Vec<Value> = parts
                    .iter()
                    .filter(|part| !matches!(part, Part::Text(text) if text.trim().is_empty()))
                    .map(|part| match part {
                        Part::Text(text) => json!({ "type": "text", "text": text }),
                        Part::Image { data, mime_type } => json!({ "type": "image", "source": { "type": "base64", "media_type": mime_type, "data": data } }),
                    })
                    .collect();
                if !blocks.is_empty() {
                    params.push(json!({ "role": "user", "content": blocks }));
                }
            }
            Message::Assistant(reply) => {
                let mut blocks = Vec::new();
                for block in &reply.content {
                    match block {
                        AssistantBlock::Text { text, .. } if !text.trim().is_empty() => blocks.push(json!({ "type": "text", "text": text })),
                        AssistantBlock::Text { .. } => {}
                        AssistantBlock::Thinking { redacted: Some(true), thinking_signature, .. } => blocks.push(json!({ "type": "redacted_thinking", "data": thinking_signature.clone().unwrap_or_default() })),
                        AssistantBlock::Thinking { thinking, thinking_signature, .. } => match thinking_signature.as_deref().filter(|signature| !signature.trim().is_empty()) {
                            Some(signature) => blocks.push(json!({ "type": "thinking", "thinking": thinking, "signature": signature })),
                            // Without its signature (a stopped stream) the thinking can only go back as text.
                            None if !thinking.trim().is_empty() => blocks.push(json!({ "type": "text", "text": thinking })),
                            None => {}
                        },
                        AssistantBlock::ToolCall { id, name, arguments, .. } => blocks.push(json!({ "type": "tool_use", "id": id, "name": name, "input": arguments })),
                    }
                }
                if blocks.is_empty() {
                    index += 1;
                    continue;
                }
                if effort_messages
                    && reply.api == "anthropic-messages"
                    && reply.provider == model.provider
                    && let Some(level) = reply.rest.get("providerThinkingLevel").and_then(Value::as_str).filter(|level| matches!(*level, "low" | "medium" | "high" | "xhigh" | "max"))
                {
                    efforts.insert(params.len(), level.to_owned());
                }
                params.push(json!({ "role": "assistant", "content": blocks }));
            }
            Message::ToolResult(_) => {
                let mut results = Vec::new();
                while let Some(Message::ToolResult(result)) = replayed.get(index) {
                    results.push(json!({ "type": "tool_result", "tool_use_id": result.tool_call_id, "content": result_blocks(&result.content), "is_error": result.is_error }));
                    index += 1;
                }
                params.push(json!({ "role": "user", "content": results }));
                continue;
            }
        }
        index += 1;
    }
    // The conversation so far is cached up to its last user message.
    if let Some(last) = params.last_mut().filter(|message| message["role"] == "user")
        && let Some(block) = last["content"].as_array_mut().and_then(|blocks| blocks.last_mut())
    {
        block["cache_control"] = cache_control();
    }
    if !effort_messages {
        return params;
    }
    let mut with_efforts = Vec::new();
    for (position, message) in params.into_iter().enumerate() {
        if let Some(level) = efforts.get(&position) {
            with_efforts.push(json!({ "role": "system", "content": [], "output_config": { "effort": level } }));
        }
        with_efforts.push(message);
    }
    with_efforts.push(json!({ "role": "system", "content": [], "output_config": { "effort": active_effort } }));
    with_efforts
}

fn marks(blocks: &Value) -> usize {
    blocks.as_array().map_or(0, |blocks| blocks.iter().filter(|block| block.get("cache_control").is_some()).count())
}

/// Also mark the user message that ended the previous request: Anthropic looks back only about 20 blocks from a
/// breakpoint, so a step that adds many tool results would otherwise write the whole conversation again.
fn mark_previous_turn(body: &mut Map<String, Value>) {
    let used = marks(&body["system"]) + marks(&body["tools"]) + body["messages"].as_array().map_or(0, |messages| messages.iter().map(|message| marks(&message["content"])).sum());
    if used == 0 || used >= MAX_BREAKPOINTS {
        return;
    }
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else { return };
    let before = |messages: &[Value], from: Option<usize>, role: &str| -> Option<usize> { (0..=from?).rev().find(|index| messages[*index]["role"] == role) };
    let last_user = before(messages, messages.len().checked_sub(1), "user");
    let reply = before(messages, last_user.and_then(|index| index.checked_sub(1)), "assistant");
    let Some(previous) = before(messages, reply.and_then(|index| index.checked_sub(1)), "user") else { return };
    if marks(&messages[previous]["content"]) > 0 {
        return;
    }
    if let Some(block) = messages[previous]["content"].as_array_mut().and_then(|blocks| blocks.last_mut())
        && matches!(block["type"].as_str(), Some("text" | "image" | "tool_result"))
    {
        block["cache_control"] = cache_control();
    }
}

/// Remove every breakpoint: nothing in this request is sent again.
fn strip_cache(body: &mut Map<String, Value>) {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.remove("cache_control");
                map.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    for field in ["system", "tools", "messages"] {
        if let Some(value) = body.get_mut(field) {
            strip(value);
        }
    }
}

/// Leave the last user message unmarked: a compaction request reads the previous request's cache entry but writes
/// nothing worth keeping, since the history changes once the summary lands.
fn unmark_last_user(body: &mut Map<String, Value>) {
    if let Some(last) = body.get_mut("messages").and_then(Value::as_array_mut).and_then(|messages| messages.iter_mut().rev().find(|message| message["role"] == "user"))
        && let Some(blocks) = last["content"].as_array_mut()
    {
        for block in blocks {
            if let Some(block) = block.as_object_mut() {
                block.remove("cache_control");
            }
        }
    }
}

/// The request and its reply reader; `output` records the effort a managed-effort model answers with.
pub(crate) fn request(model: &Model, request: &Request<'_>, api_key: Option<&str>, output: &mut AssistantMessage) -> (HttpRequest, Box<dyn Reader>) {
    let options = model.claude.unwrap_or(super::catalog::ClaudeOptions { adaptive: false, effort_messages: false });
    let level = request.reasoning.filter(|level| *level != Level::Off && model.reasoning);
    let active_effort = level.map_or("high", |level| effort(model, level));
    if options.effort_messages {
        output.rest.insert("providerThinkingLevel".into(), json!(active_effort));
    }
    let mut betas: Vec<&str> = Vec::new();
    let mut body = Map::new();
    body.insert("model".into(), json!(model.id));
    body.insert("messages".into(), Value::Array(messages(model, request, active_effort)));
    let mut max_tokens = super::max_tokens(model, request);
    body.insert("stream".into(), json!(true));
    if !request.system.is_empty() {
        body.insert("system".into(), json!([{ "type": "text", "text": request.system, "cache_control": cache_control() }]));
    }
    if !request.tools.is_empty() {
        let last = request.tools.len() - 1;
        let tools: Vec<Value> = request
            .tools
            .iter()
            .enumerate()
            .map(|(index, tool)| {
                let schema = json!({ "type": "object", "properties": tool.parameters.get("properties").cloned().unwrap_or_else(|| json!({})), "required": tool.parameters.get("required").cloned().unwrap_or_else(|| json!([])) });
                let mut entry = json!({ "name": tool.name, "description": tool.description, "eager_input_streaming": true, "input_schema": schema });
                if index == last {
                    entry["cache_control"] = cache_control();
                }
                entry
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    if options.effort_messages {
        body.insert("thinking".into(), json!({ "type": "adaptive", "display": "summarized", "block_binding": { "prefix_mismatch_behavior": "drop_block" } }));
        body.insert("output_config".into(), json!({ "effort": "high" }));
        betas.extend([OUTPUT_CONFIG, THINKING_BINDING]);
    } else if model.reasoning {
        match level {
            Some(level) if options.adaptive => {
                body.insert("thinking".into(), json!({ "type": "adaptive", "display": "summarized" }));
                body.insert("output_config".into(), json!({ "effort": effort(model, level) }));
            }
            Some(level) => {
                let mut budget = BUDGETS.iter().find(|(known, _)| *known == level).map_or(16_384, |(_, budget)| *budget);
                let ceiling = (max_tokens + budget).min(model.max_tokens);
                if ceiling <= budget {
                    budget = budget.min(ceiling.saturating_sub(MIN_ANSWER_TOKENS));
                }
                max_tokens = super::clamp_to_context(model, request, ceiling);
                body.insert("thinking".into(), json!({ "type": "enabled", "budget_tokens": budget.min(max_tokens.saturating_sub(1024)), "display": "summarized" }));
                betas.push(INTERLEAVED_THINKING);
            }
            None if model.levels.has_off() => {
                body.insert("thinking".into(), json!({ "type": "disabled" }));
            }
            None => {}
        }
    }
    body.insert("max_tokens".into(), json!(max_tokens));
    if request.no_tool_calls && !request.tools.is_empty() {
        body.insert("tool_choice".into(), json!({ "type": "none" }));
    }
    if request.no_cache {
        strip_cache(&mut body);
    } else {
        mark_previous_turn(&mut body);
    }
    if request.unmark_last_user {
        unmark_last_user(&mut body);
    }
    let mut headers = vec![("anthropic-version".to_owned(), "2023-06-01".to_owned()), ("accept".to_owned(), "application/json".to_owned())];
    if let Some(key) = api_key.filter(|key| !key.is_empty()) {
        headers.push(("x-api-key".to_owned(), key.to_owned()));
    }
    if !betas.is_empty() {
        headers.push(("anthropic-beta".to_owned(), betas.join(",")));
    }
    let base = model.base_url.trim_end_matches('/');
    let url = format!("{}/v1/messages", base.strip_suffix("/v1").unwrap_or(base));
    (HttpRequest { url, headers, body: Value::Object(body) }, Box::new(AnthropicReader::default()))
}

#[derive(Default)]
struct AnthropicReader {
    /// Content indexes by the stream's block index, and the arguments of tool calls being written.
    blocks: HashMap<u64, usize>,
    arguments: HashMap<usize, String>,
}

fn stop_reason(reason: &str, details: &Value) -> Result<(&'static str, Option<String>), String> {
    Ok(match reason {
        "end_turn" | "pause_turn" | "stop_sequence" => ("stop", None),
        "max_tokens" | "model_context_window_exceeded" => ("length", None),
        "tool_use" => ("toolUse", None),
        "refusal" => ("error", Some(details["explanation"].as_str().unwrap_or("The model refused to complete the request").to_owned())),
        "sensitive" => ("error", Some("Provider stopped with: sensitive".into())),
        other => return Err(format!("Unhandled stop reason: {other}")),
    })
}

impl Reader for AnthropicReader {
    fn event(&mut self, name: &str, data: &str, output: &mut AssistantMessage, emit: &mut dyn FnMut(Event, &AssistantMessage)) -> Result<(), String> {
        let Ok(event) = serde_json::from_str::<Value>(data) else { return Ok(()) };
        let kind = event["type"].as_str().unwrap_or(name);
        let number = |value: &Value| value.as_u64().unwrap_or(0);
        match kind {
            "message_start" => {
                let message = &event["message"];
                if let Some(id) = message["id"].as_str() {
                    output.rest.insert("responseId".into(), json!(id));
                }
                if let Some(model) = message["model"].as_str().filter(|model| *model != output.model) {
                    output.rest.insert("responseModel".into(), json!(model));
                }
                let usage = &message["usage"];
                output.usage = super::usage(number(&usage["input_tokens"]), number(&usage["output_tokens"]), number(&usage["cache_read_input_tokens"]), number(&usage["cache_creation_input_tokens"]), None);
            }
            "content_block_start" => {
                let block = &event["content_block"];
                let created = match block["type"].as_str() {
                    Some("text") => AssistantBlock::Text { text: block["text"].as_str().unwrap_or_default().into(), text_signature: None, rest: Map::new() },
                    Some("thinking") => AssistantBlock::Thinking {
                        thinking: block["thinking"].as_str().unwrap_or_default().into(),
                        thinking_signature: Some(block["signature"].as_str().unwrap_or_default().into()),
                        redacted: None,
                        rest: Map::new(),
                    },
                    Some("redacted_thinking") => {
                        AssistantBlock::Thinking { thinking: "[Reasoning redacted]".into(), thinking_signature: block["data"].as_str().map(str::to_owned), redacted: Some(true), rest: Map::new() }
                    }
                    Some("tool_use") => AssistantBlock::ToolCall {
                        id: block["id"].as_str().unwrap_or_default().into(),
                        name: block["name"].as_str().unwrap_or_default().into(),
                        arguments: block["input"].as_object().cloned().unwrap_or_default(),
                        rest: Map::new(),
                    },
                    Some("fallback") if !output.content.is_empty() => return Err("Anthropic performed an unsupported mid-output model fallback".into()),
                    _ => return Ok(()),
                };
                if matches!(created, AssistantBlock::ToolCall { .. }) {
                    self.arguments.insert(output.content.len(), String::new());
                }
                output.content.push(created);
                let index = output.content.len() - 1;
                self.blocks.insert(number(&event["index"]), index);
                emit(Event::BlockStarted(index), output);
            }
            "content_block_delta" => {
                let Some(&index) = self.blocks.get(&number(&event["index"])) else { return Ok(()) };
                let delta = &event["delta"];
                match (&mut output.content[index], delta["type"].as_str()) {
                    (AssistantBlock::Text { text, .. }, Some("text_delta")) => text.push_str(delta["text"].as_str().unwrap_or_default()),
                    (AssistantBlock::Thinking { thinking, .. }, Some("thinking_delta")) => thinking.push_str(delta["thinking"].as_str().unwrap_or_default()),
                    (AssistantBlock::Thinking { thinking_signature, .. }, Some("signature_delta")) => {
                        thinking_signature.get_or_insert_with(String::new).push_str(delta["signature"].as_str().unwrap_or_default());
                    }
                    (AssistantBlock::ToolCall { arguments, .. }, Some("input_json_delta")) => {
                        let partial = self.arguments.entry(index).or_default();
                        partial.push_str(delta["partial_json"].as_str().unwrap_or_default());
                        *arguments = preview_arguments(partial);
                    }
                    _ => return Ok(()),
                }
                emit(Event::Updated, output);
            }
            "content_block_stop" => {
                let Some(index) = self.blocks.remove(&number(&event["index"])) else { return Ok(()) };
                if let Some(partial) = self.arguments.remove(&index)
                    && let AssistantBlock::ToolCall { arguments, .. } = &mut output.content[index]
                {
                    *arguments = parse_arguments(&partial);
                }
                emit(Event::BlockEnded(index), output);
            }
            "message_delta" => {
                if let Some(reason) = event.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    output.rest.insert("rawStopReason".into(), json!(reason));
                    let (stop, error) = stop_reason(reason, &event["delta"]["stop_details"])?;
                    output.stop_reason = stop.into();
                    if error.is_some() {
                        output.error_message = error;
                    }
                }
                let usage = &event["usage"];
                let set = |field: &str, target: &mut f64| {
                    if let Some(value) = usage[field].as_u64() {
                        *target = value as f64;
                    }
                };
                set("input_tokens", &mut output.usage.input);
                set("output_tokens", &mut output.usage.output);
                set("cache_read_input_tokens", &mut output.usage.cache_read);
                set("cache_creation_input_tokens", &mut output.usage.cache_write);
                if let Some(thinking) = usage.pointer("/output_tokens_details/thinking_tokens").and_then(Value::as_u64) {
                    output.usage.reasoning = Some(thinking as f64);
                }
            }
            "error" => {
                let error = &event["error"];
                return Err(format!("{}: {}", error["type"].as_str().unwrap_or("error"), error["message"].as_str().unwrap_or("Unknown error")));
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(&mut self, output: &mut AssistantMessage, _emit: &mut dyn FnMut(Event, &AssistantMessage)) -> Result<(), String> {
        for (index, partial) in self.arguments.drain() {
            if let Some(AssistantBlock::ToolCall { arguments, .. }) = output.content.get_mut(index) {
                *arguments = parse_arguments(&partial);
            }
        }
        match output.stop_reason.as_str() {
            "pending" => Err("Anthropic stream ended without a stop reason".into()),
            "error" | "aborted" => Err(output.error_message.clone().unwrap_or_else(|| "An unknown error occurred".into())),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{Model, ToolSpec};
    use crate::settings::ModelSetup;

    fn opus() -> Model {
        let setup = ModelSetup {
            provider: "rainy-claude".into(),
            base_url: "https://api.anthropic.com".into(),
            model: "claude-opus-5-5".into(),
            context_window: 200_000,
            max_tokens: Some(64_000),
            local: false,
            api: Some("anthropic-messages".into()),
            thinking: Some("high".into()),
            thinking_format: None,
            max_tokens_field: None,
            images: None,
        };
        Model::from_setup(&setup, 64_000)
    }

    #[test]
    fn caches_and_replays_effort() {
        let model = opus();
        let mut first = crate::agent::llm::empty_reply(&model);
        first.stop_reason = "toolUse".into();
        first.rest.insert("providerThinkingLevel".into(), json!("low"));
        first.content = vec![AssistantBlock::ToolCall { id: "toolu_1".into(), name: "read".into(), arguments: Map::new(), rest: Map::new() }];
        let result = crate::agent::llm::ToolResult { tool_call_id: "toolu_1".into(), tool_name: "read".into(), content: vec![ResultContent::Text { text: "x".into() }], is_error: false, details: None, timestamp: 0.0 };
        let mut second = crate::agent::llm::empty_reply(&model);
        second.stop_reason = "stop".into();
        second.content = vec![AssistantBlock::Text { text: "done".into(), text_signature: None, rest: Map::new() }];
        let messages = vec![Message::user("one"), Message::Assistant(first), Message::ToolResult(result), Message::Assistant(second), Message::user("two")];
        let tools = vec![ToolSpec { name: "read".into(), description: "Read".into(), parameters: json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"], "additionalProperties": false }) }];
        let request = Request { system: "sys", messages: &messages, tools: &tools, reasoning: Some(Level::Max), max_tokens: None, session_id: None, unmark_last_user: false, no_cache: false, no_tool_calls: false };
        let mut output = crate::agent::llm::empty_reply(&model);
        let (http, _) = super::request(&model, &request, Some("key"), &mut output);
        let body = &http.body;
        assert_eq!(http.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(output.rest["providerThinkingLevel"], "max");
        let roles: Vec<&str> = body["messages"].as_array().unwrap().iter().map(|message| message["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "system", "assistant", "user", "assistant", "user", "system"]);
        assert_eq!(body["messages"][1]["output_config"]["effort"], "low");
        assert_eq!(body["messages"][6]["output_config"]["effort"], "max");
        // The last user message and the one that ended the previous request carry breakpoints.
        assert!(body["messages"][5]["content"][0].get("cache_control").is_some());
        assert!(body["messages"][3]["content"][0].get("cache_control").is_some());
        assert!(body["tools"][0].get("cache_control").is_some());
        assert!(body["tools"][0]["input_schema"].get("additionalProperties").is_none());
        assert!(http.headers.iter().any(|(name, value)| name == "anthropic-beta" && value.contains(OUTPUT_CONFIG)));
    }

    #[test]
    fn reads_a_stream() {
        let model = opus();
        let mut output = crate::agent::llm::empty_reply(&model);
        let mut reader = AnthropicReader::default();
        let mut emit = |_: Event, _: &AssistantMessage| {};
        for data in [
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":10,"cache_read_input_tokens":90,"cache_creation_input_tokens":5,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Plan"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\": \"a\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#,
            r#"{"type":"message_stop"}"#,
        ] {
            reader.event("", data, &mut output, &mut emit).unwrap();
        }
        reader.finish(&mut output, &mut emit).unwrap();
        assert_eq!(output.stop_reason, "toolUse");
        assert_eq!((output.usage.input, output.usage.cache_read, output.usage.cache_write, output.usage.output), (10.0, 90.0, 5.0, 42.0));
        assert!(matches!(&output.content[0], AssistantBlock::Thinking { thinking_signature: Some(signature), .. } if signature == "sig"));
        assert!(matches!(&output.content[1], AssistantBlock::ToolCall { arguments, .. } if arguments["path"] == "a"));
    }
}
