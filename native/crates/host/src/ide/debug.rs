//! Launch-only debug sessions (3.x `ide/debug.ts`): Python through debugpy, JavaScript and TypeScript through js-debug
//! on the user's Node.js, C and C++ through CodeLLDB on Windows and GDB's DAP interpreter on Linux. Every adapter,
//! transport and terminal belongs to one launched program and ends with it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, watch};

use super::dap::{Incoming, Peer};
use super::owner::Owner;
use super::pty::{Pty, Utf8Stream};
use super::resolve::{CommandSpec, ResolvedRun, executable, workspace_path};
use crate::error::{HostError, HostResult};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_STACK_FRAMES: usize = 100;
const MAX_VARIABLES: usize = 200;
const MAX_DEBUG_TARGETS: usize = 16;
const MAX_ERROR_CHARS: usize = 8 * 1024;

type IncomingSender = mpsc::UnboundedSender<(Arc<Peer>, Incoming)>;

fn error(message: impl Into<String>) -> HostError {
    HostError::new("execution-error", message)
}

#[derive(Default)]
struct Inner {
    peers: Vec<Arc<Peer>>,
    terminated: HashSet<u64>,
    active: Option<Arc<Peer>>,
    root: Option<Arc<Peer>>,
    terminal: Option<Arc<Pty>>,
    server_port: Option<u16>,
    child_claimed: bool,
    targets: usize,
    sources: Vec<(PathBuf, Vec<u64>)>,
    variable_counts: HashMap<i64, i64>,
    stop_generation: u64,
}

pub struct DebugSession {
    resolved: ResolvedRun,
    owner: Arc<Owner>,
    /// Where debugpy and js-debug are.
    tools: super::tools::Tools,
    cols: u16,
    rows: u16,
    stop_on_entry: bool,
    initial: Vec<(String, Vec<u64>)>,
    publish: Arc<dyn Fn(Value) + Send + Sync>,
    state: Mutex<Map<String, Value>>,
    inner: Mutex<Inner>,
    closing: AtomicBool,
    stopped: AtomicBool,
    incoming: IncomingSender,
    child_initialization: Mutex<Option<tokio::task::JoinHandle<HostResult<()>>>>,
    done: watch::Sender<bool>,
}

impl DebugSession {
    pub fn new(
        snapshot: Map<String, Value>,
        resolved: ResolvedRun,
        owner: Arc<Owner>,
        tools: super::tools::Tools,
        (cols, rows): (u16, u16),
        stop_on_entry: bool,
        breakpoints: Vec<(String, Vec<u64>)>,
        publish: Arc<dyn Fn(Value) + Send + Sync>,
    ) -> Arc<Self> {
        let (incoming, mut receiver) = mpsc::unbounded_channel();
        let (done, _) = watch::channel(false);
        let session = Arc::new(Self {
            resolved,
            owner,
            tools,
            cols,
            rows,
            stop_on_entry,
            initial: breakpoints,
            publish,
            state: Mutex::new(snapshot),
            inner: Mutex::default(),
            closing: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            incoming,
            child_initialization: Mutex::new(None),
            done,
        });
        let weak = Arc::downgrade(&session);
        tokio::spawn(async move {
            while let Some((peer, message)) = receiver.recv().await {
                let Some(session) = weak.upgrade() else { break };
                match message {
                    Incoming::Event { event, body } => session.event(&peer, &event, &body),
                    Incoming::Request { seq, command, arguments } => {
                        let session = session.clone();
                        tokio::spawn(async move {
                            let result = session.reverse(&command, arguments).await.map_err(|error| error.message);
                            peer.respond(seq, &command, result);
                        });
                    }
                    Incoming::Failed(reason) => {
                        let terminated = session.inner.lock().unwrap().terminated.contains(&peer.id);
                        if !session.closing() && !terminated {
                            session.fail(&reason);
                        }
                    }
                }
            }
        });
        session
    }

    pub fn snapshot(&self) -> Value {
        Value::Object(self.state.lock().unwrap().clone())
    }

    fn closing(&self) -> bool {
        self.closing.load(Ordering::Relaxed)
    }

    fn native(&self) -> bool {
        self.resolved.language == "c" || self.resolved.language == "cpp"
    }

    /// Change snapshot fields; `Null` removes one. The new snapshot is published.
    fn update(&self, changes: &[(&str, Value)]) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            for (key, value) in changes {
                if value.is_null() {
                    state.remove(*key);
                } else {
                    state.insert((*key).to_owned(), value.clone());
                }
            }
            if state.get("phase").and_then(Value::as_str) == Some("failed") {
                state.remove("reason");
                state.remove("threadId");
            }
            Value::Object(state.clone())
        };
        (self.publish)(snapshot);
    }

    fn phase(&self) -> String {
        self.state.lock().unwrap().get("phase").and_then(Value::as_str).unwrap_or_default().to_owned()
    }

    /// Wait until the adapter, the program and the terminals are joined.
    pub async fn done(&self) {
        let mut done = self.done.subscribe();
        let _ = done.wait_for(|done| *done).await;
    }

    fn source_path(&self, path: &str) -> HostResult<PathBuf> {
        workspace_path(&self.resolved.root, path, Some("file"))
    }

    fn remember_source(&self, path: PathBuf, lines: Vec<u64>) {
        let mut inner = self.inner.lock().unwrap();
        inner.sources.retain(|(known, _)| *known != path);
        inner.sources.push((path, lines));
    }

    fn remember_count(&self, value: &Value) {
        let reference = value["variablesReference"].as_i64().unwrap_or(0);
        if reference == 0 || (value["namedVariables"].is_null() && value["indexedVariables"].is_null()) {
            return;
        }
        let total = value["namedVariables"].as_i64().unwrap_or(0) + value["indexedVariables"].as_i64().unwrap_or(0);
        let mut inner = self.inner.lock().unwrap();
        if inner.variable_counts.len() > MAX_VARIABLES * MAX_STACK_FRAMES {
            if let Some(first) = inner.variable_counts.keys().next().copied() {
                inner.variable_counts.remove(&first);
            }
        }
        inner.variable_counts.insert(reference, total);
    }

    /// Start the adapter and the program, after any native build the controller ran.
    pub async fn start(self: &Arc<Self>) {
        let session = self.clone();
        let startup = async move {
            session.update(&[("phase", json!("initializing"))]);
            for (path, lines) in session.initial.clone() {
                let path = session.source_path(&path)?;
                session.remember_source(path, lines);
            }
            let peer = session.create_adapter().await?;
            {
                let mut inner = session.inner.lock().unwrap();
                inner.root = Some(peer.clone());
                inner.active = Some(peer.clone());
            }
            session.initialize(&peer, "launch", session.launch_arguments()).await?;
            let child = session.child_initialization.lock().unwrap().take();
            if let Some(child) = child {
                child.await.map_err(|_| error("The JavaScript debug target did not start."))??;
            }
            if !session.closing() && session.phase() == "initializing" {
                session.update(&[("phase", json!("running"))]);
            }
            HostResult::Ok(())
        };
        match tokio::time::timeout(STARTUP_TIMEOUT, startup).await {
            Ok(Ok(())) => {}
            Ok(Err(failure)) => self.fail(&failure.message),
            Err(_) => self.fail("The debug adapter did not finish startup before its deadline."),
        }
    }

    fn peer(&self, reader: impl tokio::io::AsyncRead + Unpin + Send + 'static, writer: impl tokio::io::AsyncWrite + Unpin + Send + 'static) -> Arc<Peer> {
        let peer = Peer::start(reader, writer, MAX_MESSAGE_BYTES, REQUEST_TIMEOUT, self.incoming.clone());
        self.inner.lock().unwrap().peers.push(peer.clone());
        peer
    }

    /// Watch a process the session owns: when it ends while the session is not closing, the session fails.
    fn track_exit(self: &Arc<Self>, mut child: tokio::process::Child, message: &'static str) {
        let session = self.clone();
        tokio::spawn(async move {
            let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
            let owner = session.owner.clone();
            tokio::join!(owner.pump(stdout, "adapter"), owner.pump(stderr, "adapter"), async {
                tokio::select! {
                    _ = child.wait() => {}
                    () = owner.cancelled() => crate::process::kill_tree(&mut child, Duration::from_millis(1500)).await,
                }
            });
            if !session.closing() {
                session.fail(message);
            }
        });
    }

    async fn create_adapter(self: &Arc<Self>) -> HostResult<Arc<Peer>> {
        let launch = &self.resolved.launch;
        let mut environment = launch.environment.clone();
        match self.resolved.language.as_str() {
            "python" => {
                let separator = if cfg!(windows) { ";" } else { ":" };
                let adapter = self.tools.debugpy().join("debugpy").join("adapter").join("__main__.py");
                super::tools::Tools::require(adapter, super::tools::Kit::Python)?;
                let own = self.tools.debugpy().display().to_string();
                let path = match environment.get("PYTHONPATH").filter(|value| !value.is_empty()) {
                    Some(existing) => format!("{own}{separator}{existing}"),
                    None => own,
                };
                environment.insert("PYTHONPATH".into(), path);
                environment.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
                let spec = CommandSpec { argv: vec![launch.argv[0].clone(), "-u".into(), "-m".into(), "debugpy.adapter".into()], cwd: launch.cwd.clone(), environment };
                let mut child = self.owner.spawn(&spec, true)?;
                let (stdout, stdin) = (child.stdout.take().ok_or_else(|| error("The debug adapter has no DAP transport."))?, child.stdin.take().ok_or_else(|| error("The debug adapter has no DAP transport."))?);
                let peer = self.peer(stdout, stdin);
                self.track_exit(child, "The debug adapter exited unexpectedly.");
                Ok(peer)
            }
            "javascript" | "typescript" => {
                let adapter = super::tools::Tools::require(self.tools.js_debug(), super::tools::Kit::Web)?;
                // js-debug is a script; it runs on the same Node.js as the program.
                let spec = CommandSpec { argv: vec![launch.argv[0].clone(), adapter.display().to_string(), "0".into(), "127.0.0.1".into()], cwd: launch.cwd.clone(), environment };
                let mut child = self.owner.spawn(&spec, false)?;
                let mut stdout = child.stdout.take().ok_or_else(|| error("The JavaScript adapter has no output stream."))?;
                let (port_sender, port) = tokio::sync::oneshot::channel::<Result<u16, String>>();
                let owner = self.owner.clone();
                tokio::spawn(async move {
                    use tokio::io::AsyncReadExt as _;
                    let mut port_sender = Some(port_sender);
                    let mut decoder = Utf8Stream::default();
                    let (mut text, mut buffer) = (String::new(), [0u8; 8192]);
                    loop {
                        let read = match stdout.read(&mut buffer).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => read,
                        };
                        let decoded = decoder.push(&buffer[..read]);
                        if port_sender.is_none() {
                            owner.emit("adapter", decoded);
                            continue;
                        }
                        text.push_str(&decoded);
                        if let Some(port) = text.split("Debug server listening at 127.0.0.1:").nth(1).and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next()).and_then(|digits| digits.parse::<u16>().ok()).filter(|_| text.contains('\n')) {
                            let _ = port_sender.take().map(|sender| sender.send(Ok(port)));
                            text.clear();
                        } else if text.len() > 256 * 1024 {
                            let _ = port_sender.take().map(|sender| sender.send(Err("The JavaScript adapter did not publish a valid listening address.".into())));
                        }
                    }
                    if let Some(sender) = port_sender {
                        let _ = sender.send(Err("The JavaScript adapter exited before publishing its listening address.".into()));
                    }
                });
                let stderr = child.stderr.take();
                let owner = self.owner.clone();
                let session = self.clone();
                tokio::spawn(async move {
                    tokio::join!(owner.pump(stderr, "adapter"), async {
                        tokio::select! {
                            _ = child.wait() => {}
                            () = owner.cancelled() => crate::process::kill_tree(&mut child, Duration::from_millis(1500)).await,
                        }
                    });
                    if !session.closing() {
                        session.fail("The JavaScript debug server exited unexpectedly.");
                    }
                });
                let port = port.await.map_err(|_| error("The JavaScript adapter exited before publishing its listening address."))?.map_err(error)?;
                self.inner.lock().unwrap().server_port = Some(port);
                self.connect().await
            }
            _ if cfg!(windows) => {
                let adapter = executable(None, "codelldb.exe", &environment)?;
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
                let port = listener.local_addr()?.port();
                let spec = CommandSpec { argv: vec![adapter, "--connect".into(), port.to_string()], cwd: launch.cwd.clone(), environment };
                let child = self.owner.spawn(&spec, false)?;
                self.track_exit(child, "The native debug adapter exited unexpectedly.");
                let (socket, _) = tokio::select! {
                    accepted = listener.accept() => accepted?,
                    () = self.owner.cancelled() => return Err(error("Native debugging was stopped.")),
                };
                let (reader, writer) = socket.into_split();
                Ok(self.peer(reader, writer))
            }
            _ => {
                let gdb = executable(None, "gdb", &environment)?;
                let mut argv = vec![gdb, "--quiet".into(), "--nx".into(), "--interpreter=dap".into()];
                if self.resolved.terminal {
                    let tty = self.native_terminal().await?;
                    argv.extend(["-iex".to_owned(), format!("set inferior-tty {tty}")]);
                }
                let spec = CommandSpec { argv, cwd: launch.cwd.clone(), environment };
                let mut child = self.owner.spawn(&spec, true)?;
                let (stdout, stdin) = (child.stdout.take().ok_or_else(|| error("The debug adapter has no DAP transport."))?, child.stdin.take().ok_or_else(|| error("The debug adapter has no DAP transport."))?);
                let peer = self.peer(stdout, stdin);
                self.track_exit(child, "The debug adapter exited unexpectedly.");
                Ok(peer)
            }
        }
    }

    /// Another connection to the session's js-debug server.
    async fn connect(&self) -> HostResult<Arc<Peer>> {
        let port = self.inner.lock().unwrap().server_port.filter(|_| !self.closing()).ok_or_else(|| error("The owned JavaScript debug server is unavailable."))?;
        let socket = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        if self.closing() {
            return Err(error("Debugging was stopped."));
        }
        let (reader, writer) = socket.into_split();
        Ok(self.peer(reader, writer))
    }

    /// A terminal for a GDB inferior: a tiny Python program reports its terminal device and then waits.
    async fn native_terminal(self: &Arc<Self>) -> HostResult<String> {
        let python = executable(None, "python3", &Default::default())?;
        let script = "import os,signal\nsignal.signal(signal.SIGINT,signal.SIG_IGN)\nprint(os.ttyname(0),flush=True)\nwhile True: signal.pause()";
        let (sender, receiver) = tokio::sync::oneshot::channel::<String>();
        let sender = Mutex::new(Some(sender));
        let prefix = Mutex::new(String::new());
        let owner = self.owner.clone();
        let spec = CommandSpec { argv: vec![python, "-u".into(), "-c".into(), script.into()], cwd: self.resolved.launch.cwd.clone(), environment: Default::default() };
        let terminal = self.owner.terminal_with(&spec, self.cols, self.rows, Arc::new(move |text: String| {
            let mut pending = sender.lock().unwrap();
            if pending.is_none() {
                owner.emit("terminal", text);
                return;
            }
            let mut prefix = prefix.lock().unwrap();
            prefix.push_str(&text);
            if let Some(newline) = prefix.find('\n') {
                let tty = prefix[..newline].trim().to_owned();
                let rest = prefix[newline + 1..].to_owned();
                let _ = pending.take().map(|sender| sender.send(tty));
                owner.emit("terminal", rest);
            }
        }))?;
        self.inner.lock().unwrap().terminal = Some(terminal);
        let tty = tokio::time::timeout(Duration::from_secs(15), receiver).await.map_err(|_| error("The debug terminal did not report its device."))?.map_err(|_| error("The debug terminal closed before reporting its device."))?;
        let valid = tty.strip_prefix("/dev/pts/").is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()));
        if !valid {
            return Err(error("The debug terminal returned an invalid device."));
        }
        Ok(tty)
    }

    fn launch_arguments(&self) -> Value {
        let launch = &self.resolved.launch;
        let mut common = json!({
            "name": self.resolved.name, "program": launch.argv[0], "cwd": launch.cwd, "args": self.resolved.arguments(),
            "env": launch.environment, "stopOnEntry": self.stop_on_entry,
        });
        let fields = common.as_object_mut().unwrap();
        let program = self.resolved.program.display().to_string();
        match self.resolved.language.as_str() {
            "python" => {
                let module = self.resolved.configuration.get("pythonModule").and_then(Value::as_str);
                fields.insert("type".into(), json!("python"));
                match module {
                    Some(module) => {
                        fields.remove("program");
                        fields.insert("module".into(), json!(module));
                    }
                    None => {
                        fields.insert("program".into(), json!(program));
                    }
                }
                fields.insert("python".into(), json!([launch.argv[0]]));
                fields.insert("console".into(), json!(if self.resolved.terminal { "integratedTerminal" } else { "internalConsole" }));
                fields.insert("redirectOutput".into(), json!(!self.resolved.terminal));
                fields.insert("justMyCode".into(), json!(true));
                fields.insert("subProcess".into(), json!(false));
            }
            "javascript" | "typescript" => {
                let index = launch.argv.iter().position(|arg| *arg == program).unwrap_or(launch.argv.len());
                fields.insert("type".into(), json!("pwa-node"));
                fields.insert("program".into(), json!(program));
                fields.insert("runtimeExecutable".into(), json!(launch.argv[0]));
                fields.insert("runtimeArgs".into(), json!(launch.argv[1..index]));
                fields.insert("console".into(), json!(if self.resolved.terminal { "integratedTerminal" } else { "internalConsole" }));
                fields.insert("sourceMaps".into(), json!(true));
                fields.insert("autoAttachChildProcesses".into(), json!(false));
                fields.insert("outputCapture".into(), json!("std"));
            }
            _ if cfg!(windows) => {
                fields.insert("terminal".into(), json!(if self.resolved.terminal { "integrated" } else { "console" }));
            }
            _ => {
                fields.insert("stopOnEntry".into(), json!(false));
                fields.insert("stopAtBeginningOfMainSubprogram".into(), json!(self.stop_on_entry));
            }
        }
        common
    }

    fn is_active(&self, peer: &Arc<Peer>) -> bool {
        self.inner.lock().unwrap().active.as_ref().is_some_and(|active| active.id == peer.id)
    }

    fn is_root(&self, peer: &Arc<Peer>) -> bool {
        self.inner.lock().unwrap().root.as_ref().is_some_and(|root| root.id == peer.id)
    }

    async fn initialize(&self, peer: &Arc<Peer>, command: &str, arguments: Value) -> HostResult<()> {
        let work = async {
            let capabilities = peer
                .request(
                    "initialize",
                    json!({
                        "clientID": "rainy-ide", "clientName": "Rainy IDE", "adapterID": self.resolved.language, "pathFormat": "path",
                        "linesStartAt1": true, "columnsStartAt1": true, "supportsRunInTerminalRequest": true,
                        "supportsStartDebuggingRequest": true, "supportsVariablePaging": true, "supportsVariableType": true, "locale": "en-US",
                    }),
                )
                .await?;
            let configuration_done = capabilities["supportsConfigurationDoneRequest"] == true;
            if self.is_active(peer) {
                self.update(&[("capabilities", json!({ "configurationDone": configuration_done, "evaluate": true, "pause": true, "stepIn": true, "stepOut": true, "next": true }))]);
            }
            let owned_target = self.inner.lock().unwrap().server_port.is_some() && arguments["__pendingTargetId"].is_string();
            let receives_breakpoints = self.is_active(peer) || self.is_root(peer);
            if owned_target {
                peer.initialized_after(0).await?;
            }
            let bootstrap = peer.initializations();
            let launching = {
                let (peer, command, arguments) = (peer.clone(), command.to_owned(), arguments.clone());
                tokio::spawn(async move { peer.request(&command, arguments).await })
            };
            tokio::pin!(launching);
            tokio::select! {
                ready = peer.initialized_after(0) => ready?,
                launched = &mut launching => {
                    launched.map_err(|_| error("The debug launch failed."))??;
                    peer.initialized_after(0).await?;
                }
            }
            if receives_breakpoints {
                let sources = self.inner.lock().unwrap().sources.clone();
                for (path, lines) in &sources {
                    self.set_breakpoints(peer, path, lines).await?;
                }
            }
            if configuration_done {
                peer.request("configurationDone", json!({})).await?;
            }
            if owned_target {
                peer.initialized_after(bootstrap).await?;
            }
            if !launching.is_finished() {
                launching.await.map_err(|_| error("The debug launch failed."))??;
            }
            if owned_target && receives_breakpoints {
                let sources = self.inner.lock().unwrap().sources.clone();
                for (path, lines) in &sources {
                    self.set_breakpoints(peer, path, lines).await?;
                }
            }
            HostResult::Ok(())
        };
        tokio::time::timeout(STARTUP_TIMEOUT, work).await.map_err(|_| error("A debug adapter connection did not finish initialization before its deadline."))?
    }

    async fn set_breakpoints(&self, peer: &Arc<Peer>, path: &Path, lines: &[u64]) -> HostResult<Value> {
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let result = peer
            .request("setBreakpoints", json!({ "source": { "name": name, "path": path }, "breakpoints": lines.iter().map(|line| json!({ "line": line })).collect::<Vec<_>>(), "sourceModified": false }))
            .await?;
        let answered = result["breakpoints"].as_array().cloned().unwrap_or_default();
        if answered.len() > lines.len() {
            return Err(error("The adapter returned more breakpoints than requested."));
        }
        let path_text = path.display().to_string();
        // js-debug returns an empty list for an unchanged set; later events verify the existing ids.
        if self.inner.lock().unwrap().server_port.is_some() && answered.is_empty() && !lines.is_empty() {
            let state = self.state.lock().unwrap();
            let unchanged: Vec<Value> = state
                .get("breakpoints")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|breakpoint| breakpoint["path"] == path_text.as_str() && breakpoint["requestedLine"].as_u64().is_some_and(|line| lines.contains(&line)))
                .cloned()
                .collect();
            if unchanged.len() == lines.len() {
                return Ok(Value::Array(unchanged));
            }
        }
        let breakpoints: Vec<Value> = answered
            .iter()
            .enumerate()
            .map(|(index, breakpoint)| {
                let mut entry = json!({ "path": path_text, "requestedLine": lines[index], "verified": breakpoint["verified"] == true });
                for key in ["id", "line", "message"] {
                    if !breakpoint[key].is_null() {
                        entry[key] = breakpoint[key].clone();
                    }
                }
                entry
            })
            .collect();
        if !self.closing() && self.is_active(peer) {
            let mut all: Vec<Value> = self.state.lock().unwrap().get("breakpoints").and_then(Value::as_array).cloned().unwrap_or_default();
            all.retain(|breakpoint| breakpoint["path"] != path_text.as_str());
            all.extend(breakpoints.clone());
            self.update(&[("breakpoints", Value::Array(all))]);
        }
        Ok(Value::Array(breakpoints))
    }

    /// A debugger operation of the page.
    pub async fn handle(self: &Arc<Self>, request: &Value) -> HostResult<Value> {
        let op = request["op"].as_str().unwrap_or_default();
        if self.closing() {
            return Err(error("This debug session has ended."));
        }
        if op == "debug.input" || op == "debug.resize" {
            let terminal = self.inner.lock().unwrap().terminal.clone().ok_or_else(|| error("This debug session has no interactive terminal."))?;
            if op == "debug.input" {
                terminal.write(request["data"].as_str().unwrap_or_default())?;
            } else {
                terminal.resize(request["cols"].as_u64().unwrap_or(80) as u16, request["rows"].as_u64().unwrap_or(24) as u16);
            }
            return Ok(json!({ "ok": true }));
        }
        let peer = self.inner.lock().unwrap().active.clone().ok_or_else(|| error("The debug adapter is still starting."))?;
        match op {
            "debug.setBreakpoints" => {
                let source = &request["source"];
                let own_root = self.resolved.configuration["rootId"].as_str().unwrap_or("primary");
                if source["rootId"].as_str().unwrap_or("primary") != own_root {
                    return Err(error("The breakpoint belongs to another project root."));
                }
                let path = self.source_path(source["path"].as_str().unwrap_or_default())?;
                let lines: Vec<u64> = source["lines"].as_array().into_iter().flatten().filter_map(Value::as_u64).collect();
                self.remember_source(path.clone(), lines.clone());
                self.set_breakpoints(&peer, &path, &lines).await
            }
            "debug.threads" => Ok(peer.request("threads", json!({})).await?["threads"].clone()),
            "debug.stack" => {
                let result = peer.request("stackTrace", json!({ "threadId": request["threadId"], "startFrame": 0, "levels": MAX_STACK_FRAMES })).await?;
                let frames: Vec<Value> = result["stackFrames"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(MAX_STACK_FRAMES)
                    .map(|frame| {
                        let mut entry = json!({ "id": frame["id"], "name": frame["name"], "line": frame["line"], "column": frame["column"] });
                        if let Some(path) = frame["source"]["path"].as_str().filter(|path| !path.is_empty()) {
                            entry["path"] = json!(path);
                        }
                        entry
                    })
                    .collect();
                Ok(Value::Array(frames))
            }
            "debug.scopes" => {
                let scopes = peer.request("scopes", json!({ "frameId": request["frameId"] })).await?["scopes"].clone();
                for scope in scopes.as_array().into_iter().flatten() {
                    self.remember_count(scope);
                }
                Ok(scopes)
            }
            "debug.variables" => {
                let reference = request["variablesReference"].as_i64().unwrap_or(0);
                let start = request["start"].as_u64().unwrap_or(0) as usize;
                let limit = request["count"].as_u64().map_or(MAX_VARIABLES, |count| (count as usize).min(MAX_VARIABLES));
                let total = self.inner.lock().unwrap().variable_counts.get(&reference).copied();
                let count = match (self.native(), total) {
                    (true, Some(total)) => limit.min((total as usize).saturating_sub(start)),
                    _ => limit,
                };
                if count == 0 {
                    return Ok(json!([]));
                }
                let unpaged = self.native() && total.is_none();
                let arguments = if unpaged { json!({ "variablesReference": reference }) } else { json!({ "variablesReference": reference, "start": start, "count": count }) };
                let variables = peer.request("variables", arguments).await?["variables"].as_array().cloned().unwrap_or_default();
                let page: Vec<Value> = if unpaged { variables.into_iter().skip(start).take(limit).collect() } else { variables.into_iter().take(limit).collect() };
                for variable in &page {
                    self.remember_count(variable);
                }
                Ok(Value::Array(page))
            }
            "debug.evaluate" => {
                if self.native() && request["context"] == "repl" {
                    return Err(error("Native debugging accepts watch expressions; GDB command execution is unavailable."));
                }
                let mut arguments = json!({ "expression": request["expression"], "context": request["context"] });
                if !request["frameId"].is_null() {
                    arguments["frameId"] = request["frameId"].clone();
                }
                peer.request("evaluate", arguments).await
            }
            "debug.control" => {
                let action = request["action"].as_str().unwrap_or_default();
                if !["continue", "pause", "next", "stepIn", "stepOut"].contains(&action) {
                    return Err(HostError::invalid("Unknown debug action."));
                }
                let before = self.inner.lock().unwrap().stop_generation;
                peer.request(action, json!({ "threadId": request["threadId"] })).await?;
                if self.native() && action != "pause" && !self.closing() && before == self.inner.lock().unwrap().stop_generation {
                    self.inner.lock().unwrap().variable_counts.clear();
                    self.update(&[("phase", json!("running")), ("threadId", Value::Null), ("reason", Value::Null)]);
                }
                Ok(json!({ "ok": true }))
            }
            _ => Err(HostError::invalid("Unknown debug operation.")),
        }
    }

    /// Requests the adapter sends: a terminal for the program, or another js-debug target.
    async fn reverse(self: &Arc<Self>, command: &str, body: Value) -> HostResult<Value> {
        if self.closing() {
            return Err(error("Debugging was stopped."));
        }
        if command == "runInTerminal" {
            if self.inner.lock().unwrap().terminal.is_some() {
                return Err(error("This debug session already owns an interactive terminal."));
            }
            let args: Vec<String> = body["args"].as_array().into_iter().flatten().filter_map(|arg| arg.as_str().map(str::to_owned)).collect();
            if args.is_empty() || args.iter().any(|arg| arg.contains('\0')) || body["argsCanBeInterpretedByShell"] == true || body["kind"].as_str().is_some_and(|kind| kind != "integrated") {
                return Err(error("Unsupported terminal request."));
            }
            let cwd = workspace_path(&self.resolved.root, body["cwd"].as_str().unwrap_or("."), Some("directory"))?;
            let mut environment = std::collections::BTreeMap::new();
            for (name, value) in body["env"].as_object().into_iter().flatten() {
                let valid = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if !valid || name.to_uppercase().starts_with("RAINY_") {
                    return Err(error("The adapter requested a reserved terminal environment variable."));
                }
                environment.insert(name.clone(), value.as_str().unwrap_or_default().to_owned());
            }
            let program = executable(Some(&args[0]).filter(|first| Path::new(first).is_absolute()).map(String::as_str), &args[0], &environment)?;
            let mut argv = vec![program];
            argv.extend(args[1..].iter().cloned());
            let terminal = self.owner.terminal(&CommandSpec { argv, cwd, environment }, self.cols, self.rows)?;
            self.inner.lock().unwrap().terminal = Some(terminal.clone());
            let session = self.clone();
            let watched = terminal.clone();
            tokio::spawn(async move {
                let outcome = watched.wait().await;
                let has_exit = session.state.lock().unwrap().contains_key("exit");
                if !has_exit {
                    session.update(&[("exit", json!({ "exitCode": outcome.exit_code, "signal": outcome.signal, "stopped": session.stopped.load(Ordering::Relaxed) }))]);
                }
            });
            return Ok(json!({ "processId": terminal.pid, "shellProcessId": terminal.pid }));
        }
        if command == "startDebugging" && self.inner.lock().unwrap().server_port.is_some() {
            let configuration = &body["configuration"];
            let (Some(kind), Some(name), Some(target)) = (configuration["type"].as_str(), configuration["name"].as_str(), configuration["__pendingTargetId"].as_str()) else {
                return Err(error("Unsupported debug target request."));
            };
            let request = body["request"].as_str().filter(|request| *request == "launch" || *request == "attach").ok_or_else(|| error("Unsupported debug target request."))?.to_owned();
            let main_target = {
                let mut inner = self.inner.lock().unwrap();
                if inner.targets >= MAX_DEBUG_TARGETS {
                    drop(inner);
                    self.fail("The launched JavaScript program exceeds the configured debug target limit.");
                    return Err(error("The launched JavaScript program exceeds the configured debug target limit."));
                }
                inner.targets += 1;
                let main = !inner.child_claimed;
                inner.child_claimed = true;
                main
            };
            let session = self.clone();
            let arguments = json!({ "type": kind, "name": name, "__pendingTargetId": target });
            let task = tokio::spawn(async move {
                let peer = session.connect().await?;
                if main_target {
                    session.inner.lock().unwrap().active = Some(peer.clone());
                }
                let result = session.initialize(&peer, &request, arguments).await;
                if let Err(failure) = &result {
                    if !session.closing() {
                        session.fail(&failure.message);
                    }
                }
                result
            });
            if main_target {
                *self.child_initialization.lock().unwrap() = Some(task);
            }
            return Ok(json!({}));
        }
        Err(error(format!("Unsupported debug adapter request: {command}")))
    }

    fn event(self: &Arc<Self>, peer: &Arc<Peer>, event: &str, body: &Value) {
        if self.closing() {
            return;
        }
        match event {
            "output" => {
                let category = body["category"].as_str();
                if category != Some("telemetry") {
                    let stream = match category {
                        Some("stderr") => "stderr",
                        Some("stdout") => "stdout",
                        _ => "adapter",
                    };
                    self.owner.emit(stream, body["output"].as_str().unwrap_or_default().to_owned());
                }
                return;
            }
            "exited" => {
                if self.is_root(peer) || self.is_active(peer) {
                    self.update(&[("exit", json!({ "exitCode": body["exitCode"], "signal": null, "stopped": self.stopped.load(Ordering::Relaxed) }))]);
                }
                return;
            }
            "terminated" => {
                self.inner.lock().unwrap().terminated.insert(peer.id);
                let own_server = self.inner.lock().unwrap().server_port.is_some();
                if !own_server || self.is_root(peer) {
                    let session = self.clone();
                    tokio::spawn(async move { session.finish().await });
                }
                return;
            }
            _ => {}
        }
        if !self.is_active(peer) {
            return;
        }
        match event {
            "stopped" => {
                {
                    let mut inner = self.inner.lock().unwrap();
                    inner.stop_generation += 1;
                    inner.variable_counts.clear();
                }
                self.update(&[("phase", json!("paused")), ("reason", body["reason"].clone()), ("threadId", body["threadId"].clone())]);
            }
            "continued" => {
                self.inner.lock().unwrap().variable_counts.clear();
                self.update(&[("phase", json!("running")), ("threadId", Value::Null), ("reason", Value::Null)]);
            }
            "breakpoint" => {
                let breakpoint = &body["breakpoint"];
                let Some(id) = breakpoint["id"].as_i64() else { return };
                let mut all: Vec<Value> = self.state.lock().unwrap().get("breakpoints").and_then(Value::as_array).cloned().unwrap_or_default();
                let mut changed = false;
                for entry in &mut all {
                    if entry["id"].as_i64() == Some(id) {
                        entry["verified"] = json!(breakpoint["verified"] == true);
                        for key in ["line", "message"] {
                            match breakpoint.get(key).filter(|value| !value.is_null()) {
                                Some(value) => entry[key] = value.clone(),
                                None => {
                                    entry.as_object_mut().map(|object| object.remove(key));
                                }
                            }
                        }
                        changed = true;
                    }
                }
                if changed {
                    self.update(&[("breakpoints", Value::Array(all))]);
                }
            }
            _ => {}
        }
    }

    /// Stop this session and wait until everything it started has stopped.
    pub async fn stop(self: &Arc<Self>) {
        if !self.closing() {
            self.stopped.store(true, Ordering::Relaxed);
            self.update(&[("phase", json!("stopping"))]);
        }
        self.finish().await;
    }

    /// A build or startup failure the controller saw.
    pub fn fail(self: &Arc<Self>, message: &str) {
        if self.closing() {
            return;
        }
        self.update(&[("phase", json!("failed")), ("error", json!(crate::text::truncate_chars(message, MAX_ERROR_CHARS)))]);
        let session = self.clone();
        tokio::spawn(async move { session.finish().await });
    }

    async fn finish(self: &Arc<Self>) {
        if self.closing.swap(true, Ordering::Relaxed) {
            self.done().await;
            return;
        }
        let peers: Vec<Arc<Peer>> = self.inner.lock().unwrap().peers.clone();
        for peer in &peers {
            peer.close();
        }
        self.owner.close().await;
        if self.phase() != "failed" {
            let exit = self.state.lock().unwrap().get("exit").cloned().unwrap_or(Value::Null);
            self.update(&[
                ("phase", json!("terminated")),
                ("threadId", Value::Null),
                ("reason", Value::Null),
                ("exit", json!({ "exitCode": exit["exitCode"], "signal": exit["signal"], "stopped": self.stopped.load(Ordering::Relaxed) })),
            ]);
        }
        let _ = self.done.send(true);
    }
}
