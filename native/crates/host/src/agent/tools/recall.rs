//! `recall`: the chat's own record, read back verbatim. Old tool outputs are cleared from the model's context and
//! long spans replaced by a checkpoint to free space; nothing is deleted from the chat file, and this tool searches
//! it or returns entries by number, so an exact error message, path or user instruction is never lost to a summary.

use std::sync::Arc;

use futures::future::BoxFuture;
use rainy_protocol::chat::TranscriptEntry;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolContext, ToolOutput, Updates, params, spec};
use crate::agent::history::{self, Kind, Pattern};
use crate::agent::llm::ToolSpec;

pub const NAME: &str = "recall";

/// Entries one call reads at most.
const MAX_ENTRIES: usize = 30;

/// The record of one chat.
pub trait History: Send + Sync {
    /// Run `look` over the chat's entries as they stand.
    fn look(&self, look: &mut dyn FnMut(&[TranscriptEntry]) -> String) -> String;
}

#[derive(Deserialize)]
struct Params {
    query: Option<String>,
    #[serde(default)]
    regex: bool,
    kind: Option<String>,
    entries: Option<String>,
    lines: Option<String>,
    limit: Option<u64>,
}

pub struct Recall {
    context: ToolContext,
    history: Arc<dyn History>,
    spec: ToolSpec,
}

impl Recall {
    pub fn new(context: ToolContext, history: Arc<dyn History>) -> Self {
        let spec = spec(
            NAME,
            "Read this chat's own earlier record verbatim: the user's messages, your replies and tool calls, and tool outputs, including those cleared from your context or folded into a checkpoint to save space. Nothing is lost when that happens; this tool brings it back. Search with query (case-insensitive text, or a regular expression with regex: true), optionally limited by kind, or read entries by number with entries. Entry numbers appear in search results, in cleared-output placeholders and in checkpoints. Use it whenever you need an exact detail from earlier (an error message, a path, a number, a decision, the user's exact wording) instead of reconstructing it from memory, and before redoing work whose result was cleared.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Text to find, case-insensitive; a regular expression when regex is true. Leave it out to read entries by number." },
                    "regex": { "type": "boolean", "description": "Treat query as a regular expression." },
                    "kind": { "type": "string", "enum": ["user", "assistant", "tool", "all"], "description": "Which entries to search: user messages (with added context), your replies and tool calls, or tool outputs. Default all." },
                    "entries": { "type": "string", "description": "Entries to read in full: a number (\"12\"), a range (\"40-45\") or a list (\"3,7,10-12\"), at most 30." },
                    "lines": { "type": "string", "description": "With a single entry: the lines to read, such as \"200-260\", for an output too long to show whole." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "description": "Matching lines a search shows at most. Default 30." },
                },
            }),
        );
        Self { context, history, spec }
    }
}

fn line_range(text: &str) -> Result<(usize, usize), String> {
    let invalid = || format!("invalid lines \"{text}\": use a range such as 200-260");
    let (from, to) = text.split_once('-').map_or((text, text), |(from, to)| (from, to));
    let (from, to) = (from.trim().parse::<usize>().map_err(|_| invalid())?, to.trim().parse::<usize>().map_err(|_| invalid())?);
    if from == 0 || to < from { Err(invalid()) } else { Ok((from, to)) }
}

impl Tool for Recall {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: Params = params(args)?;
            let tokens = (self.context.tool_tokens)();
            let kind = match params.kind.as_deref() {
                None => Kind::All,
                Some(text) => Kind::parse(text).ok_or_else(|| format!("invalid kind \"{text}\": use user, assistant, tool or all"))?,
            };
            let lines = params.lines.as_deref().map(line_range).transpose()?;
            let mut failure = None;
            let (text, details) = match (params.query.as_deref().filter(|query| !query.trim().is_empty()), params.entries.as_deref().filter(|entries| !entries.trim().is_empty())) {
                (Some(query), _) => {
                    let pattern = Pattern::new(query, params.regex)?;
                    let limit = params.limit.unwrap_or(30).clamp(1, 100) as usize;
                    (self.history.look(&mut |entries| history::search(entries, &pattern, kind, limit, tokens)), json!({ "query": query }))
                }
                (None, Some(wanted)) => {
                    let text = self.history.look(&mut |entries| match history::parse_numbers(wanted, entries.len(), MAX_ENTRIES) {
                        Ok(numbers) if lines.is_some() && numbers.len() > 1 => {
                            failure = Some("lines reads one entry: pass a single entry number with it".to_owned());
                            String::new()
                        }
                        Ok(numbers) => history::read(entries, &numbers, lines, tokens),
                        Err(error) => {
                            failure = Some(error);
                            String::new()
                        }
                    });
                    (text, json!({ "entries": wanted }))
                }
                (None, None) => return Err("pass query to search this chat, or entries to read entries by number".into()),
            };
            if let Some(error) = failure {
                return Err(error);
            }
            Ok(ToolOutput::text(text, details))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fixed(Mutex<Vec<TranscriptEntry>>);

    impl History for Fixed {
        fn look(&self, look: &mut dyn FnMut(&[TranscriptEntry]) -> String) -> String {
            look(&self.0.lock().unwrap())
        }
    }

    fn recall(dir: &std::path::Path) -> Recall {
        let entries = vec![
            serde_json::from_value(json!({ "kind": "user", "id": "u1", "ts": 1, "text": "Deploy to eu-west-1 only." })).unwrap(),
            serde_json::from_value(json!({ "kind": "toolResult", "id": "t1", "ts": 2, "toolCallId": "c1", "toolName": "bash", "content": [{ "type": "text", "text": "a\nb\nc" }], "isError": false })).unwrap(),
        ];
        Recall::new(super::super::test_context(dir), Arc::new(Fixed(Mutex::new(entries))))
    }

    async fn call(tool: &Recall, args: Value) -> Result<String, String> {
        let output = tool.execute("c".into(), args, CancellationToken::new(), super::super::no_updates()).await?;
        let [rainy_protocol::chat::ResultContent::Text { text }] = output.content.as_slice() else { panic!() };
        Ok(text.clone())
    }

    #[tokio::test]
    async fn searches_and_reads_the_chat() {
        let dir = tempfile::tempdir().unwrap();
        let tool = recall(dir.path());
        assert_eq!(call(&tool, json!({ "query": "EU-WEST" })).await.unwrap(), "#1 user\n  1: Deploy to eu-west-1 only.\n");
        assert_eq!(call(&tool, json!({ "entries": "2", "lines": "2-3" })).await.unwrap(), "#2 tool bash (3 lines)\n2: b\n3: c\n");
        assert!(call(&tool, json!({ "entries": "1-2", "lines": "1-2" })).await.unwrap_err().contains("single entry"));
        assert!(call(&tool, json!({ "entries": "9" })).await.unwrap_err().contains("2 entries"));
        assert!(call(&tool, json!({})).await.is_err());
        assert!(call(&tool, json!({ "query": "x", "kind": "files" })).await.is_err());
    }
}
