//! Terminals of the open project (`ide-execution-model.ts`). The Host owns the processes and keeps their output as
//! numbered events; this side reads the events after the cursor whenever an `ide.hint` of kind `execution` arrives.

use gpui_kit::*;
use rainy_protocol::RpcError;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};

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

#[derive(Debug, Deserialize)]
struct Status {
    terminals: Vec<TerminalInfo>,
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

#[derive(Debug, Clone)]
pub enum ExecutionEvent {
    /// Text a process wrote; `stream` is `terminal` for terminals.
    Output { operation: String, stream: String, text: String },
    TerminalsChanged,
}

pub struct Execution {
    store: Entity<Store>,
    workspace_id: Option<String>,
    cursor: u64,
    pub terminals: Vec<TerminalInfo>,
    polling: bool,
    again: bool,
}

impl EventEmitter<ExecutionEvent> for Execution {}

struct GlobalExecution(Entity<Execution>);
impl Global for GlobalExecution {}

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
                    cx.emit(ExecutionEvent::TerminalsChanged);
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
            Execution { store, workspace_id: None, cursor: 0, terminals: Vec::new(), polling: false, again: false }
        });
        cx.set_global(GlobalExecution(execution.clone()));
        execution
    }

    fn request(&self, mut params: Value, cx: &App) -> Option<impl Future<Output = Result<Value, RpcError>> + use<>> {
        let workspace = self.workspace_id.clone()?;
        params["workspaceId"] = json!(workspace);
        Some(self.store.read(cx).call("ide", params))
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
                    if poll.status.terminals != this.terminals {
                        this.terminals = poll.status.terminals;
                        cx.emit(ExecutionEvent::TerminalsChanged);
                    }
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
        let Some(call) = self.request(json!({ "op": "terminal.start", "cols": cols, "rows": rows }), cx) else {
            return Task::ready(Err(RpcError { code: "no-workspace".into(), message: String::new(), data: None }));
        };
        cx.spawn(async move |this: WeakEntity<Execution>, cx: &mut AsyncApp| {
            let terminal: TerminalInfo = serde_json::from_value(call.await?)
                .map_err(|error| RpcError { code: "invalid".into(), message: error.to_string(), data: None })?;
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

    fn send(&self, params: Value, cx: &App) {
        if let Some(call) = self.request(params, cx) {
            cx.background_spawn(async move {
                let _ = call.await;
            })
            .detach();
        }
    }

    pub fn input(&self, terminal: &str, data: &str, cx: &App) {
        self.send(json!({ "op": "terminal.input", "terminalId": terminal, "data": data }), cx);
    }

    pub fn resize(&self, terminal: &str, cols: u16, rows: u16, cx: &App) {
        self.send(json!({ "op": "terminal.resize", "terminalId": terminal, "cols": cols, "rows": rows }), cx);
    }

    pub fn stop(&self, terminal: &str, cx: &App) {
        self.send(json!({ "op": "terminal.stop", "terminalId": terminal }), cx);
    }
}
