//! Swarm hub: message board storage, ordered coordinator inbox persistence, atomic worker reservation and lifecycle.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use rainy_protocol::chat::UsageSummary;
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::{BudgetGuard, COORDINATOR, Chats, MAX_WORKERS, Member, Post, REPORT_CHARS, STARTING_GRACE, digest, excerpt, invalid, recipients, valid_name};
use crate::agent::store::{ChatStore, StoredChat};
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::rpc::RpcHub;
use crate::settings::ModelSelection;
use crate::text::now_ms;

#[derive(Default)]
struct Inbox {
    /// Posts to the coordinator it has not been given yet.
    unseen: Vec<Post>,
    /// Whether disk state for this inbox has already been loaded.
    loaded: bool,
    /// A `swarm_wait` of the coordinator is in progress.
    waiting: bool,
    notify: Arc<Notify>,
}

/// RAII guard releasing a reserved worker name when `add_worker` finishes or fails.
struct WorkerReservation<'a> {
    map: &'a Mutex<HashMap<String, HashSet<String>>>,
    coordinator: String,
    name: String,
}

impl Drop for WorkerReservation<'_> {
    fn drop(&mut self) {
        let mut reserving = self.map.lock().unwrap();
        if let Some(names) = reserving.get_mut(&self.coordinator) {
            names.remove(&self.name);
            if names.is_empty() {
                reserving.remove(&self.coordinator);
            }
        }
    }
}

/// The swarms of the Host: boards on disk and the coordinators' inboxes.
pub struct Hub {
    dir: PathBuf,
    store: Arc<ChatStore>,
    rpc: Arc<RpcHub>,
    chats: OnceLock<Weak<dyn Chats>>,
    inboxes: Mutex<HashMap<String, Inbox>>,
    guards: Mutex<HashMap<String, BudgetGuard>>,
    /// Workers currently being created asynchronously; prevents concurrent `add_worker` calls from duplicating names or exceeding `MAX_WORKERS`.
    reserving: Mutex<HashMap<String, HashSet<String>>>,
    /// Workers recently spawned or delivered a message; prevents `swarm_wait` from exiting during async startup.
    starting: Mutex<HashMap<String, Instant>>,
    writing: Mutex<()>,
}

impl Hub {
    pub fn new(dir: PathBuf, store: Arc<ChatStore>, rpc: Arc<RpcHub>) -> Arc<Self> {
        Arc::new(Self {
            dir,
            store,
            rpc,
            chats: OnceLock::new(),
            inboxes: Mutex::default(),
            guards: Mutex::default(),
            reserving: Mutex::default(),
            starting: Mutex::default(),
            writing: Mutex::default(),
        })
    }

    /// Called once the agent that owns the chats exists.
    pub fn set_chats(&self, chats: Weak<dyn Chats>) {
        let _ = self.chats.set(chats);
    }

    fn chats(&self) -> HostResult<Arc<dyn Chats>> {
        self.chats.get().and_then(Weak::upgrade).ok_or_else(|| invalid("Swarm 尚未就绪。", "Swarms are not ready."))
    }

    fn path(&self, coordinator: &str) -> PathBuf {
        self.dir.join(format!("{coordinator}.jsonl"))
    }

    fn inbox_path(&self, coordinator: &str) -> PathBuf {
        self.dir.join(format!("{coordinator}.inbox.json"))
    }

    /// Load persisted unseen inbox posts for `coordinator` on first access after Host restart.
    fn ensure_inbox_loaded(&self, coordinator: &str, inbox: &mut Inbox) {
        if inbox.loaded {
            return;
        }
        inbox.loaded = true;
        if let Ok(Some(posts)) = crate::store::read_json::<Vec<Post>>(&self.inbox_path(coordinator)) {
            if inbox.unseen.is_empty() {
                inbox.unseen = posts;
            } else {
                let mut merged = posts;
                merged.extend(std::mem::take(&mut inbox.unseen));
                inbox.unseen = merged;
            }
        }
    }

    /// Persist unseen inbox posts to disk while the caller holds `self.inboxes` (lock order: `inboxes` -> `writing`),
    /// preventing concurrent snapshots from overwriting newer disk state out of order.
    fn persist_inbox_locked(&self, coordinator: &str, unseen: &[Post]) {
        let _writing = self.writing.lock().unwrap();
        let path = self.inbox_path(coordinator);
        if unseen.is_empty() {
            let _ = std::fs::remove_file(path);
        } else if let Err(error) = crate::store::write_json(&path, unseen) {
            log::warn!("[swarm {coordinator}] saving inbox failed: {error}");
        }
    }

    fn mark_starting(&self, worker_id: &str) {
        self.starting.lock().unwrap().insert(worker_id.to_owned(), Instant::now());
    }

    fn clear_starting(&self, worker_id: &str) {
        self.starting.lock().unwrap().remove(worker_id);
    }

    fn is_starting(&self, worker_id: &str) -> bool {
        let mut starting = self.starting.lock().unwrap();
        match starting.get(worker_id) {
            Some(at) if at.elapsed() < STARTING_GRACE => true,
            Some(_) => {
                starting.remove(worker_id);
                false
            }
            None => false,
        }
    }

    /// Reset the circuit breaker when the user explicitly sends a new turn to the coordinator.
    pub fn reset_budget(&self, coordinator: &str) {
        self.guards.lock().unwrap().remove(coordinator);
    }

    /// Current circuit-breaker reason if tripped.
    fn tripped_reason(&self, coordinator: &str) -> Option<String> {
        self.guards.lock().unwrap().get(coordinator).and_then(|g| g.tripped.clone())
    }

    /// Remove board and inbox files plus in-memory state when a coordinator chat is deleted.
    pub fn cleanup_coordinator(&self, coordinator: &str) {
        self.stop(coordinator);
        self.inboxes.lock().unwrap().remove(coordinator);
        self.guards.lock().unwrap().remove(coordinator);
        self.reserving.lock().unwrap().remove(coordinator);
        let _writing = self.writing.lock().unwrap();
        let _ = std::fs::remove_file(self.path(coordinator));
        let _ = std::fs::remove_file(self.inbox_path(coordinator));
    }

    /// The board's posts, oldest first.
    pub fn posts(&self, coordinator: &str) -> Vec<Post> {
        let Ok(text) = std::fs::read_to_string(self.path(coordinator)) else { return Vec::new() };
        text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
    }

    fn worker_chats(&self, coordinator: &str) -> Vec<StoredChat> {
        let mut chats: Vec<StoredChat> =
            self.store.list().into_iter().filter(|chat| chat.id != coordinator && chat.swarm.as_ref().is_some_and(|link| link.coordinator == coordinator)).collect();
        chats.sort_by(|left, right| left.created_at.total_cmp(&right.created_at));
        chats
    }

    /// The swarm's workers, earliest first, retired ones included.
    pub fn members(&self, coordinator: &str) -> Vec<Member> {
        let chats = self.chats().ok();
        self.worker_chats(coordinator)
            .into_iter()
            .map(|chat| {
                let state = chats.as_ref().map(|chats| chats.state(&chat.id));
                Member {
                    name: chat.swarm.as_ref().map(|link| link.name.clone()).unwrap_or_default(),
                    title: chat.title.clone(),
                    status: if chat.archived { "retired".into() } else { state.as_ref().map_or_else(|| "idle".into(), |state| state.status.clone()) },
                    awaiting_approval: state.as_ref().is_some_and(|state| state.awaiting_approval),
                    model: chat.model.as_ref().map(|model| model.model.clone()),
                    worktree: chat.worktree.is_some(),
                    retired: chat.archived,
                    id: chat.id,
                }
            })
            .collect()
    }

    fn active_member(&self, coordinator: &str, name: &str) -> Option<StoredChat> {
        self.worker_chats(coordinator).into_iter().find(|chat| !chat.archived && chat.swarm.as_ref().is_some_and(|link| link.name == name))
    }

    /// The members as the coordinator's tools report them.
    pub fn roster(&self, coordinator: &str) -> String {
        let members = self.members(coordinator);
        if members.is_empty() {
            return "No workers yet.".into();
        }
        members
            .iter()
            .map(|member| {
                let status = if member.awaiting_approval { "waiting for the user's approval".to_owned() } else { member.status.clone() };
                format!("@{} — {status}{}{}", member.name, member.model.as_ref().map(|model| format!(", model {model}")).unwrap_or_default(), if member.worktree { ", own worktree" } else { "" })
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Add a worker: atomically reserves its name and capacity slot before creating the chat and sending its brief.
    pub async fn add_worker(&self, coordinator: &str, name: &str, brief: &str, model: Option<ModelSelection>, isolated: bool) -> HostResult<String> {
        if let Some(reason) = self.tripped_reason(coordinator) {
            return Err(HostError::new("swarm-budget", reason));
        }
        if !valid_name(name) {
            return Err(invalid("名字只能用小写字母、数字和连字符，最多 24 个字符。", "A name uses lower-case letters, digits and hyphens, at most 24."));
        }
        let _reservation = {
            let mut reserving = self.reserving.lock().unwrap();
            let pending = reserving.entry(coordinator.to_owned()).or_default();
            let active = self.worker_chats(coordinator).into_iter().filter(|chat| !chat.archived).collect::<Vec<_>>();
            if pending.contains(name) || active.iter().any(|chat| chat.swarm.as_ref().is_some_and(|link| link.name == name)) {
                return Err(invalid(&format!("已经有一个 @{name}。"), &format!("There is already an @{name}.")));
            }
            if active.len() + pending.len() >= MAX_WORKERS {
                return Err(invalid(&format!("一个 Swarm 最多同时有 {MAX_WORKERS} 个工作者，先移除空闲的。"), &format!("A swarm has at most {MAX_WORKERS} workers at a time; remove idle ones first.")));
            }
            pending.insert(name.to_owned());
            WorkerReservation { map: &self.reserving, coordinator: coordinator.to_owned(), name: name.to_owned() }
        };
        // The worker's role is in its system prompt; the message the user sees in its chat is the brief.
        let first = format!("{}\n\n{}", l(&format!("[Swarm] 你是 @{name}，来自 @{COORDINATOR} 的任务："), &format!("[Swarm] You are @{name}. Your brief from @{COORDINATOR}:")), brief.trim());
        let id = self.chats()?.create_worker(coordinator.to_owned(), name.to_owned(), first, model, isolated).await?;
        self.mark_starting(&id);
        self.record(coordinator, COORDINATOR, vec![name.to_owned()], brief.trim().to_owned(), false)?;
        Ok(id)
    }

    /// Resolve an optional model override string for `swarm_add`.
    pub fn resolve_model(&self, hint: &str) -> HostResult<ModelSelection> {
        self.chats()?.resolve_model(hint)
    }

    /// Append a post to the board and tell the page; no delivery.
    fn record(&self, coordinator: &str, from: &str, to: Vec<String>, text: String, auto: bool) -> HostResult<Post> {
        let post = Post { id: uuid::Uuid::new_v4().to_string(), ts: now_ms() as f64, from: from.to_owned(), to, text, auto };
        {
            let _writing = self.writing.lock().unwrap();
            std::fs::create_dir_all(&self.dir)?;
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(self.path(coordinator))?;
            writeln!(file, "{}", serde_json::to_string(&post).unwrap_or_default())?;
        }
        self.rpc.emit("swarm.post", json!({ "coordinatorId": coordinator, "post": post }));
        Ok(post)
    }

    /// Post to the board and deliver the post to each recipient but the sender.
    pub async fn post(&self, coordinator: &str, from: &str, to: &[String], text: &str, auto: bool) -> HostResult<Post> {
        if text.trim().is_empty() {
            return Err(invalid("消息不能为空。", "A post cannot be empty."));
        }
        let mut names = recipients(to);
        if names.is_empty() {
            return Err(invalid("请用 @名字 指明收件人。", "Name the recipients with @name."));
        }
        let workers = self.worker_chats(coordinator);
        let active_by_name: HashMap<&str, &str> = workers
            .iter()
            .filter(|chat| !chat.archived)
            .filter_map(|chat| Some((chat.swarm.as_ref()?.name.as_str(), chat.id.as_str())))
            .collect();
        if names.iter().any(|name| name == "all") {
            names = std::iter::once(COORDINATOR.to_owned())
                .chain(workers.iter().filter(|chat| !chat.archived).filter_map(|chat| chat.swarm.as_ref().map(|link| link.name.clone())))
                .filter(|name| name != from)
                .collect();
        }
        let mut targets = Vec::new();
        for name in &names {
            if name == COORDINATOR {
                targets.push(None);
            } else {
                let id = active_by_name.get(name.as_str()).ok_or_else(|| invalid(&format!("没有叫 @{name} 的工作者。"), &format!("There is no worker named @{name}.")))?;
                targets.push(Some((*id).to_owned()));
            }
        }
        let post = self.record(coordinator, from, names, text.trim().to_owned(), auto)?;
        let chats = self.chats()?;
        let tripped = self.tripped_reason(coordinator);
        for target in targets {
            match target {
                None if from != COORDINATOR => self.to_coordinator(coordinator, post.clone()).await,
                None => {}
                Some(id) => {
                    if tripped.is_some() {
                        continue;
                    }
                    self.mark_starting(&id);
                    let message = format!("[Swarm board · from @{from}]\n{}", post.text);
                    if let Err(error) = chats.deliver(id.clone(), message).await {
                        self.clear_starting(&id);
                        log::warn!("[swarm {coordinator}] delivering to a worker failed: {}", error.message);
                    }
                }
            }
        }
        Ok(post)
    }

    /// A post for the coordinator: a waiting `swarm_wait` takes it; an idle coordinator starts a turn with it; a busy
    /// one gets it when its turn ends. Persisted while holding `inboxes` so disk state never reorders.
    pub(super) async fn to_coordinator(&self, coordinator: &str, post: Post) {
        let deliver = {
            let mut inboxes = self.inboxes.lock().unwrap();
            let inbox = inboxes.entry(coordinator.to_owned()).or_default();
            self.ensure_inbox_loaded(coordinator, inbox);
            inbox.unseen.push(post);
            self.persist_inbox_locked(coordinator, &inbox.unseen);
            if inbox.waiting {
                inbox.notify.notify_one();
                false
            } else {
                true
            }
        };
        if deliver {
            self.hand_over(coordinator).await;
        }
    }

    /// Give an idle coordinator the posts it has not seen, as one message.
    async fn hand_over(&self, coordinator: &str) {
        let Ok(chats) = self.chats() else { return };
        let state = chats.state(coordinator);
        if (state.status != "idle" && state.status != "error") || state.awaiting_approval {
            return;
        }
        let posts = {
            let mut inboxes = self.inboxes.lock().unwrap();
            let inbox = inboxes.entry(coordinator.to_owned()).or_default();
            self.ensure_inbox_loaded(coordinator, inbox);
            if inbox.waiting || inbox.unseen.is_empty() {
                return;
            }
            let posts = std::mem::take(&mut inbox.unseen);
            self.persist_inbox_locked(coordinator, &[]);
            posts
        };
        let text = format!("[Swarm board]\n{}\n\nMembers:\n{}", digest(&posts), self.roster(coordinator));
        if let Err(error) = chats.deliver(coordinator.to_owned(), text).await {
            // Delivery failed: restore unseen posts in order and persist under lock.
            {
                let mut inboxes = self.inboxes.lock().unwrap();
                let inbox = inboxes.entry(coordinator.to_owned()).or_default();
                let mut all = posts;
                all.extend(std::mem::take(&mut inbox.unseen));
                inbox.unseen = all;
                self.persist_inbox_locked(coordinator, &inbox.unseen);
            }
            log::warn!("[swarm {coordinator}] waking the coordinator failed: {}", error.message);
        }
    }

    /// A turn of a swarm chat ended: a worker's reply goes to the coordinator; a coordinator gets what came meanwhile.
    pub async fn turn_ended(&self, chat: &StoredChat, aborted: bool, usage: Option<&UsageSummary>) {
        let Some(link) = &chat.swarm else { return };
        self.clear_starting(&chat.id);
        if link.coordinator == chat.id {
            self.hand_over(&chat.id).await;
            return;
        }
        if aborted || chat.archived {
            return;
        }
        let trip = {
            let mut guards = self.guards.lock().unwrap();
            let guard = guards.entry(link.coordinator.clone()).or_default();
            if guard.tripped.is_none() { guard.record_turn(usage) } else { None }
        };
        let reply = self.chats().ok().and_then(|chats| chats.last_reply(&chat.id)).unwrap_or_default();
        let text = if reply.trim().is_empty() { l("（这一轮结束了，没有回复）", "(finished its turn without a reply)") } else { excerpt(reply.trim(), REPORT_CHARS) };
        if let Err(error) = self.post(&link.coordinator, &link.name, &[COORDINATOR.to_owned()], &text, true).await {
            log::warn!("[swarm {}] reporting @{} failed: {}", link.coordinator, link.name, error.message);
        }
        if let Some(reason) = trip {
            log::warn!("[swarm {}] {reason}", link.coordinator);
            self.stop_workers(&link.coordinator);
            if let Ok(post) = self.record(&link.coordinator, COORDINATOR, vec![COORDINATOR.to_owned()], reason, true) {
                self.to_coordinator(&link.coordinator, post).await;
            }
        }
    }

    /// Wait until posts reach the coordinator, every worker is idle (and none are starting/queued), the user writes or `timeout` passes.
    pub async fn wait(&self, coordinator: &str, timeout: Duration, cancel: &CancellationToken) -> String {
        let notify = {
            let mut inboxes = self.inboxes.lock().unwrap();
            let inbox = inboxes.entry(coordinator.to_owned()).or_default();
            self.ensure_inbox_loaded(coordinator, inbox);
            inbox.waiting = true;
            inbox.notify.clone()
        };
        let deadline = tokio::time::Instant::now() + timeout;
        let reason = loop {
            if cancel.is_cancelled() {
                break "stopped";
            }
            if !self.inboxes.lock().unwrap().get(coordinator).is_none_or(|inbox| inbox.unseen.is_empty()) {
                break "new posts";
            }
            let chats = self.chats().ok();
            let workers = self.worker_chats(coordinator);
            let any_active = workers.iter().filter(|chat| !chat.archived).any(|chat| {
                if self.is_starting(&chat.id) {
                    return true;
                }
                chats.as_ref().is_some_and(|c| {
                    let s = c.state(&chat.id);
                    s.status == "running" || s.status == "compacting" || s.awaiting_approval || s.has_pending
                })
            });
            if !any_active {
                break "no worker is running";
            }
            if chats.as_ref().is_some_and(|chats| chats.state(coordinator).user_waiting) {
                break "the user wrote to you";
            }
            if tokio::time::Instant::now() >= deadline {
                break "the wait timed out";
            }
            tokio::select! {
                () = notify.notified() => {}
                () = tokio::time::sleep(Duration::from_millis(250)) => {}
                () = cancel.cancelled() => break "stopped",
            }
        };
        // Only drain and delete persisted unseen posts when `wait` was NOT cancelled; on cancellation the caller
        // discards this string as `Err("tool call aborted")`, so keeping `unseen` intact prevents losing messages.
        let posts = {
            let mut inboxes = self.inboxes.lock().unwrap();
            let inbox = inboxes.entry(coordinator.to_owned()).or_default();
            inbox.waiting = false;
            if reason == "stopped" || cancel.is_cancelled() {
                Vec::new()
            } else {
                let posts = std::mem::take(&mut inbox.unseen);
                self.persist_inbox_locked(coordinator, &[]);
                posts
            }
        };
        let board = if posts.is_empty() { "No new posts.".to_owned() } else { digest(&posts) };
        format!("Stopped waiting: {reason}.\n\n{board}\n\nMembers:\n{}", self.roster(coordinator))
    }

    /// Move a worker's worktree changes into the project's checkout.
    pub async fn land(&self, coordinator: &str, name: &str) -> HostResult<usize> {
        let chat = self.active_member(coordinator, name).ok_or_else(|| invalid(&format!("没有叫 @{name} 的工作者。"), &format!("There is no worker named @{name}.")))?;
        if chat.worktree.is_none() {
            return Err(invalid(&format!("@{name} 直接在项目里工作，改动已经在项目中。"), &format!("@{name} works in the project's checkout; its changes are already there.")));
        }
        let chats = self.chats()?;
        let state = chats.state(&chat.id);
        if state.status == "running" || state.status == "compacting" || state.awaiting_approval || state.has_pending || self.is_starting(&chat.id) {
            return Err(invalid(&format!("@{name} 还在工作，等它这轮结束再合入。"), &format!("@{name} is still working; land its changes once its turn ends.")));
        }
        chats.land(chat.id).await
    }

    /// Retire a worker; one with a worktree must land first unless `discard`.
    pub async fn remove(&self, coordinator: &str, name: &str, discard: bool) -> HostResult<()> {
        let chat = self.active_member(coordinator, name).ok_or_else(|| invalid(&format!("没有叫 @{name} 的工作者。"), &format!("There is no worker named @{name}.")))?;
        if chat.worktree.is_some() && !discard {
            return Err(invalid(
                &format!("@{name} 的独立工作区里可能还有改动：先用 swarm_land 合入，或者传 discard: true 丢弃。"),
                &format!("@{name}'s worktree may hold changes: land them with swarm_land first, or pass discard: true to drop them."),
            ));
        }
        self.chats()?.retire(chat.id, discard).await
    }

    /// Stop every running chat of the swarm (coordinator and workers).
    pub fn stop(&self, coordinator: &str) {
        let Ok(chats) = self.chats() else { return };
        chats.abort(coordinator);
        self.stop_workers(coordinator);
    }

    /// Stop only the workers of a swarm (used when a budget guard trips so the coordinator can still report to the user).
    pub fn stop_workers(&self, coordinator: &str) {
        let Ok(chats) = self.chats() else { return };
        for chat in self.worker_chats(coordinator).into_iter().filter(|chat| !chat.archived) {
            self.clear_starting(&chat.id);
            chats.abort(&chat.id);
        }
    }

    /// Active (non-archived) worker chat ids of a coordinator, for cascade cleanup when deleting the coordinator.
    pub fn worker_ids(&self, coordinator: &str) -> Vec<String> {
        self.worker_chats(coordinator).into_iter().map(|chat| chat.id).collect()
    }

    /// The coordinator a chat belongs to.
    pub fn coordinator_of(&self, chat_id: &str) -> Option<String> {
        self.store.get(chat_id).and_then(|chat| chat.swarm).map(|link| link.coordinator)
    }
}
