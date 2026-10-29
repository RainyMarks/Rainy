//! Editor AI (3.x `inline.ts`): ghost-text suggestions at the cursor and rewriting a selection for an instruction
//! (Ctrl+K).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use rainy_protocol::chat::AssistantMessage;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::llm::{self, Message, Request, reply_text};
use super::models::{Models, Resolved};
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::settings::{ModelSelection, Settings};

/// Characters of the file sent before and after the cursor for a suggestion.
const SUGGEST_BEFORE: usize = 3000;
const SUGGEST_AFTER: usize = 1000;
/// A suggestion is short; a longer reply is cut.
const SUGGEST_MAX_TOKENS: u64 = 192;
const SUGGEST_MAX_LINES: usize = 16;
const SUGGEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Characters of the file sent around a selection to rewrite.
const EDIT_BEFORE: usize = 6000;
const EDIT_AFTER: usize = 3000;
const EDIT_TIMEOUT: Duration = Duration::from_secs(180);
/// Longest gap between progress events while a reply streams.
const PROGRESS_INTERVAL_MS: u64 = 80;

const CURSOR: &str = "<|cursor|>";
const SELECTION_START: &str = "<|selection_start|>";
const SELECTION_END: &str = "<|selection_end|>";

const SUGGEST_SYSTEM: &str = "You are a code completion engine inside an editor. The file is shown with <|cursor|> marking the cursor. \
Reply with exactly the text to insert at the cursor: no explanation, no Markdown, no code fences, and nothing that is already before or after the cursor. \
Complete the current line, or the current statement or small block when the context makes the next lines clear. \
If nothing useful should be inserted, reply with an empty message.";

const EDIT_SYSTEM: &str = "You rewrite code in an editor. The file is shown with the selected region between <|selection_start|> and <|selection_end|>, followed by an instruction. \
Reply with only the new text for the selected region: no explanation, no Markdown, no code fences, and nothing outside the region. \
Keep the indentation and style of the surrounding code. When the region is empty, reply with the code to insert at that point.";

fn tail(text: &str, chars: usize) -> &str {
    let count = text.chars().count();
    text.char_indices().nth(count.saturating_sub(chars)).map_or(text, |(at, _)| &text[at..])
}

fn head(text: &str, chars: usize) -> &str {
    text.char_indices().nth(chars).map_or(text, |(at, _)| &text[..at])
}

/// The body of the first fenced code block when the reply has one, otherwise the reply.
fn unfence(text: &str) -> &str {
    static FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)```[^\n`]*\n(.*?)(?:\n```|$)").unwrap());
    FENCE.captures(text).and_then(|found| found.get(1)).map_or(text, |body| body.as_str())
}

/// A model reply as text to insert at the cursor; empty when nothing usable remains. Tab inserts it as it is, so what
/// the reply repeats of the text around the cursor goes: the line or the word already typed, the indentation of an
/// empty line, and the closing bracket or quote that already follows the cursor.
pub fn clean_suggestion(raw: &str, prefix: &str, suffix: &str) -> String {
    let text = unfence(raw).replace(CURSOR, "").replace("\r\n", "\n");
    let line_prefix = prefix.rsplit('\n').next().unwrap_or_default().trim_end_matches('\r');
    let mut text = without_repeated_start(&text, line_prefix).to_owned();
    let rest = suffix.split('\n').next().unwrap_or_default().trim_end_matches('\r').trim();
    if !rest.is_empty() {
        // In the middle of a line only the rest of that line fits, and it ends where the text after the cursor begins.
        text = text.split('\n').next().unwrap_or_default().to_owned();
        text = before_existing(&text, rest).to_owned();
    }
    let text = text.split('\n').take(SUGGEST_MAX_LINES).collect::<Vec<_>>().join("\n").trim_end().to_owned();
    if text.trim().is_empty() { String::new() } else { text }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `text` without what it repeats of the line before the cursor: its longest start that the line already ends with.
/// That counts when it is everything typed on the line, or two or more visible characters that begin a word or a
/// symbol there (`x = pri` answered with `print(y)`); one letter after other text (`x = l` answered with `lm.call()`)
/// may well be new text.
fn without_repeated_start<'a>(text: &'a str, line_prefix: &str) -> &'a str {
    let ends: Vec<usize> = text.char_indices().map(|(at, _)| at).skip(1).chain([text.len()]).take_while(|end| *end <= line_prefix.len()).collect();
    for &end in ends.iter().rev() {
        let repeated = &text[..end];
        if !line_prefix.ends_with(repeated) {
            continue;
        }
        let before = &line_prefix[..line_prefix.len() - end];
        let whole_line = before.trim().is_empty();
        let at_start = !before.ends_with(is_word) || !repeated.starts_with(is_word);
        if whole_line || (at_start && repeated.chars().filter(|c| !c.is_whitespace()).count() >= 2) {
            return &text[end..];
        }
    }
    text
}

/// `text` up to where the text that already follows the cursor on its line (`rest`, trimmed) takes over: without that
/// text repeated at its end, and before the first closing bracket, quote or `;` that would close what `rest` already
/// closes.
fn before_existing<'a>(text: &'a str, rest: &str) -> &'a str {
    if let Some(kept) = text.trim_end().strip_suffix(rest) {
        return kept;
    }
    let Some(next) = rest.chars().next() else { return text };
    if matches!(next, '"' | '\'' | '`') {
        // Inside a string that already has its closing quote: the reply's first one would close it again.
        let mut escaped = false;
        for (at, c) in text.char_indices() {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                _ if c == next => return &text[..at],
                _ => {}
            }
        }
        return text;
    }
    if !matches!(next, ')' | ']' | '}' | ';') {
        return text;
    }
    // Brackets the reply opens and strings it holds are its own; what is left at depth 0 is the one `rest` has.
    let (mut depth, mut quote, mut escaped) = (0usize, None, false);
    for (at, c) in text.char_indices() {
        if let Some(open) = quote {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                _ if c == open => quote = None,
                _ => {}
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' | ';' if depth == 0 && c == next => return &text[..at],
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    text
}

/// A model reply as the replacement of a selection.
pub fn clean_edit(raw: &str, selection: &str) -> String {
    static LEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:[ \t]*\r?\n)+").unwrap());
    static TRAILING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:\r?\n[ \t]*)+$").unwrap());
    let mut text = unfence(raw).replace(SELECTION_START, "").replace(SELECTION_END, "");
    if !selection.starts_with('\n') {
        text = LEADING.replace(&text, "").into_owned();
    }
    if !selection.ends_with('\n') {
        text = TRAILING.replace(&text, "").into_owned();
    }
    text
}

fn header(path: &str, language: Option<&str>) -> String {
    match language.filter(|language| !language.is_empty()) {
        Some(language) => format!("File: {path} ({language})"),
        None => format!("File: {path}"),
    }
}

fn no_model() -> String {
    l("还没有可用的模型。请先在“设置 → 模型”中添加一个模型。", "No model is set up yet. Add one in Settings → Models.")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestRequest {
    pub request_id: Option<String>,
    pub path: String,
    pub language: Option<String>,
    pub prefix: String,
    pub suffix: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditRequest {
    pub request_id: String,
    pub path: String,
    pub language: Option<String>,
    pub before: String,
    pub selection: String,
    pub after: String,
    pub instruction: String,
}

/// Suggestions and rewrites for the editor; the chat model, or another one for suggestions, answers them.
pub struct InlineAssist {
    models: Arc<Models>,
    settings: Arc<Settings>,
    progress: Box<dyn Fn(&str, &str) + Send + Sync>,
    /// The suggestion in flight and every rewrite, each with the number of its request.
    suggestion: Mutex<Option<(u64, CancellationToken)>>,
    edits: Mutex<HashMap<String, (u64, CancellationToken)>>,
    next: AtomicU64,
}

impl InlineAssist {
    /// `progress` receives the text a reply has produced so far, by request id.
    pub fn new(models: Arc<Models>, settings: Arc<Settings>, progress: impl Fn(&str, &str) + Send + Sync + 'static) -> Self {
        Self { models, settings, progress: Box::new(progress), suggestion: Mutex::default(), edits: Mutex::default(), next: AtomicU64::new(0) }
    }

    /// Stream one reply, reporting cleaned partial text at most every [`PROGRESS_INTERVAL_MS`]; `None` on timeout.
    async fn stream(&self, resolved: &Resolved, system: &str, prompt: String, max_tokens: Option<u64>, cancel: &CancellationToken, timeout: Duration, partial: impl Fn(&str) + Send + Sync) -> Option<AssistantMessage> {
        let messages = [Message::user(prompt)];
        let request = Request { max_tokens, no_cache: true, ..Request::new(system, &messages, &[]) };
        let key = self.models.api_key(&resolved.setup.provider);
        let mut sent = 0;
        let mut emit = |_: llm::Event, reply: &AssistantMessage| {
            let now = crate::text::now_ms();
            if now.saturating_sub(sent) >= PROGRESS_INTERVAL_MS {
                sent = now;
                partial(&reply_text(reply));
            }
        };
        tokio::time::timeout(timeout, self.models.llm.stream(&resolved.model, &request, key, cancel, &mut emit)).await.ok()
    }

    /// A ghost-text suggestion; a newer request ends the previous one, and failures come back as `error` so typing
    /// goes on.
    pub async fn complete(&self, request: SuggestRequest) -> Value {
        let (number, cancel) = (self.next.fetch_add(1, Ordering::SeqCst), CancellationToken::new());
        if let Some((_, previous)) = self.suggestion.lock().unwrap().replace((number, cancel.clone())) {
            previous.cancel();
        }
        let prefs = self.settings.prefs();
        if prefs.get("inlineCompletion").and_then(Value::as_bool) != Some(true) {
            return json!({ "text": "" });
        }
        let Some(resolved) = self.suggestion_model(prefs.get("completionModel").and_then(Value::as_str).unwrap_or_default()) else {
            return json!({ "text": "", "error": no_model() });
        };
        let prompt = format!("{}\n\n{}{CURSOR}{}", header(&request.path, request.language.as_deref()), tail(&request.prefix, SUGGEST_BEFORE), head(&request.suffix, SUGGEST_AFTER));
        let report = |text: &str| {
            let text = clean_suggestion(text, &request.prefix, &request.suffix);
            if let Some(id) = request.request_id.as_deref().filter(|_| !text.is_empty()) {
                (self.progress)(id, &text);
            }
        };
        let reply = self.stream(&resolved, SUGGEST_SYSTEM, prompt, Some(SUGGEST_MAX_TOKENS), &cancel, SUGGEST_TIMEOUT, report).await;
        {
            let mut current = self.suggestion.lock().unwrap();
            if current.as_ref().is_some_and(|(current, _)| *current == number) {
                *current = None;
            }
        }
        match reply {
            None => json!({ "text": "", "error": l("补全请求超时。", "The suggestion request timed out.") }),
            Some(reply) if reply.stop_reason == "aborted" || cancel.is_cancelled() => json!({ "text": "" }),
            Some(reply) if reply.stop_reason == "error" => {
                json!({ "text": "", "error": reply.error_message.unwrap_or_else(|| l("补全请求失败。", "The suggestion request failed.")) })
            }
            Some(reply) => json!({ "text": clean_suggestion(&reply_text(&reply), &request.prefix, &request.suffix) }),
        }
    }

    /// Rewrite a selection with the chat model, reporting the text as it streams.
    pub async fn edit(&self, request: EditRequest) -> HostResult<Value> {
        let resolved = self.models.resolve(None).ok_or_else(|| HostError::new("no-model", no_model()))?;
        let (number, cancel) = (self.next.fetch_add(1, Ordering::SeqCst), CancellationToken::new());
        if let Some((_, previous)) = self.edits.lock().unwrap().insert(request.request_id.clone(), (number, cancel.clone())) {
            previous.cancel();
        }
        let file = format!("{}{SELECTION_START}{}{SELECTION_END}{}", tail(&request.before, EDIT_BEFORE), request.selection, head(&request.after, EDIT_AFTER));
        let prompt = format!("{}\n\n{file}\n\nInstruction: {}", header(&request.path, request.language.as_deref()), request.instruction);
        let report = |text: &str| (self.progress)(&request.request_id, &clean_edit(text, &request.selection));
        let reply = self.stream(&resolved, EDIT_SYSTEM, prompt, None, &cancel, EDIT_TIMEOUT, report).await;
        {
            let mut edits = self.edits.lock().unwrap();
            if edits.get(&request.request_id).is_some_and(|(current, _)| *current == number) {
                edits.remove(&request.request_id);
            }
        }
        match reply {
            None => Err(HostError::other(l("修改请求超时。", "The edit request timed out."))),
            Some(reply) if reply.stop_reason == "aborted" || cancel.is_cancelled() => Err(HostError::new("aborted", l("已停止生成。", "Generation was stopped."))),
            Some(reply) if reply.stop_reason == "error" => Err(HostError::other(reply.error_message.unwrap_or_else(|| l("修改请求失败。", "The edit request failed.")))),
            Some(reply) => Ok(json!({ "text": clean_edit(&reply_text(&reply), &request.selection) })),
        }
    }

    /// Stop one rewrite.
    pub fn cancel(&self, request_id: &str) {
        if let Some((_, cancel)) = self.edits.lock().unwrap().get(request_id) {
            cancel.cancel();
        }
    }

    /// Stop every request, for shutdown.
    pub fn close(&self) {
        if let Some((_, cancel)) = self.suggestion.lock().unwrap().take() {
            cancel.cancel();
        }
        for (_, cancel) in self.edits.lock().unwrap().values() {
            cancel.cancel();
        }
    }

    fn suggestion_model(&self, provider: &str) -> Option<Resolved> {
        let setup = (!provider.is_empty()).then(|| self.settings.get().models.into_iter().find(|model| model.provider == provider)).flatten();
        let selection = setup.map(|setup| ModelSelection { provider: setup.provider, model: setup.model, thinking: None });
        self.models.resolve(selection.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_replies() {
        assert_eq!(clean_suggestion("```rust\nlet x = 1;\n```", "", ""), "let x = 1;");
        assert_eq!(clean_suggestion("    foo(bar)", "    foo", "\n"), "(bar)");
        assert_eq!(clean_suggestion("a, b)\nmore", "f(", ")"), "a, b");
        assert_eq!(clean_edit("\n  x = 2\n", "  x = 1"), "  x = 2");
        assert_eq!(clean_edit("y\n", "x\n"), "y\n");
    }

    /// Tab inserts a suggestion as it is, so nothing of it may repeat the line it continues.
    #[test]
    fn drops_what_the_line_already_has() {
        // The indentation of an empty line, the whole line, or the word being typed.
        assert_eq!(clean_suggestion("    return x", "def f():\n    ", "\n"), "return x");
        assert_eq!(clean_suggestion("return x", "def f():\n    ", ""), "return x");
        assert_eq!(clean_suggestion("    x = 1\r\n    y = 2", "if a:\r\n    ", "\r\n"), "x = 1\n    y = 2");
        assert_eq!(clean_suggestion("foo(bar)", "    foo", ""), "(bar)");
        assert_eq!(clean_suggestion("x = 1", "x", ""), " = 1");
        assert_eq!(clean_suggestion("print(y)", "    x = pri", "\n"), "nt(y)");
        assert_eq!(clean_suggestion("self, y):", "def f(self, ", ""), "y):");
        // One letter after other text may be the start of something new.
        assert_eq!(clean_suggestion("lm.call()", "x = l", ""), "lm.call()");
        assert_eq!(clean_suggestion("count + 1", "total = 1 + ", ""), "count + 1");
    }

    #[test]
    fn stops_where_the_rest_of_the_line_takes_over() {
        assert_eq!(clean_suggestion("a, b);", "foo(", ")"), "a, b");
        assert_eq!(clean_suggestion("x)", "f(g(", "))"), "x");
        assert_eq!(clean_suggestion("g(x), y)", "f(", ")"), "g(x), y");
        assert_eq!(clean_suggestion("\")\", x)", "f(", ")"), "\")\", x");
        assert_eq!(clean_suggestion("1, 2]", "v = [", "]"), "1, 2");
        assert_eq!(clean_suggestion("hello\");", "print(\"", "\")"), "hello");
        assert_eq!(clean_suggestion(r#"say \"hi\"");"#, "s = \"", "\""), r#"say \"hi\""#);
        assert_eq!(clean_suggestion("foo(1); // x", "let a = ", ";"), "foo(1)");
        assert_eq!(clean_suggestion("x > 0", "if (", ") {"), "x > 0");
    }
}
