//! Hunk-level diff extraction and per-hunk accept/revert application built on `similar`.

use std::ops::Range;

use similar::{ChangeTag, TextDiff};

/// Context lines shown before and after each change group.
const CONTEXT_RADIUS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLineKind {
    Context,
    Removed,
    Added,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkLine {
    pub kind: HunkLineKind,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    /// Line text without trailing CR/LF for display.
    pub text: String,
}

/// One contiguous diff hunk with its exact line ranges in `base` and `current`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub index: usize,
    pub header: String,
    /// Exact line range of the actual changes (excluding surrounding context) in `base`.
    pub old_change_range: Range<usize>,
    /// Exact line range of the actual changes (excluding surrounding context) in `current`.
    pub new_change_range: Range<usize>,
    pub added: usize,
    pub removed: usize,
    pub lines: Vec<HunkLine>,
}

fn split_lines_preserving_endings(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split_inclusive('\n').collect()
}

fn strip_eol(line: &str) -> String {
    line.trim_end_matches(['\r', '\n']).to_owned()
}

/// Compute interactive diff hunks between `base` and `current`.
pub fn compute_hunks(base: &str, current: &str) -> Vec<DiffHunk> {
    let diff = TextDiff::from_lines(base, current);
    let groups = diff.grouped_ops(CONTEXT_RADIUS);
    let mut hunks = Vec::with_capacity(groups.len());
    for (index, group) in groups.into_iter().enumerate() {
        let mut old_start = usize::MAX;
        let mut old_end = 0usize;
        let mut new_start = usize::MAX;
        let mut new_end = 0usize;
        let mut old_change_start = usize::MAX;
        let mut old_change_end = 0usize;
        let mut new_change_start = usize::MAX;
        let mut new_change_end = 0usize;
        let mut added = 0usize;
        let mut removed = 0usize;
        let mut lines = Vec::new();

        for op in &group {
            let old_r = op.old_range();
            let new_r = op.new_range();
            old_start = old_start.min(old_r.start);
            old_end = old_end.max(old_r.end);
            new_start = new_start.min(new_r.start);
            new_end = new_end.max(new_r.end);
            if !matches!(op, similar::DiffOp::Equal { .. }) {
                old_change_start = old_change_start.min(old_r.start);
                old_change_end = old_change_end.max(old_r.end);
                new_change_start = new_change_start.min(new_r.start);
                new_change_end = new_change_end.max(new_r.end);
            }
            for change in diff.iter_changes(op) {
                match change.tag() {
                    ChangeTag::Equal => lines.push(HunkLine {
                        kind: HunkLineKind::Context,
                        old_line: change.old_index().map(|i| i + 1),
                        new_line: change.new_index().map(|i| i + 1),
                        text: strip_eol(change.value()),
                    }),
                    ChangeTag::Delete => {
                        removed += 1;
                        lines.push(HunkLine {
                            kind: HunkLineKind::Removed,
                            old_line: change.old_index().map(|i| i + 1),
                            new_line: None,
                            text: strip_eol(change.value()),
                        });
                    }
                    ChangeTag::Insert => {
                        added += 1;
                        lines.push(HunkLine {
                            kind: HunkLineKind::Added,
                            old_line: None,
                            new_line: change.new_index().map(|i| i + 1),
                            text: strip_eol(change.value()),
                        });
                    }
                }
            }
        }
        if added == 0 && removed == 0 {
            continue;
        }
        let old_count = old_end.saturating_sub(old_start);
        let new_count = new_end.saturating_sub(new_start);
        let old_disp_start = if old_count == 0 { old_start } else { old_start + 1 };
        let new_disp_start = if new_count == 0 { new_start } else { new_start + 1 };
        let header = format!("@@ -{old_disp_start},{old_count} +{new_disp_start},{new_count} @@");
        let old_change_range = if old_change_start <= old_change_end { old_change_start..old_change_end } else { old_start..old_end };
        let new_change_range = if new_change_start <= new_change_end { new_change_start..new_change_end } else { new_start..new_end };
        hunks.push(DiffHunk { index, header, old_change_range, new_change_range, added, removed, lines });
    }
    hunks
}

fn splice_lines(target: &[&str], replace: Range<usize>, replacement: &[&str]) -> String {
    let mut out = String::new();
    let start = replace.start.min(target.len());
    let end = replace.end.min(target.len());
    for line in &target[..start] {
        out.push_str(line);
    }
    for line in replacement {
        out.push_str(line);
    }
    for line in &target[end..] {
        out.push_str(line);
    }
    out
}

/// Apply a review decision on hunk `hunk_index`, returning `(next_base, next_current)`:
/// - `accept == true`: accept the hunk in review (advance `base` to match `current` for that hunk).
/// - `accept == false`: revert the hunk in the working file (restore `current` to `base` for that hunk).
pub fn apply_hunk_decision(base: &str, current: &str, hunk_index: usize, accept: bool) -> Option<(String, String)> {
    let hunks = compute_hunks(base, current);
    let hunk = hunks.get(hunk_index)?;
    let old_lines = split_lines_preserving_endings(base);
    let new_lines = split_lines_preserving_endings(current);
    if accept {
        let replacement = &new_lines[hunk.new_change_range.start.min(new_lines.len())..hunk.new_change_range.end.min(new_lines.len())];
        let next_base = splice_lines(&old_lines, hunk.old_change_range.clone(), replacement);
        Some((next_base, current.to_owned()))
    } else {
        let replacement = &old_lines[hunk.old_change_range.start.min(old_lines.len())..hunk.old_change_range.end.min(old_lines.len())];
        let next_current = splice_lines(&new_lines, hunk.new_change_range.clone(), replacement);
        Some((base.to_owned(), next_current))
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn computes_and_reverts_individual_hunks_independently() {
        let base = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nline11\nline12\n";
        let current = "line1\nLINE2_MOD\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nLINE11_MOD\nline12\n";
        let hunks = compute_hunks(base, current);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].added, 1);
        assert_eq!(hunks[0].removed, 1);

        // Revert only the first hunk: second hunk stays modified.
        let (next_base, next_current) = apply_hunk_decision(base, current, 0, false).expect("revert hunk 0");
        assert_eq!(next_base, base);
        assert_eq!(next_current, "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nLINE11_MOD\nline12\n");
        assert_eq!(compute_hunks(&next_base, &next_current).len(), 1);

        // Accept the remaining hunk: no remaining diff.
        let (final_base, final_current) = apply_hunk_decision(&next_base, &next_current, 0, true).expect("accept remaining hunk");
        assert_eq!(final_base, final_current);
        assert!(compute_hunks(&final_base, &final_current).is_empty());
    }

    #[test]
    fn preserves_crlf_and_handles_add_or_delete_hunks() {
        let base = "a\r\nb\r\n";
        let current = "a\r\ninserted\r\nb\r\n";
        let hunks = compute_hunks(base, current);
        assert_eq!(hunks.len(), 1);
        let (_, reverted) = apply_hunk_decision(base, current, 0, false).unwrap();
        assert_eq!(reverted, base);
        let (accepted_base, _) = apply_hunk_decision(base, current, 0, true).unwrap();
        assert_eq!(accepted_base, current);
    }
}
