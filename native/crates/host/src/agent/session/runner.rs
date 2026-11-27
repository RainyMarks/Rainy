//! Native agent run loop, recovery/retry waiting, context compaction, and `run::Hooks` glue for [`Session`].

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rainy_protocol::chat::{AssistantMessage, ContextUsage, RetryWait, TranscriptEntry, UsageSummary};
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::super::budget::{Budget, estimate_messages, expected_tokens, resolve_budget};
use super::super::compaction::{self, CompactionRequest, SHARED_CACHE_WINDOW_MS, Strategy, add_usage};
use super::super::context::{build_context, cleared_ids};
use super::super::hooks;
use super::super::llm::{self, Message, context_overflow};
use super::super::models::Resolved;
use super::super::orchestrate::Phase;
use super::super::retry::{self, AUTO_RETRIES, is_passing_failure};
use super::super::run::{self, Blocked, Event, Hooks, State};
use super::super::tools::ToolOutput;
use super::messaging::settle_steering;
use super::{Inner, Meta, RunInfo, Session, StartGuard, busy, fallback_title, is_busy, jitter, no_model, request_key, tool_style};
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::text::now_ms;

impl Session {
    /// Compact the chat now (`/compact`); a message for the user.
    pub async fn compact_now(&self) -> HostResult<String> {
        if self.chat().external_backend.is_some() {
            return Err(HostError::invalid(l(
                "外部 Agent 对话的上下文压缩由外部 CLI 自行处理。",
                "Context compaction for external agent chats is managed by the external CLI.",
            )));
        }
        let resolved = self.resolved().ok_or_else(no_model)?;
        let cancel = CancellationToken::new();
        let (before, previous_status) = {
            let mut inner = self.inner.lock().unwrap();
            if inner.closing {
                return Err(HostError::new("closed", l("对话正在关闭。", "This chat is closing.")));
            }
            if is_busy(&inner) {
                return Err(busy("请等待当前任务结束后再压缩。", "Wait for the current task to finish before compacting."));
            }
            let prev = inner.status;
            inner.status = "compacting";
            inner.compact_cancel = Some(cancel.clone());
            (build_context(&inner.entries).len(), prev)
        };
        self.publish_state();
        self.services.rpc.emit("sessions.changed", self.summary());
        let result = self.compact(&resolved, "manual", Some(cancel.clone()), None).await;
        {
            let mut inner = self.inner.lock().unwrap();
            inner.compact_cancel = None;
            if inner.status == "compacting" {
                inner.status = if previous_status == "error" && inner.error.is_some() { "error" } else { "idle" };
            }
        }
        self.publish_state();
        self.services.rpc.emit("sessions.changed", self.summary());
        let Some(TranscriptEntry::Compaction { tokens_before, .. }) = result? else {
            if cancel.is_cancelled() {
                return Err(HostError::new("aborted", l("已取消压缩。", "Compaction cancelled.")));
            }
            return Ok(l("还没有可以压缩的历史。", "There is no history to compact yet."));
        };
        let after = build_context(&self.inner.lock().unwrap().entries).len();
        let items = (before + 1).saturating_sub(after);
        Ok(l(&format!("已压缩 {items} 条历史（约 {tokens_before} tokens）。"), &format!("Compacted {items} history items (~{tokens_before} tokens).")))
    }

    /// Prepare the initial state of one run iteration (used both when starting fresh and when continuing seamlessly
    /// into the next queued message inside the same `drive` task).
    pub(super) async fn prepare_run(self: &Arc<Self>, prompts: Vec<(Message, Meta)>) -> HostResult<(State, Vec<(Message, Meta)>, Resolved, HashSet<String>, CancellationToken)> {
        if self.inner.lock().unwrap().closing {
            return Err(HostError::new("closed", l("对话正在关闭。", "This chat is closing.")));
        }
        self.services.activity.assert_can_start()?;
        let resolved = self.resolved().ok_or_else(no_model)?;
        let first_text = prompts.first().and_then(|(_, meta)| if let Meta::User { text, .. } = meta { Some(text.clone()) } else { None });
        if self.chat().title.is_empty() {
            let chat = self.services.store.set_default_title(&self.id, &fallback_title(first_text.as_deref().unwrap_or_default())).await?;
            self.inner.lock().unwrap().chat = chat;
        }
        // A new message keeps what the agent changed before it, unless the user keeps or undoes each change.
        if first_text.is_some() && self.services.settings.prefs().get("autoKeepChanges").and_then(Value::as_bool) == Some(true) {
            let ids: Vec<String> = self.pending_changes().await.into_iter().flat_map(|group| group.calls.into_iter().map(|call| call.id)).collect();
            if let Err(error) = self.keep(&ids).await {
                log::warn!("[review] {error}");
            }
        }
        let reset_phase = {
            let mut inner = self.inner.lock().unwrap();
            let reset = if first_text.is_some() {
                let changed = inner.phase != Phase::Plan || inner.phase_note.is_some();
                inner.phase = Phase::Plan;
                inner.tool_errors = 0;
                inner.phase_note = None;
                changed
            } else {
                false
            };
            inner.phase_sent = Some(inner.phase);
            reset
        };
        if reset_phase {
            self.persist_orchestrate_phase(Phase::Plan, None);
        }
        let injections = self.first_request_context(&resolved).await?;
        let tools = self.tools();
        let mcp_tools: HashSet<String> = self.services.mcp.tools(None).iter().map(|tool| tool.spec().name.clone()).collect();
        let system = self.system_prompt(&tools);
        let cancel = CancellationToken::new();
        let messages = {
            let mut inner = self.inner.lock().unwrap();
            if inner.closing {
                return Err(HostError::new("closed", l("对话正在关闭。", "This chat is closing.")));
            }
            inner.run = Some(RunInfo {
                started: now_ms(),
                from: inner.entries.len(),
                requests: 0,
                usage: UsageSummary::default(),
                aborted: false,
                overflow_retried: false,
                retries: 0,
                cancel: cancel.clone(),
            });
            inner.error = None;
            build_context(&inner.entries).into_iter().map(|item| item.message).collect()
        };
        self.set_status("running");
        self.services.memory.interrupt();
        let state = State {
            model: resolved.model.clone(),
            reasoning: resolved.thinking,
            system,
            tools,
            messages,
            session_id: Some(self.id.clone()),
            api_key: self.services.models.api_key(&resolved.setup.provider),
            thread: Some(self.thread.clone()),
        };
        let prompts: Vec<(Message, Meta)> = prompts.into_iter().chain(injections).collect();
        Ok((state, prompts, resolved, mcp_tools, cancel))
    }

    /// Launch a run after the caller has atomically set `Inner::starting = true`.
    pub(super) async fn start_run_claimed(self: &Arc<Self>, prompts: Vec<(Message, Meta)>, guard: StartGuard<'_>) -> HostResult<()> {
        match self.prepare_run(prompts).await {
            Ok((state, prompts, resolved, mcp_tools, cancel)) => {
                let this = self.clone();
                let task = tokio::spawn(async move { this.drive(state, prompts, resolved, mcp_tools, cancel).await });
                *self.task.lock().unwrap() = Some(task);
                self.inner.lock().unwrap().starting = false;
                guard.disarm();
                Ok(())
            }
            Err(error) => {
                // If concurrent sends queued while we prepared, hand off to a background relay before reporting our error.
                let next = {
                    let mut inner = self.inner.lock().unwrap();
                    if inner.closing {
                        inner.starting = false;
                        None
                    } else {
                        let next_id = inner.queue.iter().find(|item| inner.pending.contains_key(&item.id)).map(|item| item.id.clone());
                        let next = next_id.and_then(|id| {
                            inner.queue.retain(|item| item.id != id);
                            inner.pending.remove(&id)
                        });
                        if next.is_none() {
                            inner.starting = false;
                        }
                        next
                    }
                };
                guard.disarm();
                if let Some(next_messages) = next {
                    let this = self.clone();
                    let relay = tokio::spawn(async move {
                        let relay_guard = StartGuard::new(&this);
                        match this.prepare_run(next_messages).await {
                            Ok((state, prompts, resolved, mcp_tools, cancel)) => {
                                this.inner.lock().unwrap().starting = false;
                                relay_guard.disarm();
                                this.drive(state, prompts, resolved, mcp_tools, cancel).await;
                            }
                            Err(relay_err) => {
                                let closing = this.inner.lock().unwrap().closing;
                                drop(relay_guard);
                                if !closing {
                                    this.inner.lock().unwrap().error = Some(relay_err.message);
                                    this.set_status("error");
                                } else {
                                    this.set_status("idle");
                                }
                            }
                        }
                    });
                    *self.task.lock().unwrap() = Some(relay);
                }
                Err(error)
            }
        }
    }

    async fn drive(self: Arc<Self>, mut state: State, mut prompts: Vec<(Message, Meta)>, mut resolved: Resolved, mut mcp_tools: HashSet<String>, mut cancel: CancellationToken) {
        loop {
            let hooks = RunHooks { session: self.clone(), mcp_tools, resolved: resolved.clone() };
            loop {
                run::run(&self.services.models.llm, &mut state, std::mem::take(&mut prompts), &hooks, &cancel).await;
                if !self.recover(&resolved, &mut state).await {
                    break;
                }
            }
            match self.finish_run(&resolved).await {
                Some(next_messages) => {
                    let guard = StartGuard::new(&self);
                    match self.prepare_run(next_messages).await {
                        Ok((next_state, next_prompts, next_resolved, next_mcp, next_cancel)) => {
                            self.inner.lock().unwrap().starting = false;
                            guard.disarm();
                            state = next_state;
                            prompts = next_prompts;
                            resolved = next_resolved;
                            mcp_tools = next_mcp;
                            cancel = next_cancel;
                        }
                        Err(error) => {
                            let closing = self.inner.lock().unwrap().closing;
                            drop(guard);
                            if !closing {
                                self.inner.lock().unwrap().error = Some(error.message);
                                self.set_status("error");
                            } else {
                                self.set_status("idle");
                            }
                            break;
                        }
                    }
                }
                None => break,
            }
        }
    }

    /// After a failed request: compact and ask again when the context overflowed, or wait and ask again when the
    /// failure is passing. Whether the run continues.
    async fn recover(&self, resolved: &Resolved, state: &mut State) -> bool {
        let last = |inner: &Inner| {
            inner.entries.iter().enumerate().rev().find_map(|(index, entry)| if let TranscriptEntry::Assistant { message, .. } = entry { Some((index, message.clone())) } else { None })
        };
        let (overflow, last) = {
            let mut inner = self.inner.lock().unwrap();
            let last = last(&inner);
            let Some(run) = &mut inner.run else { return false };
            // Only a failed request is compacted and asked again (3.x); a reply that ended normally keeps the chat.
            let overflow = !run.aborted
                && !run.overflow_retried
                && last.as_ref().is_some_and(|(_, message)| message.stop_reason == "error" && context_overflow(message, resolved.model.context_window));
            if overflow {
                run.overflow_retried = true;
            }
            (overflow, last)
        };
        // The provider refused the request as too long, so it counts as filling the whole window.
        let budget = resolve_budget(resolved.model.context_window, Some(resolved.model.max_tokens));
        if overflow
            && let Ok(budget) = budget
            && matches!(self.free_context(resolved, &budget, budget.context_window, "overflow", None, None).await, Ok(true))
        {
            self.notice("overflow-retry", l("上下文超出模型窗口，已自动压缩并重试。", "The context exceeded the model window, so the chat was compacted and the request retried.")).await;
            if self.resume(state) {
                return true;
            }
        }
        let Some((index, message)) = last else { return false };
        let attempt = {
            let mut inner = self.inner.lock().unwrap();
            let Some(run) = &mut inner.run else { return false };
            if run.aborted || run.retries >= AUTO_RETRIES || index < run.from || !is_passing_failure(&message) {
                return false;
            }
            run.retries + 1
        };
        let reason = message.error_message.clone().unwrap_or_default();
        let Some(wait) = retry::delay(attempt, &reason, jitter()) else { return false };
        if let Some(run) = &mut self.inner.lock().unwrap().run {
            run.retries = attempt;
        }
        let go = self.wait_to_retry(RetryWait { attempt, max: AUTO_RETRIES, at: (now_ms() + wait) as f64, reason }).await;
        if !go || self.inner.lock().unwrap().run.as_ref().is_none_or(|run| run.aborted) {
            return false;
        }
        self.notice("auto-retry", l(&format!("请求失败，已自动重试（第 {attempt} 次）。"), &format!("The request failed, so it was retried automatically (attempt {attempt})."))).await;
        self.resume(state)
    }

    /// Ask again from the transcript as it stands; whether there is anything to ask (not when it ends with a reply).
    fn resume(&self, state: &mut State) -> bool {
        let history: Vec<Message> = build_context(&self.inner.lock().unwrap().entries).into_iter().map(|item| item.message).collect();
        if matches!(history.last(), Some(Message::Assistant(_))) {
            return false;
        }
        state.tools = self.tools();
        state.system = self.system_prompt(&state.tools);
        state.messages = history;
        self.inner.lock().unwrap().reported = None;
        true
    }

    /// Wait for an automatic retry while the page shows a countdown: `true` to retry (time is up, or [`Self::retry`]
    /// asked for it now), `false` when the run was stopped.
    pub(super) async fn wait_to_retry(&self, wait: RetryWait) -> bool {
        let (wake, woken) = oneshot::channel();
        let delay = Duration::from_millis((wait.at as u64).saturating_sub(now_ms()));
        let run_cancel = {
            let mut inner = self.inner.lock().unwrap();
            let Some(run) = &inner.run else { return false };
            if run.aborted || run.cancel.is_cancelled() || inner.closing {
                return false;
            }
            let cancel = run.cancel.clone();
            inner.retry_wake = Some(wake);
            inner.retry_wait = Some(wait);
            cancel
        };
        self.publish_state();
        let go = tokio::select! {
            _ = tokio::time::sleep(delay) => true,
            answer = woken => answer.unwrap_or(false),
            () = run_cancel.cancelled() => false,
        };
        {
            let mut inner = self.inner.lock().unwrap();
            inner.retry_wake = None;
            inner.retry_wait = None;
        }
        self.publish_state();
        go
    }

    /// Finish one run iteration and return the next queued message batch to run inside the same task, or `None` once
    /// the session transitions atomically to `idle`/`error` (eliminating `finish_run` → `start_next` vs `send`/`close` races).
    pub(super) async fn finish_run(self: &Arc<Self>, resolved: &Resolved) -> Option<Vec<(Message, Meta)>> {
        let run = {
            let mut inner = self.inner.lock().unwrap();
            inner.streaming = None;
            inner.stream_sent = None;
            inner.running_tools.clear();
            inner.stop_after_step = false;
            inner.refused.clear();
            let run = inner.run.take();
            let (steering, inner) = (std::mem::take(&mut inner.steering), &mut *inner);
            settle_steering(steering, &mut inner.queue, &mut inner.pending, run.as_ref().is_some_and(|run| run.aborted));
            run
        };
        let aborted = run.as_ref().is_some_and(|run| run.aborted);
        let turn_usage = run.as_ref().filter(|r| !r.aborted && r.requests > 0).map(|r| r.usage.clone());
        if let Some(run) = run {
            if run.aborted {
                self.notice("stopped", l("已停止。", "Stopped.")).await;
            } else if run.requests > 0 {
                self.append(TranscriptEntry::Turn {
                    id: uuid::Uuid::new_v4().to_string(),
                    ts: now_ms() as f64,
                    duration_ms: now_ms().saturating_sub(run.started) as f64,
                    usage: run.usage,
                    provider: resolved.setup.provider.clone(),
                    model: resolved.setup.model.clone(),
                    requests: f64::from(run.requests),
                })
                .await;
                let (memory, id, workspace) = (self.services.memory.clone(), self.id.clone(), self.chat().workspace_id);
                tokio::spawn(async move {
                    if let Err(error) = memory.turn_completed(&id, workspace.as_deref()).await {
                        log::warn!("[memory] {error}");
                    }
                });
            }
        }
        // Run declarative `Stop` hooks (trusted project + global), cancelled immediately if `close()`/`abort()` fires.
        if !self.inner.lock().unwrap().closing {
            let stop_rules = self.effective_hooks();
            if stop_rules.iter().any(|r| r.enabled && r.event == "Stop") {
                let stop_cancel = CancellationToken::new();
                {
                    let mut inner = self.inner.lock().unwrap();
                    if inner.closing {
                        stop_cancel.cancel();
                    } else {
                        inner.stop_cancel = Some(stop_cancel.clone());
                    }
                }
                if !stop_cancel.is_cancelled() {
                    let ctx = self.hook_context();
                    hooks::run_stop_cancellable(&stop_rules, &ctx, aborted, Some(&stop_cancel)).await;
                }
                self.inner.lock().unwrap().stop_cancel = None;
            }
        }
        self.services.memory.resume();
        self.services.model_activity.store(now_ms(), Ordering::SeqCst);

        // Decide atomically whether any queued message awaits (including a message sent after `abort()` cleared old queues)
        // before transitioning to `idle`/`error` under the same lock so no concurrent `send()` can be stranded.
        let (next, transitioned) = {
            let mut inner = self.inner.lock().unwrap();
            let next = if inner.closing {
                None
            } else {
                let next_id = inner.queue.iter().find(|item| inner.pending.contains_key(&item.id)).map(|item| item.id.clone());
                next_id.and_then(|id| {
                    inner.queue.retain(|item| item.id != id);
                    inner.pending.remove(&id)
                })
            };
            if next.is_some() {
                inner.starting = true;
                (next, false)
            } else {
                let target = if inner.error.is_some() { "error" } else { "idle" };
                let changed = inner.status != target;
                inner.status = target;
                (None, changed)
            }
        };

        if next.is_some() {
            self.publish_state();
            if self.chat().swarm.is_some() {
                let (hub, chat, usage) = (self.services.swarm.clone(), self.chat(), turn_usage);
                tokio::spawn(async move { hub.turn_ended(&chat, aborted, usage.as_ref()).await });
            }
            return next;
        }

        if transitioned {
            self.publish_state();
            self.services.rpc.emit("sessions.changed", self.summary());
        }
        if self.chat().swarm.is_some() {
            let (hub, chat, usage) = (self.services.swarm.clone(), self.chat(), turn_usage);
            tokio::spawn(async move { hub.turn_ended(&chat, aborted, usage.as_ref()).await });
        }
        None
    }

    pub(super) async fn prepare_request(&self, initial: &Resolved, state: &mut State, cancel: &CancellationToken) {
        // The chat's model may have changed since the run started; another family works with other tools and text.
        let latest = self.request_resolved().unwrap_or_else(|| initial.clone());
        let orchestrated = self.chat().orchestrate.is_some();
        let phase_changed = {
            let mut inner = self.inner.lock().unwrap();
            let phase = inner.phase;
            inner.phase_sent.replace(phase) != Some(phase) && orchestrated
        };
        let refit = phase_changed || latest.model.id != state.model.id || tool_style(&latest.model) != tool_style(&state.model);
        state.model = latest.model.clone();
        if refit {
            state.tools = self.tools();
            state.system = self.system_prompt(&state.tools);
            self.inner.lock().unwrap().reported = None;
        }
        state.reasoning = latest.thinking;
        state.api_key = self.services.models.api_key(&latest.setup.provider);
        let Ok(budget) = resolve_budget(state.model.context_window, Some(state.model.max_tokens)) else { return };
        let specs = state.specs();
        let estimate = estimate_messages(&state.system, &specs, &state.messages);
        let model_key = format!("{}/{}", state.model.provider, state.model.id);
        let (ratio, running, reported) = {
            let mut inner = self.inner.lock().unwrap();
            inner.last_estimate = estimate;
            inner.last_model_key = model_key.clone();
            inner.last_sent = (state.messages.len(), state.system.len(), specs.len());
            (inner.calibration.factor_for(&model_key), inner.run.is_some(), inner.reported)
        };
        let mut tokens = expected_tokens(&state.system, &specs, &state.messages, reported.as_ref(), ratio);
        let key = request_key(&state.model, state.reasoning);
        if tokens >= budget.compact_at && running {
            match self.free_context(&latest, &budget, tokens, "auto", Some(cancel.clone()), Some(&state.messages)).await {
                Ok(true) => {
                    let rebuilt: Vec<Message> = build_context(&self.inner.lock().unwrap().entries).into_iter().map(|item| item.message).collect();
                    let estimate = estimate_messages(&state.system, &specs, &rebuilt);
                    tokens = (estimate as f64 * ratio).ceil() as u64;
                    let mut inner = self.inner.lock().unwrap();
                    inner.last_estimate = estimate;
                    inner.last_sent = (rebuilt.len(), state.system.len(), specs.len());
                    inner.reported = None;
                    state.messages = rebuilt;
                }
                Ok(false) => {}
                Err(error) => log::warn!("[chat {}] freeing context failed: {error}", self.id),
            }
        }
        {
            let mut inner = self.inner.lock().unwrap();
            inner.context = Some(ContextUsage {
                tokens: tokens as f64,
                context_window: budget.context_window as f64,
                input_limit: budget.input_limit as f64,
                output_tokens: budget.output_tokens as f64,
                compact_at: budget.compact_at as f64,
                kind: "estimated".into(),
            });
            inner.last_request = Some((now_ms(), key));
        }
        self.publish_state();
    }

    /// How this chat compacts a span: the `contextStrategy` preference.
    fn strategy(&self) -> Strategy {
        Strategy::from_pref(self.services.settings.prefs().get("contextStrategy").and_then(Value::as_str))
    }

    /// Free context once the chat nears its window (`tokens` expected): clear old tool outputs, oldest first, down to
    /// half the compaction line, so the next steps do not clear again at once; when clearing cannot bring it well
    /// below that line, compact a span instead, which covers those outputs too. Whether the conversation the model
    /// sees changed.
    async fn free_context(&self, resolved: &Resolved, budget: &Budget, tokens: u64, trigger: &str, cancel: Option<CancellationToken>, prefix: Option<&[Message]>) -> HostResult<bool> {
        let model_key = format!("{}/{}", resolved.model.provider, resolved.model.id);
        let (ids, freed) = {
            let inner = self.inner.lock().unwrap();
            let ratio = inner.calibration.factor_for(&model_key);
            let need = (tokens.saturating_sub(budget.compact_at / 2) as f64 / ratio).ceil() as u64;
            let (ids, freed) = compaction::select_clearable(&build_context(&inner.entries), &cleared_ids(&inner.entries), need);
            (ids, (freed as f64 * ratio) as u64)
        };
        let clear = |ids: Vec<String>, freed_tokens: u64| TranscriptEntry::Cleared {
            id: uuid::Uuid::new_v4().to_string(),
            ts: now_ms() as f64,
            entry_ids: ids,
            tokens_before: tokens as f64,
            tokens_after: tokens.saturating_sub(freed_tokens) as f64,
            trigger: trigger.to_owned(),
        };
        if !ids.is_empty() && tokens.saturating_sub(freed) <= budget.compact_at * 4 / 5 {
            self.append(clear(ids, freed)).await;
            return Ok(true);
        }
        let compacted = self.compact(resolved, trigger, cancel, prefix).await;
        if matches!(compacted, Ok(Some(_))) {
            return Ok(true);
        }
        // Clearing still helps when no span could be compacted, or the summary request failed.
        if !ids.is_empty() {
            if let Err(error) = compacted {
                log::warn!("[chat {}] compaction failed, clearing old tool outputs only: {error}", self.id);
            }
            self.append(clear(ids, freed)).await;
            return Ok(true);
        }
        // Last-resort overflow fallback: clear oversized outputs even from the latest step (still recoverable via `recall`).
        if trigger == "overflow" {
            let (emergency_ids, emergency_freed) = {
                let inner = self.inner.lock().unwrap();
                let ratio = inner.calibration.factor_for(&model_key);
                let need = (tokens.saturating_sub(budget.compact_at / 2) as f64 / ratio).ceil() as u64;
                let (ids, freed) = compaction::select_clearable_overflow(&build_context(&inner.entries), &cleared_ids(&inner.entries), need);
                (ids, (freed as f64 * ratio) as u64)
            };
            if !emergency_ids.is_empty() {
                self.append(clear(emergency_ids, emergency_freed)).await;
                return Ok(true);
            }
        }
        compacted.map(|_| false)
    }

    /// Compact the chat's context with `resolved`; `prefix` is what the next request would send when a run holds it.
    /// The appended compaction entry, or `None` when nothing was worth compacting.
    async fn compact(&self, resolved: &Resolved, trigger: &str, cancel: Option<CancellationToken>, prefix: Option<&[Message]>) -> HostResult<Option<TranscriptEntry>> {
        let previous = self.inner.lock().unwrap().status;
        if previous != "compacting" {
            self.set_status("compacting");
        }
        let tools = self.tools();
        let system = self.system_prompt(&tools);
        let specs: Vec<llm::ToolSpec> = tools.iter().map(|tool| tool.spec().clone()).collect();
        let strategy = self.strategy();
        let model_key = format!("{}/{}", resolved.model.provider, resolved.model.id);
        let (items, entries, latest_user, share_cache, ratio) = {
            let inner = self.inner.lock().unwrap();
            let latest_user = inner.entries.iter().rev().find_map(|entry| if let TranscriptEntry::User { id, .. } = entry { Some(id.clone()) } else { None });
            let warm = inner.last_request.as_ref().is_some_and(|(at, key)| *key == request_key(&resolved.model, resolved.thinking) && now_ms().saturating_sub(*at) < SHARED_CACHE_WINDOW_MS);
            // A recall index is built from the entries themselves.
            let entries = if strategy == Strategy::Recall { inner.entries.clone() } else { Vec::new() };
            (build_context(&inner.entries), entries, latest_user, warm, inner.calibration.factor_for(&model_key))
        };
        let rebuilt: Vec<Message>;
        let prefix = match prefix {
            Some(prefix) => prefix,
            None => {
                rebuilt = items.iter().map(|item| item.message.clone()).collect();
                &rebuilt
            }
        };
        let result = compaction::compact(CompactionRequest {
            models: &self.services.models,
            resolved,
            system: &system,
            tools: &specs,
            items: &items,
            prefix,
            share_cache,
            session_id: &self.id,
            estimate_ratio: ratio,
            latest_user: latest_user.as_deref(),
            trigger,
            cancel: cancel.unwrap_or_default(),
            strategy,
            entries: &entries,
        })
        .await;
        if let Ok(Some(entry)) = &result {
            self.append(entry.clone()).await;
            if let TranscriptEntry::Compaction { request: Some(request), .. } = entry
                && let Ok(usage) = serde_json::from_value::<UsageSummary>(request["usage"].clone())
            {
                self.add_run_usage(&usage);
            }
        }
        if previous != "compacting" {
            self.set_status(previous);
        }
        result
    }

    pub(super) fn add_run_usage(&self, usage: &UsageSummary) {
        if let Some(run) = &mut self.inner.lock().unwrap().run {
            run.usage = add_usage(&run.usage, usage);
        }
    }
}

struct RunHooks {
    session: Arc<Session>,
    mcp_tools: HashSet<String>,
    resolved: Resolved,
}

impl Hooks for RunHooks {
    type Meta = Meta;

    fn steering(&self) -> Vec<(Message, Meta)> {
        std::mem::take(&mut self.session.inner.lock().unwrap().steering)
    }

    async fn event(&self, event: Event<'_, Meta>) {
        self.session.on_event(event).await;
    }

    fn streaming(&self, event: llm::Event, reply: &AssistantMessage) {
        self.session.on_stream(event, reply);
    }

    async fn before_tool(&self, id: &str, name: &str, args: &Value, cancel: &CancellationToken) -> Option<Blocked> {
        self.session.before_tool(&self.mcp_tools, id, name, args, cancel).await
    }

    async fn after_tool(&self, id: &str, name: &str, args: &Value, output: &mut ToolOutput) {
        self.session.after_tool(id, name, args, output).await;
    }

    fn finish_turn(&self) -> bool {
        let mut inner = self.session.inner.lock().unwrap();
        if !inner.stop_after_step {
            return false;
        }
        inner.stop_after_step = false;
        // A message the user steered in still gets its answer.
        inner.steering.is_empty()
    }

    async fn prepare_request(&self, state: &mut State, cancel: &CancellationToken) {
        self.session.prepare_request(&self.resolved, state, cancel).await;
    }
}
