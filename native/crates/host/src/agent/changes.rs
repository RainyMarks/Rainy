//! The files a chat's write, edit and apply_patch calls changed, for review, keep and undo (3.x `session.ts` and
//! `shared/line-diff.ts`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rainy_protocol::chat::TranscriptEntry;

/// Above this many differing lines the texts count as replaced rather than compared exactly.
const MAX_EDITS: usize = 4000;

fn lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.strip_suffix('\n').unwrap_or(text).split('\n').collect()
}

/// Lines a change added and removed (Myers' diff on lines); very different texts count as wholly replaced. Both texts
/// have LF line ends.
pub fn line_counts(before: &str, after: &str) -> (u64, u64) {
    let (a, b) = (lines(before), lines(after));
    let head = a.iter().zip(&b).take_while(|(left, right)| left == right).count();
    let tail = a[head..].iter().rev().zip(b[head..].iter().rev()).take_while(|(left, right)| left == right).count();
    let (a, b) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let (n, m) = (a.len() as i64, b.len() as i64);
    if n == 0 || m == 0 {
        return (m as u64, n as u64);
    }
    let max = (n + m).min(MAX_EDITS as i64);
    let offset = max + 1;
    let mut v = vec![0i64; (2 * max + 3) as usize];
    for d in 0..=max {
        let mut k = -d;
        while k <= d {
            let at = |k: i64| (offset + k) as usize;
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) { v[at(k + 1)] } else { v[at(k - 1)] + 1 };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                return (((d - n + m) / 2) as u64, ((d + n - m) / 2) as u64);
            }
            k += 2;
        }
    }
    (m as u64, n as u64)
}

/// Text of a changed file for comparison, read in its own encoding (UTF-8, UTF-16 with a byte-order mark or
/// GB18030, as the edit tools write it): no byte-order mark, LF line ends; `None` for binary or huge files.
pub fn change_text(bytes: &[u8]) -> Option<String> {
    let utf16 = bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]);
    if bytes.len() > 4 * 1024 * 1024 || (!utf16 && bytes[..bytes.len().min(8192)].contains(&0)) {
        return None;
    }
    // Read as UTF-8, a GB18030 file's Chinese would all become the same replacement characters.
    let text = crate::agent::tools::encoding::decode(bytes).map_or_else(|| String::from_utf8_lossy(bytes).into_owned(), |decoded| decoded.text);
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).replace("\r\n", "\n"))
}

/// How the platform's file system compares a path.
pub fn path_key(path: &str) -> String {
    if cfg!(windows) { path.to_lowercase() } else { path.to_owned() }
}

/// A path as the model knows it: relative to the working directory when inside it.
pub fn path_for_model(cwd: &str, path: &str) -> String {
    match crate::paths::relative_to(Path::new(cwd), Path::new(path)) {
        Some(inside) if !inside.is_empty() => inside,
        _ => path.to_owned(),
    }
}

/// One call that changed a file.
pub struct Call {
    pub id: String,
    pub created: bool,
    pub undoable: bool,
}

/// The calls that changed one file, oldest first.
pub struct Group {
    pub path: String,
    pub calls: Vec<Call>,
}

/// One file a tool call changed: the id its undo goes by, the path as the call reported it, and how.
pub struct FileChange<'a> {
    pub id: String,
    pub path: &'a str,
    pub created: bool,
    pub undoable: bool,
}

/// The tools that change files.
pub const CHANGE_TOOLS: &[&str] = &["write", "edit", crate::agent::tools::patch::NAME];

/// The files one successful call changed: a `write` or `edit` changes one file under its call id, an `apply_patch`
/// call each file of its `details.files` under the file's own change id.
pub fn file_changes<'a>(tool_name: &str, tool_call_id: &str, details: &'a serde_json::Value) -> Vec<FileChange<'a>> {
    let one = |id: String, details: &'a serde_json::Value| {
        let path = details.get("path").and_then(|value| value.as_str())?;
        Some(FileChange {
            id,
            path,
            created: details.get("operation").and_then(|value| value.as_str()) == Some("create"),
            undoable: details.get("undoable").and_then(|value| value.as_bool()) == Some(true),
        })
    };
    match tool_name {
        "write" | "edit" => one(tool_call_id.to_owned(), details).into_iter().collect(),
        crate::agent::tools::patch::NAME => details
            .get("files")
            .and_then(|files| files.as_array())
            .into_iter()
            .flatten()
            .filter_map(|file| one(file.get("changeId")?.as_str()?.to_owned(), file))
            .collect(),
        _ => Vec::new(),
    }
}

/// Calls whose undo was recorded in the transcript.
pub fn reverted(entries: &[TranscriptEntry]) -> HashSet<String> {
    entries
        .iter()
        .filter_map(|entry| if let TranscriptEntry::Context { reverted: Some(ids), .. } = entry { Some(ids.iter().cloned()) } else { None })
        .flatten()
        .collect()
}

/// File changes neither kept nor undone, grouped by file in the order of each file's first change.
pub fn pending(entries: &[TranscriptEntry], kept: &HashSet<String>, cwd: &str) -> Vec<Group> {
    let reverted = reverted(entries);
    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        let TranscriptEntry::ToolResult { tool_call_id, tool_name, is_error: false, details: Some(details), .. } = entry else { continue };
        for change in file_changes(tool_name, tool_call_id, details) {
            if reverted.contains(&change.id) || kept.contains(&change.id) {
                continue;
            }
            let path = if Path::new(change.path).is_absolute() { PathBuf::from(change.path) } else { crate::paths::normalize(&Path::new(cwd).join(change.path)) };
            let path = path.to_string_lossy().into_owned();
            let at = *index.entry(path_key(&path)).or_insert_with(|| {
                groups.push(Group { path: path.clone(), calls: Vec::new() });
                groups.len() - 1
            });
            groups[at].calls.push(Call { id: change.id, created: change.created, undoable: change.undoable });
        }
    }
    groups
}

/// Extract file paths mentioned in a unified diff (e.g. Codex `turn/diff/updated`).
pub fn diff_paths(unified_diff: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in unified_diff.lines() {
        let candidate = line
            .strip_prefix("+++ b/")
            .or_else(|| line.strip_prefix("+++ ").filter(|p| *p != "/dev/null"));
        if let Some(raw) = candidate {
            let trimmed = raw.split('\t').next().unwrap_or(raw).trim();
            if !trimmed.is_empty() && trimmed != "/dev/null" && !paths.iter().any(|p| p == trimmed) {
                paths.push(trimmed.to_owned());
            }
        }
    }
    paths
}

fn git_cmd(cwd: &Path, args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(crate::process::scrubbed_env())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut buf = Vec::new();
        let _ = stdout.take(4 * 1024 * 1024 + 1).read_to_end(&mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()?;
    status.success().then_some(std::process::Output { status, stdout: out, stderr: Vec::new() })
}

/// Read `HEAD:<rel>` bytes of a file in a Git repository for external-agent side-by-side Diff review.
pub fn git_head_content(cwd: &Path, path: &Path) -> Option<Vec<u8>> {
    let top_out = git_cmd(cwd, &["rev-parse", "--show-toplevel"])?;
    let top = PathBuf::from(String::from_utf8_lossy(&top_out.stdout).trim());
    let rel = crate::paths::relative_to(&top, path)?;
    let spec = format!("HEAD:{}", rel.replace('\\', "/"));
    let out = git_cmd(&top, &["show", &spec])?;
    (out.stdout.len() <= 4 * 1024 * 1024).then_some(out.stdout)
}

/// Merge dirty Git files and unified-diff paths into an external chat's pending review groups (`undoable: false`).
pub fn pending_external(entries: &[TranscriptEntry], kept: &HashSet<String>, cwd: &str, external_diff: Option<&str>) -> Vec<Group> {
    let mut groups = pending(entries, kept, cwd);
    let mut seen: HashSet<String> = groups.iter().map(|g| path_key(&g.path)).collect();
    let cwd_path = Path::new(cwd);

    // 1) Paths explicitly reported in the backend's unified diff.
    if let Some(diff) = external_diff {
        for rel in diff_paths(diff) {
            let abs = if Path::new(&rel).is_absolute() { PathBuf::from(&rel) } else { crate::paths::normalize(&cwd_path.join(&rel)) };
            let abs_str = abs.to_string_lossy().into_owned();
            let id = format!("ext-diff:{rel}");
            if kept.contains(&id) || !seen.insert(path_key(&abs_str)) {
                continue;
            }
            let created = git_head_content(cwd_path, &abs).is_none();
            groups.push(Group { path: abs_str, calls: vec![Call { id, created, undoable: false }] });
        }
    }

    // 2) Git workspace dirty files (catches edits made via shell commands or ACP tools without fileChange events).
    if let Some(out) = git_cmd(cwd_path, &["status", "--porcelain=v2", "-z", "--untracked-files=all", "--", "."])
        && let Some(top_out) = git_cmd(cwd_path, &["rev-parse", "--show-toplevel"])
    {
        let top = PathBuf::from(String::from_utf8_lossy(&top_out.stdout).trim());
        let text = String::from_utf8_lossy(&out.stdout);
        let fields: Vec<&str> = text.split('\0').collect();
        let mut idx = 0;
        while idx < fields.len() {
            let field = fields[idx];
            let (rel, created) = if field.starts_with("1 ") {
                let parts: Vec<&str> = field.split(' ').collect();
                let rel = parts.get(8..).map(|s| s.join(" ")).unwrap_or_default();
                let xy = parts.get(1).copied().unwrap_or("..");
                (rel, xy.starts_with('A'))
            } else if field.starts_with("2 ") {
                let parts: Vec<&str> = field.split(' ').collect();
                let rel = parts.get(9..).map(|s| s.join(" ")).unwrap_or_default();
                idx += 1;
                (rel, false)
            } else if let Some(rel) = field.strip_prefix("? ") {
                (rel.to_owned(), true)
            } else {
                idx += 1;
                continue;
            };
            idx += 1;
            if rel.is_empty() {
                continue;
            }
            let abs = crate::paths::normalize(&top.join(&rel));
            let abs_str = abs.to_string_lossy().into_owned();
            let id = format!("ext-git:{rel}");
            if kept.contains(&id) || !seen.insert(path_key(&abs_str)) {
                continue;
            }
            groups.push(Group { path: abs_str, calls: vec![Call { id, created, undoable: false }] });
        }
    }

    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_changed_lines() {
        assert_eq!(line_counts("a\nb\nc\n", "a\nx\nc\n"), (1, 1));
        assert_eq!(line_counts("", "a\nb"), (2, 0));
        assert_eq!(line_counts("a\nb\nc", "a\nc"), (0, 1));
        assert_eq!(line_counts("same", "same"), (0, 0));
        assert_eq!(change_text(b"\xef\xbb\xbfa\r\nb").as_deref(), Some("a\nb"));
        assert_eq!(change_text(b"a\0b"), None);
        let gbk = |text: &str| crate::agent::tools::encoding::encode(crate::agent::tools::encoding::Encoding::Gb18030, false, text).unwrap();
        assert_eq!(change_text(&gbk("旧文字\n")).as_deref(), Some("旧文字\n"));
        assert_ne!(change_text(&gbk("旧\n")), change_text(&gbk("新\n")));
    }

    #[test]
    fn lists_each_file_of_a_patch() {
        let result = |id: &str, name: &str, details: serde_json::Value| TranscriptEntry::ToolResult {
            id: format!("e-{id}"),
            ts: 0.0,
            tool_call_id: id.into(),
            tool_name: name.into(),
            content: Vec::new(),
            is_error: false,
            details: Some(details),
            duration_ms: None,
        };
        let entries = [
            result("w", "write", serde_json::json!({ "path": "a.txt", "operation": "create", "undoable": true })),
            result("p", "apply_patch", serde_json::json!({ "files": [
                { "changeId": "p#0", "path": "a.txt", "operation": "update", "undoable": true },
                { "changeId": "p#1", "path": "b.txt", "operation": "delete" },
            ] })),
        ];
        let groups = pending(&entries, &HashSet::from(["p#1".to_owned()]), "/w");
        assert_eq!(groups.len(), 1);
        let ids: Vec<&str> = groups[0].calls.iter().map(|call| call.id.as_str()).collect();
        assert_eq!(ids, ["w", "p#0"]);
        assert!(groups[0].calls[0].created && groups[0].calls[1].undoable);
    }

    #[test]
    fn extracts_paths_from_unified_diff() {
        let diff = "--- /dev/null\n+++ b/src/new.rs\t2026-11-23\n@@ -0,0 +1 @@\n+fn a() {}\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old\n+new\n--- a/removed.txt\n+++ /dev/null\n";
        assert_eq!(diff_paths(diff), vec!["src/new.rs", "README.md"]);
    }
}
