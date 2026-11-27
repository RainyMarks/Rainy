//! Swarms (`/swarm <task>`): the chat becomes a coordinator that splits a large task among worker chats and does no
//! technical work itself. Workers are ordinary chats of the project, each named (`@parser`), started from a brief and
//! optionally in a Git worktree of its own so several can edit at once. Everyone talks over one board: a post names
//! its recipients (`@name`, `@coordinator`, `@all`) and reaches each of them as a message. A worker's turn that ends
//! is posted to the coordinator on its own, so the coordinator learns of results without polling. The coordinator
//! waits with `swarm_wait`; posts that reach it while it is idle start a turn of its own, and ones that come while it
//! works are handed over when its turn ends.

mod hub;
mod tools;

use std::collections::HashSet;
use std::time::Duration;

use futures::future::BoxFuture;
use rainy_protocol::chat::{SwarmLink, UsageSummary};
use serde::{Deserialize, Serialize};

pub use hub::Hub;
pub use tools::tools;

use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::settings::ModelSelection;

/// The coordinator's own name on the board.
pub const COORDINATOR: &str = "coordinator";
/// Workers one swarm may have at a time; parallel workers spend quota fast.
pub const MAX_WORKERS: usize = 8;
/// Longest a `swarm_wait` waits, and how long it waits when the model gives no time.
pub(super) const MAX_WAIT: Duration = Duration::from_secs(30 * 60);
pub(super) const DEFAULT_WAIT: Duration = Duration::from_secs(10 * 60);
/// Characters of a worker's reply posted when its turn ends.
pub(super) const REPORT_CHARS: usize = 4000;
/// Grace window after spawning or messaging a worker during which `swarm_wait` treats it as active even before its
/// async `start_run` transitions status to `"running"`.
pub(super) const STARTING_GRACE: Duration = Duration::from_secs(5);
/// Default circuit-breaker caps per swarm run window (reset when the user sends a new message to the coordinator). They
/// catch runaway swarms, not ordinary work: tokens are fresh tokens only (see `fresh_tokens`), the token cap matters
/// mostly for models without a price, and the cost cap for priced ones.
pub const DEFAULT_MAX_TURNS: u32 = 40;
pub const DEFAULT_MAX_TOKENS: u64 = 20_000_000;
pub const DEFAULT_MAX_COST_USD: f64 = 20.0;

/// Tokens a run newly consumed: uncached input, output and cache writes. Cache reads are left out — every model call
/// re-reads the whole context, so they grow with the square of a turn's length while costing a tenth of input.
pub fn fresh_tokens(usage: &UsageSummary) -> u64 {
    (usage.input + usage.output + usage.cache_write).max(0.0) as u64
}

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
    /// Whether queued/pending work is waiting to run immediately after the current turn.
    pub has_pending: bool,
}

/// The chats a swarm acts on, implemented by the agent that owns them.
pub trait Chats: Send + Sync {
    /// Create a worker chat of the coordinator's project and send it its first message.
    fn create_worker(&self, coordinator: String, name: String, first: String, model: Option<ModelSelection>, isolated: bool) -> BoxFuture<'static, HostResult<String>>;
    /// Deliver a message to a chat without preempting pending user approvals.
    fn deliver(&self, id: String, text: String) -> BoxFuture<'static, HostResult<()>>;
    /// Resolve an optional model override string (`provider/model`, `provider`, or `model`) to a configured selection.
    fn resolve_model(&self, hint: &str) -> HostResult<ModelSelection>;
    /// Move a worker's worktree changes into the project's checkout; the number of files moved.
    fn land(&self, id: String) -> BoxFuture<'static, HostResult<usize>>;
    /// Stop a worker and archive it; `discard` drops its worktree with any changes not landed.
    fn retire(&self, id: String, discard: bool) -> BoxFuture<'static, HostResult<()>>;
    fn abort(&self, id: &str);
    fn state(&self, id: &str) -> ChatState;
    /// The text of the chat's latest reply.
    fn last_reply(&self, id: &str) -> Option<String>;
}

/// Per-swarm circuit breaker tracking worker turns, tokens and reported cost.
#[derive(Clone, Debug)]
pub struct BudgetGuard {
    pub turns: u32,
    pub max_turns: u32,
    pub tokens: u64,
    pub max_tokens: u64,
    pub cost_usd: f64,
    pub max_cost_usd: f64,
    pub tripped: Option<String>,
}

impl Default for BudgetGuard {
    fn default() -> Self {
        Self {
            turns: 0,
            max_turns: DEFAULT_MAX_TURNS,
            tokens: 0,
            max_tokens: DEFAULT_MAX_TOKENS,
            cost_usd: 0.0,
            max_cost_usd: DEFAULT_MAX_COST_USD,
            tripped: None,
        }
    }
}

impl BudgetGuard {
    /// Record one completed worker turn and return the trip reason if a limit was reached.
    pub fn record_turn(&mut self, usage: Option<&UsageSummary>) -> Option<String> {
        self.turns = self.turns.saturating_add(1);
        if let Some(usage) = usage {
            self.tokens = self.tokens.saturating_add(fresh_tokens(usage));
            if let Some(cost) = usage.cost.filter(|c| c.is_finite() && *c > 0.0) {
                self.cost_usd += cost;
            }
        }
        if self.turns >= self.max_turns {
            let reason = l(
                &format!("Swarm 熔断：已达到最大工作者轮次上限（{} 轮）。用户发来新消息后重新计数。", self.max_turns),
                &format!("Swarm circuit breaker tripped: reached the maximum worker turn limit ({} turns). It starts over when the user sends a new message.", self.max_turns),
            );
            self.tripped = Some(reason.clone());
            return Some(reason);
        }
        if self.tokens >= self.max_tokens {
            let reason = l(
                &format!("Swarm 熔断：累计 Token 用量（{}）已达到上限（{}）。用户发来新消息后重新计数。", self.tokens, self.max_tokens),
                &format!("Swarm circuit breaker tripped: cumulative token usage ({}) reached the limit ({}). It starts over when the user sends a new message.", self.tokens, self.max_tokens),
            );
            self.tripped = Some(reason.clone());
            return Some(reason);
        }
        if self.cost_usd >= self.max_cost_usd {
            let reason = l(
                &format!("Swarm 熔断：累计费用（${:.2}）已达到上限（${:.2}）。用户发来新消息后重新计数。", self.cost_usd, self.max_cost_usd),
                &format!("Swarm circuit breaker tripped: cumulative cost (${:.2}) reached the limit (${:.2}). It starts over when the user sends a new message.", self.cost_usd, self.max_cost_usd),
            );
            self.tripped = Some(reason.clone());
            return Some(reason);
        }
        None
    }
}

pub(super) fn invalid(zh: &str, en: &str) -> HostError {
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
pub(super) fn recipients(to: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    to.iter()
        .flat_map(|item| item.split([',', ' ']))
        .map(|item| item.trim().trim_start_matches('@').to_ascii_lowercase())
        .filter(|item| !item.is_empty() && seen.insert(item.clone()))
        .collect()
}

pub(super) fn excerpt(text: &str, chars: usize) -> String {
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
             - Split the task into slices that do not collide (different files or areas). Add each worker with swarm_add: a short name, and a brief stating what it owns, what it must not touch, and what done means. Optionally pass `model` to run routine implementation or search workers on a faster/cheaper configured model. At most {MAX_WORKERS} workers at a time.\n\
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::agent::store::ChatStore;
    use crate::rpc::RpcHub;

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

    #[test]
    fn budget_guard_trips_on_turns_tokens_or_cost() {
        let mut by_turns = BudgetGuard { max_turns: 2, ..BudgetGuard::default() };
        assert!(by_turns.record_turn(None).is_none());
        assert!(by_turns.record_turn(None).is_some());
        assert!(by_turns.tripped.is_some());

        let mut by_tokens = BudgetGuard { max_tokens: 1000, ..BudgetGuard::default() };
        let usage = UsageSummary { input: 700.0, output: 400.0, ..UsageSummary::default() };
        assert!(by_tokens.record_turn(Some(&usage)).is_some());

        // Cache reads are not fresh tokens: a long turn re-reading its context does not trip the token cap.
        let mut cached = BudgetGuard { max_tokens: 1000, ..BudgetGuard::default() };
        let reread = UsageSummary { input: 300.0, output: 200.0, cache_read: 5_000_000.0, cache_write: 100.0, ..UsageSummary::default() };
        assert!(cached.record_turn(Some(&reread)).is_none());
        assert_eq!(cached.tokens, 600);

        let mut by_cost = BudgetGuard { max_cost_usd: 1.0, ..BudgetGuard::default() };
        let pricey = UsageSummary { input: 10.0, output: 10.0, cost: Some(1.5), ..UsageSummary::default() };
        assert!(by_cost.record_turn(Some(&pricey)).is_some());
    }

    #[tokio::test]
    async fn persists_and_restores_coordinator_inbox_and_keeps_posts_on_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::load(dir.path().join("chats")).unwrap();
        let rpc = Arc::new(RpcHub::default());
        let hub = Hub::new(dir.path().join("swarms"), store.clone(), rpc.clone());
        let post = Post { id: "p1".into(), ts: 1.0, from: "w1".into(), to: vec!["coordinator".into()], text: "hello".into(), auto: false };
        hub.to_coordinator("coord-1", post).await;
        assert!(dir.path().join("swarms/coord-1.inbox.json").is_file());

        // A cancelled wait must NOT drain or erase unseen inbox posts.
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let aborted_text = hub.wait("coord-1", Duration::from_secs(1), &cancelled).await;
        assert!(aborted_text.contains("stopped"), "{aborted_text}");
        assert!(dir.path().join("swarms/coord-1.inbox.json").is_file());

        // A fresh Hub after restart loads the persisted unseen post on a non-cancelled `wait`.
        let restarted = Hub::new(dir.path().join("swarms"), store, rpc);
        let text = restarted.wait("coord-1", Duration::from_secs(1), &CancellationToken::new()).await;
        assert!(text.contains("hello") && text.contains("new posts"), "unexpected wait output: {text}");
        assert!(!dir.path().join("swarms/coord-1.inbox.json").exists());
    }
}
