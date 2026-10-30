//! The model's checklist for a task, under the name and arguments each model family's own agent uses: `todo_write`
//! (Claude Code's TodoWrite), `update_plan` (OpenAI Codex) and `write_todos` (Gemini CLI). Every call replaces the
//! whole list; the chat shows the latest one from the call's `details.plan`.

use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolOutput, Updates, spec};
use crate::agent::llm::ToolSpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// `todo_write { todos: [{ content, status, activeForm }] }`.
    Claude,
    /// `update_plan { explanation?, plan: [{ step, status }] }`.
    Codex,
    /// `write_todos { todos: [{ description, status }] }`, which may also cancel an item.
    Gemini,
}

impl Style {
    pub fn name(self) -> &'static str {
        match self {
            Style::Claude => "todo_write",
            Style::Codex => "update_plan",
            Style::Gemini => "write_todos",
        }
    }
}

/// Whether a tool is one of the plan tools.
pub fn is_plan(name: &str) -> bool {
    matches!(name, "todo_write" | "update_plan" | "write_todos")
}

pub struct Plan {
    style: Style,
    spec: ToolSpec,
}

impl Plan {
    pub fn new(style: Style) -> Self {
        let statuses = || json!(["pending", "in_progress", "completed"]);
        let spec = match style {
            Style::Claude => spec(
                style.name(),
                "Create and update the task list for this chat. The user sees it as a checklist, so it shows them your plan and progress.\n\nUse it for work with three or more distinct steps, or when the user asks for several things at once: write the steps when you start, then update the list as you go. Skip it for a single simple step or a purely conversational request.\n\nEach call replaces the whole list. Keep exactly one item in_progress while you work, and mark an item completed as soon as it is done rather than in batches. Mark an item completed only when it is fully done: not when tests fail, the work is partial or something blocks it; add an item for what remains instead. Remove items that turn out to be unnecessary.",
                json!({
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "description": "The updated task list",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "content": { "type": "string", "minLength": 1, "description": "The step in the imperative form, such as \"Run the tests\"" },
                                    "status": { "type": "string", "enum": statuses() },
                                    "activeForm": { "type": "string", "description": "The step in the present continuous form, such as \"Running the tests\"" },
                                },
                                "required": ["content", "status"],
                            },
                        },
                    },
                    "required": ["todos"],
                }),
            ),
            Style::Codex => spec(
                style.name(),
                "Updates the task plan.\nProvide an optional explanation and a list of plan items, each with a step and status.\nAt most one step can be in_progress at a time.\n",
                json!({
                    "type": "object",
                    "properties": {
                        "explanation": { "type": "string" },
                        "plan": {
                            "type": "array",
                            "description": "The list of steps",
                            "items": {
                                "type": "object",
                                "properties": { "step": { "type": "string" }, "status": { "type": "string", "description": "One of: pending, in_progress, completed", "enum": statuses() } },
                                "required": ["step", "status"],
                                "additionalProperties": false,
                            },
                        },
                    },
                    "required": ["plan"],
                    "additionalProperties": false,
                }),
            ),
            Style::Gemini => spec(
                style.name(),
                "Lists out the subtasks required to complete a user request, and updates them as you work. The user sees the list as a checklist.\n\nUse it for complex requests that need several steps; skip it for simple requests that one or two steps settle. Each call replaces the whole list. Keep only one subtask in_progress at a time; mark a subtask completed when it is done, and cancelled when it is no longer needed. Add new subtasks as you learn more about the work.",
                json!({
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "description": "The complete list of todo items. This will replace the existing list.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "description": { "type": "string", "description": "The description of the task." },
                                    "status": { "type": "string", "description": "The current status of the task.", "enum": ["pending", "in_progress", "completed", "cancelled"] },
                                },
                                "required": ["description", "status"],
                            },
                        },
                    },
                    "required": ["todos"],
                }),
            ),
        };
        Self { style, spec }
    }

    fn run(&self, args: &Value) -> Result<ToolOutput, String> {
        let (list, text_field) = match self.style {
            Style::Claude => ("todos", "content"),
            Style::Codex => ("plan", "step"),
            Style::Gemini => ("todos", "description"),
        };
        let items = args.get(list).and_then(Value::as_array).ok_or_else(|| format!("{} needs `{list}`, a list of items", self.style.name()))?;
        let mut plan = Vec::new();
        for item in items {
            let text = item.get(text_field).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).ok_or_else(|| format!("every item needs a non-empty `{text_field}`"))?;
            let status = item.get("status").and_then(Value::as_str).unwrap_or("pending");
            let mut entry = json!({ "text": text, "status": status });
            if let Some(active) = item.get("activeForm").and_then(Value::as_str).filter(|active| !active.trim().is_empty()) {
                entry["active"] = json!(active.trim());
            }
            plan.push(entry);
        }
        let mut details = json!({ "plan": plan });
        if let Some(explanation) = args.get("explanation").and_then(Value::as_str).filter(|text| !text.trim().is_empty()) {
            details["explanation"] = json!(explanation.trim());
        }
        let text = match self.style {
            Style::Claude => "The task list is updated. Keep using it to track your progress, and carry on with the current task.".to_owned(),
            Style::Codex => "Plan updated".to_owned(),
            Style::Gemini => {
                let lines: Vec<String> = plan.iter().enumerate().map(|(index, item)| format!("{}. [{}] {}", index + 1, item["status"].as_str().unwrap_or_default(), item["text"].as_str().unwrap_or_default())).collect();
                if lines.is_empty() { "Successfully cleared the todo list.".to_owned() } else { format!("Successfully updated the todo list. The current list is now:\n{}", lines.join("\n")) }
            }
        };
        Ok(ToolOutput::text(text, details))
    }
}

impl Tool for Plan {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        let result = self.run(&args);
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_each_style() {
        let claude = Plan::new(Style::Claude).run(&json!({ "todos": [{ "content": "Run tests", "status": "in_progress", "activeForm": "Running tests" }] })).unwrap();
        assert_eq!(claude.details.unwrap()["plan"], json!([{ "text": "Run tests", "status": "in_progress", "active": "Running tests" }]));
        let codex = Plan::new(Style::Codex).run(&json!({ "explanation": "Two steps", "plan": [{ "step": "Read", "status": "completed" }, { "step": "Fix", "status": "pending" }] })).unwrap();
        let details = codex.details.unwrap();
        assert_eq!((details["plan"][1]["text"].as_str(), details["explanation"].as_str()), (Some("Fix"), Some("Two steps")));
        let gemini = Plan::new(Style::Gemini).run(&json!({ "todos": [{ "description": "Old idea", "status": "cancelled" }] })).unwrap();
        assert!(gemini.content.iter().any(|block| matches!(block, super::super::ResultContent::Text { text } if text.contains("1. [cancelled] Old idea"))));
        assert!(Plan::new(Style::Codex).run(&json!({ "plan": [{ "status": "pending" }] })).is_err());
    }
}
