//! The agent loop (what pi-agent-core's `runLoop` did for 3.x): ask the model, run the tool calls of its reply, add
//! the messages the user steered in, and ask again until the model answers without calling a tool.

use std::future::Future;
use std::sync::Arc;

use futures::future::join_all;
use rainy_protocol::chat::{AssistantBlock, AssistantMessage};
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
    /// The conversation's Claude message thread, kept across runs.
    pub thread: Option<Arc<llm::thread::Slot>>,
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

    /// After a tool finishes executing (before its result reaches the model): hooks may run post-edit actions or
    /// append LSP error diagnostics.
    fn after_tool(&self, _id: &str, _name: &str, _args: &Value, _output: &mut ToolOutput) -> impl Future<Output = ()> + Send {
        async {}
    }

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
    // A name that differs only in letter case is the tool meant; any other unknown name gets the list to choose from.
    let tool = state.tools.iter().find(|tool| tool.spec().name == name).or_else(|| {
        let mut matches = state.tools.iter().filter(|tool| tool.spec().name.eq_ignore_ascii_case(name));
        matches.next().filter(|_| matches.next().is_none())
    });
    let Some(tool) = tool.cloned() else {
        let names: Vec<&str> = state.tools.iter().map(|tool| tool.spec().name.as_str()).collect();
        return done(failed(format!("Tool {name} not found. Available tools: {}", names.join(", "))));
    };
    let name = tool.spec().name.clone();
    let name = name.as_str();
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
    let work = tool.execute(id.to_owned(), args.clone(), cancel.child_token(), updates);
    tokio::pin!(work);
    let result = loop {
        tokio::select! {
            result = &mut work => break result,
            Some(output) = receiver.recv() => hooks.event(Event::ToolProgress { id, name, output: &output }).await,
        }
    };
    let mut output = match result {
        Ok(output) => output,
        Err(message) => failed(message),
    };
    if !cancel.is_cancelled() {
        hooks.after_tool(id, name, &args, &mut output).await;
    }
    output
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
    let sequential = calls.iter().any(|(_, name, _)| state.tools.iter().any(|tool| tool.spec().name.eq_ignore_ascii_case(name) && tool.sequential()));
    if sequential {
        let mut iter = calls.into_iter();
        while let Some((id, name, args)) = iter.next() {
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
                for (skipped_id, skipped_name, _) in iter {
                    let (result, _) = result_of(skipped_id, skipped_name, failed("Tool call aborted."));
                    hooks.event(Event::Result(&result)).await;
                    results.push((result, false));
                }
                break;
            }
        }
    } else {
        let mut prepared = Vec::new();
        let mut skipped = Vec::new();
        let mut iter = calls.into_iter();
        while let Some((id, name, args)) = iter.next() {
            hooks.event(Event::ToolStarted { id, name, args }).await;
            let call = prepare(state, hooks, id, name, args, cancel).await;
            if let Prepared::Done { .. } = &call {
                hooks.event(Event::ToolEnded { id, name }).await;
            }
            prepared.push(call);
            if cancel.is_cancelled() {
                skipped.extend(iter.map(|(skipped_id, skipped_name, _)| (skipped_id.to_owned(), skipped_name.to_owned())));
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
        for (skipped_id, skipped_name) in skipped {
            let (result, _) = result_of(&skipped_id, &skipped_name, failed("Tool call aborted."));
            hooks.event(Event::Result(&result)).await;
            results.push((result, false));
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
        let request = Request { reasoning: state.reasoning, session_id: state.session_id.as_deref(), thread: state.thread.as_deref(), ..Request::new(&state.system, &state.messages, &specs) };
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::future::BoxFuture;
    use serde_json::json;

    use super::*;
    use crate::agent::llm::{Api, Model, ToolSpec};
    use crate::settings::ModelSetup;

    struct CancelAfterFirst {
        spec: ToolSpec,
        cancel: CancellationToken,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl Tool for CancelAfterFirst {
        fn spec(&self) -> &ToolSpec {
            &self.spec
        }

        fn execute(&self, call_id: String, _args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
            let cancel = self.cancel.clone();
            let active = self.active.clone();
            let max_active = self.max_active.clone();
            Box::pin(async move {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(15)).await;
                if call_id == "c1" {
                    cancel.cancel();
                }
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(ToolOutput::text("ok", json!({})))
            })
        }
    }

    struct RecordingHooks {
        results: Mutex<Vec<(String, bool)>>,
    }

    impl Hooks for RecordingHooks {
        type Meta = ();
        fn steering(&self) -> Vec<(Message, ())> {
            Vec::new()
        }
        async fn event(&self, event: Event<'_, ()>) {
            if let Event::Result(result) = event {
                self.results.lock().unwrap().push((result.tool_call_id.clone(), result.is_error));
            }
        }
        fn streaming(&self, _event: llm::Event, _reply: &AssistantMessage) {}
        async fn before_tool(&self, _id: &str, _name: &str, _args: &Value, _cancel: &CancellationToken) -> Option<Blocked> {
            None
        }
        async fn prepare_request(&self, _state: &mut State, _cancel: &CancellationToken) {}
    }

    #[tokio::test]
    async fn run_tools_matches_sequential_case_insensitively_and_completes_aborted_calls() {
        let cancel = CancellationToken::new();
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tool = Arc::new(CancelAfterFirst {
            spec: ToolSpec { name: "edit".into(), description: String::new(), parameters: json!({ "type": "object", "properties": {} }), freeform: None },
            cancel: cancel.clone(),
            active,
            max_active: max_active.clone(),
        });
        let setup = ModelSetup {
            provider: "p".into(),
            base_url: "http://127.0.0.1:1".into(),
            model: "m".into(),
            context_window: 16_000,
            max_tokens: None,
            local: true,
            api: Some(Api::Completions.as_str().into()),
            thinking: None,
            thinking_format: None,
            max_tokens_field: None,
            images: None,
        };
        let state = State {
            model: Model::from_setup(&setup, 4096),
            reasoning: None,
            system: String::new(),
            tools: vec![tool],
            messages: Vec::new(),
            session_id: None,
            api_key: None,
            thread: None,
        };
        // Capitalized "Edit" must still run sequentially and fill aborted ToolResult for c2.
        let reply: AssistantMessage = serde_json::from_value(json!({
            "role": "assistant",
            "content": [
                { "type": "toolCall", "id": "c1", "name": "Edit", "arguments": {} },
                { "type": "toolCall", "id": "c2", "name": "Edit", "arguments": {} }
            ],
            "api": "openai-completions", "provider": "p", "model": "m", "stopReason": "toolUse", "timestamp": 1, "usage": {}
        }))
        .unwrap();
        let hooks = RecordingHooks { results: Mutex::default() };
        let (results, _) = run_tools(&state, &hooks, &reply, &cancel).await;
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].tool_call_id, "c1");
        assert!(!results[0].is_error);
        assert_eq!(results[1].tool_call_id, "c2");
        assert!(results[1].is_error);
        assert_eq!(hooks.results.lock().unwrap().len(), 2);
    }
}
