//! One chat (3.x `session.ts`): its transcript on disk, the agent loop while it runs, and the events the page follows.

mod approval;
mod context_tools;
mod events;
mod external;
mod messaging;
mod review;
mod runner;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use rainy_protocol::chat::{
    AssistantMessage, BackgroundCommand, ContextUsage, ExternalTeam, ImageContent, PendingApproval, QueuedMessage, RetryWait, SessionSnapshot, SessionStateEvent, SessionSummary, TranscriptEntry,
    UsageSummary,
};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::budget::{Calibration, Reported, resolve_budget};
use super::context::SESSION_REFERENCE;
use super::external::team::AntigravityTeam;
use super::external::{ExternalManager, ExternalTurnHandle, external_provider_id};
use super::llm::{self, Level, Message};
use super::mcp::McpManager;
use super::memory::ProjectMemory;
use super::models::{Models, Resolved};
use super::orchestrate::{FRICTION_ERRORS, Phase};
use super::profile::Profile;
use super::snapshots::Snapshots;
use super::store::{ChatStore, MetaPatch, StoredChat};
use super::tools::Observations;
use super::tools::browser::Preview;
use super::tools::jobs::Jobs;
use super::tools::ToolStyle;
use super::{stream, util};
use crate::activity::Activity;
use crate::env::HostEnv;
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::network::Network;
use crate::projects::Projects;
use crate::rpc::RpcHub;
use crate::runtime::Runtime;
use crate::settings::{ModelSelection, Settings};
use crate::text::now_ms;

pub(super) const STREAM_THROTTLE: Duration = Duration::from_millis(50);
/// The streaming reply goes whole to the page this often; the updates between carry only what was added.
pub(super) const STREAM_WHOLE_MS: u64 = 2000;
pub(super) const TOOL_UPDATE_THROTTLE_MS: u64 = 200;
/// Chats referenced with `@[…](rainy-session:<id>)` in one message.
pub(super) const MAX_REFERENCES: usize = 3;
/// UTF-16 units of one referenced chat the model sees, and of each of its turns.
const REFERENCE_CHARS: usize = 64 * 1024;
const REFERENCE_TURN_CHARS: usize = 4000;
/// Model-facing result of a call still waiting when the user sent a new message.
pub(super) const SUPERSEDED: &str = "The user did not answer the approval request for this tool call and sent a new message instead. Read it before continuing.";

/// What every chat uses from the rest of the Host.
pub struct Services {
    pub env: HostEnv,
    pub settings: Arc<Settings>,
    pub models: Arc<Models>,
    pub store: Arc<ChatStore>,
    pub mcp: Arc<McpManager>,
    pub memory: Arc<ProjectMemory>,
    pub projects: Arc<Projects>,
    pub runtime: Arc<Runtime>,
    pub activity: Arc<Activity>,
    pub rpc: Arc<RpcHub>,
    /// Contents write and edit replaced, for undo.
    pub snapshots: Arc<Snapshots>,
    pub network: Arc<Network>,
    /// Carries out the agent's preview-window commands; absent where the carrier cannot show one.
    pub preview: Option<Preview>,
    /// When a chat model request last started or a run last ended (epoch ms); project memory waits for quiet after it.
    pub model_activity: Arc<AtomicU64>,
    /// Boards and inboxes of `/swarm` chats.
    pub swarm: Arc<super::swarm::Hub>,
    /// Shared Host LSP diagnostics pool (used by `lsp_diagnostics` and post-edit diagnostics).
    pub languages: Option<Arc<crate::ide::language::Languages>>,
    /// IDE formatter tools (used by `PostEdit` built-in `format` hooks).
    pub ide_tools: Option<crate::ide::tools::Tools>,
    /// Shared ProjectMap index (used by `module_deps`).
    pub maps: Arc<crate::map::ProjectMaps>,
    /// External CLI agent harness (`codex app-server --stdio`, `agy acp`, custom ACP).
    pub external: Arc<ExternalManager>,
}

/// Title from the first message: control characters removed, at most five words and 40 bytes.
pub fn fallback_title(text: &str) -> String {
    // A swarm is named after its task.
    let text = super::swarm::swarm_command(text).unwrap_or(text);
    let text = SESSION_REFERENCE.replace_all(text, "@$1");
    let cleaned: String = text.chars().map(|char| if char.is_control() && (char as u32) < 0x80 { ' ' } else { char }).collect();
    let all: Vec<&str> = cleaned.split_whitespace().collect();
    let words = all.iter().take(5).copied().collect::<Vec<_>>().join(" ");
    if words.is_empty() {
        return l("新对话", "New chat");
    }
    if words.len() <= 40 {
        return if all.len() > 5 { format!("{words}…") } else { words };
    }
    util::truncate_bytes_ellipsis(&words, 40)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// What the model sees of a referenced chat within [`REFERENCE_CHARS`]: each turn cut to [`REFERENCE_TURN_CHARS`],
/// then the first turn, which usually states the task, and as many of the latest turns as fit.
pub fn reference_snapshot(session_id: &str, title: &str, turns: &[(&'static str, String)]) -> Value {
    let mut truncated = false;
    let cut: Vec<Value> = turns
        .iter()
        .map(|(role, text)| {
            if utf16_len(text) <= REFERENCE_TURN_CHARS {
                return json!({ "role": role, "text": text });
            }
            truncated = true;
            let mut units = 0;
            let kept: String = text.chars().take_while(|char| {
                units += char.len_utf16();
                units <= REFERENCE_TURN_CHARS
            }).collect();
            json!({ "role": role, "text": format!("{kept}…") })
        })
        .collect();
    let size = |turn: &Value| utf16_len(&turn.to_string()) + 1;
    let mut used = utf16_len(&json!({ "sessionId": session_id, "title": title, "truncated": true, "turns": [] }).to_string());
    let mut latest = Vec::new();
    if let Some(first) = cut.first() {
        used += size(first);
    }
    for turn in cut.iter().skip(1).rev() {
        if used + size(turn) > REFERENCE_CHARS {
            truncated = true;
            break;
        }
        used += size(turn);
        latest.push(turn.clone());
    }
    latest.reverse();
    let kept: Vec<Value> = cut.first().cloned().into_iter().chain(latest).collect();
    if truncated { json!({ "sessionId": session_id, "title": title, "truncated": true, "turns": kept }) } else { json!({ "sessionId": session_id, "title": title, "turns": kept }) }
}

/// What two requests must share to share a prompt cache, besides their prefix.
pub(super) fn request_key(model: &llm::Model, thinking: Option<Level>) -> String {
    format!("{}/{}/{}", model.provider, model.id, thinking.map_or("off", Level::as_str))
}

pub(super) fn jitter() -> f64 {
    (uuid::Uuid::new_v4().as_u128() >> 75) as f64 / (1u64 << 53) as f64
}

/// The tools a model works with: GPT models patch files (freeform on OpenAI's own Responses API) and the plan tool
/// follows the family's own agent.
pub(super) fn tool_style(model: &llm::Model) -> ToolStyle {
    let profile = Profile::of(&model.id);
    let freeform = model.api == llm::Api::Responses && model.base_url.contains("api.openai.com");
    ToolStyle { patch: profile.patches().then_some(freeform), plan: profile.plan_style() }
}

pub(super) fn no_model() -> HostError {
    HostError::new("no-model", l("请先在 设置 → 模型 中配置模型。", "Configure a model in Settings → Models first."))
}

pub(super) fn busy(zh: &str, en: &str) -> HostError {
    HostError::new("busy", l(zh, en))
}

/// What travels with a message the run adds to the conversation.
pub enum Meta {
    User { entry_id: String, text: String, images: Vec<ImageContent>, queue_id: Option<String> },
    /// `instructions`, `memory`, `reference` or `notice`.
    Context { entry_id: String, label: &'static str, text: String },
}

/// How a wait for approval ended: run the call, or refuse it with a model-facing text and maybe end the run after
/// this step.
pub(super) enum Answer {
    Allow,
    AllowSession,
    Refuse { text: String, stop: bool, by: Option<&'static str> },
}

pub(super) fn refusal(text: impl Into<String>, stop: bool, by: Option<&'static str>) -> Answer {
    Answer::Refuse { text: text.into(), stop, by }
}

pub(super) struct RunInfo {
    pub(super) started: u64,
    /// Index of the run's first entry.
    pub(super) from: usize,
    pub(super) requests: u32,
    pub(super) usage: UsageSummary,
    pub(super) aborted: bool,
    pub(super) overflow_retried: bool,
    pub(super) retries: u32,
    pub(super) cancel: CancellationToken,
}

pub(super) struct Approval {
    pub(super) approval: PendingApproval,
    pub(super) settle: oneshot::Sender<Answer>,
}

pub(super) struct Inner {
    pub(super) chat: StoredChat,
    pub(super) entries: Vec<TranscriptEntry>,
    /// Write and edit calls whose changes the user reviewed and kept.
    pub(super) kept: HashSet<String>,
    pub(super) status: &'static str,
    /// Atomically claimed while preparing/spawning a run so concurrent `send`/`retry` calls queue instead of racing.
    pub(super) starting: bool,
    pub(super) error: Option<String>,
    pub(super) queue: Vec<QueuedMessage>,
    /// Queued messages (sent when the run ends), by queue id.
    pub(super) pending: HashMap<String, Vec<(Message, Meta)>>,
    /// Messages added before the run's next request.
    pub(super) steering: Vec<(Message, Meta)>,
    pub(super) streaming: Option<AssistantMessage>,
    /// What the page last received of the streaming reply, and when it last received it whole.
    pub(super) stream_sent: Option<(stream::Baseline, u64)>,
    pub(super) stream_armed: bool,
    pub(super) running_tools: HashMap<String, u64>,
    pub(super) tool_updates: HashMap<String, u64>,
    /// What each running tool last reported besides its output (`ToolOutput::details`), as sent to the page.
    pub(super) tool_details: HashMap<String, Value>,
    pub(super) calibration: Calibration,
    pub(super) last_estimate: u64,
    /// `provider/model` key of the latest prepared request, so `Calibration` stays isolated per model.
    pub(super) last_model_key: String,
    /// Messages, system prompt length and tools of the run's latest request.
    pub(super) last_sent: (usize, usize, usize),
    /// What the provider counted for the run's latest request; `None` once the run's messages were rebuilt.
    pub(super) reported: Option<Reported>,
    /// Start time and key of the chat's latest request, to judge whether its prompt cache is warm.
    pub(super) last_request: Option<(u64, String)>,
    pub(super) context: Option<ContextUsage>,
    pub(super) message_started: u64,
    /// Start times of thinking blocks still streaming, and durations of finished ones, by content index.
    pub(super) thinking_started: HashMap<usize, u64>,
    pub(super) thinking_ms: BTreeMap<usize, u64>,
    pub(super) run: Option<RunInfo>,
    /// Cancellation token for an active manual `/compact` request.
    pub(super) compact_cancel: Option<CancellationToken>,
    /// Cancellation token for active `Stop` lifecycle hooks inside `finish_run`.
    pub(super) stop_cancel: Option<CancellationToken>,
    /// Set by `Session::close` so no queued run can start after close begins.
    pub(super) closing: bool,
    pub(super) retry_wait: Option<RetryWait>,
    pub(super) retry_wake: Option<oneshot::Sender<bool>>,
    pub(super) touched: Vec<PathBuf>,
    pub(super) approvals: Vec<Approval>,
    /// The user refused a call without guidance: skip the rest of the step and end the run.
    pub(super) stop_after_step: bool,
    /// Calls refused by the user or by read-only mode; their entries say so for the page.
    pub(super) refused: HashMap<String, &'static str>,
    /// Orchestration: which model holds the turn, the phase the run's tools and prompt were made for, failed tool
    /// calls in a row of the executor, and why the executor handed back.
    pub(super) phase: Phase,
    pub(super) phase_sent: Option<Phase>,
    pub(super) tool_errors: u32,
    pub(super) phase_note: Option<String>,
    /// Live control handle when an external CLI agent turn is running.
    pub(super) external_turn: Option<ExternalTurnHandle>,
    /// Latest unified diff reported by an external agent (e.g. Codex `turn/diff/updated`).
    pub(super) external_diff: Option<String>,
    /// Queued user prompts for an external agent chat, keyed by `QueuedMessage::id`.
    pub(super) external_pending: HashMap<String, (String, Vec<ImageContent>)>,
    /// The team the external agent runs (Codex sub-agents), as it last sent it.
    pub(super) external_team: Option<ExternalTeam>,
    /// Antigravity's team of this chat, read from its folders, and the task that follows it.
    pub(super) agy_team: Option<Arc<Mutex<AntigravityTeam>>>,
    pub(super) agy_watch: Option<tokio::task::JoinHandle<()>>,
}

/// Whether the session currently owns or is preparing an active run/compaction.
pub(super) fn is_busy(inner: &Inner) -> bool {
    matches!(inner.status, "running" | "compacting") || inner.starting || inner.run.is_some()
}

/// Releases `Inner::starting` automatically if run preparation exits early or is cancelled before spawning.
pub(super) struct StartGuard<'a> {
    session: &'a Session,
    active: bool,
}

impl<'a> StartGuard<'a> {
    pub(super) fn new(session: &'a Session) -> Self {
        Self { session, active: true }
    }

    pub(super) fn disarm(mut self) {
        self.active = false;
    }
}

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            let mut inner = self.session.inner.lock().unwrap();
            inner.starting = false;
            if inner.run.is_none() && inner.status == "running" {
                inner.status = if inner.error.is_some() { "error" } else { "idle" };
            }
        }
    }
}

/// A loaded chat.
pub struct Session {
    pub(super) services: Arc<Services>,
    pub(super) id: String,
    pub(super) inner: Mutex<Inner>,
    pub(super) observations: Arc<Observations>,
    /// Commands of this chat that keep running after their tool call.
    pub(super) jobs: Arc<Jobs>,
    /// Keeps transcript appends in order.
    pub(super) writer: tokio::sync::Mutex<()>,
    pub(super) task: Mutex<Option<JoinHandle<()>>>,
    /// The chat's Claude message thread, continued by its next request within a few minutes.
    pub(super) thread: Arc<llm::thread::Slot>,
    pub(super) this: Weak<Session>,
}

impl Session {
    /// Load a chat from disk.
    pub async fn open(services: Arc<Services>, id: &str) -> HostResult<Arc<Self>> {
        let file = services.store.read(id).await?;
        Ok(Arc::new_cyclic(|this: &Weak<Session>| {
            let weak = this.clone();
            let jobs = Jobs::new(move || {
                if let Some(session) = weak.upgrade() {
                    session.publish_state();
                }
            });
            Session {
                id: file.chat.id.clone(),
                inner: Mutex::new(Inner {
                    phase: if file.chat.orchestrate_phase.as_deref() == Some("execute") && file.chat.orchestrate.is_some() {
                        Phase::Execute
                    } else {
                        Phase::Plan
                    },
                    phase_note: file.chat.orchestrate_note.clone(),
                    chat: file.chat,
                    entries: file.entries,
                    kept: file.kept.into_iter().collect(),
                    status: "idle",
                    starting: false,
                    error: None,
                    queue: Vec::new(),
                    pending: HashMap::new(),
                    steering: Vec::new(),
                    streaming: None,
                    stream_sent: None,
                    stream_armed: false,
                    running_tools: HashMap::new(),
                    tool_updates: HashMap::new(),
                    tool_details: HashMap::new(),
                    calibration: Calibration::default(),
                    last_estimate: 0,
                    last_model_key: String::new(),
                    last_sent: (0, 0, 0),
                    reported: None,
                    last_request: None,
                    context: None,
                    message_started: 0,
                    thinking_started: HashMap::new(),
                    thinking_ms: BTreeMap::new(),
                    run: None,
                    compact_cancel: None,
                    stop_cancel: None,
                    closing: false,
                    retry_wait: None,
                    retry_wake: None,
                    touched: Vec::new(),
                    approvals: Vec::new(),
                    stop_after_step: false,
                    refused: HashMap::new(),
                    phase_sent: None,
                    tool_errors: 0,
                    external_turn: None,
                    external_diff: None,
                    external_pending: HashMap::new(),
                    external_team: None,
                    agy_team: None,
                    agy_watch: None,
                }),
                services,
                observations: Arc::default(),
                jobs,
                writer: tokio::sync::Mutex::new(()),
                task: Mutex::new(None),
                thread: Arc::default(),
                this: this.clone(),
            }
        }))
    }

    pub(super) fn chat(&self) -> StoredChat {
        self.inner.lock().unwrap().chat.clone()
    }

    /// Keep the in-memory metadata synchronized after external `store.set_meta` updates (rename, handOff, archive, etc.).
    pub fn sync_chat(&self, chat: StoredChat) {
        let mut inner = self.inner.lock().unwrap();
        if chat.orchestrate.is_none() {
            inner.phase = Phase::Plan;
            inner.phase_note = None;
        }
        inner.chat = chat;
    }

    /// Whether a run or compaction is in progress (including atomic startup reservation).
    pub fn running(&self) -> bool {
        is_busy(&self.inner.lock().unwrap())
    }

    /// Whether this live session is completely idle and safe to evict from memory under LRU pressure.
    pub fn is_evictable(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        !is_busy(&inner)
            && inner.approvals.is_empty()
            && inner.queue.is_empty()
            && inner.pending.is_empty()
            && inner.steering.is_empty()
            && inner.external_pending.is_empty()
            && inner.agy_watch.as_ref().is_none_or(|task| task.is_finished())
            && self.jobs.list().is_empty()
    }

    /// The history-list row.
    pub fn summary(&self) -> SessionSummary {
        let inner = self.inner.lock().unwrap();
        summary_of(&self.services.store.get(&self.id).unwrap_or_else(|| inner.chat.clone()), inner.status, !inner.approvals.is_empty())
    }

    /// What the agent may do without asking: the chat's own mode, else the default from Settings.
    pub fn permission(&self) -> String {
        self.chat().permission.unwrap_or_else(|| self.services.settings.default_permission())
    }

    /// The chat's own model while it is still saved; after it is removed or renamed the chat follows the default.
    pub(super) fn own_model(&self) -> Option<ModelSelection> {
        let chat = self.chat();
        if let Some(backend) = chat.external_backend {
            if let Some(own) = chat.model {
                return Some(own);
            }
            let configured = self.services.settings.get().external_agents.into_iter().find(|c| c.kind == backend.kind);
            return Some(ModelSelection {
                provider: external_provider_id(&backend.kind),
                model: configured
                    .as_ref()
                    .and_then(|c| c.default_model.clone())
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or_else(|| "default".into()),
                thinking: configured.and_then(|c| c.default_thinking),
            });
        }
        let own = chat.model?;
        self.services.settings.get().models.iter().any(|model| model.provider == own.provider && model.model == own.model).then_some(own)
    }

    /// The model this chat uses: its own choice, else the default.
    pub fn selection(&self) -> Option<ModelSelection> {
        if self.chat().external_backend.is_some() {
            return self.own_model();
        }
        self.own_model().or_else(|| self.services.settings.get().selected)
    }

    /// The resolved model for the next request.
    pub fn resolved(&self) -> Option<Resolved> {
        self.services.models.resolve(self.own_model().as_ref())
    }

    /// Orchestration's executor while it is still saved; `None` when the chat does not orchestrate.
    pub(super) fn executor(&self) -> Option<ModelSelection> {
        let executor = self.chat().orchestrate?;
        self.services.settings.get().models.iter().any(|model| model.provider == executor.provider && model.model == executor.model).then_some(executor)
    }

    /// The model of the current request: the executor while it holds an orchestrated turn, else the chat's model.
    pub(super) fn request_resolved(&self) -> Option<Resolved> {
        let executing = self.inner.lock().unwrap().phase == Phase::Execute;
        match self.executor().filter(|_| executing) {
            Some(executor) => self.services.models.resolve(Some(&executor)).or_else(|| self.resolved()),
            None => self.resolved(),
        }
    }

    /// Persist orchestration phase/note asynchronously so Host restarts restore the exact active model and tools.
    pub(super) fn persist_orchestrate_phase(&self, phase: Phase, note: Option<String>) {
        if self.chat().orchestrate.is_none() {
            return;
        }
        let phase_str = match phase {
            Phase::Plan => "plan",
            Phase::Execute => "execute",
        };
        let (store, id) = (self.services.store.clone(), self.id.clone());
        let patch = MetaPatch {
            orchestrate_phase: Some(Some(phase_str.to_owned())),
            orchestrate_note: Some(note),
            ..MetaPatch::default()
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(error) = store.set_meta(&id, patch).await {
                    log::warn!("[chat {id}] persisting orchestrate phase failed: {}", error.message);
                }
            });
        }
    }

    /// Hand the turn to the other model of an orchestrated chat; it takes over at the next request.
    pub(super) fn switch_phase(&self, phase: Phase, text: String) {
        let note = (phase == Phase::Plan).then_some(text);
        {
            let mut inner = self.inner.lock().unwrap();
            inner.phase = phase;
            inner.tool_errors = 0;
            inner.phase_note = note.clone();
        }
        self.persist_orchestrate_phase(phase, note);
    }

    /// Count the executor's failed calls; `true` when they reached the limit and the planner took the turn back.
    pub(super) fn friction(&self, failed: bool) -> bool {
        let handed_back = {
            let mut inner = self.inner.lock().unwrap();
            if inner.phase != Phase::Execute {
                return false;
            }
            inner.tool_errors = if failed { inner.tool_errors + 1 } else { 0 };
            if inner.tool_errors < FRICTION_ERRORS {
                return false;
            }
            let note = format!("its last {FRICTION_ERRORS} tool calls failed; see the results above.");
            inner.phase = Phase::Plan;
            inner.tool_errors = 0;
            inner.phase_note = Some(note.clone());
            note
        };
        self.persist_orchestrate_phase(Phase::Plan, Some(handed_back));
        true
    }

    /// Turn orchestration on with `executor`, or off; a running chat takes it at its next request.
    pub async fn set_orchestrate(&self, executor: Option<&ModelSelection>) -> HostResult<Option<ModelSelection>> {
        if self.chat().external_backend.is_some() && executor.is_some() {
            return Err(HostError::invalid(l(
                "外部 Agent 对话由外部 CLI 自主编排，不支持双模型协作模式。",
                "External agent chats are orchestrated by the external CLI and do not support dual-model orchestration.",
            )));
        }
        let stored = match executor {
            Some(selection) => Some(
                self.services
                    .models
                    .resolve(Some(selection))
                    .filter(|resolved| resolved.setup.provider == selection.provider && resolved.setup.model == selection.model)
                    .ok_or_else(|| HostError::new("not-found", l("模型尚未配置。", "This model is not configured.")))?
                    .selection(),
            ),
            None => None,
        };
        let patch = MetaPatch {
            orchestrate: Some(stored.clone()),
            orchestrate_phase: if stored.is_none() { Some(None) } else { None },
            orchestrate_note: if stored.is_none() { Some(None) } else { None },
            ..MetaPatch::default()
        };
        let chat = self.services.store.set_meta(&self.id, patch).await?;
        {
            let mut inner = self.inner.lock().unwrap();
            inner.chat = chat;
            if stored.is_none() {
                inner.phase = Phase::Plan;
                inner.phase_note = None;
            }
        }
        self.publish_state();
        self.services.rpc.emit("sessions.changed", self.summary());
        Ok(stored)
    }

    /// Everything the page shows for this chat; `limit` keeps only the last entries (older ones load on demand).
    pub fn snapshot(&self, limit: Option<usize>) -> SessionSnapshot {
        let (model, permission, summary) = (self.selection(), self.permission(), self.summary());
        let inner = self.inner.lock().unwrap();
        let start = limit.map_or(0, |limit| inner.entries.len().saturating_sub(limit));
        SessionSnapshot {
            summary,
            entries: inner.entries[start..].to_vec(),
            model,
            queue: inner.queue.clone(),
            context: inner.context.clone(),
            streaming: inner.streaming.clone(),
            running_tools: inner.running_tools.keys().cloned().collect(),
            permission,
            approvals: inner.approvals.iter().map(|item| item.approval.clone()).collect(),
            background: self.background(),
            retry: inner.retry_wait.clone(),
            error: inner.error.clone(),
        }
    }

    /// The snapshot the page loads a chat from; the page then holds the streaming reply as it is now, so the next
    /// update is computed from that.
    pub fn page_snapshot(&self, limit: Option<usize>) -> SessionSnapshot {
        let snapshot = self.snapshot(limit);
        let mut inner = self.inner.lock().unwrap();
        if let (Some(streaming), Some((_, whole_at))) = (&snapshot.streaming, &inner.stream_sent) {
            let whole_at = *whole_at;
            inner.stream_sent = Some((stream::baseline(streaming), whole_at));
        }
        snapshot
    }

    fn background(&self) -> Vec<BackgroundCommand> {
        self.jobs
            .list()
            .into_iter()
            .map(|job| BackgroundCommand {
                id: job.id,
                command: job.command,
                description: job.description,
                cwd: job.cwd,
                started_at: job.started_at as f64,
                state: job.state.into(),
                exit_code: job.exit_code.map(i64::from),
                signal: job.signal,
                pending: job.pending as f64,
            })
            .collect()
    }

    /// The text of the latest reply.
    pub fn last_reply(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        inner.entries.iter().rev().find_map(|entry| match entry {
            TranscriptEntry::Assistant { message, .. } => Some(llm::reply_text(message)).filter(|text| !text.trim().is_empty()),
            _ => None,
        })
    }

    /// The run status, whether a call waits for approval, and whether the user wrote while it runs.
    pub fn swarm_state(&self) -> super::swarm::ChatState {
        let inner = self.inner.lock().unwrap();
        let status = if inner.starting && inner.status == "idle" { "running" } else { inner.status };
        super::swarm::ChatState {
            status: status.to_owned(),
            awaiting_approval: !inner.approvals.is_empty(),
            user_waiting: !inner.queue.is_empty() || !inner.steering.is_empty(),
            has_pending: !inner.pending.is_empty() || !inner.external_pending.is_empty(),
        }
    }

    /// Stop the run or manual compaction; queued and steered messages are discarded.
    pub fn abort(&self) {
        let external_turn = {
            let mut inner = self.inner.lock().unwrap();
            inner.queue.clear();
            inner.pending.clear();
            inner.steering.clear();
            inner.external_pending.clear();
            if let Some(run) = &mut inner.run {
                run.aborted = true;
                run.cancel.cancel();
            }
            if let Some(cancel) = &inner.compact_cancel {
                cancel.cancel();
            }
            if let Some(cancel) = &inner.stop_cancel {
                cancel.cancel();
            }
            if let Some(wake) = inner.retry_wake.take() {
                let _ = wake.send(false);
            }
            inner.external_turn.clone()
        };
        if let Some(turn) = external_turn
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn(async move { turn.interrupt().await });
        }
        self.publish_state();
    }

    /// Remove a queued message that has not been sent yet; the remaining queue.
    pub fn unqueue(&self, id: &str) -> Vec<QueuedMessage> {
        let queue = {
            let mut inner = self.inner.lock().unwrap();
            if inner.pending.remove(id).is_some() || inner.external_pending.remove(id).is_some() {
                inner.queue.retain(|item| item.id != id);
            }
            inner.queue.clone()
        };
        self.publish_state();
        queue
    }

    /// Change what the agent may do without asking; waiting calls follow the new mode.
    pub async fn set_permission(&self, mode: &str) -> HostResult<String> {
        if !matches!(mode, "read-only" | "ask" | "auto") {
            return Err(HostError::invalid("Unknown permission mode."));
        }
        let chat = self.services.store.set_meta(&self.id, MetaPatch { permission: Some(mode.to_owned()), ..MetaPatch::default() }).await?;
        let (waiting, external_turn): (Vec<String>, Option<ExternalTurnHandle>) = {
            let mut inner = self.inner.lock().unwrap();
            inner.chat = chat;
            (inner.approvals.iter().map(|item| item.approval.tool_call_id.clone()).collect(), inner.external_turn.clone())
        };
        if let Some(turn) = external_turn {
            let mode = mode.to_owned();
            tokio::spawn(async move { turn.update_permission(&mode).await });
        }
        for id in waiting {
            match mode {
                "auto" => self.settle(&id, Answer::Allow),
                "read-only" => self.settle(&id, refusal(super::permissions::READ_ONLY_REASON, false, Some("mode"))),
                _ => {}
            }
        }
        self.publish_state();
        Ok(mode.to_owned())
    }

    /// Choose the model for this chat's next requests; a running chat takes it at its next request.
    pub async fn set_model(&self, selection: &ModelSelection) -> HostResult<ModelSelection> {
        if let Some(backend) = self.chat().external_backend {
            let model_id = selection.model.trim();
            let stored = ModelSelection {
                provider: external_provider_id(&backend.kind),
                model: if model_id.is_empty() { "default".into() } else { model_id.to_owned() },
                thinking: selection.thinking.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned),
            };
            let chat = self.services.store.set_meta(&self.id, MetaPatch { model: Some(Some(stored.clone())), ..MetaPatch::default() }).await?;
            self.inner.lock().unwrap().chat = chat;
            self.publish_state();
            self.services.rpc.emit("sessions.changed", self.summary());
            return Ok(stored);
        }
        let resolved = self
            .services
            .models
            .resolve(Some(selection))
            .filter(|resolved| resolved.setup.provider == selection.provider && resolved.setup.model == selection.model)
            .ok_or_else(|| HostError::new("not-found", l("模型尚未配置。", "This model is not configured.")))?;
        let stored = resolved.selection();
        let chat = self.services.store.set_meta(&self.id, MetaPatch { model: Some(Some(stored.clone())), ..MetaPatch::default() }).await?;
        self.inner.lock().unwrap().chat = chat;
        self.publish_state();
        self.services.rpc.emit("sessions.changed", self.summary());
        Ok(stored)
    }

    /// Publish run state again after Settings changed the default mode or the models; the meter takes the new window.
    pub fn refresh(&self) {
        if let Some(budget) = self.resolved().and_then(|resolved| resolve_budget(resolved.setup.context_window, resolved.setup.max_tokens).ok()) {
            if let Some(context) = &mut self.inner.lock().unwrap().context {
                context.context_window = budget.context_window as f64;
                context.input_limit = budget.input_limit as f64;
                context.output_tokens = budget.output_tokens as f64;
                context.compact_at = budget.compact_at as f64;
            }
        }
        self.publish_state();
    }

    /// Stop one background command; the chat's commands after the stop.
    pub async fn stop_command(&self, id: &str) -> HostResult<Value> {
        self.jobs.stop(id).await.map_err(HostError::other)?;
        Ok(json!(self.background()))
    }

    /// Drop a finished background command the user cleared before the model read it.
    pub fn dismiss_command(&self, id: &str) -> HostResult<Value> {
        self.jobs.forget(id).map_err(HostError::other)?;
        Ok(json!(self.background()))
    }

    /// Stop any run and wait for it; setting `closing` prevents queued runs from starting after close begins.
    pub async fn close(&self) {
        self.inner.lock().unwrap().closing = true;
        self.abort();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let task = self.task.lock().unwrap().take();
            if let Some(task) = task {
                let _ = task.await;
                continue;
            }
            if !self.inner.lock().unwrap().starting || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.jobs.close().await;
        if self.chat().external_backend.is_some() {
            self.services.external.close_session(&self.id).await;
        }
        if let Some(watch) = self.inner.lock().unwrap().agy_watch.take() {
            watch.abort();
        }
    }

    // ── persistence and events ──

    pub(super) async fn append(&self, entry: TranscriptEntry) {
        let _writer = self.writer.lock().await;
        self.inner.lock().unwrap().entries.push(entry.clone());
        if let Err(error) = self.services.store.append(&self.id, std::slice::from_ref(&entry)).await {
            log::error!("[chat {}] saving an entry failed: {error}", self.id);
        }
        self.services.rpc.emit("session.entry", json!({ "sessionId": self.id, "entry": entry }));
    }

    pub(super) async fn notice(&self, code: &str, text: String) {
        self.append(TranscriptEntry::Notice { id: uuid::Uuid::new_v4().to_string(), ts: now_ms() as f64, level: "info".into(), text, code: Some(code.into()) }).await;
    }

    pub(super) fn set_status(&self, status: &'static str) {
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.status == status {
                return;
            }
            inner.status = status;
        }
        self.publish_state();
        self.services.rpc.emit("sessions.changed", self.summary());
    }

    pub(super) fn publish_state(&self) {
        let (model, permission, background) = (self.selection(), self.permission(), self.background());
        let event = {
            let inner = self.inner.lock().unwrap();
            SessionStateEvent {
                session_id: self.id.clone(),
                status: inner.status.into(),
                queue: inner.queue.clone(),
                model,
                context: inner.context.clone(),
                permission,
                approvals: inner.approvals.iter().map(|item| item.approval.clone()).collect(),
                background,
                retry: inner.retry_wait.clone(),
                error: inner.error.clone(),
            }
        };
        self.services.rpc.emit("session.state", event);
    }
}

/// A history-list row of a stored chat.
pub fn summary_of(chat: &StoredChat, status: &str, awaiting_approval: bool) -> SessionSummary {
    SessionSummary {
        id: chat.id.clone(),
        workspace_id: chat.workspace_id.clone(),
        cwd: chat.cwd.clone(),
        title: chat.title.clone(),
        created_at: chat.created_at,
        updated_at: chat.updated_at,
        archived: chat.archived,
        pinned: chat.pinned,
        status: status.into(),
        awaiting_approval: awaiting_approval.then_some(true),
        read_at: chat.read_at,
        worktree: chat.worktree.clone(),
        swarm: chat.swarm.clone(),
        orchestrate: chat.orchestrate.clone(),
        external_backend: chat.external_backend.clone(),
        external_thread_id: chat.external_thread_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::agent::session::messaging::{context_message, settle_steering};

    #[test]
    fn titles_and_references() {
        assert_eq!(fallback_title("fix the build on windows please now"), "fix the build on windows…");
        assert_eq!(fallback_title("/swarm port the parser"), "port the parser");
        assert_eq!(fallback_title("see @[old chat](rainy-session:abc-1) now"), "see @old chat now");
        let long = "很长的标题".repeat(10);
        assert!(fallback_title(&long).ends_with('…') && fallback_title(&long).len() <= 43);
        let turns = vec![("user", "start".to_owned()), ("assistant", "x".repeat(5000))];
        let snapshot = reference_snapshot("s", "t", &turns);
        assert_eq!(snapshot["truncated"], true);
        assert_eq!(snapshot["turns"][1]["text"].as_str().unwrap().chars().count(), 4001);
    }

    #[test]
    fn steering_left_by_a_run_is_queued_or_discarded() {
        let user = |id: &str| (Message::user(id), Meta::User { entry_id: format!("e-{id}"), text: id.into(), images: Vec::new(), queue_id: Some(id.into()) });
        let queued = |id: &str| QueuedMessage { id: id.into(), text: id.into(), mode: "steer".into(), image_count: 0 };
        let steering = || vec![context_message("instructions", "found".into()), user("a"), context_message("reference", "chat".into()), user("b")];
        let (mut queue, mut pending) = (vec![queued("q"), queued("a"), queued("b")], HashMap::new());
        settle_steering(steering(), &mut queue, &mut pending, false);
        assert_eq!(queue.iter().map(|item| item.mode.as_str()).collect::<Vec<_>>(), ["steer", "queue", "queue"]);
        assert_eq!(pending["a"].len(), 2);
        assert_eq!(pending["b"].len(), 1);
        assert_eq!(pending.len(), 2);
        let (mut queue, mut pending) = (vec![queued("q"), queued("a"), queued("b")], HashMap::new());
        settle_steering(steering(), &mut queue, &mut pending, true);
        assert_eq!(queue.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), ["q"]);
        assert!(pending.is_empty());
    }

    fn test_services(root: &Path) -> Arc<Services> {
        let env = HostEnv::for_tests(root);
        let settings = Arc::new(Settings::load(root, None).unwrap());
        settings
            .update(|data| {
                data.models.push(crate::settings::ModelSetup {
                    provider: "mock".into(),
                    base_url: "http://127.0.0.1:9".into(),
                    model: "mock-strong".into(),
                    context_window: 100_000,
                    max_tokens: Some(8000),
                    local: true,
                    api: Some("openai-completions".into()),
                    thinking: None,
                    thinking_format: None,
                    max_tokens_field: None,
                    images: None,
                });
                data.models.push(crate::settings::ModelSetup {
                    provider: "mock-fast".into(),
                    base_url: "http://127.0.0.1:9".into(),
                    model: "mock-lite".into(),
                    context_window: 64_000,
                    max_tokens: Some(4000),
                    local: true,
                    api: Some("openai-completions".into()),
                    thinking: None,
                    thinking_format: None,
                    max_tokens_field: None,
                    images: None,
                });
                data.selected = Some(ModelSelection { provider: "mock".into(), model: "mock-strong".into(), thinking: None });
                Ok(())
            })
            .unwrap();
        let network = Arc::new(Network::new(env.windows, None));
        let llm = Arc::new(llm::Llm::new(network.clone(), env.version));
        let models = Arc::new(Models::new(settings.clone(), network.clone(), llm));
        let store = ChatStore::load(root.join("chats")).unwrap();
        let mcp = McpManager::new(network.clone());
        let projects = Arc::new(Projects::load(root.join("projects.json")).unwrap());
        let registry = Arc::new(crate::registry::Registry::new(root.join("state"), "test".into()));
        let activity = Arc::new(Activity::default());
        let rpc = Arc::new(RpcHub::default());
        let runtime = Runtime::new(env.clone(), projects.clone(), registry.clone(), activity.clone()).unwrap();
        let maps = Arc::new(crate::map::ProjectMaps::new(projects.clone(), root.join("maps")));
        let memory = ProjectMemory::new(super::super::memory::Deps {
            root: root.join("memory"),
            target: "test".into(),
            models: models.clone(),
            chats: store.clone(),
            projects: projects.clone(),
            registry,
            busy: Box::new(|| false),
            last_model_activity: Box::new(|| 0),
            changed: Box::new(|_| {}),
        });
        let snapshots = Arc::new(Snapshots::new(root.join("snapshots")));
        let swarm = super::super::swarm::Hub::new(root.join("swarms"), store.clone(), rpc.clone());
        let external = ExternalManager::new(settings.clone(), env.windows);
        Arc::new(Services {
            env,
            settings,
            models,
            store,
            mcp,
            memory,
            projects,
            runtime,
            activity,
            rpc,
            snapshots,
            network,
            preview: None,
            model_activity: Arc::new(AtomicU64::new(0)),
            swarm,
            languages: None,
            ide_tools: None,
            maps,
            external,
        })
    }

    #[tokio::test]
    async fn swarm_deliver_queues_without_preempting_pending_approval() {
        let dir = tempfile::tempdir().unwrap();
        let services = test_services(dir.path());
        let chat = services
            .store
            .create(super::super::store::NewChat { cwd: dir.path().to_string_lossy().into_owned(), ..Default::default() })
            .unwrap();
        let session = Session::open(services, &chat.id).await.unwrap();
        let (tx, mut rx) = oneshot::channel();
        {
            let mut inner = session.inner.lock().unwrap();
            inner.status = "running";
            inner.approvals.push(Approval {
                approval: PendingApproval {
                    tool_call_id: "call-1".into(),
                    tool_name: "bash".into(),
                    reason: "command".into(),
                    rule: Some("git push".into()),
                    path: None,
                    workdir: None,
                    agent: None,
                },
                settle: tx,
            });
        }
        let queued = session.send_swarm("[Swarm board] update from @w1").await.unwrap();
        assert!(queued);
        // The pending approval must remain intact (not superseded!) and the message must sit in `pending`.
        assert!(rx.try_recv().is_err());
        let inner = session.inner.lock().unwrap();
        assert_eq!(inner.approvals.len(), 1);
        assert_eq!(inner.pending.len(), 1);
        assert!(inner.steering.is_empty());
        assert_eq!(inner.queue[0].mode, "queue");
    }

    #[tokio::test]
    async fn persists_orchestrate_phase_and_computes_breakdown_and_trust() {
        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path().join("repo");
        std::fs::create_dir_all(workdir.join(".rainy/skills/demo")).unwrap();
        std::fs::write(workdir.join(".rainy/skills/demo/SKILL.md"), "---\nname: Demo\ndescription: demo skill\n---\nbody").unwrap();
        std::fs::write(workdir.join(".rainy/hooks.json"), r#"[{"event":"PreToolUse","command":"echo hi"}]"#).unwrap();

        let services = test_services(dir.path());
        let chat = services
            .store
            .create(super::super::store::NewChat { cwd: workdir.to_string_lossy().into_owned(), ..Default::default() })
            .unwrap();
        let session = Session::open(services.clone(), &chat.id).await.unwrap();

        // Project skills load in any workspace, like AGENTS.md; hooks, which run commands, wait for trust.
        assert!(session.system_prompt(&session.tools()).contains("project/demo"));
        assert!(session.effective_hooks().is_empty());
        services.settings.set_workspace_trusted(&workdir.to_string_lossy(), true).unwrap();
        assert!(session.system_prompt(&session.tools()).contains("project/demo"));
        assert_eq!(session.effective_hooks().len(), 1);

        // Orchestrate phase persistence across Session::open.
        let exec = ModelSelection { provider: "mock-fast".into(), model: "mock-lite".into(), thinking: None };
        session.set_orchestrate(Some(&exec)).await.unwrap();
        session.switch_phase(Phase::Execute, "step 1".into());
        tokio::time::sleep(Duration::from_millis(50)).await;
        let reopened = Session::open(services.clone(), &chat.id).await.unwrap();
        assert_eq!(reopened.inner.lock().unwrap().phase, Phase::Execute);

        // Context breakdown returns populated segments and budget fields.
        let breakdown = reopened.context_breakdown().unwrap();
        assert_eq!(breakdown.session_id, chat.id);
        assert!(breakdown.context_window >= 64_000);
        assert!(breakdown.segments.system > 0);
        assert!(breakdown.segments.core_tools > 0);
        assert_eq!(breakdown.total, breakdown.segments.system + breakdown.segments.instructions + breakdown.segments.mcp_tools + breakdown.segments.core_tools + breakdown.segments.history + breakdown.segments.recall);

        // Model hint resolution supports provider/model, provider, and model id.
        assert_eq!(services.models.resolve_by_hint("mock-lite").unwrap().provider, "mock-fast");
        assert_eq!(services.models.resolve_by_hint("mock-fast/mock-lite").unwrap().model, "mock-lite");
        assert!(services.models.resolve_by_hint("nonexistent").is_err());
    }

    #[tokio::test]
    async fn concurrent_sends_reserve_single_run_and_queue_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let services = test_services(dir.path());
        let chat = services
            .store
            .create(super::super::store::NewChat { cwd: dir.path().to_string_lossy().into_owned(), ..Default::default() })
            .unwrap();
        let session = Session::open(services, &chat.id).await.unwrap();
        // Simulate a run currently claiming startup (`starting = true`): concurrent send must queue cleanly with mode "queue".
        session.inner.lock().unwrap().starting = true;
        let queued = session.send("follow up while starting", Vec::new(), true).await.unwrap();
        assert!(queued);
        {
            let inner = session.inner.lock().unwrap();
            assert_eq!(inner.queue.len(), 1);
            // Since `inner.run` is still None during startup, mode must accurately report "queue" rather than "steer".
            assert_eq!(inner.queue[0].mode, "queue");
            assert_eq!(inner.pending.len(), 1);
            assert!(inner.steering.is_empty());
        }
        session.inner.lock().unwrap().starting = false;
    }

    #[tokio::test]
    async fn finish_run_hands_off_message_queued_after_abort() {
        let dir = tempfile::tempdir().unwrap();
        let services = test_services(dir.path());
        let chat = services
            .store
            .create(super::super::store::NewChat { cwd: dir.path().to_string_lossy().into_owned(), ..Default::default() })
            .unwrap();
        let session = Session::open(services, &chat.id).await.unwrap();
        let resolved = session.resolved().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        {
            let mut inner = session.inner.lock().unwrap();
            inner.status = "running";
            inner.run = Some(RunInfo {
                started: now_ms(),
                from: 0,
                requests: 0,
                usage: UsageSummary::default(),
                aborted: true,
                overflow_retried: false,
                retries: 0,
                cancel,
            });
            // A fresh message that arrived after `abort()` cleared old queues.
            let qid = "post-abort-1".to_owned();
            inner.queue.push(QueuedMessage { id: qid.clone(), text: "new request".into(), mode: "queue".into(), image_count: 0 });
            inner.pending.insert(
                qid.clone(),
                vec![(Message::user("new request"), Meta::User { entry_id: "e-1".into(), text: "new request".into(), images: Vec::new(), queue_id: Some(qid) })],
            );
        }
        let next = session.finish_run(&resolved).await;
        assert!(next.is_some(), "message queued after abort must not be swallowed");
        let inner = session.inner.lock().unwrap();
        assert!(inner.starting, "finish_run should hand off under starting reservation");
        assert!(inner.queue.is_empty());
        assert!(inner.pending.is_empty());
    }

    #[tokio::test]
    async fn wait_to_retry_returns_false_immediately_when_aborted() {
        let dir = tempfile::tempdir().unwrap();
        let services = test_services(dir.path());
        let chat = services
            .store
            .create(super::super::store::NewChat { cwd: dir.path().to_string_lossy().into_owned(), ..Default::default() })
            .unwrap();
        let session = Session::open(services, &chat.id).await.unwrap();
        {
            let mut inner = session.inner.lock().unwrap();
            inner.status = "running";
            inner.run = Some(RunInfo {
                started: now_ms(),
                from: 0,
                requests: 1,
                usage: UsageSummary::default(),
                aborted: false,
                overflow_retried: false,
                retries: 1,
                cancel: CancellationToken::new(),
            });
        }
        let s = session.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            s.abort();
        });
        let start = std::time::Instant::now();
        let go = session.wait_to_retry(RetryWait { attempt: 1, max: 3, at: (now_ms() + 30_000) as f64, reason: "429".into() }).await;
        assert!(!go);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn post_edit_format_refreshes_snapshot_so_revert_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("demo.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let services = test_services(dir.path());
        let chat = services
            .store
            .create(super::super::store::NewChat { cwd: dir.path().to_string_lossy().into_owned(), ..Default::default() })
            .unwrap();
        let session = Session::open(services.clone(), &chat.id).await.unwrap();
        let details = json!({ "path": "demo.rs", "operation": "edit", "undoable": true });
        services.snapshots.record(&chat.id, "call-fmt", &file, Some(b"fn main() {}\n"), Some(b"fn main(){let x=1;}\n")).unwrap();
        session
            .append(TranscriptEntry::ToolResult {
                id: "tr-1".into(),
                ts: now_ms() as f64,
                tool_call_id: "call-fmt".into(),
                tool_name: "edit".into(),
                content: vec![rainy_protocol::chat::ResultContent::Text { text: "ok".into() }],
                is_error: false,
                details: Some(details.clone()),
                duration_ms: None,
            })
            .await;
        // PostEdit formatter reformats the file on disk.
        std::fs::write(&file, "fn main() {\n    let x = 1;\n}\n").unwrap();
        session.refresh_formatted_snapshots("edit", "call-fmt", Some(&details), dir.path(), std::slice::from_ref(&file));
        let reverted = session.revert(&["call-fmt".to_owned()]).await.unwrap();
        assert_eq!(reverted.reverted.len(), 1);
        assert!(reverted.failed.is_empty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn main() {}\n");
    }

    #[tokio::test]
    async fn tool_context_propagates_workspace_deny_paths() {
        let dir = tempfile::tempdir().unwrap();
        let services = test_services(dir.path());
        services.settings.set_permission_rule_kind("ws-1", "denyPath", "*.secret", true).unwrap();
        let chat = services
            .store
            .create(super::super::store::NewChat {
                workspace_id: Some("ws-1".into()),
                cwd: dir.path().to_string_lossy().into_owned(),
                ..Default::default()
            })
            .unwrap();
        let session = Session::open(services, &chat.id).await.unwrap();
        let ctx = session.tool_context();
        assert!(ctx.is_path_denied(&dir.path().join("keys.secret")));
        assert!(!ctx.is_path_denied(&dir.path().join("readme.md")));
    }

    #[tokio::test]
    async fn abort_cancels_manual_compact_and_sync_chat_resets_cleared_orchestration() {
        let dir = tempfile::tempdir().unwrap();
        let services = test_services(dir.path());
        let chat = services
            .store
            .create(super::super::store::NewChat { cwd: dir.path().to_string_lossy().into_owned(), ..Default::default() })
            .unwrap();
        let session = Session::open(services.clone(), &chat.id).await.unwrap();
        let compact_cancel = CancellationToken::new();
        {
            let mut inner = session.inner.lock().unwrap();
            inner.status = "compacting";
            inner.compact_cancel = Some(compact_cancel.clone());
        }
        assert!(!session.is_evictable());
        session.abort();
        assert!(compact_cancel.is_cancelled());
        {
            let mut inner = session.inner.lock().unwrap();
            inner.status = "idle";
            inner.compact_cancel = None;
        }
        assert!(session.is_evictable());

        // `sync_chat` keeps in-memory metadata fresh and resets orchestration phase when cleared.
        let exec = ModelSelection { provider: "mock-fast".into(), model: "mock-lite".into(), thinking: None };
        session.set_orchestrate(Some(&exec)).await.unwrap();
        session.switch_phase(Phase::Execute, "step".into());
        let mut updated = session.chat();
        updated.title = "Renamed externally".into();
        updated.orchestrate = None;
        session.sync_chat(updated);
        assert_eq!(session.chat().title, "Renamed externally");
        assert_eq!(session.inner.lock().unwrap().phase, Phase::Plan);
    }
}
