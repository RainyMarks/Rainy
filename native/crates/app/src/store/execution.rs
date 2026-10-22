//! Terminals, runs and debug sessions of the open project (`ide-execution-model.ts`). The Host owns the processes and
//! keeps their output as numbered events; this side reads the events after the cursor whenever an `ide.hint` of kind
//! `execution` arrives. A paused debug session's threads, stack, scopes and variables are read here too.

use std::collections::HashMap;

use gpui_kit::*;
use rainy_protocol::RpcError;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};

/// How a process exited.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProcessExit {
    #[serde(default)]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub signal: Option<String>,
    #[serde(default)]
    pub stopped: bool,
}

/// One terminal as the Host reports it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TerminalInfo {
    pub id: String,
    pub cwd: String,
    /// `starting`, `running`, `stopping`, `exited` or `failed`.
    pub phase: String,
    #[serde(default)]
    pub error: Option<String>,
}

impl TerminalInfo {
    pub fn running(&self) -> bool {
        self.phase == "starting" || self.phase == "running"
    }
}

/// The part of a run's resolved command this side reads.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RunSpec {
    #[serde(default)]
    pub terminal: bool,
}

/// One run of a program.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RunInfo {
    pub id: String,
    pub name: String,
    /// `starting`, `building`, `running`, `stopping`, `exited` or `failed`.
    pub phase: String,
    pub spec: RunSpec,
    #[serde(default)]
    pub exit: Option<ProcessExit>,
    #[serde(default)]
    pub error: Option<String>,
}

impl RunInfo {
    pub fn running(&self) -> bool {
        !matches!(self.phase.as_str(), "exited" | "failed")
    }
}

/// What the debug adapter can do.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DebugCapabilities {
    #[serde(default)]
    pub evaluate: bool,
    #[serde(default)]
    pub pause: bool,
    #[serde(default)]
    pub step_in: bool,
    #[serde(default)]
    pub step_out: bool,
    #[serde(default)]
    pub next: bool,
}

/// One debug session.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DebugInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub root_id: Option<String>,
    /// `starting`, `building`, `initializing`, `running`, `paused`, `stopping`, `terminated` or `failed`.
    pub phase: String,
    #[serde(default)]
    pub thread_id: Option<i64>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub capabilities: Option<DebugCapabilities>,
    #[serde(default)]
    pub exit: Option<ProcessExit>,
    #[serde(default)]
    pub error: Option<String>,
}

impl DebugInfo {
    pub fn running(&self) -> bool {
        !matches!(self.phase.as_str(), "terminated" | "failed")
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct DebugThread {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct DebugFrame {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    pub line: i64,
    pub column: i64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DebugScope {
    pub name: String,
    pub variables_reference: i64,
    #[serde(default)]
    pub expensive: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DebugVariable {
    pub name: String,
    pub value: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    pub variables_reference: i64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DebugEvaluation {
    pub result: String,
    pub variables_reference: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    terminals: Vec<TerminalInfo>,
    #[serde(default)]
    runs: Vec<RunInfo>,
    #[serde(default)]
    debug_sessions: Vec<DebugInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    kind: String,
    #[serde(default)]
    operation_id: Option<String>,
    #[serde(default)]
    stream: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Poll {
    events: Vec<Event>,
    cursor: u64,
    status: Status,
}

/// What kind of process an operation is, for the request that writes to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Terminal,
    Run,
    Debug,
}

impl OperationKind {
    fn ops(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            OperationKind::Terminal => ("terminal.input", "terminal.resize", "terminal.stop", "terminalId"),
            OperationKind::Run => ("run.input", "run.resize", "run.stop", "runId"),
            OperationKind::Debug => ("debug.input", "debug.resize", "debug.stop", "debugId"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ExecutionEvent {
    /// Text a process wrote; `stream` is `terminal` for pseudo-terminals.
    Output { operation: String, stream: String, text: String },
    TerminalsChanged,
    /// A run or debug session started here; the panel shows it.
    Started(String),
    /// The paused debug session's stack, scopes or variables changed.
    DebugChanged,
}

/// A paused debug session's details.
#[derive(Debug, Default, Clone)]
pub struct Pause {
    pub debug_id: String,
    pub threads: Vec<DebugThread>,
    pub thread_id: Option<i64>,
    pub frames: Vec<DebugFrame>,
    pub frame_id: Option<i64>,
    pub scopes: Vec<DebugScope>,
    /// Children by variables reference, for the scopes and the variables opened so far.
    pub variables: HashMap<i64, Vec<DebugVariable>>,
    /// Watch results, or why one failed.
    pub watches: HashMap<String, Result<String, String>>,
}

pub struct Execution {
    store: Entity<Store>,
    workspace_id: Option<String>,
    cursor: u64,
    pub terminals: Vec<TerminalInfo>,
    pub runs: Vec<RunInfo>,
    pub debugs: Vec<DebugInfo>,
    /// The debug session the debug view shows.
    pub debug_id: Option<String>,
    /// What the shown debug session's pause looks like, read once per pause.
    pub pause: Option<Pause>,
    /// Bumped on every pause and every step, so answers about an earlier pause are dropped.
    epoch: u64,
    polling: bool,
    again: bool,
}

impl EventEmitter<ExecutionEvent> for Execution {}

struct GlobalExecution(Entity<Execution>);
impl Global for GlobalExecution {}

fn invalid(error: impl std::fmt::Display) -> RpcError {
    RpcError { code: "invalid".into(), message: error.to_string(), data: None }
}

impl Execution {
    pub fn global(cx: &App) -> Entity<Execution> {
        cx.global::<GlobalExecution>().0.clone()
    }

    pub fn init(store: Entity<Store>, ide: Entity<Ide>, cx: &mut App) -> Entity<Execution> {
        let execution = cx.new(|cx| {
            cx.subscribe(&ide, |this: &mut Execution, ide, event: &IdeEvent, cx| {
                if *event == IdeEvent::WorkspaceChanged {
                    this.workspace_id = ide.read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
                    this.cursor = 0;
                    this.terminals.clear();
                    this.runs.clear();
                    this.debugs.clear();
                    this.debug_id = None;
                    this.pause = None;
                    this.epoch += 1;
                    cx.emit(ExecutionEvent::TerminalsChanged);
                    cx.emit(ExecutionEvent::DebugChanged);
                    this.poll(cx);
                }
            })
            .detach();
            cx.subscribe(&store, |this: &mut Execution, _, event: &StoreEvent, cx| {
                if let StoreEvent::Host { name, data } = event
                    && name == "ide.hint"
                    && data.get("kind").and_then(Value::as_str) == Some("execution")
                    && data.get("workspaceId").and_then(Value::as_str) == this.workspace_id.as_deref()
                {
                    this.poll(cx);
                }
            })
            .detach();
            Execution {
                store,
                workspace_id: None,
                cursor: 0,
                terminals: Vec::new(),
                runs: Vec::new(),
                debugs: Vec::new(),
                debug_id: None,
                pause: None,
                epoch: 0,
                polling: false,
                again: false,
            }
        });
        cx.set_global(GlobalExecution(execution.clone()));
        execution
    }

    fn request(&self, mut params: Value, cx: &App) -> Option<impl Future<Output = Result<Value, RpcError>> + use<>> {
        let workspace = self.workspace_id.clone()?;
        params["workspaceId"] = json!(workspace);
        Some(self.store.read(cx).call("ide", params))
    }

    fn decoded<T: serde::de::DeserializeOwned + Send + 'static>(&self, params: Value, cx: &App) -> Task<Result<T, RpcError>> {
        let Some(call) = self.request(params, cx) else {
            return Task::ready(Err(RpcError { code: "no-workspace".into(), message: String::new(), data: None }));
        };
        cx.background_spawn(async move { serde_json::from_value(call.await?).map_err(invalid) })
    }

    /// The kind of an operation the Host listed.
    pub fn kind_of(&self, id: &str) -> OperationKind {
        if self.runs.iter().any(|run| run.id == id) {
            OperationKind::Run
        } else if self.debugs.iter().any(|debug| debug.id == id) {
            OperationKind::Debug
        } else {
            OperationKind::Terminal
        }
    }

    /// Whether an operation still runs.
    pub fn running(&self, id: &str) -> bool {
        self.terminals.iter().any(|terminal| terminal.id == id && terminal.running())
            || self.runs.iter().any(|run| run.id == id && run.running())
            || self.debugs.iter().any(|debug| debug.id == id && debug.running())
    }

    /// Every operation, in the order the panel shows them: terminals, runs, debug sessions.
    pub fn operations(&self) -> Vec<String> {
        self.terminals
            .iter()
            .map(|terminal| terminal.id.clone())
            .chain(self.runs.iter().map(|run| run.id.clone()))
            .chain(self.debugs.iter().map(|debug| debug.id.clone()))
            .collect()
    }

    pub fn debug(&self) -> Option<&DebugInfo> {
        let id = self.debug_id.as_ref()?;
        self.debugs.iter().find(|debug| debug.id == *id)
    }

    /// Read the events after the cursor; a hint arriving meanwhile reads again afterwards.
    pub fn poll(&mut self, cx: &mut Context<Self>) {
        if self.polling {
            self.again = true;
            return;
        }
        let Some(call) = self.request(json!({ "op": "execution.poll", "cursor": self.cursor }), cx) else { return };
        let workspace = self.workspace_id.clone();
        self.polling = true;
        cx.spawn(async move |this: WeakEntity<Execution>, cx: &mut AsyncApp| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                this.polling = false;
                if this.workspace_id != workspace {
                    return;
                }
                if let Ok(Ok(poll)) = result.map(serde_json::from_value::<Poll>) {
                    this.cursor = poll.cursor;
                    for event in poll.events {
                        if event.kind == "output"
                            && let (Some(operation), Some(stream), Some(text)) = (event.operation_id, event.stream, event.text)
                        {
                            cx.emit(ExecutionEvent::Output { operation, stream, text });
                        }
                    }
                    let listed = poll.status.terminals != this.terminals || poll.status.runs != this.runs || poll.status.debug_sessions != this.debugs;
                    this.terminals = poll.status.terminals;
                    this.runs = poll.status.runs;
                    this.debugs = poll.status.debug_sessions;
                    if listed {
                        cx.emit(ExecutionEvent::TerminalsChanged);
                    }
                    if this.debug_id.as_ref().is_none_or(|id| !this.debugs.iter().any(|debug| debug.id == *id)) {
                        this.debug_id = this.debugs.last().map(|debug| debug.id.clone());
                    }
                    this.update_pause(cx);
                    cx.notify();
                }
                if std::mem::take(&mut this.again) {
                    this.poll(cx);
                }
            });
        })
        .detach();
    }

    /// Start a terminal in the project's primary folder.
    pub fn start_terminal(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) -> Task<Result<String, RpcError>> {
        let start = self.decoded::<TerminalInfo>(json!({ "op": "terminal.start", "cols": cols, "rows": rows }), cx);
        cx.spawn(async move |this: WeakEntity<Execution>, cx: &mut AsyncApp| {
            let terminal = start.await?;
            let id = terminal.id.clone();
            let _ = this.update(cx, |this, cx| {
                this.terminals.retain(|known| known.id != terminal.id);
                this.terminals.push(terminal);
                cx.emit(ExecutionEvent::TerminalsChanged);
                this.poll(cx);
            });
            Ok(id)
        })
    }

    /// Run a program (`run.start`).
    pub fn run(&mut self, configuration: Value, cx: &mut Context<Self>) -> Task<Result<String, RpcError>> {
        let start = self.decoded::<RunInfo>(json!({ "op": "run.start", "configuration": configuration, "cols": 100, "rows": 24 }), cx);
        cx.spawn(async move |this: WeakEntity<Execution>, cx: &mut AsyncApp| {
            let run = start.await?;
            let id = run.id.clone();
            let _ = this.update(cx, |this, cx| {
                this.runs.retain(|known| known.id != run.id);
                this.runs.push(run);
                cx.emit(ExecutionEvent::TerminalsChanged);
                cx.emit(ExecutionEvent::Started(id.clone()));
                this.poll(cx);
            });
            Ok(id)
        })
    }

    /// Debug a program (`debug.start`) with the breakpoints of its folder.
    pub fn start_debug(&mut self, configuration: Value, breakpoints: Value, cx: &mut Context<Self>) -> Task<Result<String, RpcError>> {
        let start = self.decoded::<DebugInfo>(
            json!({ "op": "debug.start", "configuration": configuration, "breakpoints": breakpoints, "cols": 100, "rows": 24 }),
            cx,
        );
        cx.spawn(async move |this: WeakEntity<Execution>, cx: &mut AsyncApp| {
            let debug = start.await?;
            let id = debug.id.clone();
            let _ = this.update(cx, |this, cx| {
                this.debugs.retain(|known| known.id != debug.id);
                this.debugs.push(debug);
                this.debug_id = Some(id.clone());
                this.pause = None;
                this.epoch += 1;
                cx.emit(ExecutionEvent::TerminalsChanged);
                cx.emit(ExecutionEvent::Started(id.clone()));
                cx.emit(ExecutionEvent::DebugChanged);
                this.poll(cx);
            });
            Ok(id)
        })
    }

    fn send(&self, params: Value, cx: &App) {
        if let Some(call) = self.request(params, cx) {
            cx.background_spawn(async move {
                let _ = call.await;
            })
            .detach();
        }
    }

    pub fn input(&self, id: &str, data: &str, cx: &App) {
        let (op, _, _, field) = self.kind_of(id).ops();
        self.send(json!({ "op": op, field: id, "data": data }), cx);
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16, cx: &App) {
        let kind = self.kind_of(id);
        // A run without a pseudo-terminal has no size.
        if kind == OperationKind::Run && self.runs.iter().any(|run| run.id == id && !run.spec.terminal) {
            return;
        }
        let (_, op, _, field) = kind.ops();
        self.send(json!({ "op": op, field: id, "cols": cols, "rows": rows }), cx);
    }

    pub fn stop(&self, id: &str, cx: &App) {
        let (_, _, op, field) = self.kind_of(id).ops();
        self.send(json!({ "op": op, field: id }), cx);
    }

    /// Replace the breakpoints of one file in the running debug session of its folder.
    pub fn set_breakpoints(&self, source: Value, cx: &App) {
        let Some(debug) = self.debug().filter(|debug| debug.running()) else { return };
        let root = source.get("rootId").and_then(Value::as_str).map(str::to_owned);
        if debug.root_id != root {
            return;
        }
        self.send(json!({ "op": "debug.setBreakpoints", "debugId": debug.id, "source": source }), cx);
    }

    /// Continue, pause or step the shown debug session.
    pub fn control(&mut self, action: &str, cx: &mut Context<Self>) {
        let Some(debug) = self.debug().cloned() else { return };
        let thread = self.pause.as_ref().and_then(|pause| pause.thread_id).or(debug.thread_id);
        self.epoch += 1;
        if action != "pause" {
            self.pause = None;
            cx.emit(ExecutionEvent::DebugChanged);
        }
        let threads = match thread {
            Some(_) => None,
            None => Some(self.decoded::<Vec<DebugThread>>(json!({ "op": "debug.threads", "debugId": debug.id }), cx)),
        };
        let Some(workspace) = self.workspace_id.clone() else { return };
        let store = self.store.clone();
        let action = action.to_owned();
        cx.spawn(async move |this: WeakEntity<Execution>, cx: &mut AsyncApp| {
            let thread = match (thread, threads) {
                (Some(thread), _) => Some(thread),
                (None, Some(threads)) => threads.await.ok().and_then(|threads| threads.first().map(|thread| thread.id)),
                (None, None) => None,
            };
            let Some(thread) = thread else { return };
            let call = store.read_with(cx, |store, _| {
                store.call("ide", json!({ "op": "debug.control", "workspaceId": workspace, "debugId": debug.id, "action": action, "threadId": thread }))
            });
            let _ = call.await;
            let _ = this.update(cx, |this, cx| this.poll(cx));
        })
        .detach();
        cx.notify();
    }

    /// Read a new pause's threads, stack and top frame once; forget them when the session runs on.
    fn update_pause(&mut self, cx: &mut Context<Self>) {
        let Some(debug) = self.debug().cloned() else {
            if self.pause.take().is_some() {
                cx.emit(ExecutionEvent::DebugChanged);
            }
            return;
        };
        if debug.phase != "paused" {
            if self.pause.take().is_some() {
                self.epoch += 1;
                cx.emit(ExecutionEvent::DebugChanged);
            }
            return;
        }
        if self.pause.as_ref().is_some_and(|pause| pause.debug_id == debug.id) {
            return;
        }
        self.epoch += 1;
        let epoch = self.epoch;
        self.pause = Some(Pause { debug_id: debug.id.clone(), ..Pause::default() });
        let threads = self.decoded::<Vec<DebugThread>>(json!({ "op": "debug.threads", "debugId": debug.id }), cx);
        cx.spawn(async move |this: WeakEntity<Execution>, cx: &mut AsyncApp| {
            let Ok(threads) = threads.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                let thread = threads.iter().find(|thread| Some(thread.id) == debug.thread_id).or(threads.first()).map(|thread| thread.id);
                if let Some(pause) = this.pause.as_mut() {
                    pause.threads = threads;
                    pause.thread_id = thread;
                }
                cx.emit(ExecutionEvent::DebugChanged);
                if let Some(thread) = thread {
                    this.load_stack(thread, true, cx);
                }
            });
        })
        .detach();
    }

    /// Read a thread's stack; on a new pause the top frame is shown in the editor.
    pub fn select_thread(&mut self, thread: i64, cx: &mut Context<Self>) {
        if let Some(pause) = self.pause.as_mut() {
            pause.thread_id = Some(thread);
        }
        self.load_stack(thread, false, cx);
    }

    fn load_stack(&mut self, thread: i64, reveal: bool, cx: &mut Context<Self>) {
        let Some(debug_id) = self.pause.as_ref().map(|pause| pause.debug_id.clone()) else { return };
        let epoch = self.epoch;
        let stack = self.decoded::<Vec<DebugFrame>>(json!({ "op": "debug.stack", "debugId": debug_id, "threadId": thread }), cx);
        cx.spawn(async move |this: WeakEntity<Execution>, cx: &mut AsyncApp| {
            let Ok(frames) = stack.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                let top = frames.first().cloned();
                if let Some(pause) = this.pause.as_mut() {
                    pause.frames = frames;
                }
                cx.emit(ExecutionEvent::DebugChanged);
                if let Some(frame) = top {
                    if reveal {
                        this.reveal(&frame, cx);
                    }
                    this.select_frame(frame.id, cx);
                }
            });
        })
        .detach();
    }

    /// Show a frame's line in the editor.
    pub fn reveal(&self, frame: &DebugFrame, cx: &mut App) {
        let Some(path) = frame.path.clone() else { return };
        let ide = Ide::global(cx);
        let Some(window) = ide.read(cx).window_handle() else { return };
        let line = (frame.line.max(1) - 1) as u32;
        let column = (frame.column.max(1) - 1) as u32;
        let _ = window.update(cx, |_, window, cx| {
            ide.update(cx, |ide, cx| {
                if let Some(key) = ide.key_from_absolute(&path) {
                    ide.reveal(key, line, column, window, cx);
                }
            })
        });
    }

    /// Read a frame's scopes, the variables of the cheap ones, and the watches.
    pub fn select_frame(&mut self, frame: i64, cx: &mut Context<Self>) {
        let Some(pause) = self.pause.as_mut() else { return };
        pause.frame_id = Some(frame);
        pause.scopes.clear();
        pause.variables.clear();
        pause.watches.clear();
        let debug_id = pause.debug_id.clone();
        self.epoch += 1;
        let epoch = self.epoch;
        let scopes = self.decoded::<Vec<DebugScope>>(json!({ "op": "debug.scopes", "debugId": debug_id, "frameId": frame }), cx);
        cx.spawn(async move |this: WeakEntity<Execution>, cx: &mut AsyncApp| {
            let Ok(scopes) = scopes.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                let eager: Vec<i64> = scopes.iter().filter(|scope| !scope.expensive && scope.variables_reference > 0).map(|scope| scope.variables_reference).collect();
                if let Some(pause) = this.pause.as_mut() {
                    pause.scopes = scopes;
                }
                cx.emit(ExecutionEvent::DebugChanged);
                for reference in eager {
                    this.expand(reference, cx);
                }
                let watches = Ide::global(cx).read(cx).execution().get("watches").and_then(Value::as_array).cloned().unwrap_or_default();
                for watch in watches.iter().filter_map(Value::as_str) {
                    this.evaluate(watch.to_owned(), "watch", cx).detach();
                }
            });
        })
        .detach();
    }

    /// Read the children of a scope or variable.
    pub fn expand(&mut self, reference: i64, cx: &mut Context<Self>) {
        let Some(pause) = self.pause.as_ref() else { return };
        if reference <= 0 || pause.variables.contains_key(&reference) {
            return;
        }
        let epoch = self.epoch;
        let read = self.decoded::<Vec<DebugVariable>>(json!({ "op": "debug.variables", "debugId": pause.debug_id, "variablesReference": reference }), cx);
        cx.spawn(async move |this: WeakEntity<Execution>, cx: &mut AsyncApp| {
            let Ok(variables) = read.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                if let Some(pause) = this.pause.as_mut() {
                    pause.variables.insert(reference, variables);
                }
                cx.emit(ExecutionEvent::DebugChanged);
            });
        })
        .detach();
    }

    /// Forget a variable's children, folding it.
    pub fn collapse(&mut self, reference: i64, cx: &mut Context<Self>) {
        if let Some(pause) = self.pause.as_mut() {
            pause.variables.remove(&reference);
            cx.emit(ExecutionEvent::DebugChanged);
        }
    }

    /// Evaluate an expression in the selected frame; a watch keeps its result in the pause.
    pub fn evaluate(&mut self, expression: String, context: &'static str, cx: &mut Context<Self>) -> Task<Result<String, String>> {
        let Some(pause) = self.pause.as_ref() else {
            return Task::ready(Err(crate::ui::tr(cx, "ide.ideDebugNotPaused").to_string()));
        };
        let mut params = json!({ "op": "debug.evaluate", "debugId": pause.debug_id, "expression": expression, "context": context });
        if let Some(frame) = pause.frame_id {
            params["frameId"] = json!(frame);
        }
        let epoch = self.epoch;
        let read = self.decoded::<DebugEvaluation>(params, cx);
        cx.spawn(async move |this: WeakEntity<Execution>, cx: &mut AsyncApp| {
            let result = read.await.map(|evaluation| evaluation.result).map_err(|error| error.message);
            if context == "watch" {
                let _ = this.update(cx, |this, cx| {
                    if this.epoch == epoch
                        && let Some(pause) = this.pause.as_mut()
                    {
                        pause.watches.insert(expression, result.clone());
                        cx.emit(ExecutionEvent::DebugChanged);
                    }
                });
            }
            result
        })
    }
}
