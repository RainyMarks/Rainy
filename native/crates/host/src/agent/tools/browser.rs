//! `browser_open`, `browser_read`, `browser_act` and `browser_screenshot`: the agent looks at the application it is
//! building and checks it itself. The page runs in a hidden window the carrier owns, which only opens loopback
//! addresses — the project's own development server.

use std::sync::Arc;

use futures::future::BoxFuture;
use rainy_protocol::chat::ResultContent;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolContext, ToolOutput, Updates, params, spec};
use crate::agent::llm::ToolSpec;

/// Sends one command (`open`, `read`, `click`, `type`, `press`, `screenshot`, `close`) to the carrier's preview.
pub type Preview = Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

fn strings(answer: &Value, field: &str) -> Vec<String> {
    answer[field].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect()
}

/// The page as the model reads it: where it is, what it says, what can be clicked, and what went wrong.
pub fn render_page(answer: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(error) = answer["error"].as_str() {
        parts.push(format!("[{error}]"));
    }
    let url = answer["url"].as_str();
    if let Some(url) = url {
        let title = answer["title"].as_str().filter(|title| !title.is_empty()).map_or_else(String::new, |title| format!("\nTitle: {title}"));
        parts.push(format!("URL: {url}{title}"));
    }
    let (failed, logged) = (strings(answer, "failed"), strings(answer, "console"));
    if !failed.is_empty() {
        parts.push(format!("Requests that failed ({}):\n{}", failed.len(), failed.join("\n")));
    }
    if !logged.is_empty() {
        parts.push(format!("Console ({}):\n{}", logged.len(), logged.join("\n")));
    }
    if logged.is_empty() && failed.is_empty() && url.is_some() {
        parts.push("Console: no errors or warnings.".into());
    }
    let elements = strings(answer, "elements");
    if !elements.is_empty() {
        parts.push(format!("Elements:\n{}", elements.join("\n")));
    }
    match answer["text"].as_str().filter(|text| !text.is_empty()) {
        Some(text) => parts.push(format!("Page text:\n{text}")),
        None if url.is_some() => parts.push("Page text: (the page shows no text)".into()),
        None => {}
    }
    parts.join("\n\n")
}

fn details(answer: &Value) -> Value {
    let errors = strings(answer, "failed").len() + strings(answer, "console").iter().filter(|line| line.starts_with("error")).count();
    let mut details = json!({ "errors": errors });
    for field in ["url", "title"] {
        if let Some(value) = answer[field].as_str() {
            details[field] = json!(value);
        }
    }
    details
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Open,
    Read,
    Act,
    Screenshot,
}

pub struct Browser {
    context: ToolContext,
    preview: Preview,
    kind: Kind,
    spec: ToolSpec,
}

#[derive(Deserialize)]
struct OpenParams {
    url: String,
}

#[derive(Deserialize)]
struct ActParams {
    action: String,
    target: String,
    text: Option<String>,
}

impl Browser {
    /// The browser tools; the screenshot only when the chat's model reads images.
    pub fn tools(context: &ToolContext, preview: Preview, vision: bool) -> Vec<Arc<dyn Tool>> {
        let make = |kind: Kind, name: &str, description: &str, parameters: Value| -> Arc<dyn Tool> {
            Arc::new(Browser { context: context.clone(), preview: preview.clone(), kind, spec: spec(name, description, parameters) })
        };
        let empty = json!({ "type": "object", "properties": {} });
        let mut tools = vec![
            make(
                Kind::Open,
                "browser_open",
                "Open the application you are building in a preview window and report what the page shows: its text, the elements that can be clicked or typed into, and the console errors and failed requests. Use it to check your own work after starting a development server, instead of asking the user whether the page works. Only addresses on this machine (localhost, 127.0.0.1) open.",
                json!({ "type": "object", "properties": { "url": { "type": "string", "description": "Address of the running application on this machine, such as http://localhost:5173/. Only loopback addresses open." } }, "required": ["url"] }),
            ),
            make(
                Kind::Read,
                "browser_read",
                "Read the preview page again: its text, its elements, and the console errors and failed requests since it was opened. Use it after the application reloads itself or after waiting for something to finish.",
                empty.clone(),
            ),
            make(
                Kind::Act,
                "browser_act",
                "Click an element, type into a field or press a key in the preview page, then report the page again. Name the element by the visible text browser_read listed, or by a CSS selector.",
                json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["click", "type", "press"], "description": "click an element, type text into a field, or press a key in it (such as Enter)." },
                        "target": { "type": "string", "description": "The element: its visible text or label as browser_read listed it, or a CSS selector." },
                        "text": { "type": "string", "description": "Text to type, for action \"type\"; the key to press, for action \"press\" (default Enter)." },
                    },
                    "required": ["action", "target"],
                }),
            ),
        ];
        if vision {
            tools.push(make(Kind::Screenshot, "browser_screenshot", "Take a picture of the preview page, for what text cannot tell you: layout, spacing, colors and overlap.", empty));
        }
        tools
    }

    async fn run(&self, args: Value) -> Result<ToolOutput, String> {
        let command = match self.kind {
            Kind::Open => json!({ "action": "open", "url": params::<OpenParams>(args)?.url }),
            Kind::Read => json!({ "action": "read" }),
            Kind::Screenshot => json!({ "action": "screenshot" }),
            Kind::Act => {
                let act: ActParams = params(args)?;
                if act.target.trim().is_empty() {
                    return Err("invalid target: name the element by its visible text or a CSS selector".into());
                }
                if !["click", "type", "press"].contains(&act.action.as_str()) {
                    return Err("invalid action: expected click, type or press".into());
                }
                if act.action == "press" {
                    json!({ "action": "press", "target": act.target, "key": act.text.unwrap_or_else(|| "Enter".into()) })
                } else {
                    json!({ "action": act.action, "target": act.target, "text": act.text.unwrap_or_default() })
                }
            }
        };
        let answer = (self.preview)(command).await?;
        if self.kind == Kind::Screenshot {
            let Some(image) = answer["image"].as_str() else {
                let mut output = ToolOutput::text(render_page(&answer), details(&answer));
                output.is_error = true;
                return Ok(output);
            };
            let label = format!("Screenshot of {}", answer["url"].as_str().unwrap_or("the preview"));
            return Ok(ToolOutput {
                content: vec![ResultContent::Text { text: label }, ResultContent::Image { data: image.to_owned(), mime_type: "image/png".into() }],
                details: Some(details(&answer)),
                is_error: false,
                terminate: false,
            });
        }
        let mut output = ToolOutput::text(self.context.limit_output("browser", render_page(&answer)), details(&answer));
        let failed = answer["error"].is_string() && (self.kind == Kind::Act || answer["url"].is_null());
        output.is_error = failed;
        Ok(output)
    }
}

impl Tool for Browser {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(self.run(args))
    }
}
