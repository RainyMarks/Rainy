//! `apply_patch`: the file-oriented diff format GPT models are trained to edit with in OpenAI's Codex (format and
//! matching rules after `codex-rs/apply-patch`, Apache-2.0). Offered to those models in place of `write` and `edit`.
//!
//! Every hunk is checked against the files before anything is written, so a patch that does not apply changes
//! nothing. Each changed file keeps its own undo snapshot, under the change id `<call id>#<n>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::encoding::{decode, encode, round_trips};
use super::write::{MAX_SNAPSHOT_BYTES, diff_sides, editable};
use super::{Tool, ToolContext, ToolOutput, Updates, version_of};
use crate::agent::llm::ToolSpec;

pub const NAME: &str = "apply_patch";

/// The patch grammar the Responses API constrains a freeform `apply_patch` call to (Codex `apply_patch.lark`).
pub const GRAMMAR: &str = r#"start: begin_patch hunk+ end_patch
begin_patch: "*** Begin Patch" LF
end_patch: "*** End Patch" LF?

hunk: add_hunk | delete_hunk | update_hunk
add_hunk: "*** Add File: " filename LF add_line+
delete_hunk: "*** Delete File: " filename LF
update_hunk: "*** Update File: " filename LF change_move? change?

filename: /(.+)/
add_line: "+" /(.*)/ LF -> line

change_move: "*** Move to: " filename LF
change: (change_context | change_line)+ eof_line?
change_context: ("@@" | "@@ " /(.+)/) LF
change_line: ("+" | "-" | " ") /(.*)/ LF
eof_line: "*** End of File" LF

%import common.LF
"#;

/// The description of the freeform tool, whose input the grammar shapes.
pub const FREEFORM_DESCRIPTION: &str = "Use the `apply_patch` tool to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.";

/// The description of the JSON function form, for APIs without freeform tools.
pub const FUNCTION_DESCRIPTION: &str = r#"Use the `apply_patch` tool to edit files.
Your patch language is a stripped-down, file-oriented diff format designed to be easy to parse and safe to apply. You can think of it as a high-level envelope:

*** Begin Patch
[ one or more file sections ]
*** End Patch

Within that envelope, you get a sequence of file operations.
You MUST include a header to specify the action you are taking.
Each operation starts with one of three headers:

*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).
*** Delete File: <path> - remove an existing file. Nothing follows.
*** Update File: <path> - patch an existing file in place (optionally with a rename).

May be immediately followed by *** Move to: <new path> if you want to rename the file.
Then one or more "hunks", each introduced by @@ (optionally followed by a hunk header).
Within a hunk each line starts with:

For instructions on [context_before] and [context_after]:
- By default, show 3 lines of code immediately above and 3 lines immediately below each change. If a change is within 3 lines of a previous change, do NOT duplicate the first change's [context_after] lines in the second change's [context_before] lines.
- If 3 lines of context is insufficient to uniquely identify the snippet of code within the file, use the @@ operator to indicate the class or function to which the snippet belongs. For instance, we might have:
@@ class BaseClass
[3 lines of pre-context]
- [old_code]
+ [new_code]
[3 lines of post-context]

- If a code block is repeated so many times in a class or function such that even a single @@ statement and 3 lines of context cannot uniquely identify the snippet of code, you can use multiple @@ statements to jump to the right context. For instance:

@@ class BaseClass
@@ 	 def method():
[3 lines of pre-context]
- [old_code]
+ [new_code]
[3 lines of post-context]

The full grammar definition is below:
Patch := Begin { FileOp } End
Begin := "*** Begin Patch" NEWLINE
End := "*** End Patch" NEWLINE
FileOp := AddFile | DeleteFile | UpdateFile
AddFile := "*** Add File: " path NEWLINE { "+" line NEWLINE }
DeleteFile := "*** Delete File: " path NEWLINE
UpdateFile := "*** Update File: " path NEWLINE [ MoveTo ] { Hunk }
MoveTo := "*** Move to: " newPath NEWLINE
Hunk := "@@" [ header ] NEWLINE { HunkLine } [ "*** End of File" NEWLINE ]
HunkLine := (" " | "-" | "+") text NEWLINE

A full patch can combine several operations:

*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-print("Hi")
+print("Hello, world!")
*** Delete File: obsolete.txt
*** End Patch

It is important to remember:

- You must include a header with your intended action (Add/Delete/Update)
- You must prefix new lines with `+` even when creating a new file
- File references can only be relative, NEVER ABSOLUTE."#;

/// One hunk of an update: the lines it replaces and their replacement, found after each `context` line in turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub context: Vec<String>,
    pub old: Vec<String>,
    pub new: Vec<String>,
    /// The old lines end the file.
    pub end_of_file: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Hunk {
    Add { path: String, content: String },
    Delete { path: String },
    Update { path: String, move_to: Option<String>, chunks: Vec<Chunk> },
}

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const ADD: &str = "*** Add File: ";
const DELETE: &str = "*** Delete File: ";
const UPDATE: &str = "*** Update File: ";
const MOVE: &str = "*** Move to: ";
const END_OF_FILE: &str = "*** End of File";

/// The patch inside a shell heredoc (`apply_patch <<'EOF' … EOF`), which models sometimes send.
fn unwrap_heredoc(text: &str) -> &str {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() >= 4
        && lines[0].contains("<<")
        && let Some(last) = lines.last()
        && lines[0].trim_end().trim_end_matches(['\'', '"']).ends_with(last.trim())
        && let (Some(start), Some(end)) = (text.find('\n'), text.rfind('\n'))
        && start < end
    {
        return &text[start + 1..end];
    }
    text
}

/// Read a patch; the message says what is wrong with it.
pub fn parse(patch: &str) -> Result<Vec<Hunk>, String> {
    let text = unwrap_heredoc(patch.trim());
    let lines: Vec<&str> = text.lines().map(|line| line.strip_suffix('\r').unwrap_or(line)).collect();
    if lines.first().map(|line| line.trim()) != Some(BEGIN) {
        return Err(format!("invalid patch: the first line must be '{BEGIN}'"));
    }
    if lines.len() < 2 || lines.last().map(|line| line.trim()) != Some(END) {
        return Err(format!("invalid patch: the last line must be '{END}'"));
    }
    let body = &lines[1..lines.len() - 1];
    let mut hunks = Vec::new();
    let mut at = 0;
    while at < body.len() {
        let line = body[at].trim_end();
        let header = line.trim_start();
        if header.is_empty() {
            at += 1;
            continue;
        }
        let path_of = |rest: &str| -> Result<String, String> {
            let path = rest.trim();
            if path.is_empty() { Err(format!("invalid patch: '{line}' names no file")) } else { Ok(path.to_owned()) }
        };
        if let Some(rest) = header.strip_prefix(ADD) {
            let path = path_of(rest)?;
            at += 1;
            let mut content = String::new();
            while at < body.len() && !is_header(body[at]) {
                let Some(added) = body[at].strip_prefix('+') else {
                    return Err(format!("invalid patch: line {} of the new file {path} does not start with '+': {}", at + 2, body[at]));
                };
                content.push_str(added);
                content.push('\n');
                at += 1;
            }
            hunks.push(Hunk::Add { path, content });
        } else if let Some(rest) = header.strip_prefix(DELETE) {
            hunks.push(Hunk::Delete { path: path_of(rest)? });
            at += 1;
        } else if let Some(rest) = header.strip_prefix(UPDATE) {
            let path = path_of(rest)?;
            at += 1;
            let mut move_to = None;
            if let Some(rest) = body.get(at).and_then(|line| line.trim_start().strip_prefix(MOVE)) {
                move_to = Some(path_of(rest)?);
                at += 1;
            }
            let mut chunks: Vec<Chunk> = Vec::new();
            while at < body.len() && !is_header(body[at]) {
                let line = body[at];
                let trimmed = line.trim();
                if trimmed == "@@" || trimmed.starts_with("@@ ") {
                    let context = trimmed.strip_prefix("@@").map(str::trim).filter(|context| !context.is_empty()).map(str::to_owned);
                    // A second `@@` before any change line narrows the context further.
                    match chunks.last_mut() {
                        Some(chunk) if chunk.old.is_empty() && chunk.new.is_empty() && !chunk.end_of_file => chunk.context.extend(context),
                        _ => chunks.push(Chunk { context: context.into_iter().collect(), old: Vec::new(), new: Vec::new(), end_of_file: false }),
                    }
                    at += 1;
                    continue;
                }
                if trimmed == END_OF_FILE {
                    match chunks.last_mut() {
                        Some(chunk) => chunk.end_of_file = true,
                        None => return Err(format!("invalid patch: '{END_OF_FILE}' before any change in {path}")),
                    }
                    at += 1;
                    continue;
                }
                if chunks.last().is_none_or(|chunk| chunk.end_of_file) {
                    // The first hunk of a file may leave out its `@@`.
                    chunks.push(Chunk { context: Vec::new(), old: Vec::new(), new: Vec::new(), end_of_file: false });
                }
                let chunk = chunks.last_mut().unwrap();
                match line.chars().next() {
                    Some('+') => chunk.new.push(line[1..].to_owned()),
                    Some('-') => chunk.old.push(line[1..].to_owned()),
                    Some(' ') => {
                        chunk.old.push(line[1..].to_owned());
                        chunk.new.push(line[1..].to_owned());
                    }
                    // A blank line is an empty context line whose leading space was lost.
                    None => {
                        chunk.old.push(String::new());
                        chunk.new.push(String::new());
                    }
                    _ => {
                        return Err(format!(
                            "invalid patch: line {} in the update of {path} must start with ' ', '+', '-' or '@@': {line}",
                            at + 2
                        ));
                    }
                }
                at += 1;
            }
            chunks.retain(|chunk| !(chunk.old.is_empty() && chunk.new.is_empty()));
            if chunks.is_empty() && move_to.is_none() {
                return Err(format!("invalid patch: the update of {path} has no changes"));
            }
            hunks.push(Hunk::Update { path, move_to, chunks });
        } else {
            return Err(format!("invalid patch: '{line}' is not a valid hunk header; expected '{ADD}', '{DELETE}' or '{UPDATE}'"));
        }
    }
    if hunks.is_empty() {
        return Err("invalid patch: it changes no file".into());
    }
    Ok(hunks)
}

fn is_header(line: &str) -> bool {
    let line = line.trim_start();
    [ADD, DELETE, UPDATE].iter().any(|header| line.starts_with(header))
}

/// The paths a patch writes, created, deleted or moved to, in order; for the permission check.
pub fn paths(patch: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in unwrap_heredoc(patch.trim()).lines() {
        let line = line.trim();
        if let Some(rest) = [ADD, DELETE, UPDATE, MOVE].iter().find_map(|header| line.strip_prefix(header)) {
            let path = rest.trim();
            if !path.is_empty() {
                paths.push(path.to_owned());
            }
        }
    }
    paths
}

/// Typographic characters models and editors swap for their ASCII forms.
fn normalize(line: &str) -> String {
    line.trim()
        .chars()
        .map(|c| match c {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}' | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Where `pattern` first appears in `lines` at or after `start`, compared exactly, then ignoring trailing and then
/// surrounding whitespace, then also typographic punctuation; `end_of_file` looks at the file's end first.
fn seek(lines: &[String], pattern: &[String], start: usize, end_of_file: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let last = lines.len() - pattern.len();
    let from = if end_of_file { last } else { start };
    if from > last {
        return None;
    }
    type Equal = dyn Fn(&str, &str) -> bool;
    let passes: [&Equal; 4] = [
        &|a, b| a == b,
        &|a, b| a.trim_end() == b.trim_end(),
        &|a, b| a.trim() == b.trim(),
        &|a, b| normalize(a) == normalize(b),
    ];
    for equal in passes {
        if let Some(found) = (from..=last).find(|&index| pattern.iter().enumerate().all(|(offset, line)| equal(&lines[index + offset], line))) {
            return Some(found);
        }
    }
    // A hunk marked as ending the file may still match earlier.
    if end_of_file && start < from { seek(lines, pattern, start, false) } else { None }
}

/// The text of a file after an update's chunks; `text` has LF line ends.
pub fn apply_chunks(text: &str, chunks: &[Chunk], display: &str) -> Result<String, String> {
    let trailing_newline = text.is_empty() || text.ends_with('\n');
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut at = 0;
    for chunk in chunks {
        for context in &chunk.context {
            let found = seek(&lines, std::slice::from_ref(context), at, false).ok_or_else(|| format!("Failed to find context '{context}' in {display}"))?;
            at = found + 1;
        }
        if chunk.old.is_empty() {
            // Pure additions go at the end of the file, or right after their context.
            let index = if !chunk.context.is_empty() && !chunk.end_of_file { at } else { lines.len() };
            replacements.push((index, 0, chunk.new.clone()));
            continue;
        }
        let (mut old, mut new) = (chunk.old.as_slice(), chunk.new.as_slice());
        let mut found = seek(&lines, old, at, chunk.end_of_file);
        // A trailing empty line in the hunk stands for the file's final newline.
        if found.is_none() && old.last().is_some_and(String::is_empty) {
            old = &old[..old.len() - 1];
            if new.last().is_some_and(String::is_empty) {
                new = &new[..new.len() - 1];
            }
            found = seek(&lines, old, at, chunk.end_of_file);
        }
        let index = found.ok_or_else(|| format!("Failed to find expected lines in {display}:\n{}", chunk.old.join("\n")))?;
        replacements.push((index, old.len(), new.to_vec()));
        at = index + old.len();
    }
    replacements.sort_by_key(|(index, _, _)| *index);
    for (index, length, new) in replacements.into_iter().rev() {
        lines.splice(index..index + length, new);
    }
    let mut result = lines.join("\n");
    if trailing_newline && !result.is_empty() {
        result.push('\n');
    }
    Ok(result)
}

/// A file's planned state: its bytes now on disk and after the patch (`None`: absent).
struct Planned {
    absolute: PathBuf,
    display: String,
    original: Option<Vec<u8>>,
    result: Option<Vec<u8>>,
    moved_from: Option<String>,
}

pub struct ApplyPatch {
    context: ToolContext,
    spec: ToolSpec,
}

impl ApplyPatch {
    /// `freeform`: offered as a grammar-constrained custom tool (OpenAI Responses API) rather than a JSON function.
    pub fn new(context: ToolContext, freeform: bool) -> Self {
        let spec = if freeform {
            ToolSpec { name: NAME.into(), description: FREEFORM_DESCRIPTION.into(), parameters: json!({ "type": "object", "properties": { "input": { "type": "string" } }, "required": ["input"] }), freeform: Some(GRAMMAR.into()) }
        } else {
            ToolSpec {
                name: NAME.into(),
                description: FUNCTION_DESCRIPTION.into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "input": { "type": "string", "description": "The entire contents of the apply_patch command" } },
                    "required": ["input"],
                    "additionalProperties": false,
                }),
                freeform: None,
            }
        };
        Self { context, spec }
    }

    fn plan(&self, hunks: &[Hunk]) -> Result<Vec<Planned>, String> {
        let context = &self.context;
        let mut planned: Vec<Planned> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        // The file's planned state, read from disk the first time the patch touches it.
        let mut slot = |path: &str, planned: &mut Vec<Planned>| -> Result<usize, String> {
            let (absolute, display) = context.resolve(path)?;
            let key = super::path_key(&absolute);
            if let Some(at) = index.get(&key) {
                return Ok(*at);
            }
            let original = match std::fs::metadata(&absolute) {
                Ok(info) if info.is_dir() => return Err(format!("cannot patch \"{display}\": it is a directory")),
                Ok(_) => Some(std::fs::read(&absolute).map_err(|error| format!("cannot read \"{display}\": {error}"))?),
                Err(_) => None,
            };
            planned.push(Planned { absolute, display, result: original.clone(), original, moved_from: None });
            index.insert(key, planned.len() - 1);
            Ok(planned.len() - 1)
        };
        for hunk in hunks {
            match hunk {
                Hunk::Add { path, content } => {
                    let at = slot(path, &mut planned)?;
                    let file = &mut planned[at];
                    let bytes = match &file.result {
                        None => content.clone().into_bytes(),
                        Some(existing) => {
                            // Replacing a file the patch did not create needs it read first, as `write` does.
                            if file.original.is_some() && file.result == file.original {
                                context.observations.assert_current(&file.absolute, version_of(&file.absolute)?, &file.display, "write")?;
                            }
                            match decode(existing).filter(|decoded| round_trips(existing, decoded)) {
                                Some(decoded) => {
                                    let text = if decoded.text.contains("\r\n") { content.replace('\n', "\r\n") } else { content.clone() };
                                    encode(decoded.encoding, decoded.bom, &text).unwrap_or_else(|| content.clone().into_bytes())
                                }
                                None => content.clone().into_bytes(),
                            }
                        }
                    };
                    file.result = Some(bytes);
                }
                Hunk::Delete { path } => {
                    let at = slot(path, &mut planned)?;
                    let file = &mut planned[at];
                    if file.result.is_none() {
                        return Err(format!("cannot delete \"{}\": not found", file.display));
                    }
                    file.result = None;
                }
                Hunk::Update { path, move_to, chunks } => {
                    let at = slot(path, &mut planned)?;
                    let display = planned[at].display.clone();
                    let bytes = planned[at].result.clone().ok_or_else(|| format!("cannot update \"{display}\": not found"))?;
                    let decoded = editable(&bytes, &display)?;
                    let sample = crate::text::truncate_chars(&decoded.text, 4096);
                    let crlf = sample.matches("\r\n").count();
                    let lf = sample.matches('\n').count() - crlf;
                    let normalized = decoded.text.replace("\r\n", "\n");
                    let patched = apply_chunks(&normalized, chunks, &display)?;
                    let text = if crlf > lf { patched.replace('\n', "\r\n") } else { patched };
                    let output = encode(decoded.encoding, decoded.bom, &text)
                        .ok_or_else(|| format!("cannot update \"{display}\": it is stored as {}, which cannot store some of the new characters", decoded.encoding.label()))?;
                    match move_to {
                        Some(target) => {
                            let to = slot(target, &mut planned)?;
                            if to == at {
                                planned[at].result = Some(output);
                            } else {
                                if planned[to].result.is_some() {
                                    return Err(format!("cannot move \"{display}\" to \"{}\": a file already exists there", planned[to].display));
                                }
                                planned[at].result = None;
                                planned[to].result = Some(output);
                                planned[to].moved_from = Some(display);
                            }
                        }
                        None => planned[at].result = Some(output),
                    }
                }
            }
        }
        Ok(planned)
    }

    async fn run(&self, call_id: String, args: Value) -> Result<ToolOutput, String> {
        let input = args.get("input").and_then(Value::as_str).or_else(|| args.get("patch").and_then(Value::as_str)).ok_or("apply_patch needs the patch text in `input`")?;
        let hunks = parse(input)?;
        let planned = self.plan(&hunks)?;
        let context = &self.context;
        let mut summary = Vec::new();
        let mut files = Vec::new();
        for file in planned.iter().filter(|file| file.result != file.original) {
            match &file.result {
                Some(bytes) => {
                    if let Some(parent) = file.absolute.parent() {
                        std::fs::create_dir_all(parent).map_err(|error| applied_so_far(&summary, format!("cannot create the folder of \"{}\": {error}", file.display)))?;
                    }
                    std::fs::write(&file.absolute, bytes).map_err(|error| applied_so_far(&summary, format!("cannot write \"{}\": {error}", file.display)))?;
                    if let Some(written) = version_of(&file.absolute)? {
                        context.observations.record(&file.absolute, Some(written));
                    }
                }
                None => {
                    std::fs::remove_file(&file.absolute).map_err(|error| applied_so_far(&summary, format!("cannot delete \"{}\": {error}", file.display)))?;
                    context.observations.record(&file.absolute, None);
                }
            }
            context.touched(&file.absolute);
            let operation = match (&file.original, &file.result) {
                (None, _) => "create",
                (_, None) => "delete",
                _ => "update",
            };
            summary.push(format!("{} {}", match operation { "create" => 'A', "delete" => 'D', _ => 'M' }, file.display));
            let change_id = format!("{call_id}#{}", files.len());
            let small = file.original.as_ref().is_none_or(|bytes| bytes.len() as u64 <= MAX_SNAPSHOT_BYTES);
            let undoable = small && match &context.record_change {
                Some(record) => record(change_id.clone(), file.absolute.clone(), file.original.clone(), file.result.clone()).await,
                None => false,
            };
            let mut entry = json!({ "changeId": change_id, "path": file.display, "operation": operation });
            let text = |bytes: &Option<Vec<u8>>| bytes.as_deref().map(|bytes| decode(bytes).map_or_else(|| String::from_utf8_lossy(bytes).into_owned(), |decoded| decoded.text).replace("\r\n", "\n"));
            let (before, after) = (text(&file.original), text(&file.result));
            diff_sides(&mut entry, before.as_deref(), after.as_deref().unwrap_or_default());
            if let Some(from) = &file.moved_from {
                entry["movedFrom"] = json!(from);
            }
            if undoable {
                entry["undoable"] = json!(true);
            }
            files.push(entry);
        }
        if files.is_empty() {
            return Ok(ToolOutput::text("The patch applied, but it left every file as it was.", json!({ "files": [] })));
        }
        Ok(ToolOutput::text(format!("Success. Updated the following files:\n{}", summary.join("\n")), json!({ "files": files })))
    }
}

/// An error after part of a patch was written: the model must know which files already changed.
fn applied_so_far(summary: &[String], error: String) -> String {
    if summary.is_empty() { error } else { format!("{error}\nThese files were already changed:\n{}", summary.join("\n")) }
}

impl Tool for ApplyPatch {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(self.run(call_id, args))
    }
}

/// The absolute paths a patch writes, for the permission check.
pub fn targets(patch: &str, cwd: &Path) -> Vec<PathBuf> {
    paths(patch).iter().map(|path| crate::paths::normalize(&cwd.join(path))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn parses_every_hunk_kind() {
        let patch = "*** Begin Patch\n*** Add File: a.txt\n+one\n+two\n*** Delete File: b.txt\n*** Update File: c.py\n*** Move to: d.py\n@@ def f():\n-    x = 1\n+    x = 2\n*** End of File\n*** End Patch";
        let hunks = parse(patch).unwrap();
        assert_eq!(hunks[0], Hunk::Add { path: "a.txt".into(), content: "one\ntwo\n".into() });
        assert_eq!(hunks[1], Hunk::Delete { path: "b.txt".into() });
        assert_eq!(
            hunks[2],
            Hunk::Update {
                path: "c.py".into(),
                move_to: Some("d.py".into()),
                chunks: vec![Chunk { context: lines(&["def f():"]), old: lines(&["    x = 1"]), new: lines(&["    x = 2"]), end_of_file: true }],
            }
        );
        assert_eq!(paths(patch), ["a.txt", "b.txt", "c.py", "d.py"]);
        assert!(parse("*** Update File: a\n-x\n+y\n*** End Patch").unwrap_err().contains("first line"));
        assert!(parse("*** Begin Patch\n*** Frobnicate: a\n*** End Patch").unwrap_err().contains("not a valid hunk header"));
        // A heredoc wrapper and a missing first `@@` are accepted.
        let wrapped = "apply_patch <<'EOF'\n*** Begin Patch\n*** Update File: a\n-x\n+y\n*** End Patch\nEOF";
        assert_eq!(parse(wrapped).unwrap()[0], Hunk::Update { path: "a".into(), move_to: None, chunks: vec![Chunk { context: Vec::new(), old: lines(&["x"]), new: lines(&["y"]), end_of_file: false }] });
    }

    #[test]
    fn applies_chunks_with_lenient_matching() {
        let text = "fn a() {\n    one();\n}\n\nfn b() {\n    one();\n}\n";
        let chunks = vec![Chunk { context: lines(&["fn b() {"]), old: lines(&["    one();"]), new: lines(&["    two();"]), end_of_file: false }];
        assert_eq!(apply_chunks(text, &chunks, "f").unwrap(), "fn a() {\n    one();\n}\n\nfn b() {\n    two();\n}\n");
        // Trailing spaces and typographic dashes or quotes do not stop a match.
        let chunks = vec![Chunk { context: Vec::new(), old: lines(&["let s = \"a-b\";"]), new: lines(&["let s = \"c\";"]), end_of_file: false }];
        assert_eq!(apply_chunks("let s = \u{201C}a\u{2013}b\u{201D};  \n", &chunks, "f").unwrap(), "let s = \"c\";\n");
        // A file without a final newline keeps it that way; additions without old lines go at the end.
        let chunks = vec![Chunk { context: Vec::new(), old: Vec::new(), new: lines(&["z"]), end_of_file: true }];
        assert_eq!(apply_chunks("x\ny", &chunks, "f").unwrap(), "x\ny\nz");
        let missing = vec![Chunk { context: Vec::new(), old: lines(&["nope"]), new: lines(&["x"]), end_of_file: false }];
        assert!(apply_chunks("a\n", &missing, "f").unwrap_err().starts_with("Failed to find expected lines in f"));
    }

    #[tokio::test]
    async fn applies_a_patch_to_files() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        std::fs::write(dir.join("crlf.txt"), b"a\r\nb\r\nc\r\n").unwrap();
        // GB18030 text keeps its encoding.
        std::fs::write(dir.join("gbk.txt"), encode(super::super::encoding::Encoding::Gb18030, false, "\u{4f60}\u{597d}\nx\n").unwrap()).unwrap();
        std::fs::write(dir.join("old.txt"), b"keep\n").unwrap();
        std::fs::write(dir.join("gone.txt"), b"bye\n").unwrap();
        let tool = ApplyPatch::new(super::super::test_context(dir), false);
        let patch = "*** Begin Patch\n*** Update File: crlf.txt\n a\n-b\n+B\n c\n*** Update File: gbk.txt\n-x\n+\u{4e16}\u{754c}\n*** Update File: old.txt\n*** Move to: sub/new.txt\n-keep\n+kept\n*** Delete File: gone.txt\n*** Add File: made.txt\n+hello\n*** End Patch";
        let output = tool.run("c1".into(), json!({ "input": patch })).await.unwrap();
        assert_eq!(std::fs::read(dir.join("crlf.txt")).unwrap(), b"a\r\nB\r\nc\r\n");
        let gbk = decode(&std::fs::read(dir.join("gbk.txt")).unwrap()).unwrap();
        assert_eq!((gbk.encoding, gbk.text.as_str()), (super::super::encoding::Encoding::Gb18030, "\u{4f60}\u{597d}\n\u{4e16}\u{754c}\n"));
        assert!(!dir.join("old.txt").exists() && !dir.join("gone.txt").exists());
        assert_eq!(std::fs::read_to_string(dir.join("sub/new.txt")).unwrap(), "kept\n");
        assert_eq!(std::fs::read_to_string(dir.join("made.txt")).unwrap(), "hello\n");
        let files = output.details.as_ref().unwrap()["files"].as_array().unwrap().clone();
        let operations: Vec<(&str, &str)> = files.iter().map(|file| (file["path"].as_str().unwrap(), file["operation"].as_str().unwrap())).collect();
        assert_eq!(operations, [("crlf.txt", "update"), ("gbk.txt", "update"), ("old.txt", "delete"), ("sub/new.txt", "create"), ("gone.txt", "delete"), ("made.txt", "create")]);
        assert_eq!(files[3]["movedFrom"], "old.txt");
        assert_eq!(files[0]["changeId"], "c1#0");
        // A patch whose context is missing writes nothing at all.
        let bad = "*** Begin Patch\n*** Add File: first.txt\n+x\n*** Update File: crlf.txt\n-missing\n+y\n*** End Patch";
        assert!(tool.run("c2".into(), json!({ "input": bad })).await.unwrap_err().contains("Failed to find expected lines in crlf.txt"));
        assert!(!dir.join("first.txt").exists());
        // Replacing an existing file with Add File needs it read first.
        std::fs::write(dir.join("unread.txt"), b"x\n").unwrap();
        assert!(tool.run("c3".into(), json!({ "input": "*** Begin Patch\n*** Add File: unread.txt\n+new\n*** End Patch" })).await.unwrap_err().contains("has not been read"));
    }
}
