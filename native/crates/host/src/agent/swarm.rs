//! Swarms (`/swarm <task>`): the chat becomes a coordinator that splits a large task among worker chats and does no
//! technical work itself. Workers are ordinary chats of the project, each named (`@parser`), started from a brief and
//! optionally in a Git worktree of its own so several can edit at once. Everyone talks over one board: a post names
//! its recipients (`@name`, `@coordinator`, `@all`) and reaches each of them as a message. A worker's turn that ends
//! is posted to the coordinator on its own, so the coordinator learns of results without polling. The coordinator
//! waits with `swarm_wait`; posts that reach it while it is idle start a turn of its own, and ones that come while it
//! works are handed over when its turn ends.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use futures::future::BoxFuture;
use rainy_protocol::chat::SwarmLink;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::llm::ToolSpec;
use super::store::{ChatStore, StoredChat};
use super::tools::{Tool, ToolOutput, Updates, params, spec};
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::rpc::RpcHub;
use crate::settings::ModelSelection;
use crate::text::now_ms;

/// The coordinator's own name on the board.
pub const COORDINATOR: &str = "coordinator";
/// Workers one swarm may have at a time; parallel workers spend quota fast.
pub const MAX_WORKERS: usize = 8;
/// Longest a `swarm_wait` waits, and how long it waits when the model gives no time.
const MAX_WAIT: Duration = Duration::from_secs(30 * 60);
const DEFAULT_WAIT: Duration = Duration::from_secs(10 * 60);
/// Characters of a worker's reply posted when its turn ends.
const REPORT_CHARS: usize = 4000;

/// One message on a swarm's board.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Post {
    pub id: String,
    pub ts: f64,
    pub from: String,
    pub to: Vec<String>,
    pub text: String,
    /// Posted by Rainy for the member (the end of a worker's turn), not by its model.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
}

/// A member as the board and the swarm pane show it.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub id: String,
    pub name: String,
    pub title: String,
    pub status: String,
    pub awaiting_approval: bool,
    pub model: Option<String>,
    pub worktree: bool,
    /// Removed from the swarm (archived); it can no longer be messaged.
    pub retired: bool,
}

/// What a chat reports of itself to the swarm.
pub struct ChatState {
    pub status: String,
    pub awaiting_approval: bool,
    /// The user wrote to it while it runs (a queued or steering message).
    pub user_waiting: bool,
}

/// The chats a swarm acts on, implemented by the agent that owns them.
pub trait Chats: Send + Sync {
    /// Create a worker chat of the coordinator's project and send it its first message.
    fn create_worker(&self, coordinator: String, name: String, first: String, model: Option<ModelSelection>, isolated: bool) -> BoxFuture<'static, HostResult<String>>;
    /// Send a message to a chat: it starts a turn when idle, else it reaches the running turn at its next step.
    fn deliver(&self, id: String, text: String) -> BoxFuture<'static, HostResult<()>>;
    /// Move a worker's worktree changes into the project's checkout; the number of files moved.
    fn land(&self, id: String) -> BoxFuture<'static, HostResult<usize>>;
    /// Stop a worker and archive it; `discard` drops its worktree with any changes not landed.
    fn retire(&self, id: String, discard: bool) -> BoxFuture<'static, HostResult<()>>;
    fn abort(&self, id: &str);
    fn state(&self, id: &str) -> ChatState;
    /// The text of the chat's latest reply.
    fn last_reply(&self, id: &str) -> Option<String>;
}

#[derive(Default)]
struct Inbox {
    /// Posts to the coordinator it has not been given yet.
    unseen: Vec<Post>,
    /// A `swarm_wait` of the coordinator is in progress.
    waiting: bool,
    notify: Arc<Notify>,
}

/// The swarms of the Host: boards on disk and the coordinators' inboxes.
pub struct Hub {
    dir: PathBuf,
    store: Arc<ChatStore>,
    rpc: Arc<RpcHub>,
    chats: OnceLock<Weak<dyn Chats>>,
    inboxes: Mutex<HashMap<String, Inbox>>,
    writing: Mutex<()>,
}

fn invalid(zh: &str, en: &str) -> HostError {
    HostError::new("swarm", l(zh, en))
}

/// A worker name: lower-case letters, digits and hyphens, at most 24, not `coordinator` or `all`.
pub fn valid_name(name: &str) -> bool {
    (1..=24).contains(&name.len())
        && name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && !matches!(name, COORDINATOR | "all" | "user")
}

/// Recipients as written by a model: `@name`, `name`, `@all`, with spaces and commas.
fn recipients(to: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    to.iter()
        .flat_map(|item| item.split([',', ' ']))
        .map(|item| item.trim().trim_start_matches('@').to_ascii_lowercase())
        .filter(|item| !item.is_empty() && seen.insert(item.clone()))
        .collect()
}

fn excerpt(text: &str, chars: usize) -> String {
    let count = text.chars().count();
    if count <= chars {
        return text.to_owned();
    }
    format!("{}…", text.chars().take(chars).collect::<String>())
}

/// Posts as one message, oldest first.
pub fn digest(posts: &[Post]) -> String {
    posts
        .iter()
        .map(|post| {
            let to = post.to.iter().map(|to| format!("@{to}")).collect::<Vec<_>>().join(" ");
            // A worker's own report, posted when its turn ended.
            let kind = if post.auto { " (turn report)" } else { "" };
            format!("@{} → {to}{kind}:\n{}", post.from, post.text.trim())
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

impl Hub {
    pub fn new(dir: PathBuf, store: Arc<ChatStore>, rpc: Arc<RpcHub>) -> Arc<Self> {
        Arc::new(Self { dir, store, rpc, chats: OnceLock::new(), inboxes: Mutex::default(), writing: Mutex::default() })
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

    /// Add a worker: its chat, its brief on the board and its first message.
    pub async fn add_worker(&self, coordinator: &str, name: &str, brief: &str, model: Option<ModelSelection>, isolated: bool) -> HostResult<String> {
        if !valid_name(name) {
            return Err(invalid("名字只能用小写字母、数字和连字符，最多 24 个字符。", "A name uses lower-case letters, digits and hyphens, at most 24."));
        }
        if self.active_member(coordinator, name).is_some() {
            return Err(invalid(&format!("已经有一个 @{name}。"), &format!("There is already an @{name}.")));
        }
        if self.worker_chats(coordinator).iter().filter(|chat| !chat.archived).count() >= MAX_WORKERS {
            return Err(invalid(&format!("一个 Swarm 最多同时有 {MAX_WORKERS} 个工作者，先移除空闲的。"), &format!("A swarm has at most {MAX_WORKERS} workers at a time; remove idle ones first.")));
        }
        // The worker's role is in its system prompt; the message the user sees in its chat is the brief.
        let first = format!("{}\n\n{}", l(&format!("[Swarm] 你是 @{name}，来自 @{COORDINATOR} 的任务："), &format!("[Swarm] You are @{name}. Your brief from @{COORDINATOR}:")), brief.trim());
        let id = self.chats()?.create_worker(coordinator.to_owned(), name.to_owned(), first, model, isolated).await?;
        self.record(coordinator, COORDINATOR, vec![name.to_owned()], brief.trim().to_owned(), false)?;
        Ok(id)
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
        if names.iter().any(|name| name == "all") {
            names = std::iter::once(COORDINATOR.to_owned())
                .chain(self.worker_chats(coordinator).into_iter().filter(|chat| !chat.archived).filter_map(|chat| chat.swarm.map(|link| link.name)))
                .filter(|name| name != from)
                .collect();
        }
        let mut targets = Vec::new();
        for name in &names {
            if name == COORDINATOR {
                targets.push(None);
            } else {
                let chat = self.active_member(coordinator, name).ok_or_else(|| invalid(&format!("没有叫 @{name} 的工作者。"), &format!("There is no worker named @{name}.")))?;
                targets.push(Some(chat.id));
            }
        }
        let post = self.record(coordinator, from, names, text.trim().to_owned(), auto)?;
        let chats = self.chats()?;
        for target in targets {
            match target {
                None if from != COORDINATOR => self.to_coordinator(coordinator, post.clone()).await,
                None => {}
                Some(id) => {
                    let message = format!("[Swarm board · from @{from}]\n{}", post.text);
                    if let Err(error) = chats.deliver(id, message).await {
                        log::warn!("[swarm {coordinator}] delivering to a worker failed: {}", error.message);
                    }
                }
            }
        }
        Ok(post)
    }

    /// A post for the coordinator: a waiting `swarm_wait` takes it; an idle coordinator starts a turn with it; a busy
    /// one gets it when its turn ends.
    async fn to_coordinator(&self, coordinator: &str, post: Post) {
        let deliver = {
            let mut inboxes = self.inboxes.lock().unwrap();
            let inbox = inboxes.entry(coordinator.to_owned()).or_default();
            inbox.unseen.push(post);
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
        if chats.state(coordinator).status != "idle" && chats.state(coordinator).status != "error" {
            return;
        }
        let posts = {
            let mut inboxes = self.inboxes.lock().unwrap();
            let Some(inbox) = inboxes.get_mut(coordinator).filter(|inbox| !inbox.waiting) else { return };
            std::mem::take(&mut inbox.unseen)
        };
        if posts.is_empty() {
            return;
        }
        let text = format!("[Swarm board]\n{}\n\nMembers:\n{}", digest(&posts), self.roster(coordinator));
        if let Err(error) = chats.deliver(coordinator.to_owned(), text).await {
            log::warn!("[swarm {coordinator}] waking the coordinator failed: {}", error.message);
        }
    }

    /// A turn of a swarm chat ended: a worker's reply goes to the coordinator; a coordinator gets what came meanwhile.
    pub async fn turn_ended(&self, chat: &StoredChat, aborted: bool) {
        let Some(link) = &chat.swarm else { return };
        if link.coordinator == chat.id {
            self.hand_over(&chat.id).await;
            return;
        }
        if aborted || chat.archived {
            return;
        }
        let reply = self.chats().ok().and_then(|chats| chats.last_reply(&chat.id)).unwrap_or_default();
        let text = if reply.trim().is_empty() { l("（这一轮结束了，没有回复）", "(finished its turn without a reply)") } else { excerpt(reply.trim(), REPORT_CHARS) };
        if let Err(error) = self.post(&link.coordinator, &link.name, &[COORDINATOR.to_owned()], &text, true).await {
            log::warn!("[swarm {}] reporting @{} failed: {}", link.coordinator, link.name, error.message);
        }
    }

    /// Wait until posts reach the coordinator, every worker is idle, the user writes or `timeout` passes.
    pub async fn wait(&self, coordinator: &str, timeout: Duration, cancel: &CancellationToken) -> String {
        let notify = {
            let mut inboxes = self.inboxes.lock().unwrap();
            let inbox = inboxes.entry(coordinator.to_owned()).or_default();
            inbox.waiting = true;
            inbox.notify.clone()
        };
        let deadline = tokio::time::Instant::now() + timeout;
        let reason = loop {
            if !self.inboxes.lock().unwrap().get(coordinator).is_none_or(|inbox| inbox.unseen.is_empty()) {
                break "new posts";
            }
            let chats = self.chats().ok();
            let members = self.members(coordinator);
            if members.iter().filter(|member| !member.retired).all(|member| member.status != "running" && member.status != "compacting") {
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
                () = tokio::time::sleep(Duration::from_secs(1)) => {}
                () = cancel.cancelled() => break "stopped",
            }
        };
        let posts = {
            let mut inboxes = self.inboxes.lock().unwrap();
            let inbox = inboxes.entry(coordinator.to_owned()).or_default();
            inbox.waiting = false;
            std::mem::take(&mut inbox.unseen)
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
        if chats.state(&chat.id).status == "running" {
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

    /// Stop every running chat of the swarm.
    pub fn stop(&self, coordinator: &str) {
        let Ok(chats) = self.chats() else { return };
        chats.abort(coordinator);
        for chat in self.worker_chats(coordinator).into_iter().filter(|chat| !chat.archived) {
            chats.abort(&chat.id);
        }
    }

    /// The coordinator a chat belongs to.
    pub fn coordinator_of(&self, chat_id: &str) -> Option<String> {
        self.store.get(chat_id).and_then(|chat| chat.swarm).map(|link| link.coordinator)
    }
}

/// The link that makes a chat a coordinator.
pub fn coordinator_link(id: &str) -> SwarmLink {
    SwarmLink { coordinator: id.to_owned(), name: COORDINATOR.into() }
}

/// `/swarm <task>`: the task, or `None` when the text is no swarm command.
pub fn swarm_command(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix("/swarm")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

/// The part of the system prompt that gives a swarm chat its role.
pub fn prompt_section(link: &SwarmLink, self_id: &str) -> String {
    if link.coordinator == self_id {
        format!(
            "\n\n# Swarm\nYou are the coordinator of a swarm, started with /swarm. You do not do the technical work: you split the task, brief workers, judge their results and report to the user. You cannot edit files yourself.\n\
             - Split the task into slices that do not collide (different files or areas). Add each worker with swarm_add: a short name, and a brief stating what it owns, what it must not touch, and what done means. At most {MAX_WORKERS} workers at a time.\n\
             - Give workers that edit at the same time their own worktree (isolated: true, needs a Git project); a worker that only reads or works alone can share the checkout.\n\
             - For every two to four workers add a @verifier that reviews diffs and runs tests, and reports problems to the author.\n\
             - Then call swarm_wait. A worker's finished turn is posted to you on its own; workers and you talk with swarm_post (@name, @all). Answer questions, redirect, add or remove workers as needed.\n\
             - When a worker with a worktree is done and verified, move its changes into the project with swarm_land, one at a time, then swarm_remove it. Workers without a worktree changed the project directly.\n\
             - Finish with a report to the user: what was done, by whom, what was verified and what is left.\n\
             Board posts that reach you while you are not waiting arrive as a message starting with [Swarm board]."
        )
    } else {
        format!(
            "\n\n# Swarm\nYou are @{}, a worker of a swarm coordinated by @{COORDINATOR}. Do the work of your brief only; leave other areas to their owners. When you need something from another member or have news for them, use swarm_post with @name, @{COORDINATOR} or @all. When your turn ends, your final reply is posted to @{COORDINATOR} on its own: end each turn with a concise report (what changed, how it was verified, what is open). Messages from the board arrive as [Swarm board · from @name].",
            link.name
        )
    }
}

// ── tools ──

fn tool_error(error: HostError) -> String {
    error.message
}

#[derive(Deserialize)]
struct AddParams {
    name: String,
    brief: String,
    #[serde(default)]
    isolated: bool,
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
            let id = self.hub.add_worker(&self.coordinator, &name, &params.brief, None, params.isolated).await.map_err(tool_error)?;
            Ok(ToolOutput::text(
                format!("@{name} started on its brief{}.\n\nMembers:\n{}", if params.isolated { " in its own worktree" } else { "" }, self.hub.roster(&self.coordinator)),
                json!({ "name": name, "sessionId": id, "isolated": params.isolated }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_recipients() {
        assert!(valid_name("api-tests") && valid_name("fix-1"));
        assert!(!valid_name("Parser") && !valid_name("all") && !valid_name("coordinator") && !valid_name("-x") && !valid_name(""));
        assert_eq!(recipients(&["@parser, @Docs".into(), "parser".into()]), vec!["parser", "docs"]);
    }

    #[test]
    fn the_swarm_command() {
        assert_eq!(swarm_command("/swarm fix the tests"), Some("fix the tests"));
        assert_eq!(swarm_command("  /swarm"), Some(""));
        assert_eq!(swarm_command("/swarming"), None);
        assert_eq!(swarm_command("please /swarm"), None);
    }

    #[test]
    fn a_digest_names_sender_and_recipients() {
        let post = Post { id: "1".into(), ts: 0.0, from: "parser".into(), to: vec!["coordinator".into()], text: " done \n".into(), auto: true };
        let brief = Post { id: "2".into(), ts: 0.0, from: "coordinator".into(), to: vec!["parser".into(), "lexer".into()], text: "port it".into(), auto: false };
        assert_eq!(digest(&[brief, post]), "@coordinator → @parser @lexer:\nport it\n\n@parser → @coordinator (turn report):\ndone");
        assert_eq!(excerpt("一二三四", 2), "一二…");
    }
}
