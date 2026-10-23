//! Text search across a project's folders and replacement in the files it found (3.x `search/index.ts`). The walk is
//! ripgrep's own (`ignore`: .gitignore honoured, hidden files searched); patterns use JavaScript's syntax, lookaround
//! and back-references included.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fancy_regex::Regex;
use serde_json::{Value, json};

use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::paths::{contains, realpath, relative_path};
use crate::projects::{Projects, Root};

/// Matches a search returns at most; the rest of the project is not searched.
pub const MAX_MATCHES: usize = 5000;
const TIMEOUT: Duration = Duration::from_secs(20);
/// Files larger than this are neither searched nor replaced in.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Lines longer than this are previewed around their match.
const PREVIEW_CHARS: usize = 240;
const PREVIEW_BEFORE: usize = 40;
const EXCLUDED: [&str; 4] = [".git", "node_modules", ".venv", "__pycache__"];

/// The expression a search runs: literal unless `regex`, case-insensitive unless `caseSensitive`, `\b…\b` for whole words.
pub fn expression(options: &Value) -> HostResult<Regex> {
    let pattern = options["pattern"].as_str().unwrap_or_default();
    let mut source = if options["regex"] == true { pattern.to_owned() } else { fancy_regex::escape(pattern).into_owned() };
    if options["wholeWord"] == true {
        source = format!(r"\b(?:{source})\b");
    }
    if options["caseSensitive"] != true {
        source = format!("(?i){source}");
    }
    Regex::new(&source).map_err(|error| HostError::new("invalid-pattern", l(&format!("正则表达式有误：{error}"), &format!("Invalid regular expression: {error}"))))
}

/// A replacement in the regex crate's syntax: literal text as typed; for a pattern, `\n`, `\t`, `\\` expanded and
/// JavaScript's `$&` meaning the whole match.
fn replacement(options: &Value, text: &str) -> String {
    if options["regex"] != true {
        return text.replace('$', "$$");
    }
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('\\', Some('n')) => {
                chars.next();
                out.push('\n');
            }
            ('\\', Some('t')) => {
                chars.next();
                out.push('\t');
            }
            ('\\', Some('\\')) => {
                chars.next();
                out.push('\\');
            }
            ('$', Some('&')) => {
                chars.next();
                out.push_str("${0}");
            }
            _ => out.push(c),
        }
    }
    out
}

/// Replace every match line by line, as the search finds them; the new text and the count.
pub fn replace_in_text(text: &str, regex: &Regex, replacement: &str) -> (String, usize) {
    let mut count = 0;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let end = rest.find(['\n', '\r']).unwrap_or(rest.len());
        let separator = if rest[end..].starts_with("\r\n") { 2 } else { usize::from(end < rest.len()) };
        let line = &rest[..end];
        let found = regex.find_iter(line).filter(Result::is_ok).count();
        if found > 0 {
            count += found;
            out.push_str(&regex.replace_all(line, replacement));
        } else {
            out.push_str(line);
        }
        out.push_str(&rest[end..end + separator]);
        rest = &rest[end + separator..];
    }
    (out, count)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The part of a long line around a match, and where the match starts in it (UTF-16 columns).
fn preview(line: &str, column: usize) -> (String, usize) {
    let units: Vec<u16> = line.encode_utf16().collect();
    if units.len() <= PREVIEW_CHARS {
        return (line.to_owned(), column);
    }
    let start = column.saturating_sub(PREVIEW_BEFORE);
    let end = (start + PREVIEW_CHARS).min(units.len());
    let middle = String::from_utf16_lossy(&units[start..end]);
    let text = format!("{}{middle}{}", if start > 0 { "…" } else { "" }, if end < units.len() { "…" } else { "" });
    (text, column - start + usize::from(start > 0))
}

/// The typed include and exclude lists as globs; a plain name also covers a folder of that name.
pub fn globs(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|glob| !glob.is_empty())
        .flat_map(|glob| {
            let clean = glob.trim_start_matches("./").replace('\\', "/");
            if clean.ends_with('/') {
                vec![format!("{clean}**")]
            } else if clean.contains(['*', '?', '[', ']', '{', '}']) {
                vec![clean]
            } else {
                vec![clean.clone(), format!("{clean}/**"), format!("**/{clean}/**")]
            }
        })
        .collect()
}

pub struct TextSearch {
    projects: Arc<Projects>,
    /// The running search of each project; a newer one ends it.
    running: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl TextSearch {
    pub fn new(projects: Arc<Projects>) -> Self {
        Self { projects, running: Mutex::default() }
    }

    fn roots(&self, workspace_id: &str) -> HostResult<Vec<Root>> {
        self.projects.get(workspace_id).map(|project| project.roots()).ok_or_else(|| HostError::new("workspace-not-found", l("工作区不存在。", "This workspace does not exist.")))
    }

    /// Every match of a pattern in a project's folders, grouped by file in path order.
    pub fn search(&self, request: &Value) -> HostResult<Value> {
        if request["pattern"].as_str().unwrap_or_default().is_empty() {
            return Ok(json!({ "files": [], "matches": 0, "truncated": false }));
        }
        let workspace_id = request["workspaceId"].as_str().unwrap_or_default().to_owned();
        let roots = self.roots(&workspace_id)?;
        let regex = expression(request)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        if let Some(previous) = self.running.lock().unwrap().insert(workspace_id.clone(), cancelled.clone()) {
            previous.store(true, Ordering::Relaxed);
        }
        let deadline = Instant::now() + TIMEOUT;
        let (mut files, mut total, mut truncated) = (Vec::new(), 0usize, false);
        for root in &roots {
            let found = search_root(root, request, &regex, MAX_MATCHES - total, &cancelled, deadline)?;
            total += found.1;
            files.extend(found.0);
            if found.2 {
                truncated = true;
                break;
            }
        }
        let mut running = self.running.lock().unwrap();
        if running.get(&workspace_id).is_some_and(|current| Arc::ptr_eq(current, &cancelled)) {
            running.remove(&workspace_id);
        }
        Ok(json!({ "files": files, "matches": total, "truncated": truncated || cancelled.load(Ordering::Relaxed) }))
    }

    /// Replace every match in the listed files; files that are not UTF-8 text, too large or outside the project are
    /// reported, not changed.
    pub fn replace(&self, request: &Value) -> HostResult<Value> {
        let roots = self.roots(request["workspaceId"].as_str().unwrap_or_default())?;
        let regex = expression(request)?;
        let replacement = replacement(request, request["replacement"].as_str().unwrap_or_default());
        let (mut done, mut failed) = (Vec::new(), Vec::new());
        for file in request["files"].as_array().into_iter().flatten() {
            let (root_id, path) = (file["rootId"].as_str().unwrap_or_default(), file["path"].as_str().unwrap_or_default());
            let result = roots
                .iter()
                .find(|root| root.root_id == root_id)
                .ok_or_else(|| l("这个文件夹已不在项目中。", "This folder is no longer in the project."))
                .and_then(|root| replace_file(root, path, &regex, &replacement));
            match result {
                Ok(count) => done.push(json!({ "rootId": root_id, "path": path, "replaced": count })),
                Err(message) => failed.push(json!({ "rootId": root_id, "path": path, "message": message })),
            }
        }
        Ok(json!({ "files": done, "failed": failed }))
    }

    pub fn close(&self) {
        for cancelled in self.running.lock().unwrap().values() {
            cancelled.store(true, Ordering::Relaxed);
        }
    }
}

fn replace_file(root: &Root, path: &str, regex: &Regex, replacement: &str) -> Result<usize, String> {
    relative_path(path, false).map_err(|error| error.message)?;
    let canonical_root = realpath(Path::new(&root.path)).map_err(|error| error.to_string())?;
    let absolute = realpath(&crate::paths::join_relative(&canonical_root, path)).map_err(|error| error.to_string())?;
    if !contains(&canonical_root, &absolute) {
        return Err(l("文件在项目之外。", "The file is outside the project."));
    }
    let info = std::fs::symlink_metadata(&absolute).map_err(|error| error.to_string())?;
    if !info.is_file() {
        return Err(l("不是文件。", "Not a file."));
    }
    if info.len() > MAX_FILE_BYTES {
        return Err(l("文件太大，没有替换。", "The file is too large to replace in."));
    }
    let bytes = std::fs::read(&absolute).map_err(|error| error.to_string())?;
    let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
    let text = std::str::from_utf8(if bom { &bytes[3..] } else { &bytes }).map_err(|_| l("不是 UTF-8 文本，没有替换。", "Not UTF-8 text, so nothing was replaced."))?;
    let (next, count) = replace_in_text(text, regex, replacement);
    if count > 0 {
        let mut out = if bom { vec![0xef, 0xbb, 0xbf] } else { Vec::new() };
        out.extend_from_slice(next.as_bytes());
        std::fs::write(&absolute, out).map_err(|error| error.to_string())?;
    }
    Ok(count)
}

/// Search one folder: its files, their match count, and whether the search stopped early.
fn search_root(root: &Root, request: &Value, regex: &Regex, budget: usize, cancelled: &Arc<AtomicBool>, deadline: Instant) -> HostResult<(Vec<Value>, usize, bool)> {
    let base = Path::new(&root.path);
    let mut overrides = ignore::overrides::OverrideBuilder::new(base);
    let invalid = |error: ignore::Error| HostError::invalid(error.to_string());
    for glob in globs(request["include"].as_str().unwrap_or_default()) {
        overrides.add(&glob).map_err(invalid)?;
    }
    for glob in globs(request["exclude"].as_str().unwrap_or_default()) {
        overrides.add(&format!("!{glob}")).map_err(invalid)?;
    }
    for name in EXCLUDED {
        overrides.add(&format!("!**/{name}/**")).map_err(invalid)?;
        overrides.add(&format!("!{name}")).map_err(invalid)?;
    }
    let overrides = overrides.build().map_err(invalid)?;
    let walker = ignore::WalkBuilder::new(base).hidden(false).require_git(false).overrides(overrides).threads(4).build_parallel();
    let found: Mutex<Vec<(String, Vec<Value>)>> = Mutex::default();
    let count = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    walker.run(|| {
        Box::new(|entry| {
            if cancelled.load(Ordering::Relaxed) || stopped.load(Ordering::Relaxed) || Instant::now() > deadline {
                stopped.store(true, Ordering::Relaxed);
                return ignore::WalkState::Quit;
            }
            let Ok(entry) = entry else { return ignore::WalkState::Continue };
            if !entry.file_type().is_some_and(|kind| kind.is_file()) || entry.metadata().map_or(true, |info| info.len() > MAX_FILE_BYTES) {
                return ignore::WalkState::Continue;
            }
            let Ok(bytes) = std::fs::read(entry.path()) else { return ignore::WalkState::Continue };
            // ripgrep skips a file once it sees a NUL byte: binary data.
            if bytes[..bytes.len().min(8192)].contains(&0) {
                return ignore::WalkState::Continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let mut matches = Vec::new();
            for (index, line) in text.lines().enumerate() {
                for found in regex.find_iter(line).flatten() {
                    if found.start() == found.end() {
                        continue;
                    }
                    if count.fetch_add(1, Ordering::Relaxed) >= budget {
                        stopped.store(true, Ordering::Relaxed);
                        break;
                    }
                    let column = utf16_len(&line[..found.start()]);
                    let (shown, shown_column) = preview(line, column);
                    matches.push(json!({ "line": index + 1, "column": column, "length": utf16_len(found.as_str()), "preview": shown, "previewColumn": shown_column }));
                }
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
            }
            if !matches.is_empty() {
                let relative = entry.path().strip_prefix(base).map(|path| path.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                found.lock().unwrap().push((relative, matches));
            }
            if stopped.load(Ordering::Relaxed) { ignore::WalkState::Quit } else { ignore::WalkState::Continue }
        })
    });
    let mut files = found.into_inner().unwrap();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let total: usize = files.iter().map(|(_, matches)| matches.len()).sum();
    let values = files.into_iter().map(|(path, matches)| json!({ "rootId": root.root_id, "path": path, "matches": matches })).collect();
    Ok((values, total, stopped.load(Ordering::Relaxed)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_line_by_line_like_3x() {
        let options = json!({ "pattern": "(\\w+)@", "regex": true, "caseSensitive": true });
        let regex = expression(&options).unwrap();
        let (text, count) = replace_in_text("a@ b@\r\nc@\n", &regex, &replacement(&options, "<$1>\\n$&"));
        assert_eq!((text.as_str(), count), ("<a>\na@ <b>\nb@\r\n<c>\nc@\n", 3));
        let literal = json!({ "pattern": "a.b", "regex": false });
        let (text, count) = replace_in_text("a.b axb", &expression(&literal).unwrap(), &replacement(&literal, "$1"));
        assert_eq!((text.as_str(), count), ("$1 axb", 1));
        assert!(expression(&json!({ "pattern": "(?<=x)y", "regex": true })).is_ok());
    }

    #[test]
    fn finds_matches_with_utf16_columns() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "雨天 rain\nRAIN\n").unwrap();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        std::fs::write(dir.path().join("node_modules").join("b.txt"), "rain").unwrap();
        let root = Root { root_id: "primary".into(), path: dir.path().display().to_string(), title: "t".into(), primary: true };
        let options = json!({ "pattern": "rain" });
        let (files, total, truncated) = search_root(&root, &options, &expression(&options).unwrap(), 10, &Arc::default(), Instant::now() + TIMEOUT).unwrap();
        assert_eq!((total, truncated), (2, false));
        assert_eq!(files[0]["path"], "a.txt");
        assert_eq!(files[0]["matches"][0]["column"], 3);
        assert_eq!(globs("src, *.py, build/"), ["src", "src/**", "**/src/**", "*.py", "build/**"]);
    }
}
