//! Project map, Git and inline editor assist RPC handlers.

use std::sync::Arc;

use serde_json::{Value, json};

use super::params::{GitRequest, RequestRef, SessionRef, WorkspaceRef};
use super::{Agent, GIT_MESSAGE_TIMEOUT, no_model};
use crate::agent::inline::{EditRequest, SuggestRequest};
use crate::agent::llm::{Level, reply_text};
use crate::agent::models::Completion;
use crate::error::{HostError, HostResult};
use crate::git;
use crate::locale::l;
use crate::rpc;

impl Agent {
    pub(super) fn register_tools(self: &Arc<Self>) {
        self.on("map.explain", |agent, params: WorkspaceRef| async move {
            agent.maps.explain(&params.workspace_id, &agent.services.models, &agent.services.settings.locale()).await
        });
        self.on("map.footprint", |agent, params: SessionRef| async move {
            // Avoid permanently pinning an offline chat in `Agent.sessions` just to compute its footprint.
            let turns = if let Some(live) = agent.live(&params.session_id) {
                let snapshot = live.snapshot(None);
                crate::map::footprint::footprint(&snapshot.entries, snapshot.streaming.as_ref(), &snapshot.summary.cwd)
            } else {
                let file = agent.services.store.read(&params.session_id).await?;
                crate::map::footprint::footprint(&file.entries, None, &file.chat.cwd)
            };
            Ok(json!({ "turns": turns }))
        });
        for method in ["git.status", "git.stage", "git.unstage", "git.discard", "git.commit", "git.init", "git.message"] {
            let agent = self.clone();
            self.services.rpc.register(method, move |params| {
                let agent = agent.clone();
                async move {
                    let request: GitRequest = rpc::params(params)?;
                    let (workspace, root, git) = (request.workspace_id.as_str(), request.root_id.as_str(), &agent.git);
                    match method {
                        "git.status" => git.status(workspace).await,
                        "git.stage" => git.stage(workspace, root, &request.paths).await.map(|()| Value::Null),
                        "git.unstage" => git.unstage(workspace, root, &request.paths).await.map(|()| Value::Null),
                        "git.discard" => git.discard(workspace, root, &request.paths).await.map(|()| Value::Null),
                        "git.commit" => git.commit(workspace, root, &request.message, request.stage_all).await,
                        "git.init" => git.init(workspace, root).await.map(|()| Value::Null),
                        _ => agent.commit_message(workspace, root).await,
                    }
                }
            });
        }
        self.on("inline.complete", |agent, request: SuggestRequest| async move { Ok(agent.inline.complete(request).await) });
        self.on("inline.edit", |agent, request: EditRequest| async move { agent.inline.edit(request).await });
        self.on("inline.cancel", |agent, params: RequestRef| async move {
            agent.inline.cancel(&params.request_id);
            Ok(Value::Null)
        });
    }

    /// A commit message for the changes of one folder, written by the default model.
    async fn commit_message(&self, workspace_id: &str, root_id: &str) -> HostResult<Value> {
        let resolved = self.services.models.resolve(None).ok_or_else(no_model)?;
        let prompt = self.git.message_prompt(workspace_id, root_id).await?;
        let options = Completion { system: Some(git::MESSAGE_SYSTEM.into()), max_tokens: Some(600), thinking: Some(Level::Off), cancel: None };
        let reply = self.services.models.complete(&resolved, &prompt, &[], GIT_MESSAGE_TIMEOUT, options).await;
        if reply.stop_reason == "error" || reply.stop_reason == "aborted" {
            return Err(HostError::new("message-failed", reply.error_message.unwrap_or_else(|| l("生成提交说明失败。", "The commit message could not be written."))));
        }
        let message = git::clean_message(&reply_text(&reply));
        if message.is_empty() {
            return Err(HostError::new("message-failed", l("模型没有给出提交说明，请重试。", "The model gave no commit message. Try again.")));
        }
        Ok(json!({ "message": message }))
    }
}
