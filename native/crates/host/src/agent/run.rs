//! The agent loop (what pi-agent-core's `runLoop` did for 3.x): ask the model, run the tool calls of its reply, add
//! the messages the user steered in, and ask again until the model answers without calling a tool.

use std::future::Future;
use std::sync::Arc;

use futures::future::join_all;
use rainy_protocol::chat::{AssistantBlock, AssistantMessage, ResultContent};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use super::llm::{self, Level, Llm, Message, Model, Request, ToolResult};
use super::tools::{Tool, ToolOutput, Updates, schema};

/// What one run works with; the hooks may replace any of it before a request.
pub struct State {
    pub model: Model,
    pub reasoning: Option<Level>,
    pub system: String,
    pub tools: Vec<Arc<dyn Tool>>,
    pub messages: Vec<Message>,
    pub session_id: Option<String>,
    pub api_key: Option<String>,
}

impl State {
    pub fn specs(&self) -> Vec<llm::ToolSpec> {
        self.tools.iter().map(|tool| tool.spec().clone()).collect()
    }
}

/// A tool call the hooks refused, with the text the model reads; `terminate` ends the run after this step.
pub struct Blocked {
    pub reason: String,
    pub terminate: bool,
}

/// What happened, for the hooks that follow a run.
pub enum Event<'a, M> {
    /// A message the user (or the chat) added; `meta` is what the hooks gave with it.
    Prompt(&'a Message, &'a M),
    /// The reply started streaming.
    ReplyStarted,
    /// The reply is done (or failed).
    Reply(&'a AssistantMessage),
    ToolStarted { id: &'a str, name: &'a str, args: &'a Map<String, Value> },
    ToolProgress { id: &'a str, name: &'a str, output: &'a ToolOutput },
    ToolEnded { id: &'a str, name: &'a str },
    /// A tool's answer, as the model will read it.
    Result(&'a ToolResult),
    /// A step (one reply and its tool calls) is complete.
    TurnEnded,
}

/// What a run asks of the chat that runs it.
pub trait Hooks: Send + Sync {
    /// What travels with a prompt message, given back with its [`Event::Prompt`].
    type Meta: Send + Sync;

    /// Messages the user sent while the run was busy; they are added before the next request.
    fn steering(&self) -> Vec<(Message, Self::Meta)>;

    fn event(&self, event: Event<'_, Self::Meta>) -> impl Future<Output = ()> + Send;

    /// The reply growing; called often, so it must not wait.
    fn streaming(&self, event: llm::Event, reply: &AssistantMessage);

    /// Refuse a call, or let it run (`None`).
    fn before_tool(&self, id: &str, name: &str, args: &Value, cancel: &CancellationToken) -> impl Future<Output = Option<Blocked>> + Send;

    /// After a step: whether the run ends now.
    fn finish_turn(&self) -> bool {
        false
    }

    /// Before each request: the hooks may compact the conversation or change the model.
    fn prepare_request(&self, state: &mut State, cancel: &CancellationToken) -> impl Future<Output = ()> + Send;
}

fn failed(text: impl Into<String>) -> ToolOutput {
    ToolOutput::error(text)
}

/// One call of a reply, checked and ready, or already answered.
enum Prepared {
    Run { id: String, name: String, tool: Arc<dyn Tool>, args: Value },
    Done { id: String, name: String, output: ToolOutput },
}

async fn prepare<H: Hooks>(state: &State, hooks: &H, id: &str, name: &str, arguments: &Map<String, Value>, cancel: &CancellationToken) -> Prepared {
    let done = |output: ToolOutput| Prepared::Done { id: id.to_owned(), name: name.to_owned(), output };
    let Some(tool) = state.tools.iter().find(|tool| tool.spec().name == name).cloned() else { return done(failed(format!("Tool {name} not found"))) };
    let args = match schema::validate(name, &tool.spec().parameters, arguments) {
        Ok(args) => args,
        Err(message) => return done(failed(message)),
    };
    let blocked = hooks.before_tool(id, name, &args, cancel).await;
    if cancel.is_cancelled() {
        return done(failed("Operation aborted"));
    }
    if let Some(blocked) = blocked {
        let mut output = failed(if blocked.reason.is_empty() { "Tool execution was blocked".to_owned() } else { blocked.reason });
        output.terminate = blocked.terminate;
        return done(output);
    }
    Prepared::Run { id: id.to_owned(), name: name.to_owned(), tool, args }
}

async fn execute<H: Hooks>(hooks: &H, id: &str, name: &str, tool: &Arc<dyn Tool>, args: Value, cancel: &CancellationToken) -> ToolOutput {
    if cancel.is_cancelled() {
        return failed("Operation aborted");
    }
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<ToolOutput>();
    let updates: Updates = Arc::new(move |output| {
        let _ = sender.send(output);
    });
    let work = tool.execute(id.to_owned(), args, cancel.child_token(), updates);
    tokio::pin!(work);
    let result = loop {
        tokio::select! {
            result = &mut work => break result,
            Some(output) = receiver.recv() => hooks.event(Event::ToolProgress { id, name, output: &output }).await,
        }
    };
    match result {
        Ok(output) => output,
        Err(message) => failed(message),
    }
}

fn result_of(id: &str, name: &str, output: ToolOutput) -> (ToolResult, bool) {
    let content = if output.content.is_empty() { Vec::new() } else { output.content };
    (ToolResult { tool_call_id: id.to_owned(), tool_name: name.to_owned(), content, is_error: output.is_error, details: output.details, timestamp: crate::text::now_ms() as f64 }, output.terminate)
}

/// Run the calls of one reply; their answers, in order, and whether every one asked to end the run.
async fn run_tools<H: Hooks>(state: &State, hooks: &H, reply: &AssistantMessage, cancel: &CancellationToken) -> (Vec<ToolResult>, bool) {
    let calls: Vec<(&str, &str, &Map<String, Value>)> =
        reply.content.iter().filter_map(|block| if let AssistantBlock::ToolCall { id, name, arguments, .. } = block { Some((id.as_str(), name.as_str(), arguments)) } else { None }).collect();
    let mut results: Vec<(ToolResult, bool)> = Vec::new();
    // A reply cut at the output limit may carry cut arguments; none of its calls is run.
    if reply.stop_reason == "length" {
        for (id, name, args) in calls {
            hooks.event(Event::ToolStarted { id, name, args }).await;
            let output = failed(format!("Tool call \"{name}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments."));
            hooks.event(Event::ToolEnded { id, name }).await;
            results.push(result_of(id, name, output));
        }
        let terminate = false;
        let results: Vec<ToolResult> = results.into_iter().map(|(result, _)| result).collect();
        for result in &results {
            hooks.event(Event::Result(result)).await;
        }
        return (results, terminate);
    }
    let sequential = calls.iter().any(|(_, name, _)| state.tools.iter().any(|tool| tool.spec().name == *name && tool.sequential()));
    if sequential {
        for (id, name, args) in calls {
            hooks.event(Event::ToolStarted { id, name, args }).await;
            let output = match prepare(state, hooks, id, name, args, cancel).await {
                Prepared::Done { output, .. } => output,
                Prepared::Run { tool, args, .. } => execute(hooks, id, name, &tool, args, cancel).await,
            };
            hooks.event(Event::ToolEnded { id, name }).await;
            let (result, terminate) = result_of(id, name, output);
            hooks.event(Event::Result(&result)).await;
            results.push((result, terminate));
            if cancel.is_cancelled() {
                break;
            }
        }
    } else {
        let mut prepared = Vec::new();
        for (id, name, args) in calls {
            hooks.event(Event::ToolStarted { id, name, args }).await;
            let call = prepare(state, hooks, id, name, args, cancel).await;
            if let Prepared::Done { .. } = &call {
                hooks.event(Event::ToolEnded { id, name }).await;
            }
            prepared.push(call);
            if cancel.is_cancelled() {
                break;
            }
        }
        let outputs = join_all(prepared.into_iter().map(|call| async move {
            match call {
                Prepared::Done { id, name, output } => (id, name, output),
                Prepared::Run { id, name, tool, args } => {
                    let output = execute(hooks, &id, &name, &tool, args, cancel).await;
                    hooks.event(Event::ToolEnded { id: &id, name: &name }).await;
                    (id, name, output)
                }
            }
        }))
        .await;
        for (id, name, output) in outputs {
            let (result, terminate) = result_of(&id, &name, output);
            hooks.event(Event::Result(&result)).await;
            results.push((result, terminate));
        }
    }
    let terminate = !results.is_empty() && results.iter().all(|(_, terminate)| *terminate);
    (results.into_iter().map(|(result, _)| result).collect(), terminate)
}

/// Ask the model and run its tool calls until it answers without one, the run is stopped, or the hooks end it.
/// `prompts` are added first; without any, the model answers the conversation as it stands.
pub async fn run<H: Hooks>(llm: &Llm, state: &mut State, prompts: Vec<(Message, H::Meta)>, hooks: &H, cancel: &CancellationToken) {
    let mut pending = prompts;
    if pending.is_empty() {
        pending = hooks.steering();
    }
    let mut more_calls = true;
    while more_calls || !pending.is_empty() {
        for (message, meta) in pending.drain(..) {
            hooks.event(Event::Prompt(&message, &meta)).await;
            state.messages.push(message);
        }
        hooks.prepare_request(state, cancel).await;
        let specs = state.specs();
        let request = Request { reasoning: state.reasoning, session_id: state.session_id.as_deref(), ..Request::new(&state.system, &state.messages, &specs) };
        hooks.event(Event::ReplyStarted).await;
        let mut stream = |event: llm::Event, reply: &AssistantMessage| hooks.streaming(event, reply);
        let reply = llm.stream(&state.model, &request, state.api_key.clone(), cancel, &mut stream).await;
        hooks.event(Event::Reply(&reply)).await;
        state.messages.push(Message::Assistant(reply.clone()));
        if reply.stop_reason == "error" || reply.stop_reason == "aborted" {
            hooks.finish_turn();
            hooks.event(Event::TurnEnded).await;
            return;
        }
        more_calls = false;
        if reply.content.iter().any(|block| matches!(block, AssistantBlock::ToolCall { .. })) {
            let (results, terminate) = run_tools(state, hooks, &reply, cancel).await;
            more_calls = !terminate;
            state.messages.extend(results.into_iter().map(Message::ToolResult));
        }
        let end = hooks.finish_turn();
        hooks.event(Event::TurnEnded).await;
        // A stopped run asks once more and records the stopped reply, as 3.x did.
        if end {
            return;
        }
        pending = hooks.steering();
    }
}

/// Text of a tool result, for progress cards.
pub fn result_text(content: &[ResultContent]) -> String {
    llm::text_parts(content)
}
