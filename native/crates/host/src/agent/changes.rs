//! The files a chat's write and edit calls changed, for review, keep and undo (3.x `session.ts` and
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

/// Text of a changed file for comparison: no byte-order mark, LF line ends; `None` for binary or huge files.
pub fn change_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() > 4 * 1024 * 1024 || bytes[..bytes.len().min(8192)].contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
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

/// Calls whose undo was recorded in the transcript.
pub fn reverted(entries: &[TranscriptEntry]) -> HashSet<String> {
    entries
        .iter()
        .filter_map(|entry| if let TranscriptEntry::Context { reverted: Some(ids), .. } = entry { Some(ids.iter().cloned()) } else { None })
        .flatten()
        .collect()
}

/// Write and edit calls neither kept nor undone, grouped by file in the order of each file's first change.
pub fn pending(entries: &[TranscriptEntry], kept: &HashSet<String>, cwd: &str) -> Vec<Group> {
    let reverted = reverted(entries);
    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        let TranscriptEntry::ToolResult { tool_call_id, tool_name, is_error: false, details: Some(details), .. } = entry else { continue };
        if (tool_name != "write" && tool_name != "edit") || reverted.contains(tool_call_id) || kept.contains(tool_call_id) {
            continue;
        }
        let Some(path) = details.get("path").and_then(|value| value.as_str()) else { continue };
        let path = if Path::new(path).is_absolute() { PathBuf::from(path) } else { crate::paths::normalize(&Path::new(cwd).join(path)) };
        let path = path.to_string_lossy().into_owned();
        let at = *index.entry(path_key(&path)).or_insert_with(|| {
            groups.push(Group { path: path.clone(), calls: Vec::new() });
            groups.len() - 1
        });
        groups[at].calls.push(Call {
            id: tool_call_id.clone(),
            created: details.get("operation").and_then(|value| value.as_str()) == Some("create"),
            undoable: details.get("undoable").and_then(|value| value.as_bool()) == Some(true),
        });
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
    }
}
