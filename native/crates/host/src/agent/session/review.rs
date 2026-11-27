//! File-change review, keep, side-by-side base lookup, and undo revert for [`Session`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use rainy_protocol::chat::{ChatChange, RevertFailure, RevertResult, TranscriptEntry};
use serde_json::json;

use super::super::changes::{self, change_text, line_counts, path_for_model, path_key};
use super::super::util;
use super::{Session, busy, is_busy};
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::text::now_ms;

const MAX_REVIEW_FILE_BYTES: u64 = 4 * 1024 * 1024;

impl Session {
    /// This chat and the chats it was forked from, nearest first; a fork can undo changes made before it.
    pub(super) fn lineage(&self) -> Vec<String> {
        let mut ids = vec![self.id.clone()];
        let mut parent = self.chat().parent;
        while let Some(next) = parent.filter(|next| ids.len() < 16 && !ids.contains(&next.session_id)) {
            parent = self.services.store.get(&next.session_id).and_then(|chat| chat.parent);
            ids.push(next.session_id);
        }
        ids
    }

    /// Resolve a scrubbed child environment including the workspace runtime PATH (so bundled MinGit is found on Windows).
    fn git_env(&self, cwd: &Path, workspace_id: Option<&str>) -> Option<BTreeMap<OsString, OsString>> {
        let resolved = workspace_id
            .and_then(|id| self.services.runtime.resolve_workspace(id).ok())
            .or_else(|| self.services.runtime.resolve(cwd).ok())?;
        Some(util::scrubbed_child_env(&resolved.environment))
    }

    /// Compute pending changes without holding `self.inner` across any external Git subprocess.
    fn pending_changes_blocking(&self) -> Vec<changes::Group> {
        let (entries, kept, cwd, is_external, workspace_id, external_diff) = {
            let inner = self.inner.lock().unwrap();
            (
                inner.entries.clone(),
                inner.kept.clone(),
                inner.chat.cwd.clone(),
                inner.chat.external_backend.is_some(),
                inner.chat.workspace_id.clone(),
                inner.external_diff.clone(),
            )
        };
        if is_external {
            let env = self.git_env(Path::new(&cwd), workspace_id.as_deref());
            changes::pending_external_with_env(&entries, &kept, &cwd, external_diff.as_deref(), env.as_ref())
        } else {
            changes::pending(&entries, &kept, &cwd)
        }
    }

    /// Async pending-change listing: keeps `self.inner` unlocked and moves external Git subprocesses onto blocking threads.
    pub(super) async fn pending_changes(&self) -> Vec<changes::Group> {
        let (entries, kept, cwd, is_external, workspace_id, external_diff) = {
            let inner = self.inner.lock().unwrap();
            (
                inner.entries.clone(),
                inner.kept.clone(),
                inner.chat.cwd.clone(),
                inner.chat.external_backend.is_some(),
                inner.chat.workspace_id.clone(),
                inner.external_diff.clone(),
            )
        };
        if !is_external {
            return changes::pending(&entries, &kept, &cwd);
        }
        let env = self.git_env(Path::new(&cwd), workspace_id.as_deref());
        tokio::task::spawn_blocking(move || changes::pending_external_with_env(&entries, &kept, &cwd, external_diff.as_deref(), env.as_ref()))
            .await
            .unwrap_or_default()
    }

    /// Undo file changes of this chat by their change ids, newest first; a file that changed after the call is left alone.
    pub async fn revert(&self, tool_call_ids: &[String]) -> HostResult<RevertResult> {
        let (position, done, cwd) = {
            let inner = self.inner.lock().unwrap();
            if is_busy(&inner) {
                return Err(busy("请等待当前任务结束后再撤销修改。", "Wait for the current task to finish before undoing changes."));
            }
            let position: HashMap<String, usize> = inner
                .entries
                .iter()
                .enumerate()
                .flat_map(|(index, entry)| match entry {
                    TranscriptEntry::ToolResult { tool_call_id, tool_name, details: Some(details), .. } => {
                        changes::file_changes(tool_name, tool_call_id, details).into_iter().map(|change| (change.id, index)).collect::<Vec<_>>()
                    }
                    _ => Vec::new(),
                })
                .collect();
            (position, changes::reverted(&inner.entries), inner.chat.cwd.clone())
        };
        let mut ids: Vec<String> = tool_call_ids.iter().collect::<HashSet<_>>().into_iter().filter(|id| position.contains_key(*id) && !done.contains(*id)).cloned().collect();
        ids.sort_by_key(|id| std::cmp::Reverse(position[id]));
        let lineage = self.lineage();
        let snapshots = self.services.snapshots.clone();
        let cwd_for_blocking = cwd.clone();
        let (result, paths) = tokio::task::spawn_blocking(move || {
            let mut result = RevertResult { reverted: Vec::new(), failed: Vec::new() };
            let mut paths: Vec<String> = Vec::new();
            for id in ids {
                match snapshots.revert(&lineage, &id) {
                    Ok(path) => {
                        let shown = path_for_model(&cwd_for_blocking, &path);
                        if !paths.contains(&shown) {
                            paths.push(shown);
                        }
                        result.reverted.push(json!({ "toolCallId": id, "path": path }));
                    }
                    Err(failure) => result.failed.push(RevertFailure { tool_call_id: id, path: failure.path, reason: failure.reason.into(), message: failure.message }),
                }
            }
            (result, paths)
        })
        .await
        .map_err(|error| HostError::other(error.to_string()))?;
        if !result.reverted.is_empty() {
            let reverted: Vec<String> = result.reverted.iter().filter_map(|item| item["toolCallId"].as_str().map(str::to_owned)).collect();
            let text = format!(
                "The user undid {} of your file changes. These files are back to their content before those changes; read them again before changing them:\n{}",
                reverted.len(),
                paths.iter().map(|path| format!("- {path}")).collect::<Vec<_>>().join("\n")
            );
            self.append(TranscriptEntry::Context { id: uuid::Uuid::new_v4().to_string(), ts: now_ms() as f64, label: "notice".into(), text, reverted: Some(reverted) }).await;
            self.services.rpc.emit("session.changes", json!({ "sessionId": self.id }));
        }
        Ok(result)
    }

    /// Files the agent changed in this chat whose changes the user has neither kept nor undone; a file back to its
    /// content before the changes is left out. Files are read and compared off the async threads.
    pub async fn changes(self: &Arc<Self>) -> Vec<ChatChange> {
        let session = self.clone();
        tokio::task::spawn_blocking(move || session.read_changes()).await.unwrap_or_default()
    }

    fn read_changes(&self) -> Vec<ChatChange> {
        let lineage = self.lineage();
        let chat = self.chat();
        let cwd = chat.cwd;
        let is_external = chat.external_backend.is_some();
        let env = is_external.then(|| self.git_env(Path::new(&cwd), chat.workspace_id.as_deref())).flatten();
        let mut result = Vec::new();
        for group in self.pending_changes_blocking() {
            let first = &group.calls[0];
            let before = if first.created {
                Some(None)
            } else {
                self.services.snapshots.original(&lineage, &first.id).or_else(|| {
                    if is_external {
                        changes::git_head_content_with_env(Path::new(&cwd), Path::new(&group.path), env.as_ref()).map(Some)
                    } else {
                        None
                    }
                })
            };
            // A file larger than the compared text is read only far enough to tell.
            let current = read_limited_bytes(Path::new(&group.path), MAX_REVIEW_FILE_BYTES + 1);
            let base = match &before {
                None => None,
                Some(None) => Some(String::new()),
                Some(Some(bytes)) => change_text(bytes),
            };
            let now = match &current {
                None => Some(String::new()),
                Some(bytes) => change_text(bytes),
            };
            let comparable = base.is_some() && now.is_some();
            if comparable && base == now {
                continue;
            }
            let (added, removed) = match (&base, &now) {
                (Some(base), Some(now)) => line_counts(base, now),
                _ => (0, 0),
            };
            result.push(ChatChange {
                display: path_for_model(&cwd, &group.path),
                tool_call_ids: group.calls.iter().map(|call| call.id.clone()).collect(),
                created: first.created,
                deleted: current.is_none(),
                added,
                removed,
                comparable,
                undoable: group.calls.iter().all(|call| call.undoable),
                path: group.path,
            });
        }
        result
    }

    /// A changed file's text before the agent's changes and now (`None` when gone), for a side-by-side review.
    pub async fn change_base(&self, path: &str) -> HostResult<(String, Option<String>)> {
        let key = path_key(path);
        let group = self
            .pending_changes()
            .await
            .into_iter()
            .find(|group| path_key(&group.path) == key)
            .ok_or_else(|| HostError::new("not-found", l("这个文件没有待审查的改动。", "This file has no changes to review.")))?;
        let first_id = group.calls[0].id.clone();
        let first_created = group.calls[0].created;
        let group_path = group.path;
        let chat = self.chat();
        let is_external = chat.external_backend.is_some();
        let lineage = self.lineage();
        let snapshots = self.services.snapshots.clone();
        let env = is_external.then(|| self.git_env(Path::new(&chat.cwd), chat.workspace_id.as_deref())).flatten();
        tokio::task::spawn_blocking(move || {
            let base = if first_created {
                Some(String::new())
            } else {
                snapshots
                    .original(&lineage, &first_id)
                    .or_else(|| {
                        if is_external {
                            changes::git_head_content_with_env(Path::new(&chat.cwd), Path::new(&group_path), env.as_ref()).map(Some)
                        } else {
                            None
                        }
                    })
                    .and_then(|before| before.map_or(Some(String::new()), |bytes| change_text(&bytes)))
            };
            let base = base.ok_or_else(|| HostError::new("unavailable", l("改动前的内容没有保留，无法对比。", "The content before the change is not kept, so it cannot be compared.")))?;
            let current = read_limited_bytes(Path::new(&group_path), MAX_REVIEW_FILE_BYTES + 1).and_then(|bytes| change_text(&bytes));
            Ok((base, current))
        })
        .await
        .map_err(|error| HostError::other(error.to_string()))?
    }

    /// Record that the user reviewed and kept the changes of some calls; ids that are not pending changes are ignored.
    pub async fn keep(&self, tool_call_ids: &[String]) -> HostResult<()> {
        let pending: HashSet<String> = self.pending_changes().await.into_iter().flat_map(|group| group.calls.into_iter().map(|call| call.id)).collect();
        let mut ids: Vec<String> = Vec::new();
        for id in tool_call_ids {
            if pending.contains(id) && !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        if ids.is_empty() {
            return Ok(());
        }
        self.services.store.keep(&self.id, &ids).await?;
        self.inner.lock().unwrap().kept.extend(ids);
        self.services.rpc.emit("session.changes", json!({ "sessionId": self.id }));
        Ok(())
    }
}

fn read_limited_bytes(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}
