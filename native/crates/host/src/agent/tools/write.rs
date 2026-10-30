//! `write` and `edit`: whole-file writes and literal replacements, both guarded by a prior read.

use std::path::Path;

use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::encoding::{Decoded, Encoding, decode, encode, round_trips};
use super::{Tool, ToolContext, ToolOutput, Updates, params, spec, version_of};
use crate::agent::llm::ToolSpec;

/// Files larger than this keep no snapshot, so their changes cannot be undone.
pub const MAX_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;
/// Text kept per side of a change for the page's diff card.
const DIFF_TEXT_LIMIT: usize = 256 * 1024;

/// Keep the replaced content for undo; a failure costs only the undo. `before` is `None` for a new file.
async fn keep_for_undo(context: &ToolContext, call_id: &str, path: &Path, before: Option<Option<Vec<u8>>>, after: Vec<u8>) -> bool {
    match (before, &context.record_change) {
        (Some(before), Some(record)) => record(call_id.to_owned(), path.to_path_buf(), before, Some(after)).await,
        _ => false,
    }
}

/// Before and after text for the diff card; both are left out when either is too large.
pub(super) fn diff_sides(details: &mut Value, before: Option<&str>, after: &str) {
    if before.is_some_and(|before| before.len() > DIFF_TEXT_LIMIT) || after.len() > DIFF_TEXT_LIMIT {
        return;
    }
    if let Some(before) = before {
        details["before"] = json!(before);
    }
    details["after"] = json!(after);
}

/// A file's text for an edit, in an encoding it can be written back in without touching the rest of the file.
pub(super) fn editable(bytes: &[u8], display: &str) -> Result<Decoded, String> {
    let decoded = decode(bytes).ok_or_else(|| format!("cannot modify \"{display}\": it is binary or its text encoding is not UTF-8, UTF-16 with a byte-order mark or GB18030"))?;
    if !round_trips(bytes, &decoded) {
        let why = if decoded.damaged { "UTF-8 text has invalid bytes".to_owned() } else { format!("{} text does not convert back to the same bytes", decoded.encoding.label()) };
        return Err(format!("cannot modify \"{display}\": its {why}, so an edit could damage it; change it with a command that keeps its bytes"));
    }
    Ok(decoded)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(path, bytes).map_err(|error| error.to_string())
}

#[derive(Deserialize)]
struct WriteParams {
    file_path: String,
    content: String,
}

pub struct Write {
    context: ToolContext,
    spec: ToolSpec,
}

impl Write {
    pub fn new(context: ToolContext) -> Self {
        let spec = spec(
            "write",
            "Create or replace a file. Inspect existing files before overwriting. A replaced file keeps its text encoding when it can.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to write, resolved by the filesystem backend." },
                    "content": { "type": "string", "description": "Full UTF-8 text content to write." },
                },
                "required": ["file_path", "content"],
            }),
        );
        Self { context, spec }
    }

    async fn run(&self, call_id: String, args: Value) -> Result<ToolOutput, String> {
        let params: WriteParams = params(args)?;
        let context = &self.context;
        let (absolute, display) = context.resolve(&params.file_path)?;
        let current = version_of(&absolute)?;
        context.observations.assert_current(&absolute, current.clone(), &display, "write")?;
        // `Some(None)`: the file is new; `None`: too large to keep for undo.
        let original: Option<Option<Vec<u8>>> = match &current {
            None => Some(None),
            Some(_) => {
                let size = std::fs::metadata(&absolute).map_err(|error| error.to_string())?.len();
                if size > MAX_SNAPSHOT_BYTES { None } else { Some(Some(std::fs::read(&absolute).map_err(|error| error.to_string())?)) }
            }
        };
        let old_bytes = original.clone().flatten();
        let decoded = old_bytes.as_deref().and_then(decode);
        let before = old_bytes.as_ref().filter(|bytes| bytes.len() <= DIFF_TEXT_LIMIT).map(|bytes| decoded.as_ref().map_or_else(|| String::from_utf8_lossy(bytes).into_owned(), |decoded| decoded.text.clone()));
        // A replaced file keeps its encoding and byte-order mark when its text converts back unchanged.
        let kept = match (&old_bytes, decoded) {
            (Some(bytes), Some(decoded)) if round_trips(bytes, &decoded) => Some(decoded),
            _ => None,
        };
        let stored = kept.as_ref().and_then(|kept| {
            let content = if kept.bom { params.content.strip_prefix('\u{feff}').unwrap_or(&params.content) } else { &params.content };
            encode(kept.encoding, kept.bom, content)
        });
        let bytes = stored.clone().unwrap_or_else(|| params.content.clone().into_bytes());
        write_file(&absolute, &bytes)?;
        if let Some(written) = version_of(&absolute)? {
            context.observations.record(&absolute, Some(written));
        }
        context.touched(&absolute);
        let undoable = keep_for_undo(context, &call_id, &absolute, original, bytes).await;
        let operation = if current.is_none() { "create" } else { "update" };
        let encoding = match &kept {
            Some(kept) if kept.encoding != Encoding::Utf8 && stored.is_none() => format!(" as UTF-8: {}, its previous encoding, cannot store some of the new characters", kept.encoding.label()),
            Some(kept) if kept.encoding != Encoding::Utf8 => format!(" in its {} encoding", kept.encoding.label()),
            _ => String::new(),
        };
        let mut details = json!({ "path": display, "operation": operation });
        diff_sides(&mut details, before.as_deref(), &params.content);
        if undoable {
            details["undoable"] = json!(true);
        }
        let verb = if operation == "create" { "Created" } else { "Updated" };
        Ok(ToolOutput::text(format!("<path>{display}</path>\n<type>file</type>\n<content>\n{verb} file{encoding}\n</content>"), details))
    }
}

impl Tool for Write {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(self.run(call_id, args))
    }
}

/// Replace literal text in LF-normalized content; CRLF inside the strings is normalized too.
pub fn apply_edit(content: &str, old: &str, new: &str, replace_all: bool, display: &str) -> Result<String, String> {
    let (old, new) = (old.replace("\r\n", "\n"), new.replace("\r\n", "\n"));
    if old.is_empty() {
        return Err("old_string must be a non-empty string".into());
    }
    if old == new {
        return Err("old_string and new_string must differ".into());
    }
    let matches = content.matches(&old).count();
    if matches == 0 {
        return Err(format!("old_string was not found in \"{display}\""));
    }
    if !replace_all && matches > 1 {
        return Err(format!("old_string matched {matches} times in \"{display}\"; provide a more specific old_string or set replace_all to true"));
    }
    Ok(if replace_all { content.replace(&old, &new) } else { content.replacen(&old, &new, 1) })
}

#[derive(Deserialize)]
struct EditParams {
    file_path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

pub struct Edit {
    context: ToolContext,
    spec: ToolSpec,
}

impl Edit {
    pub fn new(context: ToolContext) -> Self {
        let spec = spec(
            "edit",
            "Apply an exact text replacement to a previously read file. The file keeps its text encoding.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to edit, resolved by the filesystem backend." },
                    "old_string": { "type": "string", "description": "Literal text to replace." },
                    "new_string": { "type": "string", "description": "Literal replacement text. Use an empty string to delete the match." },
                    "replace_all": { "type": "boolean", "description": "Replace all matches. Defaults to false; when false, old_string must appear exactly once." },
                },
                "required": ["file_path", "old_string", "new_string"],
            }),
        );
        Self { context, spec }
    }

    async fn run(&self, call_id: String, args: Value) -> Result<ToolOutput, String> {
        let params: EditParams = params(args)?;
        let context = &self.context;
        let (absolute, display) = context.resolve(&params.file_path)?;
        let current = version_of(&absolute)?.ok_or_else(|| format!("cannot modify \"{display}\": not found"))?;
        context.observations.assert_current(&absolute, Some(current), &display, "modify")?;
        let bytes = std::fs::read(&absolute).map_err(|error| error.to_string())?;
        let decoded = editable(&bytes, &display)?;
        // The file keeps its dominant line ending.
        let sample = crate::text::truncate_chars(&decoded.text, 4096);
        let crlf = sample.matches("\r\n").count();
        let lf = sample.matches('\n').count() - crlf;
        let normalized = decoded.text.replace("\r\n", "\n");
        let edited = apply_edit(&normalized, &params.old_string, &params.new_string, params.replace_all, &display)?;
        let text = if crlf > lf { edited.replace('\n', "\r\n") } else { edited.clone() };
        let output = encode(decoded.encoding, decoded.bom, &text)
            .ok_or_else(|| format!("cannot modify \"{display}\": it is stored as {}, which cannot store some characters of new_string", decoded.encoding.label()))?;
        std::fs::write(&absolute, &output).map_err(|error| error.to_string())?;
        if let Some(written) = version_of(&absolute)? {
            context.observations.record(&absolute, Some(written));
        }
        context.touched(&absolute);
        let before = (bytes.len() as u64 <= MAX_SNAPSHOT_BYTES).then(|| Some(bytes));
        let undoable = keep_for_undo(context, &call_id, &absolute, before, output).await;
        let message = if params.replace_all {
            format!("The file {display} has been updated. All occurrences were successfully replaced.")
        } else {
            format!("The file {display} has been updated successfully.")
        };
        let mut details = json!({ "path": display, "operation": "update" });
        diff_sides(&mut details, Some(&normalized), &edited);
        if undoable {
            details["undoable"] = json!(true);
        }
        Ok(ToolOutput::text(message, details))
    }
}

impl Tool for Edit {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(self.run(call_id, args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_like_3x() {
        assert_eq!(apply_edit("a\nb\na", "b", "c", false, "f").unwrap(), "a\nc\na");
        assert!(apply_edit("a\na", "a", "b", false, "f").unwrap_err().contains("matched 2 times"));
        assert_eq!(apply_edit("a\na", "a", "b", true, "f").unwrap(), "b\nb");
        assert_eq!(apply_edit("x\ny", "x\r\ny", "z", false, "f").unwrap(), "z");
        assert!(apply_edit("x", "q", "z", false, "f").is_err());
    }
}
