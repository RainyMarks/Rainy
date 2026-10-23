//! Model requests (3.x `llm.ts` on pi-ai): the three wire protocols Rainy speaks — OpenAI Chat Completions, OpenAI
//! Responses and Anthropic Messages — streamed into one reply format, the pi-ai `AssistantMessage` chats store.
//!
//! One request runs at a time per local server and four per remote origin; failed connections and overloaded servers
//! are retried twice before the reply starts; a stream that sends nothing for five minutes fails.

pub mod anthropic;
pub mod catalog;
pub mod completions;
pub mod json;
pub mod responses;
mod sse;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use futures::StreamExt as _;
use rainy_protocol::chat::{AssistantBlock, AssistantMessage, ResultContent, Usage};
use regex::Regex;
use serde_json::{Map, Value, json};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::locale::l;
use crate::network::Network;
use crate::settings::ModelSetup;

/// No stream event for this long fails the request.
pub const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Upper bound for one model request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(1800);
/// Retries of a request that failed before its reply started.
const MAX_RETRIES: u32 = 2;
/// A server asking for a longer wait fails the request instead; the chat's own retry takes over.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(10);
/// Error bodies are cut to this many characters.
const MAX_ERROR_BODY_CHARS: usize = 4000;

/// The wire protocol of a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Api {
    Completions,
    Responses,
    Anthropic,
}

impl Api {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "openai-completions" => Self::Completions,
            "openai-responses" => Self::Responses,
            "anthropic-messages" => Self::Anthropic,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completions => "openai-completions",
            Self::Responses => "openai-responses",
            Self::Anthropic => "anthropic-messages",
        }
    }
}

/// Rainy's reasoning levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Off,
    Low,
    High,
    Max,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::Off, Level::Low, Level::High, Level::Max];

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "off" => Self::Off,
            "low" => Self::Low,
            "high" => Self::High,
            "max" => Self::Max,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::High => "high",
            Self::Max => "max",
        }
    }
}

/// How a model names each reasoning level on the wire (pi-ai `thinkingLevelMap`): a level it does not list keeps its
/// own name, one listed as `None` is not offered.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LevelMap(pub &'static [(&'static str, Option<&'static str>)]);

impl LevelMap {
    /// The OpenAI-style reasoning efforts of a model Rainy describes itself.
    pub const EFFORTS: LevelMap =
        LevelMap(&[("off", Some("none")), ("minimal", None), ("low", Some("low")), ("medium", None), ("high", Some("high")), ("xhigh", None), ("max", Some("max"))]);

    fn entry(&self, level: &str) -> Option<Option<&'static str>> {
        self.0.iter().find(|(name, _)| *name == level).map(|(_, value)| *value)
    }

    /// Whether the model offers a level.
    pub fn offers(&self, level: &str) -> bool {
        self.entry(level) != Some(None)
    }

    /// The level's wire name, or `None` when it is not offered.
    pub fn name<'a>(&self, level: &'a str) -> Option<&'a str> {
        match self.entry(level) {
            Some(Some(name)) => Some(name),
            Some(None) => None,
            None => Some(level),
        }
    }

    /// Whether `off` is a level of its own (it is unless the map refuses it).
    pub fn has_off(&self) -> bool {
        self.entry("off") != Some(None)
    }
}

/// Dollars per million tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// A model resolved for requests (pi-ai `Model`).
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub api: Api,
    pub id: String,
    pub provider: String,
    pub base_url: String,
    pub reasoning: bool,
    pub images: bool,
    pub context_window: u64,
    /// The output budget of a request.
    pub max_tokens: u64,
    pub levels: LevelMap,
    pub cost: Cost,
    /// Claude's request options, for a model of the Anthropic catalog.
    pub claude: Option<catalog::ClaudeOptions>,
    /// `max_tokens` or `max_completion_tokens` (Chat Completions).
    pub max_tokens_field: Option<String>,
    /// `openai`, `deepseek` or `qwen` (Chat Completions).
    pub thinking_format: Option<String>,
}

impl Model {
    /// The model a saved profile streams; its output budget comes from the profile.
    pub fn from_setup(setup: &ModelSetup, output_tokens: u64) -> Self {
        let api = setup.api.as_deref().and_then(Api::parse).unwrap_or(if setup.local { Api::Completions } else { Api::Responses });
        if api == Api::Anthropic
            && let Some(claude) = catalog::claude(&setup.model)
        {
            return Model {
                api,
                id: setup.model.clone(),
                provider: setup.provider.clone(),
                base_url: setup.base_url.clone(),
                reasoning: claude.reasoning,
                images: true,
                context_window: setup.context_window,
                max_tokens: output_tokens,
                levels: claude.levels.clone(),
                cost: claude.cost,
                claude: Some(claude.options),
                max_tokens_field: None,
                thinking_format: None,
            };
        }
        let reasoning = setup.thinking.is_some();
        Model {
            api,
            id: setup.model.clone(),
            provider: setup.provider.clone(),
            base_url: setup.base_url.clone(),
            reasoning,
            images: false,
            context_window: setup.context_window,
            max_tokens: output_tokens,
            levels: if reasoning { LevelMap::EFFORTS } else { LevelMap::default() },
            cost: Cost::default(),
            claude: None,
            max_tokens_field: (api == Api::Completions).then(|| setup.max_tokens_field.clone().unwrap_or_else(|| "max_tokens".into())),
            thinking_format: setup.thinking_format.clone(),
        }
    }

    /// Whether the model behind a local address, which serves one request at a time.
    pub fn local(&self) -> bool {
        url::Url::parse(&self.base_url).ok().and_then(|url| url.host_str().map(str::to_owned)).is_some_and(|host| {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            host == "localhost" || host == "::1" || host.starts_with("127.")
        })
    }
}

/// Text or image of a user message or tool result.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    Image { data: String, mime_type: String },
}

/// A user message: plain text, or parts (text with images).
#[derive(Clone, Debug, PartialEq)]
pub enum UserContent {
    Text(String),
    Parts(Vec<Part>),
}

/// A tool's answer to one call.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<ResultContent>,
    pub is_error: bool,
    pub details: Option<Value>,
    pub timestamp: f64,
}

/// One message of a conversation sent to a model.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    User { content: UserContent, timestamp: f64 },
    Assistant(AssistantMessage),
    ToolResult(ToolResult),
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Message::User { content: UserContent::Text(text.into()), timestamp: crate::text::now_ms() as f64 }
    }

    pub fn timestamp(&self) -> f64 {
        match self {
            Message::User { timestamp, .. } => *timestamp,
            Message::Assistant(message) => message.timestamp,
            Message::ToolResult(result) => result.timestamp,
        }
    }
}

/// A tool offered to a model: its name, what it does and its JSON-schema parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// How long the provider keeps the prompt cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cache {
    Short,
    Long,
}

/// One model request.
pub struct Request<'a> {
    pub system: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolSpec],
    /// The reasoning level; `None` and `Off` request no reasoning.
    pub reasoning: Option<Level>,
    /// Output cap below the model's budget.
    pub max_tokens: Option<u64>,
    /// Groups the requests of one chat for prompt caching.
    pub session_id: Option<&'a str>,
    /// For Anthropic: leave the conversation's last user message without a cache breakpoint (compaction).
    pub unmark_last_user: bool,
    /// Write no prompt cache: the request's prefix will not be sent again.
    pub no_cache: bool,
    /// The tools stay declared but the model may not call them.
    pub no_tool_calls: bool,
}

impl<'a> Request<'a> {
    /// A request with the defaults of a chat request.
    pub fn new(system: &'a str, messages: &'a [Message], tools: &'a [ToolSpec]) -> Self {
        Self { system, messages, tools, reasoning: None, max_tokens: None, session_id: None, unmark_last_user: false, no_cache: false, no_tool_calls: false }
    }
}

/// What changed in the reply being streamed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The reply started; it is empty.
    Started,
    /// A content block was added at this index.
    BlockStarted(usize),
    /// A block grew.
    Updated,
    /// The block at this index is complete.
    BlockEnded(usize),
}

/// A request ready to send.
pub(crate) struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

/// A protocol's reading of the reply stream.
pub(crate) trait Reader: Send {
    /// One server-sent event; errors end the reply.
    fn event(&mut self, name: &str, data: &str, output: &mut AssistantMessage, emit: &mut dyn FnMut(Event, &AssistantMessage)) -> Result<(), String>;
    /// The stream ended.
    fn finish(&mut self, output: &mut AssistantMessage, emit: &mut dyn FnMut(Event, &AssistantMessage)) -> Result<(), String>;
}

/// A reply with nothing in it yet.
pub fn empty_reply(model: &Model) -> AssistantMessage {
    let mut rest = Map::new();
    rest.insert("role".into(), json!("assistant"));
    AssistantMessage {
        content: Vec::new(),
        api: model.api.as_str().into(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: usage(0, 0, 0, 0, None),
        stop_reason: "pending".into(),
        error_message: None,
        timestamp: crate::text::now_ms() as f64,
        rest,
    }
}

/// Token counts in pi-ai's form, priced later by [`price`].
pub fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64, reasoning: Option<u64>) -> Usage {
    let mut rest = Map::new();
    rest.insert("totalTokens".into(), json!(input + output + cache_read + cache_write));
    rest.insert("cost".into(), json!({ "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }));
    Usage { input: input as f64, output: output as f64, cache_read: cache_read as f64, cache_write: cache_write as f64, reasoning: reasoning.map(|value| value as f64), rest }
}

/// Fill in the total and the cost of a reply's usage.
pub fn price(model: &Model, usage: &mut Usage) {
    let part = |rate: f64, tokens: f64| rate / 1_000_000.0 * tokens;
    let (input, output, read, write) = (part(model.cost.input, usage.input), part(model.cost.output, usage.output), part(model.cost.cache_read, usage.cache_read), part(model.cost.cache_write, usage.cache_write));
    usage.rest.insert("totalTokens".into(), json!(usage.input + usage.output + usage.cache_read + usage.cache_write));
    usage.rest.insert("cost".into(), json!({ "input": input, "output": output, "cacheRead": read, "cacheWrite": write, "total": input + output + read + write }));
}

/// The cost a provider's prices give a reply, when it has one.
pub fn cost_of(usage: &Usage) -> f64 {
    usage.rest.get("cost").and_then(|cost| cost.get("total")).and_then(Value::as_f64).unwrap_or(0.0)
}

/// Arguments the page shows of a tool call still being written: the paths, commands and queries named so far.
pub fn preview_arguments(partial: &str) -> Map<String, Value> {
    static PREVIEW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""(file_path|path|command|pattern|query|url|description)"\s*:\s*"((?:[^"\\]|\\.)*)""#).unwrap());
    let mut preview = Map::new();
    for captures in PREVIEW.captures_iter(crate::text::truncate_chars(partial, 4096)) {
        if preview.contains_key(&captures[1]) {
            continue;
        }
        if let Ok(Value::String(text)) = serde_json::from_str(&format!("\"{}\"", &captures[2])) {
            preview.insert(captures[1].to_owned(), Value::String(text));
        }
    }
    preview
}

/// The text of a reply.
pub fn reply_text(message: &AssistantMessage) -> String {
    message.content.iter().filter_map(|block| if let AssistantBlock::Text { text, .. } = block { Some(text.as_str()) } else { None }).collect()
}

static NON_RETRYABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)GoUsageLimitError|FreeUsageLimitError|Monthly usage limit reached|available balance|insufficient_quota|out of budget|quota exceeded|billing|subscription_sharing_usage_limit_exceeded").unwrap()
});
static RETRYABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)overloaded|currently experiencing high demand|rate.?limit|too many requests|429|500|502|503|504|520|524|service.?unavailable|server.?error|internal.?error|",
        r"provider.?returned.?error|exceeded request buffer limit while retrying upstream|network.?error|connection.?error|connection.?refused|connection.?lost|",
        r"other side closed|fetch failed|getaddrinfo|ENOTFOUND|EAI_AGAIN|upstream.?connect|reset before headers|socket hang up|socket connection was closed|",
        r"timed? out|timeout|terminated|websocket.?closed|websocket.?error|ended without|stream ended before message_stop|",
        r"stream ended before a terminal response event|http2 request did not get a response|retry delay|you can retry your request|try your request again|",
        r"please retry your request|ResourceExhausted|subscription_sharing_usage_unavailable|subscription_sharing_user_unavailable|",
        r"error sending request|connection closed|connection reset|broken pipe"
    ))
    .unwrap()
});

/// Whether a failed reply looks like a passing failure: overload, rate limits, dropped connections, timeouts.
pub fn retryable(message: &AssistantMessage) -> bool {
    let Some(text) = message.error_message.as_deref().filter(|_| message.stop_reason == "error") else { return false };
    !NON_RETRYABLE.is_match(text) && RETRYABLE.is_match(text)
}

static OVERFLOW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)prompt (?:is )?too long|prompt exceeds max length|request_too_large|input is too long for requested model|exceeds the context window|",
        r"exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))|input token count.*exceeds the maximum|maximum prompt length is \d+|",
        r"reduce the length of the messages|maximum context length is \d+ tokens|exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?|",
        r"input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)|exceeds the limit of \d+|exceeds the available context size|",
        r"greater than the context length|context window exceeds limit|exceeded model token limit|too large for model with \d+ maximum context length|",
        r"prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?|model_context_window_exceeded|prompt too long; exceeded (?:max )?context length|",
        r"range of input length should be|context[_ ]length[_ ]exceeded|too many tokens|token limit exceeded"
    ))
    .unwrap()
});
static NOT_OVERFLOW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(Throttling error|Service unavailable):|rate limit|too many requests").unwrap());

/// Whether a reply failed (or was cut) because the conversation no longer fits the model's window.
pub fn context_overflow(message: &AssistantMessage, context_window: u64) -> bool {
    if message.stop_reason == "error"
        && let Some(text) = message.error_message.as_deref()
        && !NOT_OVERFLOW.is_match(text)
        && OVERFLOW.is_match(text)
    {
        return true;
    }
    let input = message.usage.input + message.usage.cache_read;
    (message.stop_reason == "stop" && input > context_window as f64) || (message.stop_reason == "length" && message.usage.output == 0.0 && input >= context_window as f64 * 0.99)
}

/// Rough tokens of a conversation (pi-ai `estimateContextTokens`): what the provider counted for the last reply, plus
/// four characters a token for what came after it.
pub fn context_tokens(messages: &[Message]) -> u64 {
    let chars = |message: &Message| -> usize {
        match message {
            Message::User { content: UserContent::Text(text), .. } => text.chars().count(),
            Message::User { content: UserContent::Parts(parts), .. } => parts.iter().map(|part| if let Part::Text(text) = part { text.chars().count() } else { 4800 }).sum(),
            Message::ToolResult(result) => result.content.iter().map(|part| if let ResultContent::Text { text } = part { text.chars().count() } else { 4800 }).sum(),
            Message::Assistant(reply) => reply
                .content
                .iter()
                .map(|block| match block {
                    AssistantBlock::Text { text, .. } => text.chars().count(),
                    AssistantBlock::Thinking { thinking, .. } => thinking.chars().count(),
                    AssistantBlock::ToolCall { name, arguments, .. } => name.chars().count() + Value::Object(arguments.clone()).to_string().chars().count(),
                })
                .sum(),
        }
    };
    let counted = messages.iter().enumerate().rev().find_map(|(index, message)| match message {
        Message::Assistant(reply) if reply.stop_reason != "error" && reply.stop_reason != "aborted" => {
            let total = reply.usage.input + reply.usage.output + reply.usage.cache_read + reply.usage.cache_write;
            (total > 0.0).then_some((index, total as u64))
        }
        _ => None,
    });
    let (from, base) = counted.map_or((0, 0), |(index, total)| (index + 1, total));
    base + messages[from..].iter().map(|message| chars(message).div_ceil(4) as u64).sum::<u64>()
}

/// The output cap of a request: the asked cap within the model's budget, and within what the window leaves.
pub fn max_tokens(model: &Model, request: &Request<'_>) -> u64 {
    let used = context_tokens(request.messages) + (request.system.chars().count() as u64).div_ceil(4);
    match request.max_tokens {
        None => model.max_tokens.min(model.context_window.saturating_sub(used + 4096).max(1)),
        Some(cap) => cap.min(model.max_tokens).min(model.context_window.saturating_sub(used).max(1)),
    }
}

/// A failure before the reply started.
struct Failure {
    message: String,
    /// The server may answer next time.
    retry: bool,
    /// The wait the server asked for.
    delay: Option<Duration>,
}

/// Sends model requests.
pub struct Llm {
    network: Arc<Network>,
    lanes: Mutex<HashMap<String, Arc<Semaphore>>>,
    user_agent: String,
}

impl Llm {
    pub fn new(network: Arc<Network>, version: &str) -> Self {
        Self { network, lanes: Mutex::default(), user_agent: format!("Rainy/{version}") }
    }

    fn lane(&self, model: &Model) -> Arc<Semaphore> {
        let origin = url::Url::parse(&model.base_url).map(|url| url.origin().ascii_serialization()).unwrap_or_else(|_| model.base_url.clone());
        let mut lanes = self.lanes.lock().unwrap();
        lanes.entry(origin).or_insert_with(|| Arc::new(Semaphore::new(if model.local() { 1 } else { 4 }))).clone()
    }

    /// Stream one reply. `emit` sees the reply grow; the finished reply is returned, with failures recorded in its
    /// `stopReason` (`error` or `aborted`) and `errorMessage`, never as an `Err`.
    pub async fn stream(&self, model: &Model, request: &Request<'_>, api_key: Option<String>, cancel: &CancellationToken, emit: &mut (dyn FnMut(Event, &AssistantMessage) + Send)) -> AssistantMessage {
        let mut output = empty_reply(model);
        let result = tokio::select! {
            result = self.run(model, request, api_key, &mut output, emit) => result,
            () = cancel.cancelled() => Err(l("请求已停止。", "Request was aborted.")),
        };
        if let Err(message) = result {
            output.stop_reason = if cancel.is_cancelled() { "aborted".into() } else { "error".into() };
            output.error_message = Some(message);
        }
        price(model, &mut output.usage);
        output
    }

    async fn run(&self, model: &Model, request: &Request<'_>, api_key: Option<String>, output: &mut AssistantMessage, emit: &mut (dyn FnMut(Event, &AssistantMessage) + Send)) -> Result<(), String> {
        let lane = self.lane(model);
        let _permit = lane.acquire().await.map_err(|error| error.to_string())?;
        let (http, mut reader): (HttpRequest, Box<dyn Reader>) = match model.api {
            Api::Completions => completions::request(model, request, api_key.as_deref(), &self.user_agent),
            Api::Responses => responses::request(model, request, api_key.as_deref(), &self.user_agent),
            Api::Anthropic => anthropic::request(model, request, api_key.as_deref(), output),
        };
        let response = self.send(&http).await?;
        emit(Event::Started, output);
        let mut events = sse::Events::default();
        let mut body = response.bytes_stream();
        loop {
            let chunk = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, body.next()).await {
                Err(_) => {
                    let seconds = STREAM_IDLE_TIMEOUT.as_secs();
                    return Err(l(&format!("模型在 {seconds} 秒内没有返回数据，请求已超时。"), &format!("The model sent nothing for {seconds} seconds, so the request timed out.")));
                }
                Ok(None) => break,
                Ok(Some(Err(error))) => return Err(transport_error(&error)),
                Ok(Some(Ok(chunk))) => chunk,
            };
            for (name, data) in events.push(&chunk) {
                reader.event(&name, &data, output, emit)?;
            }
        }
        for (name, data) in events.finish() {
            reader.event(&name, &data, output, emit)?;
        }
        reader.finish(output, emit)
    }

    /// Send a request, retrying connection failures and overloaded servers before the reply starts.
    async fn send(&self, http: &HttpRequest) -> Result<reqwest::Response, String> {
        let url = url::Url::parse(&http.url).map_err(|error| error.to_string())?;
        let client = self.network.client(&url).await;
        let mut attempt = 0;
        loop {
            let failure = match self.attempt(&client, http).await {
                Ok(response) => return Ok(response),
                Err(failure) => failure,
            };
            if attempt >= MAX_RETRIES || !failure.retry {
                return Err(failure.message);
            }
            let delay = match failure.delay {
                Some(delay) if delay > MAX_RETRY_DELAY => {
                    return Err(format!("Server requested {}s retry delay (max: {}s). {}", delay.as_secs_f64().ceil(), MAX_RETRY_DELAY.as_secs(), failure.message));
                }
                Some(delay) => delay,
                None => {
                    let base = (0.5 * 2f64.powi(attempt as i32)).min(8.0);
                    Duration::from_secs_f64(base * (1.0 - jitter() * 0.25))
                }
            };
            attempt += 1;
            tokio::time::sleep(delay).await;
        }
    }

    async fn attempt(&self, client: &reqwest::Client, http: &HttpRequest) -> Result<reqwest::Response, Failure> {
        let mut builder = client.post(&http.url).timeout(REQUEST_TIMEOUT).header("user-agent", &self.user_agent).header("content-type", "application/json");
        for (name, value) in &http.headers {
            builder = builder.header(name, value);
        }
        let response = builder.body(http.body.to_string()).send().await.map_err(|error| Failure { message: transport_error(&error), retry: true, delay: None })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let headers = response.headers().clone();
        let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_owned);
        let delay = header("retry-after-ms").and_then(|value| value.parse::<f64>().ok()).map(|ms| Duration::from_secs_f64(ms.max(0.0) / 1000.0)).or_else(|| {
            header("retry-after").and_then(|value| value.parse::<f64>().ok()).map(|seconds| Duration::from_secs_f64(seconds.max(0.0)))
        });
        let retry = match header("x-should-retry").as_deref() {
            Some("true") => true,
            Some("false") => false,
            _ => matches!(status.as_u16(), 408 | 409 | 429) || status.is_server_error(),
        };
        let text = response.text().await.unwrap_or_default();
        Err(Failure { message: http_error(status.as_u16(), &text), retry, delay })
    }
}

fn jitter() -> f64 {
    (uuid::Uuid::new_v4().as_u128() % 1_000_000) as f64 / 1_000_000.0
}

/// `"<status> <what the server said>"`, the way the OpenAI and Anthropic SDKs word a failed request.
pub(crate) fn http_error(status: u16, body: &str) -> String {
    let body = body.trim();
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let detail = parsed
        .as_ref()
        .and_then(|value| {
            let error = value.get("error").unwrap_or(value);
            error.get("message").and_then(Value::as_str).map(str::to_owned).or_else(|| error.as_str().map(str::to_owned))
        })
        .map(|message| {
            let kind = parsed.as_ref().and_then(|value| value.get("error")).and_then(|error| error.get("type").or_else(|| error.get("code"))).and_then(Value::as_str);
            match kind {
                Some(kind) if !message.contains(kind) => format!("{message} ({kind})"),
                _ => message,
            }
        })
        .unwrap_or_else(|| if body.is_empty() { "status code (no body)".into() } else { body.to_owned() });
    let detail = if detail.chars().count() > MAX_ERROR_BODY_CHARS { format!("{}... [truncated]", crate::text::truncate_chars(&detail, MAX_ERROR_BODY_CHARS)) } else { detail };
    format!("{status} {detail}")
}

fn transport_error(error: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut message = if error.is_timeout() { "Request timed out".to_owned() } else { "Connection error".to_owned() };
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    message
}

/// Drop text blocks that hold only whitespace and thinking that says nothing, as replay does.
pub(crate) fn text_parts(content: &[ResultContent]) -> String {
    content.iter().filter_map(|part| if let ResultContent::Text { text } = part { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join("\n")
}

/// The conversation as a model replays it (pi-ai `transformMessages`): failed and stopped replies are left out, a tool
/// call left without an answer gets an empty failed one, reasoning from another model becomes text, and tool call ids
/// from another model are rewritten with `normalize` where the provider needs another form.
pub(crate) fn replay(messages: &[Message], model: &Model, normalize: &dyn Fn(&str, &AssistantMessage) -> String) -> Vec<Message> {
    let mut renamed: HashMap<String, String> = HashMap::new();
    let mut result: Vec<Message> = Vec::new();
    // Calls of the last reply, and the ones answered so far.
    let mut pending: Vec<(String, String)> = Vec::new();
    let mut answered: Vec<String> = Vec::new();
    let close = |result: &mut Vec<Message>, pending: &mut Vec<(String, String)>, answered: &mut Vec<String>| {
        for (id, name) in pending.drain(..) {
            if !answered.contains(&id) {
                result.push(Message::ToolResult(ToolResult {
                    tool_call_id: id,
                    tool_name: name,
                    content: vec![ResultContent::Text { text: "No result provided".into() }],
                    is_error: true,
                    details: None,
                    timestamp: crate::text::now_ms() as f64,
                }));
            }
        }
        answered.clear();
    };
    for message in messages {
        match message {
            Message::User { content, timestamp } => {
                close(&mut result, &mut pending, &mut answered);
                let content = match content {
                    UserContent::Parts(parts) if !model.images => UserContent::Parts(without_images(parts, "(image omitted: model does not support images)")),
                    other => other.clone(),
                };
                result.push(Message::User { content, timestamp: *timestamp });
            }
            Message::ToolResult(tool) => {
                let mut tool = tool.clone();
                if let Some(id) = renamed.get(&tool.tool_call_id) {
                    tool.tool_call_id = id.clone();
                }
                if !model.images {
                    let parts: Vec<Part> = tool.content.iter().map(result_part).collect();
                    tool.content = without_images(&parts, "(tool image omitted: model does not support images)").into_iter().map(part_result).collect();
                }
                answered.push(tool.tool_call_id.clone());
                result.push(Message::ToolResult(tool));
            }
            Message::Assistant(reply) => {
                close(&mut result, &mut pending, &mut answered);
                if reply.stop_reason == "error" || reply.stop_reason == "aborted" {
                    continue;
                }
                let same = reply.provider == model.provider && reply.api == model.api.as_str() && reply.model == model.id;
                let source = reply.clone();
                let mut reply = reply.clone();
                reply.content = source
                    .content
                    .iter()
                    .cloned()
                    .filter_map(|block| match block {
                        AssistantBlock::Thinking { redacted: Some(true), .. } => same.then_some(block),
                        AssistantBlock::Thinking { ref thinking_signature, .. } if same && thinking_signature.as_deref().is_some_and(|signature| !signature.is_empty()) => Some(block),
                        AssistantBlock::Thinking { thinking, .. } if thinking.trim().is_empty() => None,
                        AssistantBlock::Thinking { thinking, .. } if !same => Some(AssistantBlock::Text { text: thinking, text_signature: None, rest: Map::new() }),
                        AssistantBlock::Text { text, .. } if !same => Some(AssistantBlock::Text { text, text_signature: None, rest: Map::new() }),
                        AssistantBlock::ToolCall { id, name, arguments, mut rest } if !same => {
                            rest.remove("thoughtSignature");
                            let normalized = normalize(&id, &source);
                            if normalized != id {
                                renamed.insert(id, normalized.clone());
                            }
                            Some(AssistantBlock::ToolCall { id: normalized, name, arguments, rest })
                        }
                        other => Some(other),
                    })
                    .collect();
                pending = reply.content.iter().filter_map(|block| if let AssistantBlock::ToolCall { id, name, .. } = block { Some((id.clone(), name.clone())) } else { None }).collect();
                result.push(Message::Assistant(reply));
            }
        }
    }
    close(&mut result, &mut pending, &mut answered);
    result
}

fn result_part(content: &ResultContent) -> Part {
    match content {
        ResultContent::Text { text } => Part::Text(text.clone()),
        ResultContent::Image { data, mime_type } => Part::Image { data: data.clone(), mime_type: mime_type.clone() },
    }
}

fn part_result(part: Part) -> ResultContent {
    match part {
        Part::Text(text) => ResultContent::Text { text },
        Part::Image { data, mime_type } => ResultContent::Image { data, mime_type },
    }
}

/// Images replaced by one placeholder per run of them.
fn without_images(parts: &[Part], placeholder: &str) -> Vec<Part> {
    let mut result = Vec::new();
    let mut previous_placeholder = false;
    for part in parts {
        match part {
            Part::Image { .. } => {
                if !previous_placeholder {
                    result.push(Part::Text(placeholder.into()));
                }
                previous_placeholder = true;
            }
            Part::Text(text) => {
                previous_placeholder = text == placeholder;
                result.push(part.clone());
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_http_errors_like_the_sdks() {
        assert_eq!(http_error(429, r#"{"error":{"message":"Rate limit reached","type":"rate_limit_error"}}"#), "429 Rate limit reached (rate_limit_error)");
        assert_eq!(http_error(400, ""), "400 status code (no body)");
        assert_eq!(http_error(502, "<html>bad gateway</html>"), "502 <html>bad gateway</html>");
    }

    #[test]
    fn previews_tool_arguments() {
        let preview = preview_arguments(r#"{"path": "src/a.rs", "content": "fn main() {\n"#);
        assert_eq!(preview.get("path"), Some(&json!("src/a.rs")));
        assert!(!preview.contains_key("content"));
    }
}
