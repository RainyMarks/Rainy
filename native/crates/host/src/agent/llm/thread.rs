//! Claude message threads (Anthropic's `message-threads-2026-08-12` beta): requests to Anthropic's own API send a
//! conversation once and then only what was added after the previous reply, which Anthropic keeps for five minutes
//! after each request. Token use and prices stay the same; the requests get smaller and the first token comes sooner
//! in long conversations.
//!
//! The chat's transcript stays the record. A thread is started again from it whenever the previous one may be gone or
//! no longer matches the conversation: it expired, the conversation was compacted, cleared or edited, the model, tools
//! or reasoning changed, a request failed or was stopped. A conversation Anthropic refuses a thread for goes on with
//! whole requests.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher as _;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use super::{Api, HttpRequest, Model};

pub(crate) const BETA: &str = "message-threads-2026-08-12";
/// Anthropic keeps a thread five minutes after its last request; a thread older than this is started again rather
/// than continued, which would likely fail.
const KEEP: Duration = Duration::from_secs(270);
/// The fields a thread fixes at its start: a continuation must send the same, or leaves out (system, tools).
const FIXED: [&str; 9] = ["model", "tool_choice", "thinking", "output_config", "context_management", "mcp_servers", "cache_control", "system", "tools"];

/// The thread one conversation continues: a chat's, or a sub-agent's.
#[derive(Default)]
pub struct Slot(Mutex<State>);

#[derive(Default)]
struct State {
    thread: Option<Thread>,
    /// Anthropic refused a thread for this conversation; its requests go out whole.
    off: bool,
}

struct Thread {
    /// The reply the next request continues from.
    id: String,
    /// The fixed fields it was started with.
    key: u64,
    /// The messages of the request that got the reply, as hashes; the reply follows them on the server.
    sent: Vec<u64>,
    stored: Instant,
}

/// A request planned on a thread.
pub(crate) struct Plan {
    /// The request that continues the thread, when the conversation still extends it.
    pub continued: Option<HttpRequest>,
    /// The request that starts a thread with the whole conversation.
    pub created: HttpRequest,
    key: u64,
    sent: Vec<u64>,
}

/// Whether requests to the model may use threads: Anthropic's own API only, unless `RAINY_CLAUDE_THREADS=0`.
pub(crate) fn applies(model: &Model) -> bool {
    model.api == Api::Anthropic
        && url::Url::parse(&model.base_url).ok().is_some_and(|url| url.host_str() == Some("api.anthropic.com"))
        && std::env::var("RAINY_CLAUDE_THREADS").as_deref() != Ok("0")
}

fn hash(value: &impl std::hash::Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Remove every breakpoint from the messages: a thread keeps them in place, so one on the last message would stay
/// there; the top-level `cache_control` marks each request's last block instead.
fn strip_breakpoints(messages: &mut [Value]) {
    for message in messages {
        if let Some(blocks) = message["content"].as_array_mut() {
            for block in blocks.iter_mut().filter_map(Value::as_object_mut) {
                block.remove("cache_control");
            }
        }
    }
}

/// The thread requests for a whole request `http`; `None` when the conversation goes out whole. The thread is taken
/// from the slot until [`stored`] gives the next one back, so a failed or stopped request starts a new thread.
pub(crate) fn plan(slot: &Slot, http: &HttpRequest) -> Option<Plan> {
    let previous = {
        let mut state = slot.0.lock().unwrap();
        if state.off {
            return None;
        }
        state.thread.take()
    };
    let mut body: Map<String, Value> = http.body.as_object()?.clone();
    let mut messages = body.remove("messages")?.as_array()?.clone();
    strip_breakpoints(&mut messages);
    // System prompt and last tool keep their breakpoints; with this one a request carries three of the four allowed.
    body.insert("cache_control".into(), json!({ "type": "ephemeral", "ttl": "1h" }));
    let mut headers = http.headers.clone();
    match headers.iter_mut().find(|(name, _)| name == "anthropic-beta") {
        Some((_, value)) => *value = format!("{value},{BETA}"),
        None => headers.push(("anthropic-beta".into(), BETA.into())),
    }
    let mut betas: Vec<&str> = headers.iter().filter(|(name, _)| name == "anthropic-beta").flat_map(|(_, value)| value.split(',')).collect();
    betas.sort_unstable();
    let fixed: Vec<String> = FIXED.iter().map(|field| body.get(*field).map(Value::to_string).unwrap_or_default()).collect();
    let key = hash(&(fixed, betas));
    let sent: Vec<u64> = messages.iter().map(|message| hash(&message.to_string())).collect();
    let continued = previous.filter(|thread| {
        let held = thread.sent.len();
        thread.key == key
            && thread.stored.elapsed() < KEEP
            && sent.len() > held + 1
            && sent[..held] == thread.sent[..]
            && messages[held]["role"] == "assistant"
            && messages[held + 1]["role"] == "user"
    });
    let continued = continued.map(|thread| {
        let mut body = body.clone();
        body.remove("system");
        body.remove("tools");
        body.insert("messages".into(), Value::Array(messages[thread.sent.len() + 1..].to_vec()));
        body.insert("thread".into(), json!({ "type": "continue", "previous_message_id": thread.id }));
        HttpRequest { url: http.url.clone(), headers: headers.clone(), body: Value::Object(body) }
    });
    body.insert("messages".into(), Value::Array(messages));
    body.insert("thread".into(), json!({ "type": "create" }));
    Some(Plan { continued, created: HttpRequest { url: http.url.clone(), headers, body: Value::Object(body) }, key, sent })
}

/// Anthropic stored the reply `id` to a request of `plan`: the next request continues from it.
pub(crate) fn stored(slot: &Slot, plan: Plan, id: &str) {
    let mut state = slot.0.lock().unwrap();
    if !state.off {
        state.thread = Some(Thread { id: id.to_owned(), key: plan.key, sent: plan.sent, stored: Instant::now() });
    }
}

/// Anthropic refused a thread for the conversation: its requests go out whole from now on.
pub(crate) fn give_up(slot: &Slot) {
    let mut state = slot.0.lock().unwrap();
    state.off = true;
    state.thread = None;
}

/// What a failed thread request means.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The thread can't be continued: start it again with the whole conversation.
    Restart,
    /// Threads don't work for this conversation: send it whole.
    Unthreaded,
    /// Not about the thread.
    Other,
}

/// How to go on after a thread request failed with `status` and `body` before its reply started.
pub(crate) fn refusal(continuing: bool, status: u16, body: &str) -> Refusal {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = parsed.pointer("/error/details/error_code").and_then(Value::as_str).unwrap_or_default();
    match (continuing, status) {
        // A continuation the server can't take, for whatever reason, is safe to start again whole.
        (true, 400) => Refusal::Restart,
        (true, 404) if code == "thread_not_found" => Refusal::Restart,
        // Starting a thread failed where the same request without one may not: an organization without threads, a
        // conversation too large to keep, or a field threads don't take.
        (false, 400) => Refusal::Unthreaded,
        (false, 404) if code == "thread_not_found" => Refusal::Unthreaded,
        _ => Refusal::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(messages: Value) -> HttpRequest {
        let body = json!({
            "model": "claude-haiku-5-5",
            "max_tokens": 1000,
            "stream": true,
            "system": [{ "type": "text", "text": "sys", "cache_control": { "type": "ephemeral", "ttl": "1h" } }],
            "tools": [{ "name": "read", "input_schema": { "type": "object" }, "cache_control": { "type": "ephemeral", "ttl": "1h" } }],
            "thinking": { "type": "adaptive" },
            "messages": messages,
        });
        HttpRequest { url: "https://api.anthropic.com/v1/messages".into(), headers: vec![("anthropic-beta".into(), "a".into())], body }
    }

    fn user(text: &str, marked: bool) -> Value {
        let mut block = json!({ "type": "text", "text": text });
        if marked {
            block["cache_control"] = json!({ "type": "ephemeral", "ttl": "1h" });
        }
        json!({ "role": "user", "content": [block] })
    }

    fn reply(text: &str) -> Value {
        json!({ "role": "assistant", "content": [{ "type": "text", "text": text }] })
    }

    #[test]
    fn continues_with_the_messages_after_the_reply() {
        let slot = Slot::default();
        let first = plan(&slot, &whole(json!([user("one", true)]))).unwrap();
        assert!(first.continued.is_none());
        let body = &first.created.body;
        assert_eq!(body["thread"], json!({ "type": "create" }));
        assert!(body["messages"][0]["content"][0].get("cache_control").is_none());
        assert_eq!(body["cache_control"]["type"], "ephemeral");
        assert!(body["system"][0].get("cache_control").is_some());
        assert_eq!(first.created.headers, [("anthropic-beta".to_owned(), format!("a,{BETA}"))]);
        stored(&slot, first, "msg_1");
        // The next request holds the same messages, the reply, and new ones; breakpoints moved with the end.
        let next = plan(&slot, &whole(json!([user("one", false), reply("r1"), user("two", true)]))).unwrap();
        let continued = next.continued.as_ref().unwrap();
        assert_eq!(continued.body["thread"], json!({ "type": "continue", "previous_message_id": "msg_1" }));
        assert_eq!(continued.body["messages"], json!([user("two", false)]));
        assert!(continued.body.get("system").is_none() && continued.body.get("tools").is_none());
        assert_eq!(continued.body["max_tokens"], 1000);
        stored(&slot, next, "msg_2");
        let third = plan(&slot, &whole(json!([user("one", false), reply("r1"), user("two", false), reply("r2"), user("three", true)]))).unwrap();
        assert_eq!(third.continued.unwrap().body["messages"], json!([user("three", false)]));
    }

    #[test]
    fn starts_again_when_the_conversation_no_longer_extends_the_thread() {
        let slot = Slot::default();
        let started = |slot: &Slot| {
            let first = plan(slot, &whole(json!([user("one", true)]))).unwrap();
            stored(slot, first, "msg_1");
        };
        // Compacted or edited history.
        started(&slot);
        assert!(plan(&slot, &whole(json!([user("summary", false), reply("r1"), user("two", true)]))).unwrap().continued.is_none());
        // Another model or reasoning setting.
        started(&slot);
        let mut other = whole(json!([user("one", false), reply("r1"), user("two", true)]));
        other.body["thinking"] = json!({ "type": "disabled" });
        assert!(plan(&slot, &other).unwrap().continued.is_none());
        // A request that failed or was stopped took the thread with it.
        started(&slot);
        let lost = plan(&slot, &whole(json!([user("one", false), reply("r1"), user("two", true)]))).unwrap();
        assert!(lost.continued.is_some());
        assert!(plan(&slot, &whole(json!([user("one", false), reply("r1"), user("two", true)]))).unwrap().continued.is_none());
        // An expired thread.
        started(&slot);
        slot.0.lock().unwrap().thread.as_mut().unwrap().stored -= KEEP;
        assert!(plan(&slot, &whole(json!([user("one", false), reply("r1"), user("two", true)]))).unwrap().continued.is_none());
        // A refused thread is given up for the conversation.
        give_up(&slot);
        assert!(plan(&slot, &whole(json!([user("one", true)]))).is_none());
    }

    #[test]
    fn reads_what_a_failed_request_means() {
        let not_found = r#"{"type":"error","error":{"type":"not_found_error","message":"x","details":{"error_code":"thread_not_found"}}}"#;
        assert_eq!(refusal(true, 404, not_found), Refusal::Restart);
        assert_eq!(refusal(true, 400, r#"{"error":{"details":{"error_code":"thread_fingerprint_mismatch"}}}"#), Refusal::Restart);
        assert_eq!(refusal(true, 404, r#"{"error":{"type":"not_found_error","message":"model: x"}}"#), Refusal::Other);
        assert_eq!(refusal(false, 400, r#"{"error":{"details":{"error_code":"thread_unsupported_request"}}}"#), Refusal::Unthreaded);
        assert_eq!(refusal(false, 404, not_found), Refusal::Unthreaded);
        assert_eq!(refusal(true, 429, ""), Refusal::Other);
        assert_eq!(refusal(false, 500, ""), Refusal::Other);
    }

    #[test]
    fn applies_to_anthropic_only() {
        use crate::agent::llm::test_support::setup;
        let model = |api: Api, base: &str| Model::from_setup(&setup(api, base, "claude-haiku-5-5"), 1000);
        assert!(applies(&model(Api::Anthropic, "https://api.anthropic.com")));
        assert!(applies(&model(Api::Anthropic, "https://api.anthropic.com/v1")));
        assert!(!applies(&model(Api::Anthropic, "https://relay.example.com")));
        assert!(!applies(&model(Api::Completions, "https://api.anthropic.com")));
    }
}
