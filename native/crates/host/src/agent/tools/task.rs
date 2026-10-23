//! `task`: a sub-agent that investigates on its own and reports back in one answer. It keeps a long search out of the
//! chat's own context; it only reads, so no call of its own ever waits for the user.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolContext, ToolOutput, Updates, params, spec};
use crate::agent::llm::ToolSpec;

/// Sub-agents of one chat that may run at the same time; the model is told to stay within this.
pub const MAX_PARALLEL_TASKS: usize = 4;

/// Progress of a running sub-agent, for the chat's tool card.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    /// Tool calls it made, newest last.
    pub activity: Vec<String>,
    pub steps: usize,
    /// Its answer so far.
    pub text: String,
}

/// A sub-agent's report.
pub struct Report {
    pub text: String,
    pub steps: usize,
    pub activity: Vec<String>,
    /// `steps` or `time` when it stopped at a limit.
    pub stopped: Option<&'static str>,
}

/// Runs sub-agents for one chat.
pub trait Subagents: Send + Sync {
    fn run(&self, prompt: String, description: String, progress: Arc<dyn Fn(Progress) + Send + Sync>, cancel: CancellationToken) -> BoxFuture<'static, Result<Report, String>>;
}

#[derive(Deserialize)]
struct Params {
    description: String,
    prompt: String,
}

pub struct Task {
    context: ToolContext,
    runner: Arc<dyn Subagents>,
    spec: ToolSpec,
}

impl Task {
    pub fn new(context: ToolContext, runner: Arc<dyn Subagents>) -> Self {
        let spec = spec(
            "task",
            &format!(
                "Hand a self-contained investigation to a sub-agent that reads files, searches the project and searches the web, then answers in one report. Use it when finding the answer would otherwise fill this conversation with searching: locating where something is implemented, comparing several candidates, or reading a large area of the project. Several sub-agents (at most {MAX_PARALLEL_TASKS}) run at the same time when a step consists of task calls only; any other tool in the same step makes them run one after another. A sub-agent cannot change files, run commands or ask the user, and it sees nothing of this conversation beyond the prompt you give it, so state the task in full and say exactly what to report."
            ),
            json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "What this sub-agent is for, 3-6 words, shown in the interface. Example: \"Find the login flow\"." },
                    "prompt": { "type": "string", "description": "The task, written for an assistant that cannot see this conversation: what to look for, where to start, and exactly what to report back. The sub-agent reads files, searches the project and searches the web; it cannot change anything." },
                },
                "required": ["description", "prompt"],
            }),
        );
        Self { context, runner, spec }
    }
}

impl Tool for Task {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn sequential(&self) -> bool {
        false
    }

    fn execute(&self, _call_id: String, args: Value, cancel: CancellationToken, updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: Params = params(args)?;
            if params.prompt.trim().is_empty() {
                return Err("invalid prompt: expected the task for the sub-agent".into());
            }
            if params.description.trim().is_empty() {
                return Err("invalid description: expected a short label".into());
            }
            let description = params.description.clone();
            let progress: Arc<dyn Fn(Progress) + Send + Sync> = Arc::new(move |progress: Progress| {
                updates(ToolOutput::text(progress.text.clone(), json!({ "description": description, "steps": progress.steps, "activity": progress.activity })));
            });
            let report = self.runner.run(params.prompt, params.description.clone(), progress, cancel).await?;
            let note = match report.stopped {
                Some("steps") => "\n\n[the sub-agent reached its step limit; its report may be incomplete]",
                Some(_) => "\n\n[the sub-agent reached its time limit; its report may be incomplete]",
                None => "",
            };
            let text = if report.text.trim().is_empty() { "(the sub-agent reported nothing)".to_owned() } else { report.text };
            let mut details = json!({ "description": params.description, "steps": report.steps, "activity": report.activity });
            if let Some(stopped) = report.stopped {
                details["stopped"] = json!(stopped);
            }
            Ok(ToolOutput::text(self.context.limit_output("task", text + note), details))
        })
    }
}
