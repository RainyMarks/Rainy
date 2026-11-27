//! Swarm tools exposed to coordinator and worker chats.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use rainy_protocol::chat::SwarmLink;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{COORDINATOR, DEFAULT_WAIT, Hub, MAX_WAIT, MAX_WORKERS};
use crate::agent::llm::ToolSpec;
use crate::agent::tools::{Tool, ToolOutput, Updates, params, spec};
use crate::error::HostError;

fn tool_error(error: HostError) -> String {
    error.message
}

#[derive(Deserialize)]
struct AddParams {
    name: String,
    brief: String,
    #[serde(default)]
    isolated: bool,
    #[serde(default)]
    model: Option<String>,
}

/// `swarm_add`: the coordinator adds a worker.
pub struct SwarmAdd {
    hub: Arc<Hub>,
    coordinator: String,
    spec: ToolSpec,
}

impl SwarmAdd {
    pub fn new(hub: Arc<Hub>, coordinator: String) -> Self {
        let spec = spec(
            "swarm_add",
            &format!("Add a named worker to the swarm: a new chat that starts on its brief right away and works in parallel with the others. At most {MAX_WORKERS} at a time."),
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Short name without @: lower-case letters, digits and hyphens (parser, api-tests, fix-1, verifier)." },
                    "brief": { "type": "string", "description": "Everything the worker needs, as it sees nothing else: the goal, what it owns, what it must not touch, how to verify, and what done means." },
                    "isolated": { "type": "boolean", "description": "Work in its own Git worktree, so it can edit and build at the same time as others; its changes reach the project with swarm_land. Default false: it works in the project's checkout." },
                    "model": { "type": "string", "description": "Optional configured model override (provider id, model id, or provider/model) so workers can use a faster or lower-cost model than the coordinator." },
                },
                "required": ["name", "brief"],
            }),
        );
        Self { hub, coordinator, spec }
    }
}

impl Tool for SwarmAdd {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: AddParams = params(args)?;
            let name = params.name.trim().trim_start_matches('@').to_ascii_lowercase();
            if params.brief.trim().is_empty() {
                return Err("invalid brief: expected the worker's task".into());
            }
            let model = match params.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
                Some(hint) => Some(self.hub.resolve_model(hint).map_err(tool_error)?),
                None => None,
            };
            let id = self.hub.add_worker(&self.coordinator, &name, &params.brief, model.clone(), params.isolated).await.map_err(tool_error)?;
            let mut details = json!({ "name": name, "sessionId": id, "isolated": params.isolated });
            if let Some(model) = &model {
                details["model"] = json!(format!("{}/{}", model.provider, model.model));
            }
            Ok(ToolOutput::text(
                format!("@{name} started on its brief{}.\n\nMembers:\n{}", if params.isolated { " in its own worktree" } else { "" }, self.hub.roster(&self.coordinator)),
                details,
            ))
        })
    }
}

#[derive(Deserialize)]
struct PostParams {
    to: Vec<String>,
    text: String,
}

/// `swarm_post`: any member posts to the board.
pub struct SwarmPost {
    hub: Arc<Hub>,
    coordinator: String,
    from: String,
    spec: ToolSpec,
}

impl SwarmPost {
    pub fn new(hub: Arc<Hub>, coordinator: String, from: String) -> Self {
        let spec = spec(
            "swarm_post",
            "Post a message on the swarm's board to the members you name; each of them receives it as a message. Everyone can read the board.",
            json!({
                "type": "object",
                "properties": {
                    "to": { "type": "array", "items": { "type": "string" }, "description": "Recipients: @name of a worker, @coordinator, or @all." },
                    "text": { "type": "string", "description": "The message." },
                },
                "required": ["to", "text"],
            }),
        );
        Self { hub, coordinator, from, spec }
    }
}

impl Tool for SwarmPost {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: PostParams = params(args)?;
            let post = self.hub.post(&self.coordinator, &self.from, &params.to, &params.text, false).await.map_err(tool_error)?;
            Ok(ToolOutput::text(format!("Posted to {}.", post.to.iter().map(|to| format!("@{to}")).collect::<Vec<_>>().join(" ")), json!({ "to": post.to })))
        })
    }
}

#[derive(Deserialize)]
struct WaitParams {
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

/// `swarm_wait`: the coordinator waits for the board.
pub struct SwarmWait {
    hub: Arc<Hub>,
    coordinator: String,
    spec: ToolSpec,
}

impl SwarmWait {
    pub fn new(hub: Arc<Hub>, coordinator: String) -> Self {
        let spec = spec(
            "swarm_wait",
            "Wait for the swarm: returns the posts that reached you (a worker's finished turn is posted to you on its own) as soon as there are any, or when no worker is running, the user writes to you, or the time is up. Use it after briefing workers instead of checking repeatedly.",
            json!({
                "type": "object",
                "properties": {
                    "timeout_seconds": { "type": "integer", "description": "Longest wait, 10 to 1800 seconds; default 600." },
                },
            }),
        );
        Self { hub, coordinator, spec }
    }
}

impl Tool for SwarmWait {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: WaitParams = params(args)?;
            let timeout = params.timeout_seconds.map_or(DEFAULT_WAIT, |seconds| Duration::from_secs(seconds.clamp(10, MAX_WAIT.as_secs())));
            let text = self.hub.wait(&self.coordinator, timeout, &cancel).await;
            if cancel.is_cancelled() {
                return Err("tool call aborted".into());
            }
            Ok(ToolOutput::text(text, json!({})))
        })
    }
}

/// `swarm_members`: the roster with each member's state.
pub struct SwarmMembers {
    hub: Arc<Hub>,
    coordinator: String,
    spec: ToolSpec,
}

impl SwarmMembers {
    pub fn new(hub: Arc<Hub>, coordinator: String) -> Self {
        let spec = spec("swarm_members", "List the swarm's workers with their state (running, idle, waiting for the user's approval), model and whether they have their own worktree.", json!({ "type": "object", "properties": {} }));
        Self { hub, coordinator, spec }
    }
}

impl Tool for SwarmMembers {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, _args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move { Ok(ToolOutput::text(self.hub.roster(&self.coordinator), json!({}))) })
    }
}

#[derive(Deserialize)]
struct NameParams {
    name: String,
    #[serde(default)]
    discard: bool,
}

/// `swarm_land`: move a worker's worktree changes into the project.
pub struct SwarmLand {
    hub: Arc<Hub>,
    coordinator: String,
    spec: ToolSpec,
}

impl SwarmLand {
    pub fn new(hub: Arc<Hub>, coordinator: String) -> Self {
        let spec = spec(
            "swarm_land",
            "Move everything a worker changed in its own worktree into the project's checkout as uncommitted changes; the worker then works in the checkout. Nothing changes when the checkout has conflicting changes. Land workers one at a time, after they are done and verified.",
            json!({ "type": "object", "properties": { "name": { "type": "string", "description": "The worker's name." } }, "required": ["name"] }),
        );
        Self { hub, coordinator, spec }
    }
}

impl Tool for SwarmLand {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: NameParams = params(args)?;
            let name = params.name.trim().trim_start_matches('@').to_ascii_lowercase();
            let files = self.hub.land(&self.coordinator, &name).await.map_err(tool_error)?;
            Ok(ToolOutput::text(format!("Moved {files} changed file(s) of @{name} into the project."), json!({ "name": name, "files": files })))
        })
    }
}

/// `swarm_remove`: retire a worker.
pub struct SwarmRemove {
    hub: Arc<Hub>,
    coordinator: String,
    spec: ToolSpec,
}

impl SwarmRemove {
    pub fn new(hub: Arc<Hub>, coordinator: String) -> Self {
        let spec = spec(
            "swarm_remove",
            "Remove a worker that is done or no longer needed: it stops and its chat is archived. This is permanent; add a new worker if one is needed again. A worker with its own worktree must be landed first, or removed with discard: true to drop its changes.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The worker's name." },
                    "discard": { "type": "boolean", "description": "Drop the worker's worktree with any changes not landed. Default false." },
                },
                "required": ["name"],
            }),
        );
        Self { hub, coordinator, spec }
    }
}

impl Tool for SwarmRemove {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: NameParams = params(args)?;
            let name = params.name.trim().trim_start_matches('@').to_ascii_lowercase();
            self.hub.remove(&self.coordinator, &name, params.discard).await.map_err(tool_error)?;
            Ok(ToolOutput::text(format!("@{name} was removed.\n\nMembers:\n{}", self.hub.roster(&self.coordinator)), json!({ "name": name })))
        })
    }
}

/// The swarm tools of a chat: all of them for the coordinator, posting and the roster for a worker.
pub fn tools(hub: &Arc<Hub>, link: &SwarmLink, self_id: &str) -> Vec<Arc<dyn Tool>> {
    let coordinator = link.coordinator.clone();
    if coordinator == self_id {
        vec![
            Arc::new(SwarmAdd::new(hub.clone(), coordinator.clone())),
            Arc::new(SwarmPost::new(hub.clone(), coordinator.clone(), COORDINATOR.into())),
            Arc::new(SwarmWait::new(hub.clone(), coordinator.clone())),
            Arc::new(SwarmMembers::new(hub.clone(), coordinator.clone())),
            Arc::new(SwarmLand::new(hub.clone(), coordinator.clone())),
            Arc::new(SwarmRemove::new(hub.clone(), coordinator)),
        ]
    } else {
        vec![Arc::new(SwarmPost::new(hub.clone(), coordinator.clone(), link.name.clone())), Arc::new(SwarmMembers::new(hub.clone(), coordinator))]
    }
}
