//! Permission evaluation, user approval waits, declarative lifecycle hooks, and post-edit snapshot refresh for [`Session`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rainy_protocol::chat::{PendingApproval, ResultContent};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::super::changes::{self, path_for_model, path_key};
use super::super::hooks::{self, HookContext, PreHookOutcome};
use super::super::permissions::{self, Decision, canonical_path, decide, denial_text};
use super::super::run::Blocked;
use super::super::store::StoredChat;
use super::super::tools::{ToolOutput, diagnostics, version_of};
use super::{Answer, Approval, Session, refusal};
use crate::error::{HostError, HostResult};
use crate::locale::l;

const MAX_SNAPSHOT_UPDATE_BYTES: u64 = 16 * 1024 * 1024;

impl Session {
    /// Whether this chat's workspace is trusted to auto-load project `.rainy/skills`, `.mcp.json` and `.rainy/hooks.json`.
    pub(super) fn workspace_trusted(&self, chat: &StoredChat) -> bool {
        self.services.settings.is_workspace_trusted(chat.workspace_id.as_deref(), Some(Path::new(&chat.cwd)))
    }

    /// Effective declarative lifecycle hooks for this chat (global + trusted workspace `.rainy/hooks.json`).
    pub(super) fn effective_hooks(&self) -> Vec<hooks::HookRule> {
        let chat = self.chat();
        let global = self.services.settings.hooks();
        hooks::effective_hooks(&global, Path::new(&chat.cwd), self.workspace_trusted(&chat))
    }

    pub(super) fn hook_context(&self) -> HookContext {
        let chat = self.chat();
        let cwd = PathBuf::from(&chat.cwd);
        let environment = chat
            .workspace_id
            .as_deref()
            .and_then(|id| self.services.runtime.resolve_workspace(id).ok())
            .or_else(|| self.services.runtime.resolve(&cwd).ok())
            .map(|r| r.environment)
            .unwrap_or_default();
        HookContext {
            cwd,
            session_id: self.id.clone(),
            windows: self.services.env.windows,
            pwsh_path: self.services.env.pwsh_path.clone(),
            environment,
            ide_tools: self.services.ide_tools.clone(),
        }
    }

    /// The chat's permission decision for a call a `task` sub-agent makes. A sub-agent has no approval card, so a call
    /// the chat would ask about is refused; deny rules and project boundaries hold for it as they do for the chat.
    pub(super) fn subagent_refusal(&self, name: &str, args: &Value) -> Option<String> {
        let chat = self.chat();
        let rules = chat.workspace_id.as_deref().map(|workspace| self.services.settings.permission_rules(workspace)).unwrap_or_default();
        let read_only_tools = self.services.mcp.read_only_tools();
        let roots = || self.canonical_roots(&chat);
        let spill_dir = self.services.env.home.join("spill").join(&self.id);
        let decision = decide(&permissions::Input {
            mode: &self.permission(),
            tool: name,
            args,
            cwd: Path::new(&chat.cwd),
            roots: &roots,
            rules: &rules,
            read_only_tools: &read_only_tools,
            can_remember: false,
            windows: self.services.env.windows,
            spill_dir: Some(&spill_dir),
        });
        match decision {
            Decision::Allow => None,
            Decision::Deny(reason) => Some(reason),
            Decision::Ask(_) => Some(format!(
                "Sub-agents cannot ask for approval, so `{name}` was not run: it needs the user's permission in this chat. Report what you could not check; the main assistant can do it itself."
            )),
        }
    }

    pub(super) async fn before_tool(&self, mcp_tools: &HashSet<String>, id: &str, name: &str, args: &Value, cancel: &CancellationToken) -> Option<Blocked> {
        if self.inner.lock().unwrap().stop_after_step {
            return Some(Blocked { reason: "Skipped: the user refused an earlier tool call in this step.".into(), terminate: true });
        }
        // The tool list is fixed for the run; an MCP tool turned off or disconnected since then is refused here.
        if mcp_tools.contains(name) && !self.services.mcp.allows(name) {
            return Some(Blocked { reason: "This MCP tool was turned off in Settings or its server disconnected. Do not call it again.".into(), terminate: false });
        }
        let chat = self.chat();
        let rules = chat.workspace_id.as_deref().map(|workspace| self.services.settings.permission_rules(workspace)).unwrap_or_default();
        let read_only_tools = self.services.mcp.read_only_tools();
        let roots = || self.canonical_roots(&chat);
        let spill_dir = self.services.env.home.join("spill").join(&self.id);
        let decision = decide(&permissions::Input {
            mode: &self.permission(),
            tool: name,
            args,
            cwd: Path::new(&chat.cwd),
            roots: &roots,
            rules: &rules,
            read_only_tools: &read_only_tools,
            can_remember: chat.workspace_id.is_some(),
            windows: self.services.env.windows,
            spill_dir: Some(&spill_dir),
        });
        let fields = match decision {
            Decision::Allow => None,
            Decision::Deny(reason) => {
                self.inner.lock().unwrap().refused.insert(id.to_owned(), "mode");
                return Some(Blocked { reason, terminate: false });
            }
            Decision::Ask(fields) => Some(fields),
        };
        if let Some(fields) = fields {
            let text = |key: &str| fields.get(key).and_then(Value::as_str).map(str::to_owned);
            let approval = PendingApproval {
                tool_call_id: id.to_owned(),
                tool_name: name.to_owned(),
                reason: text("reason").unwrap_or_default(),
                rule: text("rule"),
                path: text("path"),
                workdir: text("workdir"),
                agent: None,
            };
            match self.wait_for_approval(approval, cancel).await {
                Answer::Allow | Answer::AllowSession => {}
                Answer::Refuse { text, stop, by } => {
                    let mut inner = self.inner.lock().unwrap();
                    if let Some(by) = by {
                        inner.refused.insert(id.to_owned(), by);
                    }
                    if stop {
                        inner.stop_after_step = true;
                    }
                    return Some(Blocked { reason: text, terminate: stop });
                }
            }
        }
        // Run declarative `PreToolUse` hooks; exit code 2 blocks the call and feeds output back to the model.
        let hook_rules = self.effective_hooks();
        if hook_rules.iter().any(|r| r.enabled && r.event == "PreToolUse") {
            let ctx = self.hook_context();
            if let PreHookOutcome::Block(feedback) = hooks::run_pre_tool_cancellable(&hook_rules, &ctx, name, args, Some(cancel)).await {
                self.inner.lock().unwrap().refused.insert(id.to_owned(), "hook");
                return Some(Blocked { reason: feedback, terminate: false });
            }
        }
        None
    }

    /// Run `PostEdit` hooks (including built-in `ide::format`), append Error-level LSP diagnostics after edits, and run
    /// `PostToolUse` hooks before the tool result is recorded and sent to the model.
    pub(super) async fn after_tool(&self, id: &str, name: &str, args: &Value, output: &mut ToolOutput) {
        let append_text = |output: &mut ToolOutput, extra: &str| {
            if extra.is_empty() {
                return;
            }
            if let Some(ResultContent::Text { text }) = output.content.iter_mut().find(|c| matches!(c, ResultContent::Text { .. })) {
                text.push_str(extra);
            } else {
                output.content.push(ResultContent::Text { text: extra.trim_start().to_owned() });
            }
        };

        let cancel = self.inner.lock().unwrap().run.as_ref().map(|run| run.cancel.clone());
        let hook_rules = self.effective_hooks();
        let hook_ctx = if !hook_rules.is_empty() { Some(self.hook_context()) } else { None };
        let cwd = PathBuf::from(self.chat().cwd);

        if !output.is_error && changes::CHANGE_TOOLS.contains(&name) {
            let mut edited_files: Vec<PathBuf> = Vec::new();
            if name == super::super::tools::patch::NAME {
                if let Some(files) = output.details.as_ref().and_then(|d| d.get("files")).and_then(Value::as_array) {
                    for file in files {
                        if file.get("operation").and_then(Value::as_str) == Some("delete") {
                            continue;
                        }
                        if let Some(path) = file.get("path").and_then(Value::as_str) {
                            edited_files.push(crate::paths::normalize(&cwd.join(path)));
                        }
                    }
                }
            } else if let Some(path) = output
                .details
                .as_ref()
                .and_then(|d| d.get("path"))
                .and_then(Value::as_str)
                .or_else(|| args.get("file_path").and_then(Value::as_str))
            {
                edited_files.push(crate::paths::normalize(&cwd.join(path)));
            }

            if let Some(ctx) = &hook_ctx
                && hook_rules.iter().any(|r| r.enabled && r.event == "PostEdit")
            {
                let before_versions: Vec<(PathBuf, Option<super::super::tools::FileVersion>)> =
                    edited_files.iter().map(|path| (path.clone(), version_of(path).ok().flatten())).collect();
                let report = hooks::run_post_edit_cancellable(&hook_rules, ctx, name, &edited_files, cancel.as_ref()).await;
                let mut changed_files = report.formatted_files.clone();
                for (path, before) in before_versions {
                    let after = version_of(&path).ok().flatten();
                    if after != before && !changed_files.iter().any(|existing| existing == &path) {
                        changed_files.push(path);
                    }
                }
                self.refresh_formatted_snapshots(name, id, output.details.as_ref(), &cwd, &changed_files);
                if !report.formatted_files.is_empty() {
                    let names: Vec<String> = report.formatted_files.iter().map(|p| path_for_model(&self.chat().cwd, &p.to_string_lossy())).collect();
                    append_text(output, &format!("\n[PostEdit formatted: {}]", names.join(", ")));
                }
                for note in &report.notes {
                    append_text(output, &format!("\n{note}"));
                }
            }

            // Attach Error-level LSP diagnostics for affected files (silently skipped when no LSP is available).
            if !edited_files.is_empty() && self.services.languages.is_some() {
                let tool_ctx = self.tool_context();
                if let Some((summary, diag_details)) = diagnostics::post_edit_error_summary(&tool_ctx, &edited_files).await {
                    append_text(output, &summary);
                    if let Some(Value::Object(map)) = &mut output.details {
                        map.insert("diagnostics".into(), json!(diag_details));
                    }
                }
            }
        }

        if let Some(ctx) = &hook_ctx
            && hook_rules.iter().any(|r| r.enabled && r.event == "PostToolUse")
        {
            let report = hooks::run_post_tool_cancellable(&hook_rules, ctx, name, args, output.is_error, cancel.as_ref()).await;
            for note in &report.notes {
                append_text(output, &format!("\n{note}"));
            }
        }
    }

    /// Update file observations and undo snapshot `after` digests for files rewritten by a `PostEdit` formatter,
    /// keeping the original `before` snapshot so user Undo still succeeds.
    pub(super) fn refresh_formatted_snapshots(&self, tool_name: &str, tool_call_id: &str, details: Option<&Value>, cwd: &Path, formatted_files: &[PathBuf]) {
        if formatted_files.is_empty() {
            return;
        }
        let file_changes = details.map(|d| changes::file_changes(tool_name, tool_call_id, d)).unwrap_or_default();
        for formatted in formatted_files {
            if let Ok(Some(ver)) = version_of(formatted) {
                self.observations.record(formatted, Some(ver));
            }
            let formatted_key = path_key(&formatted.to_string_lossy());
            let Some(change) = file_changes.iter().find(|change| {
                let resolved = if Path::new(change.path).is_absolute() {
                    PathBuf::from(change.path)
                } else {
                    crate::paths::normalize(&cwd.join(change.path))
                };
                path_key(&resolved.to_string_lossy()) == formatted_key
            }) else {
                continue;
            };
            if let Ok(meta) = std::fs::metadata(formatted)
                && meta.is_file()
                && meta.len() <= MAX_SNAPSHOT_UPDATE_BYTES
                && let Ok(bytes) = std::fs::read(formatted)
                && let Err(error) = self.services.snapshots.update_after(&self.id, &change.id, Some(&bytes))
            {
                log::warn!("[chat {}] updating undo snapshot after PostEdit format failed for {}: {error}", self.id, formatted.display());
            }
        }
    }

    /// Project folders with links resolved; a chat outside any project has none, so its every write asks.
    pub(super) fn canonical_roots(&self, chat: &StoredChat) -> Vec<PathBuf> {
        let Some(project) = chat.workspace_id.as_deref().and_then(|workspace| self.services.projects.get(workspace)) else { return Vec::new() };
        std::iter::once(chat.cwd.clone()).chain(project.roots().into_iter().map(|root| root.path)).map(|path| canonical_path(Path::new(&path))).collect()
    }

    pub(super) async fn wait_for_approval(&self, approval: PendingApproval, cancel: &CancellationToken) -> Answer {
        let id = approval.tool_call_id.clone();
        let (settle, settled) = oneshot::channel();
        self.inner.lock().unwrap().approvals.push(Approval { approval, settle });
        self.publish_state();
        self.services.rpc.emit("sessions.changed", self.summary());
        tokio::select! {
            answer = settled => answer.unwrap_or_else(|_| refusal("Operation aborted", true, None)),
            _ = cancel.cancelled() => {
                // The card goes away; nobody waits for the answer any more.
                self.settle(&id, refusal("Operation aborted", true, None));
                refusal("Operation aborted", true, None)
            }
        }
    }

    pub(super) fn settle(&self, tool_call_id: &str, answer: Answer) {
        let approval = {
            let mut inner = self.inner.lock().unwrap();
            let Some(at) = inner.approvals.iter().position(|item| item.approval.tool_call_id == tool_call_id) else { return };
            inner.approvals.remove(at)
        };
        let _ = approval.settle.send(answer);
        self.publish_state();
        self.services.rpc.emit("sessions.changed", self.summary());
    }

    /// Answer a waiting tool call: `allow`, `always` (allow and remember for the project) or `deny`, with what the
    /// user told the model; a refusal without a reason stops the run after this step.
    pub fn approve(&self, tool_call_id: &str, decision: &str, reason: Option<&str>) -> HostResult<()> {
        let approval = self.inner.lock().unwrap().approvals.iter().find(|item| item.approval.tool_call_id == tool_call_id).map(|item| item.approval.clone());
        let approval = approval.ok_or_else(|| HostError::new("not-found", l("这个操作已不在等待批准。", "This action is no longer waiting for approval.")))?;
        let is_external = self.chat().external_backend.is_some();
        if decision == "always" && !is_external {
            let workspace = self.chat().workspace_id;
            let (Some(rule), Some(workspace)) = (approval.rule.as_deref(), workspace) else {
                return Err(HostError::new("invalid", l("这个操作只能单次允许。", "This action can only be allowed once.")));
            };
            let command = approval.reason != "mcp" && approval.reason != "web";
            self.services.settings.set_permission_rule(&workspace, command, rule, true)?;
        }
        let said = reason.unwrap_or_default().trim();
        let answer = match decision {
            "deny" => refusal(denial_text(said), said.is_empty(), Some("user")),
            "always" if is_external => Answer::AllowSession,
            _ => Answer::Allow,
        };
        self.settle(tool_call_id, answer);
        Ok(())
    }
}
