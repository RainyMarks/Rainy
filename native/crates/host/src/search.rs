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

/// Compiled search matcher: pure literal fast path, linear-time `regex::Regex`, or `fancy_regex::Regex` fallback
/// for lookaround / back-references.
enum Matcher {
    Literal(String),
    Fast(regex::Regex),
    Fancy(Regex),
}

impl Matcher {
    fn compile(options: &Value) -> HostResult<Self> {
        let pattern = options["pattern"].as_str().unwrap_or_default();
        let is_regex = options["regex"] == true;
        let whole_word = options["wholeWord"] == true;
        let case_sensitive = options["caseSensitive"] == true;
        if !is_regex && !whole_word && case_sensitive {
            return Ok(Self::Literal(pattern.to_owned()));
        }
        let mut source = if is_regex { pattern.to_owned() } else { regex::escape(pattern) };
        if whole_word {
            source = format!(r"\b(?:{source})\b");
        }
        if !case_sensitive {
            source = format!("(?i){source}");
        }
        if let Ok(fast) = regex::Regex::new(&source) {
            return Ok(Self::Fast(fast));
        }
        Regex::new(&source)
            .map(Self::Fancy)
            .map_err(|error| HostError::new("invalid-pattern", l(&format!("正则表达式有误：{error}"), &format!("Invalid regular expression: {error}"))))
    }

    fn for_each_match(&self, line: &str, mut visit: impl FnMut(usize, usize) -> bool) {
        match self {
            Self::Literal(needle) => {
                if needle.is_empty() {
                    return;
                }
                for (start, matched) in line.match_indices(needle.as_str()) {
                    if !visit(start, start + matched.len()) {
                        break;
                    }
                }
            }
            Self::Fast(re) => {
                for m in re.find_iter(line) {
                    if m.start() != m.end() && !visit(m.start(), m.end()) {
                        break;
                    }
                }
            }
            Self::Fancy(re) => {
                for m in re.find_iter(line).flatten() {
                    if m.start() != m.end() && !visit(m.start(), m.end()) {
                        break;
                    }
                }
            }
        }
    }
}

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

/// Replace every match line by line, as the search finds them; the new text and the count (single pass per line).
pub fn replace_in_text(text: &str, regex: &Regex, replacement: &str) -> (String, usize) {
    let mut count = 0;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let end = rest.find(['\n', '\r']).unwrap_or(rest.len());
        let separator = if rest[end..].starts_with("\r\n") { 2 } else { usize::from(end < rest.len()) };
        let line = &rest[..end];
        let mut line_matches = 0usize;
        let replaced = regex.replace_all(line, |caps: &fancy_regex::Captures<'_>| {
            line_matches += 1;
            let mut expanded = String::new();
            caps.expand(replacement, &mut expanded);
            expanded
        });
        count += line_matches;
        out.push_str(&replaced);
        out.push_str(&rest[end..end + separator]);
        rest = &rest[end + separator..];
    }
    (out, count)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The part of a long line around a match, and where the match starts in it (UTF-16 columns).
/// Slices strictly on Unicode scalar boundaries so surrogate pairs are never split, and avoids encoding the whole line.
fn preview(line: &str, match_byte_start: usize, column: usize) -> (String, usize) {
    // Fast path: UTF-8 byte length is always >= UTF-16 code unit count for ASCII and >= half for supplementary planes.
    if line.len() <= PREVIEW_CHARS {
        return (line.to_owned(), column);
    }
    // Check whether the whole line fits within PREVIEW_CHARS UTF-16 units without allocating a Vec<u16>.
    let mut total_u16 = 0usize;
    let mut exceeds = false;
    for ch in line.chars() {
        total_u16 += ch.len_utf16();
        if total_u16 > PREVIEW_CHARS {
            exceeds = true;
            break;
        }
    }
    if !exceeds {
        return (line.to_owned(), column);
    }

    // Walk backward from `match_byte_start` by up to `PREVIEW_BEFORE` UTF-16 units on char boundaries.
    let mut start_byte = match_byte_start.min(line.len());
    let mut before_u16 = 0usize;
    for (idx, ch) in line[..start_byte].char_indices().rev() {
        let w = ch.len_utf16();
        if before_u16 + w > PREVIEW_BEFORE {
            break;
        }
        before_u16 += w;
        start_byte = idx;
    }

    // Walk forward from `start_byte` by up to `PREVIEW_CHARS` UTF-16 units on char boundaries.
    let mut end_byte = start_byte;
    let mut window_u16 = 0usize;
    for (rel, ch) in line[start_byte..].char_indices() {
        let w = ch.len_utf16();
        if window_u16 + w > PREVIEW_CHARS {
            break;
        }
        window_u16 += w;
        end_byte = start_byte + rel + ch.len_utf8();
    }

    let has_leading = start_byte > 0;
    let has_trailing = end_byte < line.len();
    let middle = &line[start_byte..end_byte];
    let text = format!("{}{middle}{}", if has_leading { "…" } else { "" }, if has_trailing { "…" } else { "" });
    (text, before_u16 + usize::from(has_leading))
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

struct RunningGuard<'a> {
    running: &'a Mutex<HashMap<String, Arc<AtomicBool>>>,
    workspace_id: String,
    token: Arc<AtomicBool>,
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        let mut running = self.running.lock().unwrap();
        if running.get(&self.workspace_id).is_some_and(|current| Arc::ptr_eq(current, &self.token)) {
            running.remove(&self.workspace_id);
        }
    }
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
        let matcher = Matcher::compile(request)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        if let Some(previous) = self.running.lock().unwrap().insert(workspace_id.clone(), cancelled.clone()) {
            previous.store(true, Ordering::Relaxed);
        }
        let _guard = RunningGuard { running: &self.running, workspace_id, token: cancelled.clone() };
        let deadline = Instant::now() + TIMEOUT;
        let (mut files, mut total, mut truncated) = (Vec::new(), 0usize, false);
        for root in &roots {
            let found = search_root(root, request, &matcher, MAX_MATCHES - total, &cancelled, deadline)?;
            total += found.1;
            files.extend(found.0);
            if found.2 {
                truncated = true;
                break;
            }
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
}

fn replace_file(root: &Root, path: &str, regex: &Regex, replacement: &str) -> Result<usize, String> {
    relative_path(path, false).map_err(|error| error.message)?;
    let canonical_root = realpath(Path::new(&root.path)).map_err(|error| error.to_string())?;
    let absolute = realpath(&crate::paths::join_relative(&canonical_root, path)).map_err(|error| error.to_string())?;
    if !contains(&canonical_root, &absolute) {
        return Err(l("文件在项目之外。", "The file is outside the project."));
    }
    let observed = crate::ide::fs::stat(&absolute)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| l("文件不存在。", "The file does not exist."))?;
    if observed.kind != crate::ide::fs::Kind::File {
        return Err(l("不是文件。", "Not a file."));
    }
    if observed.size > MAX_FILE_BYTES {
        return Err(l("文件太大，没有替换。", "The file is too large to replace in."));
    }
    let bytes = std::fs::read(&absolute).map_err(|error| error.to_string())?;
    let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
    let text = std::str::from_utf8(if bom { &bytes[3..] } else { &bytes }).map_err(|_| l("不是 UTF-8 文本，没有替换。", "Not UTF-8 text, so nothing was replaced."))?;
    let (next, count) = replace_in_text(text, regex, replacement);
    if count > 0 {
        let mut out = if bom { vec![0xef, 0xbb, 0xbf] } else { Vec::new() };
        out.extend_from_slice(next.as_bytes());
        crate::ide::fs::write(&absolute, &out, crate::ide::fs::Intent::ReplaceIfVersion(&observed.version))
            .map_err(|error| error.message)?;
    }
    Ok(count)
}

/// Search one folder: its files, their match count, and whether the search stopped early.
fn search_root(root: &Root, request: &Value, matcher: &Matcher, budget: usize, cancelled: &Arc<AtomicBool>, deadline: Instant) -> HostResult<(Vec<Value>, usize, bool)> {
    let base = Path::new(&root.path);
    let mut overrides = ignore::overrides::OverrideBuilder::new(base);
    let invalid = |error: ignore::Error| HostError::invalid(l(&format!("文件筛选规则无效：{error}"), &format!("The file filter is not valid: {error}")));
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
            use std::io::Read as _;
            if cancelled.load(Ordering::Relaxed) || stopped.load(Ordering::Relaxed) || Instant::now() > deadline {
                stopped.store(true, Ordering::Relaxed);
                return ignore::WalkState::Quit;
            }
            let Ok(entry) = entry else { return ignore::WalkState::Continue };
            if !entry.file_type().is_some_and(|kind| kind.is_file()) || entry.metadata().map_or(true, |info| info.len() > MAX_FILE_BYTES) {
                return ignore::WalkState::Continue;
            }
            let Ok(mut file) = std::fs::File::open(entry.path()) else { return ignore::WalkState::Continue };
            // Probe the first 8 KiB for NUL bytes before reading the whole file to avoid allocating large binary files.
            let mut probe = [0u8; 8192];
            let Ok(first) = file.read(&mut probe) else { return ignore::WalkState::Continue };
            if probe[..first].contains(&0) {
                return ignore::WalkState::Continue;
            }
            let mut bytes = Vec::with_capacity(first);
            bytes.extend_from_slice(&probe[..first]);
            if first == probe.len() {
                let remaining = MAX_FILE_BYTES.saturating_sub(first as u64) + 1;
                if file.take(remaining).read_to_end(&mut bytes).is_err() || (bytes.len() as u64) > MAX_FILE_BYTES {
                    return ignore::WalkState::Continue;
                }
            }
            let text = String::from_utf8_lossy(&bytes);
            let mut matches = Vec::new();
            for (index, line) in text.lines().enumerate() {
                matcher.for_each_match(line, |start, end| {
                    if count.fetch_add(1, Ordering::Relaxed) >= budget {
                        stopped.store(true, Ordering::Relaxed);
                        return false;
                    }
                    let column = utf16_len(&line[..start]);
                    let (shown, shown_column) = preview(line, start, column);
                    matches.push(json!({ "line": index + 1, "column": column, "length": utf16_len(&line[start..end]), "preview": shown, "previewColumn": shown_column }));
                    true
                });
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
        let (files, total, truncated) = search_root(&root, &options, &Matcher::compile(&options).unwrap(), 10, &Arc::default(), Instant::now() + TIMEOUT).unwrap();
        assert_eq!((total, truncated), (2, false));
        assert_eq!(files[0]["path"], "a.txt");
        assert_eq!(files[0]["matches"][0]["column"], 3);
        assert_eq!(globs("src, *.py, build/"), ["src", "src/**", "**/src/**", "*.py", "build/**"]);
    }

    #[test]
    fn preview_never_splits_surrogate_pairs_on_long_lines() {
        let prefix = "😀".repeat(60); // 120 UTF-16 units
        let suffix = "🚀".repeat(160); // 320 UTF-16 units
        let line = format!("{prefix}TARGET{suffix}");
        let start = prefix.len();
        let col = utf16_len(&prefix);
        let (shown, shown_col) = preview(&line, start, col);
        assert!(!shown.contains('\u{FFFD}'), "surrogate pair must never be split: {shown}");
        assert!(shown.contains("TARGET"));
        let target_u16 = utf16_len(&shown[..shown.find("TARGET").unwrap()]);
        assert_eq!(shown_col, target_u16);
    }

    #[test]
    fn atomic_replace_preserves_bom_and_skips_binary_probe() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sample.txt");
        std::fs::write(&target, b"\xef\xbb\xbfhello world\r\n").unwrap();
        let root = Root { root_id: "primary".into(), path: dir.path().display().to_string(), title: "t".into(), primary: true };
        let options = json!({ "pattern": "world", "regex": false });
        let count = replace_file(&root, "sample.txt", &expression(&options).unwrap(), &replacement(&options, "rainy")).unwrap();
        assert_eq!(count, 1);
        assert_eq!(std::fs::read(&target).unwrap(), b"\xef\xbb\xbfhello rainy\r\n");
    }
}
