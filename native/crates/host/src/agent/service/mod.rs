//! The agent side of the Host over RPC (3.x `agent/index.ts`): chats, models, MCP, skills, project memory, Git
//! messages, map explanations, editor AI, proxy and permission settings.

mod handlers_config;
mod handlers_session;
mod handlers_tools;
mod params;
mod swarm_bridge;

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rainy_protocol::chat::SessionSummary;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use self::params::ProxyParams;
use self::swarm_bridge::SwarmChats;
use super::external::ExternalManager;
use super::inline::InlineAssist;
use super::llm::Llm;
use super::mcp::McpManager;
use super::memory::{self, ProjectMemory};
use super::models::{Models, deepseek_flash};
use super::session::{Services, Session, summary_of};
use super::skills;
use super::snapshots::Snapshots;
use super::store::{ChatStore, MetaPatch};
use super::tools::browser::Preview;
use crate::Host;
use crate::error::{HostError, HostResult};
use crate::git::Git;
use crate::locale::l;
use crate::map::ProjectMaps;
use crate::network::{PASSWORD_MASK, proxy_url};
use crate::rpc;
use crate::settings::{McpServerConfig, ProxySettings};

/// Spill files and undo snapshots older than this are deleted at startup.
const RETENTION: Duration = Duration::from_secs(30 * 24 * 3600);
pub(super) const GIT_MESSAGE_TIMEOUT: Duration = Duration::from_secs(90);
/// Maximum number of live sessions retained in memory before idle LRU sessions are evicted.
const MAX_LIVE_SESSIONS: usize = 24;

pub(super) fn not_found_chat() -> HostError {
    HostError::new("not-found", l("对话不存在。", "This chat does not exist."))
}

pub(super) fn no_worktree() -> HostError {
    HostError::new("no-worktree", l("这个对话没有独立工作区。", "This chat has no worktree."))
}

pub(super) fn no_model() -> HostError {
    HostError::new("no-model", l("还没有可用的模型。请先在“设置 → 模型”中添加一个模型。", "No model is set up yet. Add one in Settings → Models."))
}

pub(super) fn permission_mode(mode: &str) -> HostResult<String> {
    if matches!(mode, "read-only" | "ask" | "auto") { Ok(mode.to_owned()) } else { Err(HostError::invalid("Unknown permission mode.")) }
}

pub(super) struct LiveSession {
    pub(super) session: Arc<Session>,
    pub(super) last_used: u64,
}

/// The agent side of one Host.
pub struct Agent {
    pub(super) services: Arc<Services>,
    pub(super) sessions: Arc<Mutex<HashMap<String, LiveSession>>>,
    /// Serializes opening, deleting and handing off chats so closed/deleted chats cannot be reopened concurrently.
    pub(super) opening: tokio::sync::Mutex<()>,
    pub(super) inline: InlineAssist,
    pub(super) git: Git,
    pub(super) maps: Arc<ProjectMaps>,
    /// The folder Settings → Skills last showed, for the status sent when an MCP server changes.
    pub(super) extensions_cwd: Mutex<Option<String>>,
    /// What swarms act on chats through; the hub holds it weakly.
    swarm_chats: Mutex<Option<Arc<dyn super::swarm::Chats>>>,
}

impl Agent {
    /// Build the agent side of `host` and register its RPC methods.
    pub async fn start(host: &Arc<Host>, preview: Option<Preview>) -> HostResult<Arc<Self>> {
        let env = host.env.clone();
        let store = ChatStore::load(env.home.join("chats"))?;
        let llm = Arc::new(Llm::new(host.network.clone(), env.version));
        let models = Arc::new(Models::new(host.settings.clone(), host.network.clone(), llm));
        if env.configure_deepseek {
            models.configure(&serde_json::to_value(deepseek_flash())?)?;
        }
        let mcp = McpManager::new(host.network.clone());
        {
            // Web MCP servers that failed may be reachable through the new proxy settings.
            let mcp = mcp.clone();
            host.network.on_change(move || mcp.retry_web_servers());
        }
        let model_activity = Arc::new(AtomicU64::new(0));
        let memory = {
            let (activity, last, rpc) = (host.activity.clone(), model_activity.clone(), host.rpc.clone());
            ProjectMemory::new(memory::Deps {
                root: env.carrier_state_root.join("project-memory"),
                target: env.target_id.clone(),
                models: models.clone(),
                chats: store.clone(),
                projects: host.projects.clone(),
                registry: host.registry.clone(),
                busy: Box::new(move || activity.active()),
                last_model_activity: Box::new(move || last.load(Ordering::SeqCst)),
                changed: Box::new(move |workspace_id| rpc.emit("memory.changed", json!({ "workspaceId": workspace_id }))),
            })
        };
        let snapshots = Arc::new(Snapshots::new(env.home.join("snapshots")));
        let swarm = super::swarm::Hub::new(env.home.join("swarms"), store.clone(), host.rpc.clone());
        let external = ExternalManager::new(host.settings.clone(), env.windows);
        let services = Arc::new(Services {
            env: env.clone(),
            settings: host.settings.clone(),
            models: models.clone(),
            store,
            mcp: mcp.clone(),
            memory,
            projects: host.projects.clone(),
            runtime: host.runtime.clone(),
            activity: host.activity.clone(),
            rpc: host.rpc.clone(),
            snapshots: snapshots.clone(),
            network: host.network.clone(),
            preview,
            model_activity,
            swarm,
            languages: host.ide.get().map(|ide| ide.languages.clone()),
            ide_tools: host.ide.get().map(|ide| ide.tools.clone()),
            maps: host.maps.clone(),
            external,
        });
        let rpc = host.rpc.clone();
        let agent = Arc::new(Agent {
            inline: InlineAssist::new(models.clone(), host.settings.clone(), move |request_id, text| rpc.emit("inline.progress", json!({ "requestId": request_id, "text": text }))),
            git: Git::new(host.projects.clone(), host.runtime.clone()),
            maps: host.maps.clone(),
            services,
            sessions: Arc::default(),
            opening: tokio::sync::Mutex::new(()),
            extensions_cwd: Mutex::new(None),
            swarm_chats: Mutex::new(None),
        });
        {
            let sessions = agent.sessions.clone();
            host.activity.add(move || sessions.lock().unwrap().values().any(|entry| entry.session.running()));
        }
        mcp.sync(&host.settings.get().mcp_servers);
        {
            let agent = Arc::downgrade(&agent);
            mcp.on_change(move || {
                if let Some(agent) = agent.upgrade() {
                    let cwd = agent.extensions_cwd.lock().unwrap().clone();
                    agent.services.rpc.emit("extensions.changed", agent.extensions_status(cwd.as_deref()));
                }
            });
        }
        {
            let agent = Arc::downgrade(&agent);
            models.on_change(move |status| {
                let Some(agent) = agent.upgrade() else { return };
                agent.services.rpc.emit("models.changed", status);
                // Open chats show the default model and its context window; both may have changed.
                for session in agent.live_sessions() {
                    session.refresh();
                }
            });
        }
        let home = env.home.clone();
        tokio::task::spawn_blocking(move || {
            sweep_spill(&home.join("spill"));
            snapshots.sweep(RETENTION);
        });
        let chats: Arc<dyn super::swarm::Chats> = Arc::new(SwarmChats(Arc::downgrade(&agent)));
        agent.services.swarm.set_chats(Arc::downgrade(&chats));
        *agent.swarm_chats.lock().unwrap() = Some(chats);
        agent.register();
        Ok(agent)
    }

    /// Stop chats, memory, MCP servers and editor requests, and save the chat index.
    pub async fn close(&self) {
        self.inline.close();
        let sessions = self.live_sessions();
        futures::future::join_all(sessions.iter().map(|session| session.close())).await;
        self.services.external.close().await;
        self.services.memory.close().await;
        self.services.mcp.close().await;
        if let Err(error) = self.services.store.flush() {
            log::error!("[chats] saving the index failed: {error}");
        }
    }

    pub(super) fn live_sessions(&self) -> Vec<Arc<Session>> {
        self.sessions.lock().unwrap().values().map(|entry| entry.session.clone()).collect()
    }

    pub(super) fn live(&self, id: &str) -> Option<Arc<Session>> {
        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions.get_mut(id)?;
        entry.last_used = crate::text::now_ms();
        Some(entry.session.clone())
    }

    /// A chat, loaded from disk the first time it is asked for; evicts oldest idle sessions beyond `MAX_LIVE_SESSIONS`.
    pub(super) async fn session(&self, id: &str) -> HostResult<Arc<Session>> {
        if let Some(session) = self.live(id) {
            return Ok(session);
        }
        let (session, evicted) = {
            let _opening = self.opening.lock().await;
            if let Some(session) = self.live(id) {
                return Ok(session);
            }
            if self.services.store.get(id).is_none() {
                return Err(not_found_chat());
            }
            let session = Session::open(self.services.clone(), id).await?;
            let now = crate::text::now_ms();
            let mut evicted = Vec::new();
            {
                let mut sessions = self.sessions.lock().unwrap();
                sessions.insert(id.to_owned(), LiveSession { session: session.clone(), last_used: now });
                while sessions.len() > MAX_LIVE_SESSIONS {
                    let candidate = sessions
                        .iter()
                        .filter(|(key, entry)| key.as_str() != id && entry.session.is_evictable())
                        .min_by_key(|(_, entry)| entry.last_used)
                        .map(|(key, _)| key.clone());
                    let Some(key) = candidate else { break };
                    if let Some(removed) = sessions.remove(&key) {
                        evicted.push(removed.session);
                    }
                }
            }
            (session, evicted)
        };
        for stale in evicted {
            tokio::spawn(async move {
                stale.close().await;
            });
        }
        Ok(session)
    }

    pub(super) fn summary(&self, id: &str) -> HostResult<SessionSummary> {
        if let Some(session) = self.live(id) {
            return Ok(session.summary());
        }
        self.services.store.get(id).map(|chat| summary_of(&chat, "idle", false)).ok_or_else(not_found_chat)
    }

    /// Persist metadata changes, sync any live in-memory `Session.inner.chat`, and emit `sessions.changed`.
    pub(super) async fn set_meta(&self, id: &str, patch: MetaPatch) -> HostResult<SessionSummary> {
        let updated = self.services.store.set_meta(id, patch).await?;
        let changed = if let Some(session) = self.live(id) {
            session.sync_chat(updated);
            session.summary()
        } else {
            summary_of(&updated, "idle", false)
        };
        self.services.rpc.emit("sessions.changed", &changed);
        Ok(changed)
    }

    pub(super) fn cwd_for(&self, workspace_id: Option<&str>) -> Option<String> {
        workspace_id.and_then(|workspace| self.services.projects.get(workspace)).map(|project| project.path)
    }

    /// The primary folder of a project, where its chats start.
    pub(super) fn project_folder(&self, workspace_id: Option<&str>) -> Option<PathBuf> {
        workspace_id.and_then(|id| self.services.projects.get(id)).map(|project| PathBuf::from(project.path))
    }

    pub(super) fn extensions_status(&self, cwd: Option<&str>) -> Value {
        let trusted = cwd.is_some_and(|dir| self.services.settings.is_workspace_trusted(None, Some(Path::new(dir))));
        json!({
            "skills": skills::discover(cwd.map(Path::new)),
            "servers": self.services.settings.get().mcp_servers,
            "status": self.services.mcp.status(),
            "trusted": trusted,
        })
    }

    pub(super) fn security_status(&self, workspace_id: Option<&str>) -> Value {
        let rules = workspace_id.map(|workspace| self.services.settings.permission_rules(workspace)).unwrap_or_default();
        let workspace_path = self.cwd_for(workspace_id);
        let trusted = self.services.settings.is_workspace_trusted(workspace_id, workspace_path.as_deref().map(Path::new));
        let mut status = json!({
            "defaultMode": self.services.settings.default_permission(),
            "rules": rules,
            "trusted": trusted,
            "trustedWorkspaces": self.services.settings.trusted_workspaces(),
        });
        if let Some(path) = workspace_path {
            status["workspacePath"] = json!(path);
        }
        status
    }

    pub(super) fn save_servers(&self, change: impl FnOnce(&mut Vec<McpServerConfig>) -> HostResult<()>) -> HostResult<Value> {
        let data = self.services.settings.update(|data| change(&mut data.mcp_servers))?;
        self.services.mcp.sync(&data.mcp_servers);
        let cwd = self.extensions_cwd.lock().unwrap().clone();
        let status = self.extensions_status(cwd.as_deref());
        self.services.rpc.emit("extensions.changed", &status);
        Ok(status)
    }

    /// The proxy choice from Settings, where a masked password stands for the stored one.
    fn proxy_choice(&self, params: ProxyParams) -> HostResult<ProxySettings> {
        if !matches!(params.mode.as_str(), "system" | "off" | "manual") {
            return Err(HostError::invalid("Unknown proxy mode."));
        }
        let manual = params.mode == "manual";
        let typed = ProxySettings { url: manual.then(|| params.url.unwrap_or_default().trim().to_owned()), mode: params.mode };
        let choice = self.services.network.with_stored_password(typed);
        if !manual {
            return Ok(choice);
        }
        let url = proxy_url(choice.url.as_deref())
            .ok_or_else(|| HostError::invalid(l("代理地址无效，应为 host:port 或 http://host:port。", "The proxy address is invalid. Use host:port or http://host:port.")))?;
        let password = url::Url::parse(&url).ok().and_then(|url| url.password().map(|password| percent_encoding::percent_decode_str(password).decode_utf8_lossy().into_owned()));
        // The mask stands for the saved password only at the saved address.
        if password.as_deref() == Some(PASSWORD_MASK) {
            return Err(HostError::invalid(l("代理地址已更改，请重新输入代理密码。", "The proxy address changed. Enter the proxy password again.")));
        }
        Ok(choice)
    }

    pub(super) async fn network_status(&self) -> Value {
        let target = self.services.models.resolve(None).map(|resolved| resolved.setup.base_url);
        json!(self.services.network.status(target.as_deref()).await)
    }

    /// Register one method whose parameters deserialize into `P`.
    pub(super) fn on<P, T, F, Fut>(self: &Arc<Self>, method: &str, handler: F)
    where
        P: DeserializeOwned + Send + 'static,
        T: Serialize + 'static,
        F: Fn(Arc<Agent>, P) -> Fut + Copy + Send + Sync + 'static,
        Fut: Future<Output = HostResult<T>> + Send + 'static,
    {
        let agent = self.clone();
        self.services.rpc.register(method, move |params| {
            let agent = agent.clone();
            async move { handler(agent, rpc::params(params)?).await }
        });
    }

    fn register(self: &Arc<Self>) {
        self.register_models();
        self.register_external();
        self.register_sessions();
        self.register_chat();
        self.register_tools();
        self.register_settings();
    }
}

/// Delete spill files older than [`RETENTION`] and prune emptied session spill folders.
fn sweep_spill(root: &Path) {
    let Ok(sessions) = std::fs::read_dir(root) else { return };
    let cutoff = std::time::SystemTime::now() - RETENTION;
    for directory in sessions.flatten() {
        let dir_path = directory.path();
        let Ok(files) = std::fs::read_dir(&dir_path) else { continue };
        let mut remaining = 0usize;
        for file in files.flatten() {
            let path: PathBuf = file.path();
            if file.metadata().and_then(|info| info.modified()).is_ok_and(|modified| modified < cutoff) {
                if std::fs::remove_file(path).is_err() {
                    remaining += 1;
                }
            } else {
                remaining += 1;
            }
        }
        if remaining == 0 {
            let _ = std::fs::remove_dir(&dir_path);
        }
    }
}
