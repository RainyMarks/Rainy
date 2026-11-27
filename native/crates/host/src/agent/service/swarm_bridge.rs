//! Bridge implementing [`crate::agent::swarm::Chats`] on top of [`super::Agent`].

use std::path::Path;
use std::sync::{Arc, Weak};

use super::{Agent, no_worktree, not_found_chat};
use crate::agent::session::summary_of;
use crate::agent::store::{MetaPatch, NewChat};
use crate::agent::swarm::{ChatState, Chats};
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::settings::ModelSelection;

/// The chats a swarm acts on: the agent's own operations on them.
pub(super) struct SwarmChats(pub(super) Weak<Agent>);

impl SwarmChats {
    fn agent(&self) -> HostResult<Arc<Agent>> {
        self.0.upgrade().ok_or_else(|| HostError::new("closed", l("Host 正在关闭。", "The Host is closing.")))
    }
}

impl Chats for SwarmChats {
    fn create_worker(&self, coordinator: String, name: String, first: String, model: Option<ModelSelection>, isolated: bool) -> futures::future::BoxFuture<'static, HostResult<String>> {
        let agent = self.agent();
        Box::pin(async move {
            let agent = agent?;
            let lead = agent.services.store.get(&coordinator).ok_or_else(not_found_chat)?;
            // Workers start from the project's checkout, also when the coordinator runs in a worktree of its own.
            let base = lead.worktree.as_ref().map_or_else(|| lead.cwd.clone(), |worktree| worktree.repository.clone());
            let (cwd, worktree) = if isolated {
                let (worktree, inside) = agent.git.worktree_add(Path::new(&base), &agent.services.env.home.join("worktrees")).await?;
                (inside, Some(worktree))
            } else {
                (base, None)
            };
            let lead_model = agent.live(&coordinator).and_then(|session| session.selection()).or(lead.model.clone());
            let chat = agent.services.store.create(NewChat {
                workspace_id: lead.workspace_id.clone(),
                cwd,
                parent: None,
                entries: Vec::new(),
                title: format!("@{name}"),
                model: model.or(lead_model),
                permission: Some(lead.permission.clone().unwrap_or_else(|| agent.services.settings.default_permission())),
                worktree,
                swarm: Some(rainy_protocol::chat::SwarmLink { coordinator: coordinator.clone(), name }),
                external_backend: None,
            })?;
            agent.services.rpc.emit("sessions.changed", summary_of(&chat, "idle", false));
            agent.session(&chat.id).await?.send(&first, Vec::new(), false).await?;
            Ok(chat.id)
        })
    }

    fn deliver(&self, id: String, text: String) -> futures::future::BoxFuture<'static, HostResult<()>> {
        let agent = self.agent();
        Box::pin(async move {
            agent?.session(&id).await?.send_swarm(&text).await?;
            Ok(())
        })
    }

    fn resolve_model(&self, hint: &str) -> HostResult<ModelSelection> {
        self.agent()?.services.models.resolve_by_hint(hint)
    }

    fn land(&self, id: String) -> futures::future::BoxFuture<'static, HostResult<usize>> {
        let agent = self.agent();
        Box::pin(async move {
            let agent = agent?;
            let _opening = agent.opening.lock().await;
            let chat = agent.services.store.get(&id).ok_or_else(not_found_chat)?;
            let worktree = chat.worktree.ok_or_else(no_worktree)?;
            let live = agent.sessions.lock().unwrap().remove(&id);
            if let Some(entry) = live {
                entry.session.close().await;
            }
            let files = agent.git.worktree_hand_off(&worktree, &agent.services.env.home.join("worktrees")).await?;
            agent.set_meta(&id, MetaPatch { cwd: Some(worktree.repository.clone()), worktree: Some(None), ..MetaPatch::default() }).await?;
            Ok(files)
        })
    }

    fn retire(&self, id: String, discard: bool) -> futures::future::BoxFuture<'static, HostResult<()>> {
        let agent = self.agent();
        Box::pin(async move {
            let agent = agent?;
            if let Some(session) = agent.live(&id) {
                session.abort();
            }
            let chat = agent.services.store.get(&id).ok_or_else(not_found_chat)?;
            if let (Some(worktree), true) = (&chat.worktree, discard) {
                agent.git.worktree_remove(worktree).await;
                agent.set_meta(&id, MetaPatch { cwd: Some(worktree.repository.clone()), worktree: Some(None), ..MetaPatch::default() }).await?;
            }
            agent.set_meta(&id, MetaPatch { archived: Some(true), ..MetaPatch::default() }).await?;
            Ok(())
        })
    }

    fn abort(&self, id: &str) {
        if let Some(session) = self.0.upgrade().and_then(|agent| agent.live(id)) {
            session.abort();
        }
    }

    fn state(&self, id: &str) -> ChatState {
        match self.0.upgrade().and_then(|agent| agent.live(id)) {
            Some(session) => session.swarm_state(),
            None => ChatState { status: "idle".into(), awaiting_approval: false, user_waiting: false, has_pending: false },
        }
    }

    fn last_reply(&self, id: &str) -> Option<String> {
        self.0.upgrade().and_then(|agent| agent.live(id)).and_then(|session| session.last_reply())
    }
}
