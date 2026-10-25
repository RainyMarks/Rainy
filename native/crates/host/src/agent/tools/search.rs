//! `grep` and `glob`: content and file-name search, with ripgrep's walk (the `ignore` crate) and its regular
//! expressions (the `regex` crate), in process. Both only read, so they never wait for approval.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolContext, ToolOutput, Updates, params, spec};
use crate::agent::llm::ToolSpec;

/// Output gathered before formatting; the search stops beyond it.
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Lines or files returned when the model sets no limit.
pub const DEFAULT_RESULT_LIMIT: usize = 200;
const MAX_RESULT_LIMIT: usize = 2000;
/// Files whose modification times are compared to sort glob results; beyond this, the walk's order is kept.
const GLOB_SORT_LIMIT: usize = 5000;
/// Longer lines are shown cut, as ripgrep's `--max-columns 500 --max-columns-preview` prints them.
const MAX_COLUMNS: usize = 500;
/// The start of a file that decides, as in ripgrep, whether it is binary data or UTF-16 text.
const SNIFF_BYTES: usize = 8 * 1024;
/// Longer lines are searched in their first part only, so a file without line breaks is never held whole in memory.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
/// Text read between checks for Stop and the search deadline.
const CHECK_BYTES: usize = 256 * 1024;

fn clamp_limit(limit: Option<f64>) -> usize {
    (limit.unwrap_or(DEFAULT_RESULT_LIMIT as f64).floor().max(1.0) as usize).min(MAX_RESULT_LIMIT)
}

/// The search root and how a found path is shown: relative to the working directory when inside it.
fn search_root(context: &ToolContext, requested: Option<&str>) -> Result<PathBuf, String> {
    let requested = requested.filter(|path| !path.trim().is_empty()).unwrap_or(".");
    if requested.contains('\0') {
        return Err("invalid path".into());
    }
    Ok(crate::paths::normalize(&context.cwd.join(requested)))
}

/// The files of a walk: .gitignore honoured inside repositories, hidden files included, `.git` left out.
fn walk(root: &Path, glob: Option<&str>, cancelled: &AtomicBool, deadline: Instant) -> Result<Vec<PathBuf>, String> {
    let mut overrides = ignore::overrides::OverrideBuilder::new(root);
    overrides.add("!.git").map_err(|error| error.to_string())?;
    if let Some(glob) = glob.filter(|glob| !glob.trim().is_empty()) {
        overrides.add(glob).map_err(|error| format!("invalid glob: {error}"))?;
    }
    let overrides = overrides.build().map_err(|error| error.to_string())?;
    let mut files = Vec::new();
    for entry in ignore::WalkBuilder::new(root).hidden(false).overrides(overrides).build().flatten() {
        if cancelled.load(Ordering::Relaxed) {
            return Err("tool call aborted".into());
        }
        if Instant::now() > deadline {
            break;
        }
        if entry.file_type().is_some_and(|kind| kind.is_file()) {
            files.push(entry.into_path());
        }
    }
    Ok(files)
}

fn preview(line: &str) -> String {
    if line.len() <= MAX_COLUMNS {
        return line.to_owned();
    }
    let mut end = MAX_COLUMNS;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} [... omitted end of long line]", &line[..end])
}

/// A file's text for searching, or None when it cannot be opened or is binary data found by a walk. The first 8 KiB
/// decide, as in ripgrep: UTF-16 with a byte order mark (PowerShell 5.1 output, .reg and .rc files) is decoded, a UTF-8
/// byte order mark is dropped, and a NUL byte means binary data, searched up to the NUL only when the file was named.
fn open_text(path: &Path, explicit: bool) -> Option<Box<dyn BufRead>> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    file.by_ref().take(SNIFF_BYTES as u64).read_to_end(&mut head).ok()?;
    let utf16 = match head.get(..2) {
        Some([0xff, 0xfe]) => Some(encoding_rs::UTF_16LE),
        Some([0xfe, 0xff]) => Some(encoding_rs::UTF_16BE),
        _ => None,
    };
    if let Some(encoding) = utf16 {
        return Some(Box::new(BufReader::with_capacity(64 * 1024, Utf16Text::new(encoding, Cursor::new(head).chain(file)))));
    }
    if let Some(at) = head.iter().position(|byte| *byte == 0) {
        if !explicit {
            return None;
        }
        head.truncate(at);
        return Some(Box::new(Cursor::new(head)));
    }
    let bom = if head.starts_with(&[0xef, 0xbb, 0xbf]) { 3 } else { 0 };
    let mut head = Cursor::new(head);
    head.set_position(bom);
    Some(Box::new(BufReader::with_capacity(64 * 1024, head.chain(file))))
}

/// UTF-16 text read as UTF-8 a block at a time; the byte order mark is dropped and unpaired surrogates read as U+FFFD.
struct Utf16Text<R> {
    inner: R,
    decoder: encoding_rs::Decoder,
    input: Vec<u8>,
    output: Vec<u8>,
    position: usize,
    done: bool,
}

impl<R: Read> Utf16Text<R> {
    fn new(encoding: &'static encoding_rs::Encoding, inner: R) -> Self {
        Self { inner, decoder: encoding.new_decoder_with_bom_removal(), input: vec![0; 32 * 1024], output: Vec::new(), position: 0, done: false }
    }
}

impl<R: Read> Read for Utf16Text<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while self.position == self.output.len() && !self.done {
            let read = self.inner.read(&mut self.input)?;
            self.done = read == 0;
            // An output of the decoder's maximum length always takes the whole input.
            self.output.resize(self.decoder.max_utf8_buffer_length(read).unwrap_or(4 * read + 16), 0);
            let (_, _, written, _) = self.decoder.decode_to_utf8(&self.input[..read], &mut self.output, self.done);
            self.output.truncate(written);
            self.position = 0;
        }
        let count = buffer.len().min(self.output.len() - self.position);
        buffer[..count].copy_from_slice(&self.output[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

/// Read the next line without its line feed into `line`, keeping at most `cap` bytes of it; false at the end of the text.
fn next_line(reader: &mut dyn BufRead, line: &mut Vec<u8>, cap: usize) -> std::io::Result<bool> {
    line.clear();
    let mut started = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return Ok(started);
        }
        started = true;
        let (piece, used, ended) = match available.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&available[..at], at + 1, true),
            None => (available, available.len(), false),
        };
        let room = cap.saturating_sub(line.len());
        line.extend_from_slice(&piece[..piece.len().min(room)]);
        reader.consume(used);
        if ended {
            return Ok(true);
        }
    }
}

#[derive(Deserialize)]
struct GrepParams {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    output_mode: Option<String>,
    ignore_case: Option<bool>,
    context: Option<f64>,
    limit: Option<f64>,
}

/// One printed line of a content search.
struct Row {
    path: String,
    number: usize,
    matched: bool,
    text: String,
}

/// What a content search found, file by file, in the order the walk visited them.
enum Found {
    Files(Vec<String>),
    Counts(Vec<(String, usize)>),
    Rows(Vec<Option<Row>>),
}

pub struct Grep {
    context: ToolContext,
    spec: ToolSpec,
}

impl Grep {
    pub fn new(context: ToolContext) -> Self {
        let spec = spec(
            "grep",
            "Search file contents with a regular expression (ripgrep). Respects .gitignore, includes hidden files except .git, and skips binary files; use the shell for binaries.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression in ripgrep (Rust) syntax. Escape literal characters such as { } ( ) . and |." },
                    "path": { "type": "string", "description": "File or directory to search. Defaults to the session workspace." },
                    "glob": { "type": "string", "description": "Only search files whose path matches this glob, such as \"*.py\" or \"src/**/*.ts\"." },
                    "output_mode": { "type": "string", "enum": ["files_with_matches", "content", "count"], "description": "\"files_with_matches\" (default) lists matching files; \"content\" shows matching lines with line numbers; \"count\" shows matches per file." },
                    "ignore_case": { "type": "boolean", "description": "Match case-insensitively." },
                    "context": { "type": "number", "description": "Lines shown before and after each match in content mode." },
                    "limit": { "type": "number", "description": format!("Maximum lines or files returned. Defaults to {DEFAULT_RESULT_LIMIT}.") },
                },
                "required": ["pattern"],
            }),
        );
        Self { context, spec }
    }

    fn search(&self, params: GrepParams, cancelled: &AtomicBool) -> Result<ToolOutput, String> {
        if params.pattern.is_empty() {
            return Err("invalid pattern: expected a non-empty regular expression".into());
        }
        let regex = regex::RegexBuilder::new(&params.pattern).case_insensitive(params.ignore_case == Some(true)).build().map_err(|error| error.to_string())?;
        let root = search_root(&self.context, params.path.as_deref())?;
        let mode = params.output_mode.as_deref().unwrap_or("files_with_matches");
        let limit = clamp_limit(params.limit);
        let around = params.context.map_or(0, |lines| lines.floor().clamp(0.0, 20.0) as usize);
        let info = std::fs::metadata(&root).map_err(|_| format!("path not found: {}", params.path.as_deref().unwrap_or(".")))?;
        let deadline = Instant::now() + SEARCH_TIMEOUT;
        let mut files = if info.is_dir() { walk(&root, params.glob.as_deref(), cancelled, deadline)? } else { vec![root.clone()] };
        files.sort();
        let explicit = !info.is_dir();
        let (mut found_files, mut counts, mut rows) = (Vec::new(), Vec::new(), Vec::new());
        let mut size = 0;
        let mut truncated = false;
        let mut line = Vec::new();
        'files: for file in files {
            if cancelled.load(Ordering::Relaxed) {
                return Err("tool call aborted".into());
            }
            if size > MAX_OUTPUT_BYTES || Instant::now() > deadline {
                truncated = true;
                break;
            }
            let Some(mut reader) = open_text(&file, explicit) else { continue };
            let shown = self.context.display(&file);
            let (mut index, mut unchecked, mut count) = (0, 0, 0);
            // Content mode: unprinted lines kept for the context of the next match, the last printed line, and the
            // context lines still due after a match.
            let mut before: VecDeque<(usize, String)> = VecDeque::new();
            let (mut last, mut after): (Option<usize>, usize) = (None, 0);
            // A read error ends the file with what was found so far.
            while next_line(&mut *reader, &mut line, MAX_LINE_BYTES).unwrap_or(false) {
                unchecked += line.len() + 1;
                if unchecked >= CHECK_BYTES {
                    unchecked = 0;
                    if cancelled.load(Ordering::Relaxed) {
                        return Err("tool call aborted".into());
                    }
                    if Instant::now() > deadline {
                        truncated = true;
                        break 'files;
                    }
                }
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                // Binary data past the sniffed start ends the search of the file there, as in ripgrep.
                let binary = line.iter().position(|byte| *byte == 0);
                if let Some(at) = binary {
                    line.truncate(at);
                }
                let text = String::from_utf8_lossy(&line);
                match mode {
                    "files_with_matches" => {
                        if regex.is_match(&text) {
                            size += shown.len() + 1;
                            found_files.push(shown);
                            if found_files.len() > limit {
                                truncated = true;
                                break 'files;
                            }
                            continue 'files;
                        }
                    }
                    "count" => count += regex.find_iter(&text).count(),
                    _ => {
                        let mut push = |rows: &mut Vec<Option<Row>>, number: usize, matched: bool, text: String| {
                            size += shown.len() + text.len() + 8;
                            rows.push(Some(Row { path: shown.clone(), number: number + 1, matched, text }));
                        };
                        if regex.is_match(&text) {
                            let first = before.front().map_or(index, |(at, _)| *at);
                            if around > 0 && last.map_or(!rows.is_empty(), |last| first > last + 1) {
                                rows.push(None);
                            }
                            for (at, text) in before.drain(..) {
                                push(&mut rows, at, false, text);
                            }
                            push(&mut rows, index, true, preview(&text));
                            (last, after) = (Some(index), around);
                        } else if after > 0 {
                            push(&mut rows, index, false, preview(&text));
                            (last, after) = (Some(index), after - 1);
                        } else if around > 0 {
                            before.push_back((index, preview(&text)));
                            if before.len() > around {
                                before.pop_front();
                            }
                        }
                        // Rows past the limit are never shown; one more only marks the result as cut.
                        if size > MAX_OUTPUT_BYTES || rows.len() > limit {
                            truncated = true;
                            break 'files;
                        }
                    }
                }
                if binary.is_some() {
                    break;
                }
                index += 1;
            }
            if count > 0 {
                size += shown.len() + 8;
                counts.push((shown, count));
                if counts.len() > limit {
                    truncated = true;
                    break;
                }
            }
        }
        let found = match mode {
            "files_with_matches" => Found::Files(found_files),
            "count" => Found::Counts(counts),
            _ => Found::Rows(rows),
        };
        let base_path = self.context.display(&root);
        let mut details = json!({ "pattern": params.pattern, "path": if base_path.is_empty() { ".".to_owned() } else { base_path } });
        let lines: Vec<String> = match found {
            Found::Files(paths) => {
                let kept: Vec<String> = paths.iter().take(limit).cloned().collect();
                details["truncated"] = json!(truncated || paths.len() > limit);
                details["paths"] = json!(kept);
                kept
            }
            Found::Counts(counts) => {
                let kept: Vec<&(String, usize)> = counts.iter().take(limit).collect();
                details["truncated"] = json!(truncated || counts.len() > limit);
                details["counts"] = Value::Array(kept.iter().map(|(path, count)| json!({ "path": path, "count": count })).collect());
                kept.iter().map(|(path, count)| format!("{path}:{count}")).collect()
            }
            Found::Rows(rows) => {
                let kept = &rows[..rows.len().min(limit)];
                details["truncated"] = json!(truncated || rows.len() > limit);
                let mut groups: Vec<Value> = Vec::new();
                for row in kept.iter().flatten() {
                    if groups.last().is_none_or(|group| group["path"] != row.path) {
                        groups.push(json!({ "path": row.path, "matches": [] }));
                    }
                    groups.last_mut().unwrap()["matches"].as_array_mut().unwrap().push(json!({ "lineNumber": row.number, "line": row.text }));
                }
                details["files"] = Value::Array(groups);
                // Matches read path:line:text, context lines path-line-text, as grep prints them.
                kept.iter().map(|row| row.as_ref().map_or_else(|| "--".to_owned(), |row| format!("{}{}{}{}{}", row.path, if row.matched { ':' } else { '-' }, row.number, if row.matched { ':' } else { '-' }, row.text))).collect()
            }
        };
        if lines.is_empty() {
            return Ok(ToolOutput::text("No matches found.", details));
        }
        let note = if details["truncated"] == true {
            format!("\n[Showing the first {} {}; narrow the search with path or glob, or raise limit.]", lines.len(), if mode == "content" { "lines" } else { "files" })
        } else {
            String::new()
        };
        Ok(ToolOutput::text(self.context.limit_output("grep", lines.join("\n") + &note), details))
    }
}

/// Run a search off the async threads, stopping when the call is cancelled.
async fn blocking<T: Send + 'static>(cancel: CancellationToken, work: impl FnOnce(&AtomicBool) -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let task = tokio::task::spawn_blocking(move || work(&flag));
    tokio::select! {
        result = task => result.map_err(|error| error.to_string())?,
        () = cancel.cancelled() => {
            cancelled.store(true, Ordering::Relaxed);
            Err("tool call aborted".into())
        }
    }
}

impl Tool for Grep {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: GrepParams = params(args)?;
            let tool = Grep { context: self.context.clone(), spec: self.spec.clone() };
            blocking(cancel, move |cancelled| tool.search(params, cancelled)).await
        })
    }
}

#[derive(Deserialize)]
struct GlobParams {
    pattern: String,
    path: Option<String>,
    limit: Option<f64>,
}

pub struct Glob {
    context: ToolContext,
    spec: ToolSpec,
}

impl Glob {
    pub fn new(context: ToolContext) -> Self {
        let spec = spec(
            "glob",
            "Find files by name pattern, most recently modified first. Respects .gitignore and includes hidden files except .git.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob such as \"**/*.py\", \"*.pcap\" or \"src/**/test_*.c\". A pattern without \"/\" matches file names at any depth." },
                    "path": { "type": "string", "description": "Directory to search. Defaults to the session workspace." },
                    "limit": { "type": "number", "description": format!("Maximum files returned. Defaults to {DEFAULT_RESULT_LIMIT}.") },
                },
                "required": ["pattern"],
            }),
        );
        Self { context, spec }
    }

    fn search(&self, params: GlobParams, cancelled: &AtomicBool) -> Result<ToolOutput, String> {
        if params.pattern.trim().is_empty() {
            return Err("invalid pattern: expected a non-empty glob".into());
        }
        let root = search_root(&self.context, params.path.as_deref())?;
        if !std::fs::metadata(&root).is_ok_and(|info| info.is_dir()) {
            return Err(format!("not a directory: {}", params.path.as_deref().unwrap_or(".")));
        }
        let limit = clamp_limit(params.limit);
        let files = walk(&root, Some(&params.pattern), cancelled, Instant::now() + SEARCH_TIMEOUT)?;
        let mut timed: Vec<(PathBuf, std::time::SystemTime)> =
            files.iter().take(GLOB_SORT_LIMIT).map(|file| (file.clone(), std::fs::metadata(file).and_then(|info| info.modified()).unwrap_or(std::time::UNIX_EPOCH))).collect();
        timed.sort_by(|left, right| right.1.cmp(&left.1));
        let ordered: Vec<&PathBuf> = timed.iter().map(|(file, _)| file).chain(files.iter().skip(GLOB_SORT_LIMIT)).collect();
        let shown: Vec<String> = ordered.iter().take(limit).map(|file| self.context.display(file)).collect();
        let truncated = ordered.len() > limit;
        let base = self.context.display(&root);
        let details = json!({ "pattern": params.pattern, "path": if base.is_empty() { ".".to_owned() } else { base }, "truncated": truncated, "paths": shown });
        if shown.is_empty() {
            return Ok(ToolOutput::text("No files found.", details));
        }
        let note = if truncated { format!("\n[Showing {} of {} files; narrow the pattern or path.]", shown.len(), ordered.len()) } else { String::new() };
        Ok(ToolOutput::text(self.context.limit_output("glob", shown.join("\n") + &note), details))
    }
}

impl Tool for Glob {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: GlobParams = params(args)?;
            let tool = Glob { context: self.context.clone(), spec: self.spec.clone() };
            blocking(cancel, move |cancelled| tool.search(params, cancelled)).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(cwd: &Path) -> ToolContext {
        crate::agent::tools::test_context(cwd)
    }

    #[test]
    fn greps_and_globs() {
        let root = std::env::temp_dir().join(format!("rainy-grep-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn main() {}
let x = 1;
fn helper() {}
").unwrap();
        std::fs::write(root.join("b.bin"), b"fn \0 binary").unwrap();
        let grep = Grep::new(context(&root));
        let flag = AtomicBool::new(false);
        let params = |mode: &str| GrepParams { pattern: "^fn".into(), path: None, glob: None, output_mode: Some(mode.into()), ignore_case: None, context: Some(1.0), limit: None };
        let files = grep.search(params("files_with_matches"), &flag).unwrap();
        assert_eq!(files.details.unwrap()["paths"], json!(["src/a.rs"]));
        let content = grep.search(params("content"), &flag).unwrap();
        assert!(matches!(&content.content[0], rainy_protocol::chat::ResultContent::Text { text } if text == "src/a.rs:1:fn main() {}
src/a.rs-2-let x = 1;
src/a.rs:3:fn helper() {}"));
        let count = grep.search(params("count"), &flag).unwrap();
        assert_eq!(count.details.unwrap()["counts"], json!([{ "path": "src/a.rs", "count": 2 }]));
        let glob = Glob::new(context(&root)).search(GlobParams { pattern: "*.rs".into(), path: None, limit: None }, &flag).unwrap();
        assert_eq!(glob.details.unwrap()["paths"], json!(["src/a.rs"]));
        let _ = std::fs::remove_dir_all(root);
    }

    fn text(output: &ToolOutput) -> &str {
        match &output.content[0] {
            rainy_protocol::chat::ResultContent::Text { text } => text,
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn greps_utf16_late_binary_data_and_context_groups() {
        let root = std::env::temp_dir().join(format!("rainy-grep-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        // PowerShell 5.1 `>` writes UTF-16 LE with a byte order mark; .reg files may be UTF-16 BE.
        let utf16 = |big: bool, text: &str| {
            let mut bytes = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
            bytes.extend(text.encode_utf16().flat_map(|unit| if big { unit.to_be_bytes() } else { unit.to_le_bytes() }));
            bytes
        };
        std::fs::write(root.join("a.txt"), utf16(false, "other\r\nneedle 中文\r\n")).unwrap();
        std::fs::write(root.join("b.reg"), utf16(true, "needle\n")).unwrap();
        // A UTF-8 byte order mark does not hide the first line from `^`.
        std::fs::write(root.join("c.txt"), b"\xef\xbb\xbfneedle\n").unwrap();
        // NUL past the sniffed start ends the file there; earlier matches stay.
        let mut late = "needle\n".repeat(2).into_bytes();
        late.extend(std::iter::repeat_n(b'x', SNIFF_BYTES));
        late.extend(b"\0needle\n");
        std::fs::write(root.join("d.log"), late).unwrap();
        std::fs::write(root.join("e.bin"), b"needle\0").unwrap();
        std::fs::write(root.join("f.txt"), "needle\nx\nx\nx\nneedle\n").unwrap();
        let grep = Grep::new(context(&root));
        let flag = AtomicBool::new(false);
        let params = |mode: &str, context: f64| GrepParams { pattern: "^needle".into(), path: None, glob: None, output_mode: Some(mode.into()), ignore_case: None, context: Some(context), limit: None };
        let counts = grep.search(params("count", 0.0), &flag).unwrap();
        assert_eq!(
            counts.details.unwrap()["counts"],
            json!([{ "path": "a.txt", "count": 1 }, { "path": "b.reg", "count": 1 }, { "path": "c.txt", "count": 1 }, { "path": "d.log", "count": 2 }, { "path": "f.txt", "count": 2 }])
        );
        let content = grep.search(GrepParams { glob: Some("{a,f}.txt".into()), ..params("content", 1.0) }, &flag).unwrap();
        assert_eq!(text(&content), "a.txt-1-other\na.txt:2:needle 中文\n--\nf.txt:1:needle\nf.txt-2-x\n--\nf.txt-4-x\nf.txt:5:needle");
        // A named binary file is searched up to its first NUL.
        let named = grep.search(GrepParams { path: Some("e.bin".into()), ..params("files_with_matches", 0.0) }, &flag).unwrap();
        assert_eq!(named.details.unwrap()["paths"], json!(["e.bin"]));
        // The limit stops the search; the result is marked as cut.
        let limited = grep.search(GrepParams { limit: Some(2.0), ..params("files_with_matches", 0.0) }, &flag).unwrap();
        assert_eq!(limited.details.unwrap()["truncated"], json!(true));
        flag.store(true, Ordering::Relaxed);
        assert!(grep.search(params("content", 0.0), &flag).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn long_lines_are_kept_in_part() {
        let mut reader = Cursor::new(b"abcdef\r\ngh".to_vec());
        let mut line = Vec::new();
        assert!(next_line(&mut reader, &mut line, 4).unwrap());
        assert_eq!(line, b"abcd");
        assert!(next_line(&mut reader, &mut line, 4).unwrap());
        assert_eq!(line, b"gh");
        assert!(!next_line(&mut reader, &mut line, 4).unwrap());
    }
}

