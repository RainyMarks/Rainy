//! OpenAI Responses (`POST {base}/responses`) without server-side state (`store: false`): reasoning comes back as
//! encrypted items the next request replays.

use std::collections::HashMap;

use rainy_protocol::chat::{AssistantBlock, AssistantMessage, ResultContent};
use serde_json::{Map, Value, json};

use super::json::parse_arguments;
use super::{Event, HttpRequest, Level, Message, Model, Part, Reader, Request, UserContent, preview_arguments, replay, usage};

/// Providers whose tool call ids carry the Responses item id (`call|item`).
const ITEM_ID_PROVIDERS: &[&str] = &["openai", "openai-codex", "opencode"];

fn clean_part(part: &str) -> String {
    let clean: String = part.chars().map(|char| if char.is_ascii_alphanumeric() || char == '_' || char == '-' { char } else { '_' }).take(64).collect();
    clean.trim_end_matches('_').to_owned()
}

fn short_hash(text: &str) -> String {
    crate::text::sha256_hex(text)[..16].to_owned()
}

/// A tool call id from another model in the form this provider accepts.
fn normalize_id(id: &str, model: &Model, source: &AssistantMessage) -> String {
    if !ITEM_ID_PROVIDERS.contains(&model.provider.as_str()) || !id.contains('|') {
        return clean_part(id);
    }
    let (call, item) = id.split_once('|').unwrap_or((id, ""));
    let foreign = source.provider != model.provider || source.api != model.api.as_str();
    let mut item = if foreign { format!("fc_{}", short_hash(item)) } else { clean_part(item) };
    if !item.starts_with("fc_") {
        item = clean_part(&format!("fc_{item}"));
    }
    format!("{}|{item}", clean_part(call))
}

/// `{"v":1,"id":…,"phase":…}` written on a text block, or a plain id from older replies.
fn text_signature(signature: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(signature) = signature else { return (None, None) };
    if signature.starts_with('{')
        && let Ok(parsed) = serde_json::from_str::<Value>(signature)
        && parsed["v"] == 1
        && let Some(id) = parsed["id"].as_str()
    {
        let phase = parsed["phase"].as_str().filter(|phase| *phase == "commentary" || *phase == "final_answer").map(str::to_owned);
        return (Some(id.to_owned()), phase);
    }
    (Some(signature.to_owned()), None)
}

fn tool_output(model: &Model, content: &[ResultContent]) -> Value {
    let text = super::text_parts(content);
    let images: Vec<(&String, &String)> = content.iter().filter_map(|part| if let ResultContent::Image { data, mime_type } = part { Some((data, mime_type)) } else { None }).collect();
    if images.is_empty() || !model.images {
        return json!(if !text.is_empty() {
            text
        } else if !images.is_empty() {
            "(see attached image)".into()
        } else {
            "(no tool output)".to_owned()
        });
    }
    let mut output = Vec::new();
    if !text.is_empty() {
        output.push(json!({ "type": "input_text", "text": text }));
    }
    for (data, mime_type) in images {
        output.push(json!({ "type": "input_image", "detail": "auto", "image_url": format!("data:{mime_type};base64,{data}") }));
    }
    Value::Array(output)
}

/// The conversation as Responses input items.
fn input(model: &Model, request: &Request<'_>) -> Vec<Value> {
    let mut items = Vec::new();
    if !request.system.is_empty() {
        items.push(json!({ "role": if model.reasoning { "developer" } else { "system" }, "content": request.system }));
    }
    for (index, message) in replay(request.messages, model, &|id, source| normalize_id(id, model, source)).iter().enumerate() {
        match message {
            Message::User { content: UserContent::Text(text), .. } => items.push(json!({ "role": "user", "content": [{ "type": "input_text", "text": text }] })),
            Message::User { content: UserContent::Parts(parts), .. } => {
                let content: Vec<Value> = parts
                    .iter()
                    .map(|part| match part {
                        Part::Text(text) => json!({ "type": "input_text", "text": text }),
                        Part::Image { data, mime_type } => json!({ "type": "input_image", "detail": "auto", "image_url": format!("data:{mime_type};base64,{data}") }),
                    })
                    .collect();
                if !content.is_empty() {
                    items.push(json!({ "role": "user", "content": content }));
                }
            }
            Message::Assistant(reply) => {
                let same_api = reply.provider == model.provider && reply.api == model.api.as_str();
                let other_model = same_api && reply.model != model.id;
                let mut text_index = 0;
                for block in &reply.content {
                    match block {
                        AssistantBlock::Thinking { thinking_signature: Some(signature), .. } if !signature.is_empty() => {
                            if let Ok(item) = serde_json::from_str::<Value>(signature) {
                                items.push(item);
                            }
                        }
                        AssistantBlock::Thinking { .. } => {}
                        AssistantBlock::Text { text, text_signature: signature, .. } => {
                            let (id, phase) = text_signature(signature.as_deref());
                            let fallback = if text_index == 0 { format!("msg_pi_{index}") } else { format!("msg_pi_{index}_{text_index}") };
                            text_index += 1;
                            let id = match id {
                                None => fallback,
                                Some(id) if id.len() > 64 => format!("msg_{}", short_hash(&id)),
                                Some(id) => id,
                            };
                            let mut item = json!({ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": text, "annotations": [] }], "status": "completed", "id": id });
                            if let Some(phase) = phase {
                                item["phase"] = json!(phase);
                            }
                            items.push(item);
                        }
                        AssistantBlock::ToolCall { id, name, arguments, .. } => {
                            let (call, item) = id.split_once('|').map_or((id.as_str(), None), |(call, item)| (call, Some(item)));
                            // An item id from another model, or not naming a function call, would fail pairing checks.
                            let item = item.filter(|item| !other_model && item.starts_with("fc_"));
                            let mut entry = json!({ "type": "function_call", "call_id": call, "name": name, "arguments": Value::Object(arguments.clone()).to_string() });
                            if let Some(item) = item {
                                entry["id"] = json!(item);
                            }
                            items.push(entry);
                        }
                    }
                }
            }
            Message::ToolResult(result) => {
                let call = result.tool_call_id.split('|').next().unwrap_or_default();
                items.push(json!({ "type": "function_call_output", "call_id": call, "output": tool_output(model, &result.content) }));
            }
        }
    }
    items
}

/// The request and its reply reader.
pub(crate) fn request(model: &Model, request: &Request<'_>, api_key: Option<&str>, _user_agent: &str) -> (HttpRequest, Box<dyn Reader>) {
    let mut body = Map::new();
    body.insert("model".into(), json!(model.id));
    body.insert("input".into(), Value::Array(input(model, request)));
    body.insert("stream".into(), json!(true));
    if let Some(session) = request.session_id.filter(|_| !request.no_cache) {
        body.insert("prompt_cache_key".into(), json!(session.chars().take(64).collect::<String>()));
    }
    body.insert("store".into(), json!(false));
    body.insert("max_output_tokens".into(), json!(super::max_tokens(model, request).max(16)));
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request.tools.iter().map(|tool| json!({ "type": "function", "name": tool.name, "description": tool.description, "parameters": tool.parameters })).collect();
        body.insert("tools".into(), Value::Array(tools));
        if request.no_tool_calls {
            body.insert("tool_choice".into(), json!("none"));
        }
    }
    if model.reasoning {
        match request.reasoning.filter(|level| *level != Level::Off) {
            Some(level) => {
                body.insert("reasoning".into(), json!({ "effort": model.levels.name(level.as_str()).unwrap_or(level.as_str()), "summary": "auto" }));
                body.insert("include".into(), json!(["reasoning.encrypted_content"]));
            }
            None if model.levels.has_off() => {
                body.insert("reasoning".into(), json!({ "effort": model.levels.name("off").unwrap_or("none") }));
            }
            None => {}
        }
    }
    let mut headers = Vec::new();
    if let Some(key) = api_key.filter(|key| !key.is_empty()) {
        headers.push(("authorization".to_owned(), format!("Bearer {key}")));
    }
    if let Some(session) = request.session_id {
        if model.provider == "openrouter" || model.base_url.contains("openrouter.ai") {
            headers.push(("x-session-id".to_owned(), session.to_owned()));
        } else {
            headers.push(("session_id".to_owned(), session.to_owned()));
            headers.push(("x-client-request-id".to_owned(), session.to_owned()));
        }
    }
    let url = format!("{}/responses", model.base_url.trim_end_matches('/'));
    (HttpRequest { url, headers, body: Value::Object(body) }, Box::new(ResponsesReader::default()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Thinking(usize),
    Text(usize),
    Call(usize),
}

#[derive(Default)]
struct ResponsesReader {
    slots: HashMap<u64, Slot>,
    /// Arguments of unfinished tool calls, by content index.
    arguments: HashMap<usize, String>,
    reasoning: HashMap<String, usize>,
    terminal: bool,
}

impl ResponsesReader {
    fn slot(&mut self, at: u64, item: &Value, output: &mut AssistantMessage, emit: &mut dyn FnMut(Event, &AssistantMessage)) -> Option<Slot> {
        if let Some(slot) = self.slots.get(&at) {
            return Some(*slot);
        }
        let slot = match item["type"].as_str()? {
            "reasoning" => {
                output.content.push(AssistantBlock::Thinking { thinking: String::new(), thinking_signature: None, redacted: None, rest: Map::new() });
                Slot::Thinking(output.content.len() - 1)
            }
            "message" => {
                if item["phase"] == "final_answer" {
                    output.stop_reason = "stop".into();
                }
                output.content.push(AssistantBlock::Text { text: String::new(), text_signature: None, rest: Map::new() });
                Slot::Text(output.content.len() - 1)
            }
            "function_call" => {
                let id = format!("{}|{}", item["call_id"].as_str().unwrap_or_default(), item["id"].as_str().unwrap_or_default());
                output.content.push(AssistantBlock::ToolCall { id, name: item["name"].as_str().unwrap_or_default().into(), arguments: Map::new(), rest: Map::new() });
                let index = output.content.len() - 1;
                self.arguments.insert(index, item["arguments"].as_str().unwrap_or_default().into());
                Slot::Call(index)
            }
            _ => return None,
        };
        self.slots.insert(at, slot);
        let index = match slot {
            Slot::Thinking(index) | Slot::Text(index) | Slot::Call(index) => index,
        };
        emit(Event::BlockStarted(index), output);
        Some(slot)
    }

    fn append(output: &mut AssistantMessage, slot: Option<Slot>, delta: &str, emit: &mut dyn FnMut(Event, &AssistantMessage)) {
        match (slot, delta) {
            (Some(Slot::Thinking(index)), delta) => {
                if let AssistantBlock::Thinking { thinking, .. } = &mut output.content[index] {
                    thinking.push_str(delta);
                }
            }
            (Some(Slot::Text(index)), delta) => {
                if let AssistantBlock::Text { text, .. } = &mut output.content[index] {
                    text.push_str(delta);
                }
            }
            _ => return,
        }
        emit(Event::Updated, output);
    }

    fn finalize(&mut self, response: &Value, output: &mut AssistantMessage) {
        self.terminal = true;
        // Some servers send the encrypted reasoning only with the finished response.
        for item in response["output"].as_array().into_iter().flatten() {
            let (Some("reasoning"), Some(encrypted), Some(id)) = (item["type"].as_str(), item["encrypted_content"].as_str(), item["id"].as_str()) else { continue };
            let Some(&index) = self.reasoning.get(id) else { continue };
            if let AssistantBlock::Thinking { thinking_signature: Some(signature), .. } = &mut output.content[index]
                && let Ok(mut stored) = serde_json::from_str::<Value>(signature)
                && stored.get("encrypted_content").is_none_or(Value::is_null)
            {
                stored["encrypted_content"] = json!(encrypted);
                *signature = stored.to_string();
            }
        }
        if let Some(id) = response["id"].as_str() {
            output.rest.insert("responseId".into(), json!(id));
        }
        if let Some(raw) = response.get("usage").filter(|usage| usage.is_object()) {
            let number = |pointer: &str| raw.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
            let (read, write) = (number("/input_tokens_details/cached_tokens"), number("/input_tokens_details/cache_write_tokens"));
            output.usage = usage(number("/input_tokens").saturating_sub(read + write), number("/output_tokens"), read, write, Some(number("/output_tokens_details/reasoning_tokens")));
        }
        let status = response["status"].as_str().unwrap_or_default();
        let reason = response.pointer("/incomplete_details/reason").and_then(Value::as_str);
        output.rest.insert("rawStopReason".into(), json!(reason.map_or_else(|| status.to_owned(), |reason| format!("{status}.{reason}"))));
        output.error_message = None;
        output.stop_reason = match (status, reason) {
            ("incomplete", Some("max_output_tokens")) => "length".into(),
            ("incomplete", reason) => {
                output.error_message = Some(reason.map_or_else(|| "Response incomplete without a provider reason".into(), |reason| format!("Response incomplete: {reason}")));
                "error".into()
            }
            ("failed" | "cancelled", _) => "error".into(),
            _ => "stop".into(),
        };
        if output.stop_reason == "stop" && output.content.iter().any(|block| matches!(block, AssistantBlock::ToolCall { .. })) {
            output.stop_reason = "toolUse".into();
        }
    }
}

impl Reader for ResponsesReader {
    fn event(&mut self, name: &str, data: &str, output: &mut AssistantMessage, emit: &mut dyn FnMut(Event, &AssistantMessage)) -> Result<(), String> {
        let Ok(event) = serde_json::from_str::<Value>(data) else { return Ok(()) };
        let kind = event["type"].as_str().unwrap_or(name);
        let at = event["output_index"].as_u64().unwrap_or(0);
        let delta = event["delta"].as_str().unwrap_or_default();
        match kind {
            "response.created" => {
                if let Some(id) = event.pointer("/response/id").and_then(Value::as_str) {
                    output.rest.insert("responseId".into(), json!(id));
                }
            }
            "response.output_item.added" => {
                self.slot(at, &event["item"], output, emit);
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let slot = self.slots.get(&at).copied().filter(|slot| matches!(slot, Slot::Thinking(_)));
                Self::append(output, slot, delta, emit);
            }
            "response.reasoning_summary_part.done" => {
                let slot = self.slots.get(&at).copied().filter(|slot| matches!(slot, Slot::Thinking(_)));
                Self::append(output, slot, "\n\n", emit);
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let slot = self.slots.get(&at).copied().filter(|slot| matches!(slot, Slot::Text(_)));
                Self::append(output, slot, delta, emit);
            }
            "response.function_call_arguments.delta" | "response.function_call_arguments.done" => {
                let Some(Slot::Call(index)) = self.slots.get(&at).copied() else { return Ok(()) };
                let partial = self.arguments.entry(index).or_default();
                if kind.ends_with(".done") {
                    *partial = event["arguments"].as_str().unwrap_or_default().to_owned();
                } else {
                    partial.push_str(delta);
                }
                if let AssistantBlock::ToolCall { arguments, .. } = &mut output.content[index] {
                    *arguments = preview_arguments(partial);
                }
                emit(Event::Updated, output);
            }
            "response.output_item.done" => {
                let item = &event["item"];
                if item["type"] == "message" && item["phase"] == "final_answer" {
                    output.stop_reason = "stop".into();
                }
                match self.slot(at, item, output, emit) {
                    Some(Slot::Thinking(index)) if item["type"] == "reasoning" => {
                        let join = |field: &str| item[field].as_array().map(|parts| parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n\n")).unwrap_or_default();
                        let (summary, content) = (join("summary"), join("content"));
                        if let AssistantBlock::Thinking { thinking, thinking_signature, .. } = &mut output.content[index] {
                            if !summary.is_empty() {
                                *thinking = summary;
                            } else if !content.is_empty() {
                                *thinking = content;
                            }
                            *thinking_signature = Some(item.to_string());
                        }
                        if let Some(id) = item["id"].as_str() {
                            self.reasoning.insert(id.to_owned(), index);
                        }
                        self.slots.remove(&at);
                        emit(Event::BlockEnded(index), output);
                    }
                    Some(Slot::Text(index)) if item["type"] == "message" => {
                        let text: String = item["content"].as_array().into_iter().flatten().filter_map(|part| part["text"].as_str().or_else(|| part["refusal"].as_str())).collect();
                        let mut signature = json!({ "v": 1, "id": item["id"].as_str().unwrap_or_default() });
                        if let Some(phase) = item["phase"].as_str() {
                            signature["phase"] = json!(phase);
                        }
                        if let AssistantBlock::Text { text: held, text_signature, .. } = &mut output.content[index] {
                            *held = text;
                            *text_signature = Some(signature.to_string());
                        }
                        self.slots.remove(&at);
                        emit(Event::BlockEnded(index), output);
                    }
                    Some(Slot::Call(index)) if item["type"] == "function_call" => {
                        let written = self.arguments.remove(&index).unwrap_or_default();
                        let text = item["arguments"].as_str().filter(|text| !text.is_empty()).map_or(written, str::to_owned);
                        if let AssistantBlock::ToolCall { arguments, .. } = &mut output.content[index] {
                            *arguments = parse_arguments(if text.is_empty() { "{}" } else { &text });
                        }
                        self.slots.remove(&at);
                        emit(Event::BlockEnded(index), output);
                    }
                    _ => {}
                }
            }
            "response.completed" | "response.incomplete" => self.finalize(&event["response"], output),
            "error" => {
                return Err(format!("Error Code {}: {}", event["code"].as_str().unwrap_or("unknown"), event["message"].as_str().unwrap_or("Unknown error")));
            }
            "response.failed" => {
                self.terminal = true;
                let response = &event["response"];
                return Err(if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
                    format!("{}: {}", error["code"].as_str().unwrap_or("unknown"), error["message"].as_str().unwrap_or("no message"))
                } else if let Some(reason) = response.pointer("/incomplete_details/reason").and_then(Value::as_str) {
                    format!("incomplete: {reason}")
                } else {
                    "Unknown error (no error details in response)".into()
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(&mut self, output: &mut AssistantMessage, _emit: &mut dyn FnMut(Event, &AssistantMessage)) -> Result<(), String> {
        if !self.terminal {
            return Err("OpenAI Responses stream ended before a terminal response event".into());
        }
        if output.stop_reason == "pending" {
            return Err("OpenAI Responses stream ended without a stop reason".into());
        }
        if output.stop_reason == "error" {
            return Err(output.error_message.clone().unwrap_or_else(|| "An unknown error occurred".into()));
        }
        // Every call in a reply is run; one whose item never finished may carry cut or mixed-up arguments.
        if output.stop_reason == "toolUse"
            && let Some(AssistantBlock::ToolCall { name, id, .. }) = self.arguments.keys().next().and_then(|index| output.content.get(*index))
        {
            return Err(format!("OpenAI Responses stream completed with an unfinished tool call: {name} ({id})"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{Api, LevelMap};

    fn model() -> Model {
        Model {
            api: Api::Responses,
            id: "gpt".into(),
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            reasoning: true,
            images: true,
            context_window: 400_000,
            max_tokens: 32_000,
            levels: LevelMap::EFFORTS,
            cost: Default::default(),
            claude: None,
            max_tokens_field: None,
            thinking_format: None,
        }
    }

    #[test]
    fn reads_a_stream_and_replays_it() {
        let model = model();
        let mut output = crate::agent::llm::empty_reply(&model);
        let mut reader = ResponsesReader::default();
        let mut emit = |_: Event, _: &AssistantMessage| {};
        for data in [
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1"}}"#,
            r#"{"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"Think"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"Think"}]}}"#,
            r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read","arguments":""}}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"path\":\"a\"}"}"#,
            r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read","arguments":"{\"path\":\"a\"}"}}"#,
            r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed","output":[{"type":"reasoning","id":"rs_1","encrypted_content":"enc"}],"usage":{"input_tokens":50,"output_tokens":10,"input_tokens_details":{"cached_tokens":20}}}}"#,
        ] {
            reader.event("", data, &mut output, &mut emit).unwrap();
        }
        reader.finish(&mut output, &mut emit).unwrap();
        assert_eq!(output.stop_reason, "toolUse");
        assert_eq!((output.usage.input, output.usage.cache_read), (30.0, 20.0));
        let messages = vec![Message::user("hi"), Message::Assistant(output)];
        let request = Request { system: "sys", messages: &messages, tools: &[], reasoning: Some(Level::High), max_tokens: None, session_id: Some("s"), unmark_last_user: false, no_cache: false, no_tool_calls: false };
        let (http, _) = super::request(&model, &request, Some("k"), "ua");
        let input = http.body["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "developer");
        assert_eq!(input[2]["encrypted_content"], "enc");
        assert_eq!(input[3], json!({ "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "read", "arguments": r#"{"path":"a"}"# }));
        assert_eq!(input[4]["output"], "No result provided");
        assert_eq!(http.body["reasoning"], json!({ "effort": "high", "summary": "auto" }));
    }
}
