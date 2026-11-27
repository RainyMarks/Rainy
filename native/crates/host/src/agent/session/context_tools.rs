//! Context breakdown, instruction/memory injection, tool assembly, system prompt building, and recall/sub-agent bridges.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use futures::future::BoxFuture;
use rainy_protocol::chat::{ContextBreakdown, ContextBreakdownSegments, TranscriptEntry};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::super::budget::{Budget, estimate_breakdown, resolve_budget};
use super::super::changes;
use super::super::context::build_context;
use super::super::instructions::{load_baseline, loaded_from, render_additional, render_baseline};
use super::super::llm::{self, Message};
use super::super::models::Resolved;
use super::super::orchestrate::{self, Phase};
use super::super::permissions;
use super::super::profile::Profile;
use super::super::prompt::{self, PromptInputs};
use super::super::skills;
use super::super::subagent;
use super::super::tools::browser::Browser;
use super::super::tools::recall::{History, Recall};
use super::super::tools::task::{Progress, Report, Subagents};
use super::super::tools::web::{WebAccess, deepseek_key};
use super::super::tools::{RecordChange, Tool, ToolContext, ToolStyle, core_tools, subagent_tools};
use super::messaging::context_message;
use super::{Meta, Session, no_model, tool_style};
use crate::error::{HostError, HostResult};
use crate::locale::l;

impl Session {
    /// Detailed per-segment token breakdown for `session.contextBreakdown` / `usage_ring`.
    pub fn context_breakdown(&self) -> HostResult<ContextBreakdown> {
        if self.chat().external_backend.is_some() {
            return Err(HostError::new(
                "unsupported",
                l("外部 Agent 对话的上下文由外部 CLI 自行管理，暂不提供分段统计。", "Context for external agent chats is managed by the external CLI; per-segment breakdown is unavailable."),
            ));
        }
        let resolved = self.request_resolved().or_else(|| self.resolved()).ok_or_else(no_model)?;
        let budget = resolve_budget(resolved.setup.context_window, resolved.setup.max_tokens)?;
        let tools = self.tools();
        let specs: Vec<llm::ToolSpec> = tools.iter().map(|tool| tool.spec().clone()).collect();
        let system = self.system_prompt(&tools);
        let model_key = format!("{}/{}", resolved.model.provider, resolved.model.id);
        let (items, entries, ratio, reported_tokens, kind) = {
            let inner = self.inner.lock().unwrap();
            let items = build_context(&inner.entries);
            let ratio = inner.calibration.factor_for(&model_key);
            let reported = inner.context.as_ref().filter(|c| c.kind == "exact").map(|c| c.tokens.max(0.0) as u64);
            let kind = if reported.is_some() { "reported".to_owned() } else { "estimated".to_owned() };
            (items, inner.entries.clone(), ratio, reported, kind)
        };
        let seg = estimate_breakdown(&system, &specs, &items, &entries);
        let total = seg.total();
        let calibrated_total = reported_tokens.unwrap_or_else(|| (total as f64 * ratio).ceil() as u64);
        Ok(ContextBreakdown {
            session_id: self.id.clone(),
            context_window: budget.context_window,
            input_limit: budget.input_limit,
            compact_at: budget.compact_at,
            output_tokens: budget.output_tokens,
            total,
            calibrated_total,
            calibration_factor: ratio,
            kind,
            segments: ContextBreakdownSegments {
                system: seg.system,
                instructions: seg.instructions,
                mcp_tools: seg.mcp_tools,
                core_tools: seg.core_tools,
                history: seg.history,
                recall: seg.recall,
            },
        })
    }

    pub(super) async fn first_request_context(&self, resolved: &Resolved) -> HostResult<Vec<(Message, Meta)>> {
        let (has_instructions, has_memory) = {
            let inner = self.inner.lock().unwrap();
            let has = |wanted: &str| inner.entries.iter().any(|entry| matches!(entry, TranscriptEntry::Context { label, .. } if label == wanted));
            (has("instructions"), has("memory"))
        };
        let chat = self.chat();
        let mut messages = Vec::new();
        if !has_instructions {
            let home = self.services.env.home.clone();
            let cwd = PathBuf::from(&chat.cwd);
            let files = tokio::task::spawn_blocking(move || load_baseline(&home, &cwd))
                .await
                .map_err(|error| HostError::new("instructions", error.to_string()))?
                .map_err(|error| HostError::new("instructions", error))?;
            if let Some(text) = render_baseline(&files) {
                messages.push(context_message("instructions", text));
            }
        }
        if !has_memory {
            let budget = resolve_budget(resolved.setup.context_window, resolved.setup.max_tokens)?;
            match self.services.memory.recall(chat.workspace_id.as_deref(), budget.input_limit).await {
                Ok(Some(text)) => messages.push(context_message("memory", text)),
                Ok(None) => {}
                Err(error) => log::warn!("[memory] recall failed: {error}"),
            }
        }
        Ok(messages)
    }

    /// Instructions of folders the last step touched that the chat has not loaded, added before the next request.
    /// Reads files off-lock on a blocking thread so `self.inner` is never held across disk I/O.
    pub(super) async fn inject_touched_instructions(&self) {
        let (touched, loaded, cwd) = {
            let mut inner = self.inner.lock().unwrap();
            if inner.touched.is_empty() || inner.run.is_none() {
                return;
            }
            let touched = std::mem::take(&mut inner.touched);
            let loaded = loaded_from(inner.entries.iter().filter_map(|entry| match entry {
                TranscriptEntry::Context { label, text, .. } if label == "instructions" => Some(text.as_str()),
                _ => None,
            }));
            (touched, loaded, PathBuf::from(&inner.chat.cwd))
        };
        let extra = tokio::task::spawn_blocking(move || {
            let mut loaded = loaded;
            let mut seen = HashSet::new();
            let mut rendered = Vec::new();
            for path in touched {
                if seen.insert(path.clone())
                    && let Some(text) = render_additional(&cwd, &path, &mut loaded)
                {
                    rendered.push(text);
                }
            }
            rendered
        })
        .await
        .unwrap_or_default();
        if extra.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.run.is_some() {
            for text in extra {
                inner.steering.push(context_message("instructions", text));
            }
        }
    }

    pub(super) fn tool_context(&self) -> ToolContext {
        let env = &self.services.env;
        let chat = self.chat();
        let session = self.this.clone();
        let tool_tokens = move || {
            session.upgrade().and_then(|session| session.resolved()).and_then(|resolved| resolve_budget(resolved.setup.context_window, resolved.setup.max_tokens).ok()).map_or(4000, |budget: Budget| budget.tool_tokens)
        };
        let session = self.this.clone();
        let on_file_touched = move |path: PathBuf| {
            if let Some(session) = session.upgrade() {
                session.inner.lock().unwrap().touched.push(path);
            }
        };
        let (snapshots, id) = (self.services.snapshots.clone(), self.id.clone());
        let record_change: RecordChange = Arc::new(move |call_id: String, path: PathBuf, before: Option<Vec<u8>>, after: Option<Vec<u8>>| {
            let (snapshots, id) = (snapshots.clone(), id.clone());
            Box::pin(async move {
                match snapshots.record(&id, &call_id, &path, before.as_deref(), after.as_deref()) {
                    Ok(()) => true,
                    Err(error) => {
                        log::warn!("[chat {id}] keeping {} for undo failed: {error}", path.display());
                        false
                    }
                }
            }) as BoxFuture<'static, bool>
        });
        let deny_paths: Option<Arc<dyn Fn(&Path) -> bool + Send + Sync>> = chat.workspace_id.clone().map(|workspace| {
            let settings = self.services.settings.clone();
            let cwd = PathBuf::from(&chat.cwd);
            let windows = env.windows;
            Arc::new(move |path: &Path| {
                let rules = settings.permission_rules(&workspace);
                !rules.deny_paths.is_empty() && permissions::path_denied(path, &cwd, &rules.deny_paths, windows).is_some()
            }) as Arc<dyn Fn(&Path) -> bool + Send + Sync>
        });
        ToolContext {
            cwd: PathBuf::from(chat.cwd),
            workspace_id: chat.workspace_id,
            session_id: self.id.clone(),
            runtime: self.services.runtime.clone(),
            spill_dir: env.home.join("spill").join(&self.id),
            tool_tokens: Arc::new(tool_tokens),
            observations: self.observations.clone(),
            pwsh_path: env.pwsh_path.clone(),
            on_file_touched: Some(Arc::new(on_file_touched)),
            record_change: Some(record_change),
            jobs: Some(self.jobs.clone()),
            languages: self.services.languages.clone(),
            maps: Some(self.services.maps.clone()),
            deny_paths,
        }
    }

    pub(super) fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let context = self.tool_context();
        let settings = self.services.settings.clone();
        let web = (self.services.settings.prefs().get("webSearch").and_then(Value::as_bool) == Some(true) && deepseek_key(&settings).is_some())
            .then(|| WebAccess { api_key: Arc::new(move || deepseek_key(&settings)), network: self.services.network.clone() });
        let subagents: Arc<dyn Subagents> = Arc::new(ChatSubagents { session: self.this.clone(), context: context.clone(), web: web.clone() });
        let resolved = self.request_resolved();
        let style = resolved.as_ref().map_or_else(ToolStyle::default, |resolved| tool_style(&resolved.model));
        let mut tools = core_tools(&context, self.services.env.windows, web, Some(subagents), style);
        tools.push(Arc::new(Recall::new(context.clone(), Arc::new(ChatHistory(self.this.clone())))));
        if let Some(executor) = self.executor() {
            let session = self.this.clone();
            let switch: orchestrate::Switch = Arc::new(move |phase, text| {
                if let Some(session) = session.upgrade() {
                    session.switch_phase(phase, text);
                }
            });
            let planner = self.selection().map(|selection| selection.model).unwrap_or_default();
            match self.inner.lock().unwrap().phase {
                Phase::Plan => tools.push(Arc::new(orchestrate::HandOff::new(&executor.model, switch))),
                Phase::Execute => tools.push(Arc::new(orchestrate::Replan::new(&planner, switch))),
            }
        }
        if let Some(preview) = &self.services.preview {
            let vision = resolved.as_ref().is_some_and(|resolved| resolved.model.images);
            tools.extend(Browser::tools(&context, preview.clone(), vision));
        }
        let limiter_ctx = context.clone();
        let mcp_limiter: super::super::mcp::OutputLimiter = Arc::new(move |tool, text| limiter_ctx.limit_output(tool, text));
        tools.extend(self.services.mcp.tools(Some(mcp_limiter)));
        if let Some(link) = self.chat().swarm {
            // The coordinator splits and judges the work; changing files is the workers' part.
            if link.coordinator == self.id {
                tools.retain(|tool| !changes::CHANGE_TOOLS.contains(&tool.spec().name.as_str()));
            }
            tools.extend(super::super::swarm::tools(&self.services.swarm, &link, &self.id));
        }
        tools.sort_by(|left, right| left.spec().name.cmp(&right.spec().name));
        tools
    }

    pub(super) fn system_prompt(&self, tools: &[Arc<dyn Tool>]) -> String {
        let chat = self.chat();
        let env = &self.services.env;
        let project = chat.workspace_id.as_deref().and_then(|workspace| self.services.projects.get(workspace));
        let extra_roots: Vec<String> = project.map(|project| project.roots().into_iter().filter(|root| !root.primary).map(|root| root.path).collect()).unwrap_or_default();
        let names: Vec<String> = tools.iter().map(|tool| tool.spec().name.clone()).collect();
        // Project skills load in every workspace like AGENTS.md (text the model may read, fenced as untrusted data);
        // only hooks, which run commands, wait for the workspace to be trusted.
        let skills = skills::section(&skills::discover(Some(Path::new(&chat.cwd))));
        let powershell_core = env.windows.then(|| {
            let component = self.services.runtime.resolve(Path::new(&chat.cwd)).ok().and_then(|resolved| resolved.powershell);
            crate::process::resolve_powershell(env.pwsh_path.as_deref(), component.as_deref()).1
        });
        let orchestration = self.executor().map(|executor| {
            let (phase, note) = {
                let inner = self.inner.lock().unwrap();
                (inner.phase, inner.phase_note.clone())
            };
            let planner = self.selection().map(|selection| selection.model).unwrap_or_default();
            orchestrate::prompt_section(phase, &planner, &executor.model, note.as_deref())
        });
        let profile = self.request_resolved().map_or_else(|| Profile::of(""), |resolved| Profile::of(&resolved.model.id));
        let settings = self.services.settings.get();
        let built = prompt::build(&PromptInputs {
            cwd: &chat.cwd,
            windows: env.windows,
            tools: &names,
            mcp_instructions: &self.services.mcp.instructions(),
            extra_roots: &extra_roots,
            skills: &skills,
            global_prompt: &settings.global_prompt,
            builtin_prompt: settings.builtin_prompts.get(profile.prompt_group()).map_or("", String::as_str),
            remote: env.kind() == "SSH",
            powershell_core,
            profile,
        });
        let swarm = chat.swarm.as_ref().map(|link| super::super::swarm::prompt_section(link, &self.id)).unwrap_or_default();
        built + &orchestration.unwrap_or_default() + &swarm
    }
}

/// The record `recall` reads: clones entries briefly and runs the lookup closure off-lock.
struct ChatHistory(Weak<Session>);

impl History for ChatHistory {
    fn look(&self, look: &mut dyn FnMut(&[TranscriptEntry]) -> String) -> String {
        match self.0.upgrade() {
            Some(session) => {
                let entries = session.inner.lock().unwrap().entries.clone();
                look(&entries)
            }
            None => look(&[]),
        }
    }
}

/// Runs the `task` sub-agents of one chat: the chat's model, read-only tools and its own transcript.
struct ChatSubagents {
    session: Weak<Session>,
    context: ToolContext,
    web: Option<WebAccess>,
}

impl Subagents for ChatSubagents {
    fn run(&self, prompt: String, description: String, model: Option<String>, progress: Arc<dyn Fn(Progress) + Send + Sync>, cancel: CancellationToken) -> BoxFuture<'static, Result<Report, String>> {
        let session = self.session.clone();
        // Its own observations: what a sub-agent read does not let the chat write that file without reading it itself.
        let context = ToolContext { observations: Arc::default(), jobs: None, record_change: None, on_file_touched: None, ..self.context.clone() };
        let web = self.web.clone();
        Box::pin(async move {
            let session = session.upgrade().ok_or("the chat was closed")?;
            let resolved = match model.as_deref() {
                Some(hint) => {
                    let selection = session.services.models.resolve_by_hint(hint).map_err(|error| error.message)?;
                    session.services.models.resolve(Some(&selection)).ok_or("the requested sub-agent model is not configured")?
                }
                None => session.resolved().ok_or("no model is configured for this chat")?,
            };
            log::info!("[chat {}] sub-agent ({}/{}): {description}", session.id, resolved.setup.provider, resolved.setup.model);
            let counted = Arc::downgrade(&session);
            let guarded = Arc::downgrade(&session);
            let input = subagent::Input {
                prompt,
                api_key: session.services.models.api_key(&resolved.setup.provider),
                resolved,
                tools: subagent_tools(&context, web),
                cwd: session.chat().cwd,
                windows: session.services.env.windows,
                progress,
                usage: Arc::new(move |usage| {
                    if let Some(session) = counted.upgrade() {
                        session.add_run_usage(&usage);
                    }
                }),
                max_time: subagent::MAX_TIME,
                guard: Arc::new(move |name: &str, args: &Value| match guarded.upgrade() {
                    Some(session) => session.subagent_refusal(name, args),
                    None => Some("The chat was closed.".into()),
                }),
            };
            subagent::run_subagent(&session.services.models.llm, input, cancel).await
        })
    }
}
