//! One run of a `task` sub-agent (3.x `subagent.ts`): its own transcript, read-only tools and limits. The chat keeps
//! only the report, so a long search costs the conversation one tool result instead of every file read.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rainy_protocol::chat::{AssistantMessage, UsageSummary};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::compaction::usage_of;
use super::llm::{self, Llm, Message, reply_text};
use super::models::Resolved;
use super::run::{self, Blocked, Event, Hooks, State};
use super::tools::Tool;
use super::tools::task::{Progress, Report};

/// Assistant turns one sub-agent may take before it must answer.
pub const MAX_STEPS: usize = 24;
/// How long one sub-agent may run.
pub const MAX_TIME: Duration = Duration::from_secs(5 * 60);
/// Tool calls kept in the progress list shown on the card.
const ACTIVITY_KEPT: usize = 12;

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
}

#[derive(Default)]
struct Track {
    activity: Vec<String>,
    steps: usize,
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
}

impl SubHooks {
    fn report(&self, track: &Track, text: &str) {
        (self.progress)(Progress { activity: track.activity.clone(), steps: track.steps, text: text.to_owned() });
    }
}

impl Hooks for SubHooks {
    type Meta = ();

    fn steering(&self) -> Vec<(Message, ())> {
        Vec::new()
    }

    async fn event(&self, event: Event<'_, ()>) {
        let mut track = self.track.lock().unwrap();
        match event {
            Event::ToolStarted { name, .. } => {
                track.activity.push(name.to_owned());
                if track.activity.len() > ACTIVITY_KEPT {
                    track.activity.remove(0);
                }
                let text = if track.text.is_empty() { track.partial.clone() } else { track.text.clone() };
                self.report(&track, &text);
            }
            Event::Reply(message) => {
                track.steps += 1;
                let said = reply_text(message);
                if !said.trim().is_empty() {
                    track.text = said;
                }
                (self.usage)(usage_of(message));
                if track.steps >= MAX_STEPS {
                    track.stopped = Some("steps");
                    self.limit.cancel();
                }
                let text = track.text.clone();
                self.report(&track, &text);
            }
            _ => {}
        }
    }

    fn streaming(&self, _event: llm::Event, reply: &AssistantMessage) {
        let mut track = self.track.lock().unwrap();
        track.partial = reply_text(reply);
        let text = track.partial.clone();
        self.report(&track, &text);
    }

    async fn before_tool(&self, _id: &str, _name: &str, _args: &Value, _cancel: &CancellationToken) -> Option<Blocked> {
        self.limit.is_cancelled().then(|| Blocked { reason: "The sub-agent was stopped.".into(), terminate: true })
    }

    async fn prepare_request(&self, _state: &mut State, _cancel: &CancellationToken) {}
}

/// Run one sub-agent to completion; `cancel` is the parent tool call's.
pub async fn run_subagent(llm: &Llm, input: Input, cancel: CancellationToken) -> Result<Report, String> {
    let limit = cancel.child_token();
    let hooks = SubHooks { track: Mutex::default(), progress: input.progress, usage: input.usage, limit: limit.clone() };
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
    Ok(Report { text: track.text, steps: track.steps, activity: track.activity, stopped: track.stopped })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_names_the_platform() {
        assert!(system_prompt("/w", false).ends_with("Working directory: /w (Linux)."));
    }
}
