//! `grep` and `glob`: content and file-name search, with ripgrep's walk (the `ignore` crate) and its regular
//! expressions (the `regex` crate), in process. Both only read, so they never wait for approval.

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
        for file in files {
            if cancelled.load(Ordering::Relaxed) {
                return Err("tool call aborted".into());
            }
            if size > MAX_OUTPUT_BYTES || Instant::now() > deadline {
                truncated = true;
                break;
            }
            let Ok(bytes) = std::fs::read(&file) else { continue };
            // Binary files are skipped, unless named explicitly, where the search stops at the binary data.
            let bytes = match bytes.iter().position(|byte| *byte == 0) {
                Some(_) if !explicit => continue,
                Some(at) => bytes[..at].to_vec(),
                None => bytes,
            };
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)).collect();
            let lines = if text.ends_with('\n') { &lines[..lines.len() - 1] } else { &lines[..] };
            let shown = self.context.display(&file);
            match mode {
                "files_with_matches" => {
                    if lines.iter().any(|line| regex.is_match(line)) {
                        size += shown.len() + 1;
                        found_files.push(shown);
                    }
                }
                "count" => {
                    let count: usize = lines.iter().map(|line| regex.find_iter(line).count()).sum();
                    if count > 0 {
                        size += shown.len() + 8;
                        counts.push((shown, count));
                    }
                }
                _ => {
                    let matched: Vec<usize> = lines.iter().enumerate().filter(|(_, line)| regex.is_match(line)).map(|(index, _)| index).collect();
                    if matched.is_empty() {
                        continue;
                    }
                    let mut last: Option<usize> = None;
                    for &index in &matched {
                        let (start, end) = (index.saturating_sub(around), (index + around).min(lines.len() - 1));
                        let from = last.map_or(start, |last| start.max(last + 1));
                        if around > 0
                            && let Some(last) = last
                            && from > last + 1
                        {
                            rows.push(None);
                        }
                        if around > 0 && last.is_none() && !rows.is_empty() {
                            rows.push(None);
                        }
                        for at in from..=end {
                            let text = preview(lines[at]);
                            size += shown.len() + text.len() + 8;
                            rows.push(Some(Row { path: shown.clone(), number: at + 1, matched: matched.binary_search(&at).is_ok(), text }));
                        }
                        last = Some(end.max(last.unwrap_or(0)));
                    }
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
        std::fs::write(root.join("b.bin"), b"fn   binary").unwrap();
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
}
