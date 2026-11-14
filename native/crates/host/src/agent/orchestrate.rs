//! Orchestration: two models share one chat turn. The chat's own model plans — it reads the code, settles what is
//! unclear and writes the plan — then `hand_off` gives the work to the executor, a faster or cheaper model that edits,
//! runs commands and tests. When the executor meets friction (it calls `replan`, or its tool calls keep failing) the
//! planner takes the turn back, repairs the plan and hands it on again. Both work on the same transcript, so nothing
//! is lost at a handoff; each request goes to the model of the current phase.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::llm::ToolSpec;
use super::tools::{Tool, ToolOutput, Updates, params, spec};

/// Failed tool calls in a row after which the executor hands the turn back to the planner.
pub const FRICTION_ERRORS: u32 = 3;

/// Which model holds the turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Plan,
    Execute,
}

/// Changes the phase of the chat the tool belongs to; the text is the plan or the reason.
pub type Switch = Arc<dyn Fn(Phase, String) + Send + Sync>;

#[derive(Deserialize)]
struct HandOffParams {
    plan: String,
}

#[derive(Deserialize)]
struct ReplanParams {
    reason: String,
}

/// `hand_off`: the planner gives the turn to the executor with the plan.
pub struct HandOff {
    spec: ToolSpec,
    switch: Switch,
}

impl HandOff {
    pub fn new(executor: &str, switch: Switch) -> Self {
        let spec = spec(
            "hand_off",
            &format!(
                "Give this task to the executor model ({executor}) once you have a plan. The executor works in this same conversation after you: it edits files, runs commands and tests, and follows your plan. Call this as the only tool of your reply, after exploring enough to plan; do not make the edits yourself. Answer a question that needs no changes directly instead of handing off."
            ),
            json!({
                "type": "object",
                "properties": {
                    "plan": { "type": "string", "description": "The plan for the executor: numbered steps naming files and functions, what each change must do, how to verify it (commands, tests), and when to call replan instead of improvising." },
                },
                "required": ["plan"],
            }),
        );
        Self { spec, switch }
    }
}

impl Tool for HandOff {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: HandOffParams = params(args)?;
            if params.plan.trim().is_empty() {
                return Err("invalid plan: expected the steps for the executor".into());
            }
            (self.switch)(Phase::Execute, params.plan.clone());
            Ok(ToolOutput::text(
                "Handed off. You are now the executor: carry out the plan above step by step, verify the result, then report to the user. Call replan if the plan turns out wrong or you are stuck.",
                json!({ "phase": "execute", "plan": params.plan }),
            ))
        })
    }
}

/// `replan`: the executor gives the turn back to the planner.
pub struct Replan {
    spec: ToolSpec,
    switch: Switch,
}

impl Replan {
    pub fn new(planner: &str, switch: Switch) -> Self {
        let spec = spec(
            "replan",
            &format!(
                "Give the turn back to the planner model ({planner}) when the plan does not fit what you find, a step fails twice, or a decision needs deeper reasoning. Say what you did, what failed and what you know now; the planner inspects the state, repairs the plan and hands it back."
            ),
            json!({
                "type": "object",
                "properties": {
                    "reason": { "type": "string", "description": "What was done, what went wrong (errors, test output) and the open question." },
                },
                "required": ["reason"],
            }),
        );
        Self { spec, switch }
    }
}

impl Tool for Replan {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: ReplanParams = params(args)?;
            if params.reason.trim().is_empty() {
                return Err("invalid reason: expected what went wrong".into());
            }
            (self.switch)(Phase::Plan, params.reason.clone());
            Ok(ToolOutput::text(
                "Handed back. You are now the planner: inspect the current state, repair the plan and call hand_off again, or finish the task yourself if only a little is left.",
                json!({ "phase": "plan", "reason": params.reason }),
            ))
        })
    }
}

/// The part of the system prompt that tells the model its role in the pair.
pub fn prompt_section(phase: Phase, planner: &str, executor: &str, note: Option<&str>) -> String {
    match phase {
        Phase::Plan => {
            let mut text = format!(
                "\n\n# Orchestration\nYou are the planner ({planner}) of an orchestrated pair. For a task that changes the project, explore first (read, grep, glob, read-only commands), resolve what is unclear, then call hand_off with a concrete plan; the executor model ({executor}) carries it out in this conversation, and gives the turn back with replan when it meets friction. Do not write or edit files yourself unless only a trivial step remains. For a question, answer directly without handing off."
            );
            if let Some(note) = note {
                text.push_str(&format!("\nThe executor handed the turn back: {note}"));
            }
            text
        }
        Phase::Execute => format!(
            "\n\n# Orchestration\nYou are the executor ({executor}) of an orchestrated pair. The planner ({planner}) wrote the plan in its hand_off call above. Carry it out step by step: edit files, run commands and tests, and verify the result; then report to the user. When the plan does not fit, a step fails twice or a decision needs deeper reasoning, call replan instead of improvising."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn the_tools_switch_the_phase_with_the_plan_or_reason() {
        let seen: Arc<Mutex<Vec<(Phase, String)>>> = Arc::default();
        let switch: Switch = {
            let seen = seen.clone();
            Arc::new(move |phase, text| seen.lock().unwrap().push((phase, text)))
        };
        let hand_off = HandOff::new("fast", switch.clone());
        let output = hand_off.execute("1".into(), json!({ "plan": "1. edit a.rs" }), CancellationToken::new(), Arc::new(|_| {})).await.unwrap();
        assert!(!output.is_error);
        assert!(hand_off.execute("2".into(), json!({ "plan": " " }), CancellationToken::new(), Arc::new(|_| {})).await.is_err());
        Replan::new("strong", switch).execute("3".into(), json!({ "reason": "tests fail" }), CancellationToken::new(), Arc::new(|_| {})).await.unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![(Phase::Execute, "1. edit a.rs".to_owned()), (Phase::Plan, "tests fail".to_owned())]);
    }

    #[test]
    fn each_phase_names_both_models() {
        let plan = prompt_section(Phase::Plan, "P", "E", Some("tests fail"));
        assert!(plan.contains("planner (P)") && plan.contains("executor model (E)") && plan.ends_with("tests fail"));
        assert!(prompt_section(Phase::Execute, "P", "E", None).contains("executor (E)"));
    }
}
