//! One run of a `task` sub-agent (3.x `subagent.ts`): its own transcript, read-only tools and limits. The chat keeps
//! only the report, so a long search costs the conversation one tool result instead of every file read.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rainy_protocol::chat::{AssistantMessage, UsageSummary};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::compaction::usage_of;
use super::llm::{self, Llm, Message, reply_text};
use super::models::Resolved;
use super::run::{self, Blocked, Event, Hooks, State};
use super::swarm::fresh_tokens;
use super::tools::Tool;
use super::tools::task::{Progress, Report};

/// Assistant turns one sub-agent may take before it must answer.
pub const MAX_STEPS: usize = 24;
/// How long one sub-agent may run.
pub const MAX_TIME: Duration = Duration::from_secs(5 * 60);
/// Fresh tokens (uncached input, output, cache writes) a single sub-agent may consume before circuit-breaking. Cache reads
/// are left out: every step re-reads the whole context from the cache, so counting them would stop ordinary sub-agents
/// long before MAX_STEPS. This only catches runaway reads (24 steps without caching over a ~60k context stay below it).
pub const MAX_TOKENS: u64 = 1_500_000;
/// Tool calls kept in the progress list shown on the card.
const ACTIVITY_KEPT: usize = 12;
/// Characters of one argument value kept for the card.
const ARGUMENT_CHARS: usize = 300;

/// The arguments of a sub-agent's tool call as the card shows them: text (cut short), numbers and switches.
fn shown_arguments(args: &Map<String, Value>) -> Value {
    let kept: Map<String, Value> = args
        .iter()
        .filter_map(|(key, value)| match value {
            Value::String(text) => Some((key.clone(), Value::String(text.chars().take(ARGUMENT_CHARS).collect()))),
            Value::Number(_) | Value::Bool(_) => Some((key.clone(), value.clone())),
            _ => None,
        })
        .collect();
    Value::Object(kept)
}

/// The sub-agent's system prompt; it never sees the conversation, so it is told to answer in full.
pub fn system_prompt(cwd: &str, windows: bool) -> String {
    format!(
        "You are a sub-agent of Rainy, a coding assistant. You are given one self-contained task by the main assistant, which cannot see your work. \
         Investigate with the read, grep and glob tools, and web search when you have it. You cannot change files, run commands or ask questions. \
         Search before reading: grep and glob narrow the work, and read targeted line ranges rather than whole files. \
         Answer with one report that stands on its own: what you found, the file paths and line numbers that matter, and what you could not determine. \
         Quote only the lines that carry the answer. Do not describe your process or ask for confirmation.\
         \n\nWorking directory: {cwd} ({}).",
        if windows { "Windows" } else { "Linux" }
    )
}

/// What one sub-agent run needs.
pub struct Input {
    pub prompt: String,
    pub resolved: Resolved,
    pub api_key: Option<String>,
    pub tools: Vec<Arc<dyn Tool>>,
    pub cwd: String,
    pub windows: bool,
    pub progress: Arc<dyn Fn(Progress) + Send + Sync>,
    /// Counts the sub-agent's model usage into the chat's run.
    pub usage: Arc<dyn Fn(UsageSummary) + Send + Sync>,
    pub max_time: Duration,
    /// The chat's permission check for each tool call: `Some(reason)` refuses it. Sub-agents cannot ask the user.
    pub guard: Guard,
}

/// Refuses a sub-agent's tool call with a reason for the model, or lets it run.
pub type Guard = Arc<dyn Fn(&str, &Value) -> Option<String> + Send + Sync>;

#[derive(Default)]
struct Track {
    /// Its latest calls: `{ "id", "name", "args", "state" }`.
    activity: Vec<Value>,
    calls: usize,
    steps: usize,
    tokens: u64,
    text: String,
    partial: String,
    stopped: Option<&'static str>,
}

struct SubHooks {
    track: Mutex<Track>,
    progress: Arc<dyn Fn(Progress) + Send + Sync>,
    usage: Arc<dyn Fn(UsageSummary) + Send + Sync>,
    /// Ends the run at a limit or when the chat stops it; it also ends a model request in flight.
    limit: CancellationToken,
    guard: Guard,
}

impl Hooks for SubHooks {
    type Meta = ();

    fn steering(&self) -> Vec<(Message, ())> {
        Vec::new()
    }

    async fn event(&self, event: Event<'_, ()>) {
        // Release `track` lock before invoking external callbacks (`progress` / `usage`) to avoid lock inversion.
        let mut emit_progress: Option<Progress> = None;
        let mut emit_usage: Option<UsageSummary> = None;
        {
            let mut track = self.track.lock().unwrap();
            match event {
                Event::ToolStarted { id, name, args } => {
                    track.activity.push(json!({ "id": id, "name": name, "args": shown_arguments(args), "state": "running" }));
                    track.calls += 1;
                    if track.activity.len() > ACTIVITY_KEPT {
                        track.activity.remove(0);
                    }
                    let text = if track.text.is_empty() { track.partial.clone() } else { track.text.clone() };
                    emit_progress = Some(Progress { activity: track.activity.clone(), calls: track.calls, steps: track.steps, text });
                }
                Event::Result(result) => {
                    if let Some(call) = track.activity.iter_mut().find(|call| call["id"] == result.tool_call_id.as_str()) {
                        call["state"] = json!(if result.is_error { "failed" } else { "done" });
                        let text = if track.text.is_empty() { track.partial.clone() } else { track.text.clone() };
                        emit_progress = Some(Progress { activity: track.activity.clone(), calls: track.calls, steps: track.steps, text });
                    }
                }
                Event::Reply(message) => {
                    track.steps += 1;
                    let said = reply_text(message);
                    if !said.trim().is_empty() {
                        track.text = said;
                    }
                    let used = usage_of(message);
                    track.tokens = track.tokens.saturating_add(fresh_tokens(&used));
                    emit_usage = Some(used);
                    if track.steps >= MAX_STEPS {
                        track.stopped = Some("steps");
                        self.limit.cancel();
                    } else if track.tokens >= MAX_TOKENS {
                        track.stopped = Some("budget");
                        self.limit.cancel();
                    }
                    emit_progress = Some(Progress { activity: track.activity.clone(), calls: track.calls, steps: track.steps, text: track.text.clone() });
                }
                _ => {}
            }
        }
        if let Some(usage) = emit_usage {
            (self.usage)(usage);
        }
        if let Some(progress) = emit_progress {
            (self.progress)(progress);
        }
    }

    fn streaming(&self, _event: llm::Event, reply: &AssistantMessage) {
        let progress = {
            let mut track = self.track.lock().unwrap();
            track.partial = reply_text(reply);
            Progress { activity: track.activity.clone(), calls: track.calls, steps: track.steps, text: track.partial.clone() }
        };
        (self.progress)(progress);
    }

    async fn before_tool(&self, _id: &str, name: &str, args: &Value, _cancel: &CancellationToken) -> Option<Blocked> {
        if self.limit.is_cancelled() {
            return Some(Blocked { reason: "The sub-agent was stopped.".into(), terminate: true });
        }
        (self.guard)(name, args).map(|reason| Blocked { reason, terminate: false })
    }

    async fn prepare_request(&self, _state: &mut State, _cancel: &CancellationToken) {}
}

/// Run one sub-agent to completion; `cancel` is the parent tool call's.
pub async fn run_subagent(llm: &Llm, input: Input, cancel: CancellationToken) -> Result<Report, String> {
    let limit = cancel.child_token();
    let hooks = SubHooks { track: Mutex::default(), progress: input.progress, usage: input.usage, limit: limit.clone(), guard: input.guard };
    let timed_out = Arc::new(AtomicBool::new(false));
    let timer = tokio::spawn({
        let (limit, timed_out, max) = (limit.clone(), timed_out.clone(), input.max_time);
        async move {
            tokio::time::sleep(max).await;
            timed_out.store(true, Ordering::SeqCst);
            limit.cancel();
        }
    });
    let mut state = State {
        model: input.resolved.model.clone(),
        reasoning: input.resolved.thinking,
        system: system_prompt(&input.cwd, input.windows),
        tools: input.tools,
        messages: Vec::new(),
        session_id: None,
        api_key: input.api_key,
        thread: Some(Arc::default()),
    };
    run::run(llm, &mut state, vec![(Message::user(input.prompt), ())], &hooks, &limit).await;
    timer.abort();
    if cancel.is_cancelled() {
        return Err("tool call aborted".into());
    }
    let mut track = hooks.track.into_inner().unwrap();
    if timed_out.load(Ordering::SeqCst) && track.stopped.is_none() {
        track.stopped = Some("time");
    }
    // A call cut short by a limit or the stop never got its result.
    for call in &mut track.activity {
        if call["state"] == "running" {
            call["state"] = json!("failed");
        }
    }
    Ok(Report { text: track.text, steps: track.steps, activity: track.activity, calls: track.calls, stopped: track.stopped })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tool_calls_pass_the_chats_permission_guard() {
        let guard: Guard = Arc::new(|name: &str, args: &Value| {
            let path = args.get("file_path").and_then(Value::as_str).unwrap_or_default();
            (name == "read" && path.starts_with("/home/user/.ssh")).then(|| "outside the project".to_owned())
        });
        let hooks = SubHooks { track: Mutex::default(), progress: Arc::new(|_| {}), usage: Arc::new(|_| {}), limit: CancellationToken::new(), guard };
        let cancel = CancellationToken::new();
        let refused = hooks.before_tool("1", "read", &serde_json::json!({ "file_path": "/home/user/.ssh/id_ed25519" }), &cancel).await.expect("refused");
        assert_eq!(refused.reason, "outside the project");
        assert!(!refused.terminate);
        assert!(hooks.before_tool("2", "read", &serde_json::json!({ "file_path": "src/main.rs" }), &cancel).await.is_none());
        hooks.limit.cancel();
        assert!(hooks.before_tool("3", "read", &serde_json::json!({ "file_path": "src/main.rs" }), &cancel).await.unwrap().terminate);
    }

    #[tokio::test]
    async fn progress_names_each_step_and_how_it_went() {
        let seen: Arc<Mutex<Vec<Progress>>> = Arc::default();
        let progress: Arc<dyn Fn(Progress) + Send + Sync> = {
            let seen = seen.clone();
            Arc::new(move |progress| seen.lock().unwrap().push(progress))
        };
        let hooks = SubHooks { track: Mutex::default(), progress, usage: Arc::new(|_| {}), limit: CancellationToken::new(), guard: Arc::new(|_: &str, _: &Value| None) };
        let last = || seen.lock().unwrap().last().cloned().expect("progress");
        let args = json!({ "pattern": "x".repeat(ARGUMENT_CHARS + 50), "path": "src", "head_limit": 5, "globs": ["*.rs"] });
        hooks.event(Event::ToolStarted { id: "1", name: "grep", args: args.as_object().unwrap() }).await;
        let step = &last().activity[0];
        assert_eq!((step["name"].as_str(), step["state"].as_str()), (Some("grep"), Some("running")));
        // Text is cut short, numbers kept, lists left out.
        assert_eq!(step["args"]["pattern"].as_str().unwrap().chars().count(), ARGUMENT_CHARS);
        assert_eq!((step["args"]["path"].as_str(), step["args"]["head_limit"].as_i64()), (Some("src"), Some(5)));
        assert!(step["args"].get("globs").is_none());
        let failed = llm::ToolResult { tool_call_id: "1".into(), tool_name: "grep".into(), content: Vec::new(), is_error: true, details: None, timestamp: 0. };
        hooks.event(Event::Result(&failed)).await;
        assert_eq!(last().activity[0]["state"], "failed");
        // The card keeps the latest calls and counts them all.
        for n in 2..=ACTIVITY_KEPT + 3 {
            let id = n.to_string();
            hooks.event(Event::ToolStarted { id: &id, name: "read", args: json!({ "file_path": "a.rs" }).as_object().unwrap() }).await;
        }
        let progress = last();
        assert_eq!((progress.activity.len(), progress.calls), (ACTIVITY_KEPT, ACTIVITY_KEPT + 3));
        assert_eq!(progress.activity[0]["id"], "4");
    }

    #[test]
    fn prompt_names_the_platform() {
        assert!(system_prompt("/w", false).ends_with("Working directory: /w (Linux)."));
    }
}
