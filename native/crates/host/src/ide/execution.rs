//! Project-scoped terminals, runs and launch-only debug sessions (3.x `ide/execution.ts`), with a bounded history of
//! ordered events the page polls with a cursor.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use super::debug::DebugSession;
use super::owner::Owner;
use super::pty::{Outcome, Pty};
use super::resolve::{CommandSpec, ResolvedRun, check_run_configuration, create_build_directory, executable, remove_build_directory, resolve_run, workspace_path};
use crate::error::{HostError, HostResult};
use crate::paths::realpath;
use crate::process::resolve_powershell;
use crate::runtime::Runtime;

const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 256 * 1024;
const MAX_OPERATIONS: usize = 16;
const MAX_CONFIGURATION_BYTES: usize = 32 * 1024;
const MAX_TERMINAL_DIMENSION: u64 = 10_000;
const DEFAULT_COLS: u16 = 100;
const DEFAULT_ROWS: u16 = 24;

fn error(message: impl Into<String>) -> HostError {
    HostError::new("execution-error", message)
}

/// Check the run settings stored with a project's editor state.
pub fn check_configuration(value: &Value) -> HostResult<()> {
    let invalid = || HostError::invalid("The run settings are not valid.");
    let object = value.as_object().ok_or_else(invalid)?;
    if object.keys().any(|key| !["profiles", "activeProfile", "breakpoints", "watches"].contains(&key.as_str())) {
        return Err(invalid());
    }
    let profiles = object.get("profiles").and_then(Value::as_array).ok_or_else(invalid)?;
    let mut names = std::collections::HashSet::new();
    for profile in profiles {
        check_run_configuration(profile)?;
        if !names.insert(profile["name"].as_str().unwrap_or_default()) {
            return Err(HostError::invalid("Run profile names must be unique."));
        }
    }
    match object.get("activeProfile") {
        Some(Value::Null) => {}
        Some(Value::String(name)) if names.contains(name.as_str()) => {}
        _ => return Err(HostError::invalid("The selected run profile does not exist.")),
    }
    for source in object.get("breakpoints").and_then(Value::as_array).ok_or_else(invalid)? {
        let valid = source["path"].as_str().is_some_and(|path| !path.is_empty() && !path.contains('\0'))
            && source["lines"].as_array().is_some_and(|lines| lines.iter().all(|line| line.as_u64().is_some_and(|line| line > 0)));
        if !valid {
            return Err(invalid());
        }
    }
    if !object.get("watches").and_then(Value::as_array).is_some_and(|watches| watches.iter().all(Value::is_string)) {
        return Err(invalid());
    }
    Ok(())
}

enum Kind {
    Run,
    Terminal,
    Debug(Arc<DebugSession>),
}

struct Operation {
    id: String,
    workspace_id: String,
    kind: Kind,
    owner: Arc<Owner>,
    snapshot: Mutex<Value>,
    stopped: AtomicBool,
    finished: AtomicBool,
    terminal: Mutex<Option<Arc<Pty>>>,
    stdin: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    done: tokio::sync::watch::Sender<bool>,
}

impl Operation {
    fn snapshot(&self) -> Value {
        match &self.kind {
            Kind::Debug(session) => session.snapshot(),
            _ => self.snapshot.lock().unwrap().clone(),
        }
    }

    fn set(&self, changes: &[(&str, Value)]) {
        let mut snapshot = self.snapshot.lock().unwrap();
        for (key, value) in changes {
            if value.is_null() {
                snapshot.as_object_mut().map(|object| object.remove(*key));
            } else {
                snapshot[*key] = value.clone();
            }
        }
    }

    async fn wait_done(&self) {
        let mut done = self.done.subscribe();
        let _ = done.wait_for(|done| *done).await;
    }
}

#[derive(Default)]
struct History {
    events: VecDeque<(Value, usize)>,
    bytes: usize,
    sequence: u64,
    removed_through: u64,
}

pub struct Execution {
    runtime: Arc<Runtime>,
    tools: PathBuf,
    pwsh_path: Option<PathBuf>,
    operations: Mutex<Vec<Arc<Operation>>>,
    history: Mutex<History>,
    hint: Arc<dyn Fn(&str) + Send + Sync>,
    disposed: AtomicBool,
}

fn exit_of(outcome: &Outcome, stopped: bool) -> Value {
    json!({ "exitCode": outcome.exit_code, "signal": outcome.signal, "stopped": stopped })
}

fn dimension(value: &Value, fallback: u16) -> HostResult<u16> {
    match value.as_u64() {
        None if value.is_null() => Ok(fallback),
        Some(number) if number > 0 && number <= MAX_TERMINAL_DIMENSION => Ok(number.min(u64::from(u16::MAX)) as u16),
        _ => Err(error("The requested terminal dimensions exceed the configured limit.")),
    }
}

impl Execution {
    pub fn new(runtime: Arc<Runtime>, tools: PathBuf, pwsh_path: Option<PathBuf>, hint: impl Fn(&str) + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { runtime, tools, pwsh_path, operations: Mutex::default(), history: Mutex::default(), hint: Arc::new(hint), disposed: AtomicBool::new(false) })
    }

    pub fn has_activity(&self) -> bool {
        self.operations.lock().unwrap().iter().any(|operation| !operation.finished.load(Ordering::Relaxed))
    }

    fn publish(&self, workspace_id: &str, mut event: Value) {
        if self.disposed.load(Ordering::Relaxed) {
            return;
        }
        (self.hint)(workspace_id);
        let mut history = self.history.lock().unwrap();
        history.sequence += 1;
        event["sequence"] = json!(history.sequence);
        event["workspaceId"] = json!(workspace_id);
        let bytes = event.to_string().len();
        if bytes > MAX_EVENT_BYTES {
            history.removed_through = history.sequence;
            return;
        }
        history.events.push_back((event, bytes));
        history.bytes += bytes;
        while history.bytes > MAX_OUTPUT_BYTES {
            let Some((removed, size)) = history.events.pop_front() else { break };
            history.bytes -= size;
            history.removed_through = history.removed_through.max(removed["sequence"].as_u64().unwrap_or(0));
        }
    }

    fn output(&self, workspace_id: &str, operation_id: &str, stream: &str, text: &str) {
        // Split output into events that fit the event limit, at character boundaries.
        let mut rest = text;
        let limit = MAX_EVENT_BYTES - 1024;
        while !rest.is_empty() {
            let mut cut = rest.len().min(limit / 2);
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            let (chunk, tail) = rest.split_at(cut);
            self.publish(workspace_id, json!({ "kind": "output", "operationId": operation_id, "stream": stream, "text": chunk }));
            rest = tail;
        }
    }

    fn publish_operation(&self, operation: &Operation) {
        let (kind, field) = match operation.kind {
            Kind::Run => ("run", "run"),
            Kind::Terminal => ("terminal", "terminal"),
            Kind::Debug(_) => ("debug", "debug"),
        };
        self.publish(&operation.workspace_id, json!({ "kind": kind, field: operation.snapshot() }));
    }

    fn status(&self, workspace_id: &str) -> Value {
        let operations: Vec<Arc<Operation>> = self.operations.lock().unwrap().iter().filter(|operation| operation.workspace_id == workspace_id).cloned().collect();
        let pick = |want: fn(&Kind) -> bool| operations.iter().filter(|operation| want(&operation.kind)).map(|operation| operation.snapshot()).collect::<Vec<_>>();
        json!({ "runs": pick(|kind| matches!(kind, Kind::Run)), "terminals": pick(|kind| matches!(kind, Kind::Terminal)), "debugSessions": pick(|kind| matches!(kind, Kind::Debug(_))) })
    }

    fn reserve(&self) -> HostResult<()> {
        if self.disposed.load(Ordering::Relaxed) {
            return Err(error("The IDE execution service is closed."));
        }
        let mut operations = self.operations.lock().unwrap();
        if operations.len() < MAX_OPERATIONS {
            return Ok(());
        }
        let Some(index) = operations.iter().position(|operation| operation.finished.load(Ordering::Relaxed)) else {
            return Err(error("The workspace execution limit is reached. Stop an operation before starting another."));
        };
        operations.remove(index);
        Ok(())
    }

    fn find(&self, workspace_id: &str, id: &str) -> HostResult<Arc<Operation>> {
        self.operations
            .lock()
            .unwrap()
            .iter()
            .find(|operation| operation.id == id && operation.workspace_id == workspace_id)
            .cloned()
            .ok_or_else(|| error("The operation does not belong to the selected workspace."))
    }

    fn new_operation(self: &Arc<Self>, workspace_id: &str, kind_of: impl FnOnce(&str, Arc<Owner>) -> Kind, snapshot: Value) -> Arc<Operation> {
        let id = uuid::Uuid::new_v4().to_string();
        let (service, workspace, operation_id) = (Arc::downgrade(self), workspace_id.to_owned(), id.clone());
        let owner = Owner::new(Arc::new(move |stream, text: String| {
            if let Some(service) = service.upgrade() {
                service.output(&workspace, &operation_id, stream, &text);
            }
        }));
        let mut snapshot = snapshot;
        if snapshot.is_object() {
            snapshot["id"] = json!(id);
        }
        let (done, _) = tokio::sync::watch::channel(false);
        let operation = Arc::new(Operation {
            kind: kind_of(&id, owner.clone()),
            id,
            workspace_id: workspace_id.to_owned(),
            owner,
            snapshot: Mutex::new(snapshot),
            stopped: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            terminal: Mutex::new(None),
            stdin: tokio::sync::Mutex::new(None),
            done,
        });
        self.operations.lock().unwrap().push(operation.clone());
        operation
    }

    fn workspace_root(&self, workspace_id: &str, root_id: Option<&str>, files: &super::files::Files) -> HostResult<PathBuf> {
        Ok(realpath(&files.resolve_workspace(workspace_id, root_id)?.root)?)
    }

    fn resolve(&self, workspace_id: &str, configuration: &Value, files: &super::files::Files) -> HostResult<ResolvedRun> {
        let root = self.workspace_root(workspace_id, configuration["rootId"].as_str(), files)?;
        let runtime = self.runtime.resolve_workspace(workspace_id).ok();
        resolve_run(&root, workspace_id, configuration, runtime.as_ref())
    }

    /// Run the build commands; the first failing command's outcome when the build failed or was stopped.
    async fn build(&self, operation: &Operation, resolved: &ResolvedRun) -> HostResult<Option<Outcome>> {
        if let Some(directory) = &resolved.owned_build_directory {
            create_build_directory(&resolved.root, directory)?;
        }
        for command in &resolved.build {
            if operation.owner.closing() {
                return Ok(Some(Outcome::default()));
            }
            let outcome = operation.owner.command(command).await?;
            if outcome.exit_code != Some(0) || operation.stopped.load(Ordering::Relaxed) {
                return Ok(Some(outcome));
            }
        }
        if !resolved.build.is_empty() {
            workspace_path(&resolved.root, &resolved.launch.argv[0], Some("file"))?;
        }
        Ok(None)
    }

    fn cleanup_build(resolved: &ResolvedRun) -> HostResult<()> {
        match &resolved.owned_build_directory {
            Some(directory) => remove_build_directory(&resolved.root, directory),
            None => Ok(()),
        }
    }

    async fn execute_run(self: Arc<Self>, operation: Arc<Operation>, resolved: ResolvedRun, cols: u16, rows: u16) {
        let stopped = |operation: &Operation| operation.stopped.load(Ordering::Relaxed);
        let result: HostResult<()> = async {
            if !resolved.build.is_empty() {
                operation.set(&[("phase", json!("building"))]);
                self.publish_operation(&operation);
            }
            if let Some(failure) = self.build(&operation, &resolved).await? {
                let stop = stopped(&operation);
                operation.set(&[
                    ("phase", json!(if stop { "exited" } else { "failed" })),
                    ("exit", exit_of(&failure, stop)),
                    ("error", if stop { Value::Null } else { json!("The build failed; the program was not launched.") }),
                ]);
                return Ok(());
            }
            operation.set(&[("phase", json!("running"))]);
            self.publish_operation(&operation);
            let outcome = if resolved.terminal {
                let terminal = operation.owner.terminal(&resolved.launch, cols, rows)?;
                *operation.terminal.lock().unwrap() = Some(terminal.clone());
                tokio::select! {
                    outcome = terminal.wait() => outcome,
                    () = operation.owner.cancelled() => {
                        terminal.terminate().await;
                        terminal.wait().await
                    }
                }
            } else {
                let mut child = operation.owner.spawn(&resolved.launch, true)?;
                *operation.stdin.lock().await = child.stdin.take();
                let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
                let owner = operation.owner.clone();
                let waiting = owner.wait_process(&mut child, stdout, stderr);
                tokio::pin!(waiting);
                tokio::select! {
                    outcome = &mut waiting => outcome,
                    () = operation.owner.cancelled() => waiting.await,
                }
            };
            operation.set(&[("phase", json!("exited")), ("exit", exit_of(&outcome, stopped(&operation)))]);
            Ok(())
        }
        .await;
        if let Err(failure) = result {
            let stop = stopped(&operation);
            if stop {
                operation.set(&[("phase", json!("exited")), ("exit", json!({ "exitCode": null, "signal": null, "stopped": true }))]);
            } else {
                operation.set(&[("phase", json!("failed")), ("error", json!(failure.message))]);
            }
        }
        operation.owner.close().await;
        if let Err(failure) = Self::cleanup_build(&resolved) {
            operation.set(&[("phase", json!("failed")), ("error", json!(failure.message))]);
            log::warn!("IDE execution cleanup failed: {}", failure.message);
        }
        operation.finished.store(true, Ordering::Relaxed);
        self.publish_operation(&operation);
        let _ = operation.done.send(true);
    }

    async fn start_run(self: &Arc<Self>, request: &Value, files: &super::files::Files) -> HostResult<Value> {
        let workspace_id = request["workspaceId"].as_str().unwrap_or_default();
        let resolved = self.resolve(workspace_id, &request["configuration"], files)?;
        let (cols, rows) = (dimension(&request["cols"], DEFAULT_COLS)?, dimension(&request["rows"], DEFAULT_ROWS)?);
        self.reserve()?;
        let snapshot = json!({ "workspaceId": workspace_id, "name": resolved.name, "phase": "starting", "spec": resolved.spec() });
        let operation = self.new_operation(workspace_id, |_, _| Kind::Run, snapshot);
        self.publish_operation(&operation);
        let snapshot = operation.snapshot();
        tokio::spawn(self.clone().execute_run(operation, resolved, cols, rows));
        Ok(snapshot)
    }

    async fn start_terminal(self: &Arc<Self>, request: &Value, files: &super::files::Files) -> HostResult<Value> {
        let workspace_id = request["workspaceId"].as_str().unwrap_or_default();
        let (cols, rows) = (dimension(&request["cols"], DEFAULT_COLS)?, dimension(&request["rows"], DEFAULT_ROWS)?);
        let root = self.workspace_root(workspace_id, request["rootId"].as_str(), files)?;
        let cwd = workspace_path(&root, request["cwd"].as_str().unwrap_or("."), Some("directory"))?;
        let runtime = self.runtime.resolve_workspace(workspace_id).ok();
        let environment = runtime.as_ref().map(|runtime| runtime.environment.clone()).unwrap_or_default();
        let argv = if cfg!(windows) {
            let (shell, _) = resolve_powershell(self.pwsh_path.as_deref(), runtime.as_ref().and_then(|runtime| runtime.powershell.as_deref()));
            vec![shell.display().to_string(), "-NoLogo".into(), "-NoProfile".into()]
        } else {
            let shell = std::env::var("SHELL").ok().filter(|shell| !shell.is_empty()).unwrap_or_else(|| "/bin/bash".into());
            vec![executable(Some(&shell).filter(|shell| shell.starts_with('/')).map(String::as_str), "bash", &environment)?, "--noprofile".into(), "--norc".into(), "-i".into()]
        };
        self.reserve()?;
        let snapshot = json!({ "workspaceId": workspace_id, "cwd": cwd, "phase": "starting" });
        let operation = self.new_operation(workspace_id, |_, _| Kind::Terminal, snapshot);
        self.publish_operation(&operation);
        let snapshot = operation.snapshot();
        let service = self.clone();
        let spec = CommandSpec { argv, cwd, environment };
        tokio::spawn(async move {
            let result: HostResult<Outcome> = async {
                let terminal = operation.owner.terminal(&spec, cols, rows)?;
                *operation.terminal.lock().unwrap() = Some(terminal.clone());
                operation.set(&[("phase", json!("running"))]);
                service.publish_operation(&operation);
                Ok(tokio::select! {
                    outcome = terminal.wait() => outcome,
                    () = operation.owner.cancelled() => {
                        terminal.terminate().await;
                        terminal.wait().await
                    }
                })
            }
            .await;
            let stopped = operation.stopped.load(Ordering::Relaxed);
            match result {
                Ok(outcome) => operation.set(&[("phase", json!("exited")), ("exit", exit_of(&outcome, stopped))]),
                Err(_) if stopped => operation.set(&[("phase", json!("exited")), ("exit", json!({ "exitCode": null, "signal": null, "stopped": true }))]),
                Err(failure) => operation.set(&[("phase", json!("failed")), ("error", json!(failure.message))]),
            }
            operation.owner.close().await;
            operation.finished.store(true, Ordering::Relaxed);
            service.publish_operation(&operation);
            let _ = operation.done.send(true);
        });
        Ok(snapshot)
    }

    async fn start_debug(self: &Arc<Self>, request: &Value, files: &super::files::Files) -> HostResult<Value> {
        let workspace_id = request["workspaceId"].as_str().unwrap_or_default().to_owned();
        if request["configuration"]["language"] == "php" {
            return Err(error("PHP supports running and terminals; no PHP debug adapter is configured."));
        }
        let resolved = self.resolve(&workspace_id, &request["configuration"], files)?;
        let (cols, rows) = (dimension(&request["cols"], DEFAULT_COLS)?, dimension(&request["rows"], DEFAULT_ROWS)?);
        self.reserve()?;
        let own_root = resolved.configuration["rootId"].as_str().unwrap_or("primary").to_owned();
        let breakpoints: Vec<(String, Vec<u64>)> = request["breakpoints"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|source| source["rootId"].as_str().unwrap_or("primary") == own_root)
            .filter_map(|source| Some((source["path"].as_str()?.to_owned(), source["lines"].as_array()?.iter().filter_map(Value::as_u64).collect())))
            .collect();
        let mut snapshot = Map::new();
        if let Some(root) = resolved.configuration.get("rootId").filter(|root| root.is_string()) {
            snapshot.insert("rootId".into(), root.clone());
        }
        snapshot.insert("workspaceId".into(), json!(workspace_id));
        snapshot.insert("name".into(), json!(resolved.name));
        snapshot.insert("language".into(), json!(resolved.language));
        snapshot.insert("phase".into(), json!(if resolved.build.is_empty() { "starting" } else { "building" }));
        snapshot.insert("breakpoints".into(), json!([]));
        let (tools, stop_on_entry) = (self.tools.clone(), request["stopOnEntry"].as_bool().unwrap_or(false));
        let service = Arc::downgrade(self);
        let publish_workspace = workspace_id.clone();
        let resolved_for_session = resolved.clone();
        let operation = self.new_operation(
            &workspace_id,
            move |id, owner| {
                snapshot.insert("id".into(), json!(id));
                let publish: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(move |snapshot| {
                    if let Some(service) = service.upgrade() {
                        service.publish(&publish_workspace, json!({ "kind": "debug", "debug": snapshot }));
                    }
                });
                Kind::Debug(DebugSession::new(snapshot, resolved_for_session, owner, tools, (cols, rows), stop_on_entry, breakpoints, publish))
            },
            Value::Null,
        );
        let Kind::Debug(session) = &operation.kind else { unreachable!() };
        let session = session.clone();
        self.publish_operation(&operation);
        let snapshot = session.snapshot();
        let service = self.clone();
        tokio::spawn(async move {
            match service.build(&operation, &resolved).await {
                Ok(None) => {
                    session.start().await;
                    session.done().await;
                }
                Ok(Some(_)) => session.fail(if operation.stopped.load(Ordering::Relaxed) { "Debugging was stopped during the build." } else { "The debug build failed; the program was not launched." }),
                Err(failure) => session.fail(&failure.message),
            }
            session.stop().await;
            if let Err(failure) = Self::cleanup_build(&resolved) {
                log::warn!("IDE debug cleanup failed: {}", failure.message);
            }
            operation.finished.store(true, Ordering::Relaxed);
            let _ = operation.done.send(true);
        });
        Ok(snapshot)
    }

    async fn stop(&self, operation: &Arc<Operation>) {
        if operation.finished.load(Ordering::Relaxed) {
            return;
        }
        operation.stopped.store(true, Ordering::Relaxed);
        match &operation.kind {
            Kind::Debug(session) => session.stop().await,
            _ => {
                operation.set(&[("phase", json!("stopping"))]);
                self.publish_operation(operation);
                operation.owner.close().await;
            }
        }
        operation.wait_done().await;
    }

    /// Stop every operation of one project, as before the project is removed.
    pub async fn stop_workspace(&self, workspace_id: &str) {
        let operations: Vec<Arc<Operation>> = self.operations.lock().unwrap().iter().filter(|operation| operation.workspace_id == workspace_id).cloned().collect();
        futures::future::join_all(operations.iter().map(|operation| self.stop(operation))).await;
        self.operations.lock().unwrap().retain(|operation| operation.workspace_id != workspace_id);
    }

    /// Stop everything; later requests are refused.
    pub async fn dispose(&self) {
        self.disposed.store(true, Ordering::Relaxed);
        let operations: Vec<Arc<Operation>> = self.operations.lock().unwrap().clone();
        futures::future::join_all(operations.iter().map(|operation| self.stop(operation))).await;
        self.operations.lock().unwrap().clear();
    }

    /// One execution operation of the `ide` method.
    pub async fn handle(self: &Arc<Self>, request: &Value, files: &super::files::Files) -> HostResult<Value> {
        if self.disposed.load(Ordering::Relaxed) {
            return Err(error("The IDE execution service is closed."));
        }
        if request.to_string().len() > MAX_CONFIGURATION_BYTES {
            return Err(error("The IDE request exceeds the configured input limit."));
        }
        let workspace_id = request["workspaceId"].as_str().filter(|id| !id.is_empty()).ok_or_else(|| HostError::invalid("workspaceId is required"))?;
        files.resolve_workspace(workspace_id, None)?;
        let op = request["op"].as_str().unwrap_or_default();
        let ok = || Ok(json!({ "ok": true }));
        match op {
            "execution.status" => Ok(self.status(workspace_id)),
            "execution.poll" => {
                let cursor = request["cursor"].as_u64().ok_or_else(|| HostError::invalid("cursor must be a number"))?;
                let history = self.history.lock().unwrap();
                let events: Vec<Value> = history.events.iter().filter(|(event, _)| event["workspaceId"] == workspace_id && event["sequence"].as_u64().unwrap_or(0) > cursor).map(|(event, _)| event.clone()).collect();
                let (sequence, truncated) = (history.sequence, cursor < history.removed_through);
                drop(history);
                Ok(json!({ "events": events, "cursor": sequence, "truncated": truncated, "status": self.status(workspace_id) }))
            }
            "run.resolve" => Ok(self.resolve(workspace_id, &request["configuration"], files)?.spec()),
            "run.start" => self.start_run(request, files).await,
            "terminal.start" => self.start_terminal(request, files).await,
            "debug.start" => self.start_debug(request, files).await,
            "terminal.input" | "terminal.resize" | "terminal.stop" | "run.input" | "run.resize" | "run.stop" => {
                let terminal_op = op.starts_with("terminal.");
                let id = request[if terminal_op { "terminalId" } else { "runId" }].as_str().unwrap_or_default();
                let operation = self.find(workspace_id, id)?;
                if terminal_op != matches!(operation.kind, Kind::Terminal) || matches!(operation.kind, Kind::Debug(_)) {
                    return Err(error(if terminal_op { "The requested terminal does not exist." } else { "The requested run does not exist." }));
                }
                let ended = operation.finished.load(Ordering::Relaxed) || operation.stopped.load(Ordering::Relaxed);
                if op.ends_with(".stop") {
                    self.stop(&operation).await;
                    return ok();
                }
                // The page can resize before it learns that the program ended; that resize has nothing to do.
                if op.ends_with(".resize") && ended {
                    return ok();
                }
                if ended {
                    return Err(error(if terminal_op { "This terminal is not running." } else { "This program is not running." }));
                }
                let terminal = operation.terminal.lock().unwrap().clone();
                if op.ends_with(".input") {
                    let data = request["data"].as_str().unwrap_or_default();
                    match terminal {
                        Some(terminal) => terminal.write(data)?,
                        None => {
                            use tokio::io::AsyncWriteExt as _;
                            let mut stdin = operation.stdin.lock().await;
                            let pipe = stdin.as_mut().ok_or_else(|| error("This operation has no open input pipe."))?;
                            pipe.write_all(data.as_bytes()).await?;
                            pipe.flush().await?;
                        }
                    }
                } else {
                    let terminal = terminal.ok_or_else(|| error("This program does not use a terminal."))?;
                    terminal.resize(dimension(&request["cols"], DEFAULT_COLS)?, dimension(&request["rows"], DEFAULT_ROWS)?);
                }
                ok()
            }
            op if op.starts_with("debug.") => {
                let operation = self.find(workspace_id, request["debugId"].as_str().unwrap_or_default())?;
                let Kind::Debug(session) = &operation.kind else { return Err(error("The requested debug session does not exist.")) };
                if op == "debug.stop" {
                    self.stop(&operation).await;
                    return ok();
                }
                session.handle(request).await
            }
            _ => Err(HostError::invalid("Unknown IDE operation.")),
        }
    }
}
