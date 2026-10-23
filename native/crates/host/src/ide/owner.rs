//! One owner for every process of an IDE operation (3.x `IdeProcessOwner`): it admits processes and terminals while the
//! operation runs, drains their output, and on close stops every process tree it started.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::AsyncReadExt as _;
use tokio::process::{Child, ChildStderr, ChildStdout};

use super::pty::{Outcome, Pty, Utf8Stream};
use super::resolve::CommandSpec;
use crate::error::{HostError, HostResult};
use crate::process::{child_env, kill_pid_tree};

pub type Output = Arc<dyn Fn(&'static str, String) + Send + Sync>;

pub struct Owner {
    output: Output,
    closing: AtomicBool,
    processes: Mutex<Vec<u32>>,
    terminals: Mutex<Vec<Arc<Pty>>>,
    stopped: tokio_util::sync::CancellationToken,
}

fn stopped_error() -> HostError {
    HostError::new("execution-error", "The IDE operation is stopping or closed.")
}

impl Owner {
    pub fn new(output: Output) -> Arc<Self> {
        Arc::new(Self { output, closing: AtomicBool::new(false), processes: Mutex::default(), terminals: Mutex::default(), stopped: tokio_util::sync::CancellationToken::new() })
    }

    /// Completes once the owner closes.
    pub async fn cancelled(&self) {
        self.stopped.cancelled().await;
    }

    pub fn closing(&self) -> bool {
        self.closing.load(Ordering::Relaxed)
    }

    pub fn emit(&self, stream: &'static str, text: String) {
        if !text.is_empty() {
            (self.output)(stream, text);
        }
    }

    /// Start a process with piped output; `stdin` keeps its input open.
    pub fn spawn(&self, spec: &CommandSpec, stdin: bool) -> HostResult<Child> {
        if self.closing() {
            return Err(stopped_error());
        }
        let (program, args) = spec.argv.split_first().ok_or_else(|| HostError::invalid("A command needs a program."))?;
        let mut command = crate::process::command(program, args, &spec.cwd, &child_env(&spec.environment));
        command.stdin(if stdin { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = command.spawn().map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::NotFound { "dependency-unavailable" } else { "execution-error" };
            HostError::new(code, format!("{program}: {error}"))
        })?;
        if let Some(pid) = child.id() {
            self.processes.lock().unwrap().push(pid);
        }
        Ok(child)
    }

    /// Start a program in a terminal.
    pub fn terminal(&self, spec: &CommandSpec, cols: u16, rows: u16) -> HostResult<Arc<Pty>> {
        if self.closing() {
            return Err(stopped_error());
        }
        let output = self.output.clone();
        let terminal = Pty::spawn(&spec.argv, &spec.cwd, &child_env(&spec.environment), cols, rows, Arc::new(move |text| output("terminal", text)))?;
        self.terminals.lock().unwrap().push(terminal.clone());
        Ok(terminal)
    }

    /// Start a program in a terminal whose output goes to `output` instead of the terminal channel.
    pub fn terminal_with(&self, spec: &CommandSpec, cols: u16, rows: u16, output: Arc<dyn Fn(String) + Send + Sync>) -> HostResult<Arc<Pty>> {
        if self.closing() {
            return Err(stopped_error());
        }
        let terminal = Pty::spawn(&spec.argv, &spec.cwd, &child_env(&spec.environment), cols, rows, output)?;
        self.terminals.lock().unwrap().push(terminal.clone());
        Ok(terminal)
    }

    /// Drain a stream as text into one output channel.
    pub async fn pump(&self, stream: Option<impl tokio::io::AsyncRead + Unpin>, channel: &'static str) {
        let Some(mut stream) = stream else { return };
        let mut decoder = Utf8Stream::default();
        let mut buffer = [0u8; 16384];
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => self.emit(channel, decoder.push(&buffer[..read])),
            }
        }
        self.emit(channel, decoder.finish());
    }

    /// Wait for a process and its output; the tree is stopped once the process itself ended.
    pub async fn wait_process(&self, child: &mut Child, stdout: Option<ChildStdout>, stderr: Option<ChildStderr>) -> Outcome {
        let pid = child.id();
        let (_, _, status) = tokio::join!(self.pump(stdout, "stdout"), self.pump(stderr, "stderr"), child.wait());
        if let Some(pid) = pid {
            self.processes.lock().unwrap().retain(|known| *known != pid);
        }
        match status {
            Ok(status) => {
                let (exit_code, signal) = crate::process::outcome(&status);
                Outcome { exit_code, signal }
            }
            Err(_) => Outcome::default(),
        }
    }

    /// Run one build command to its end.
    pub async fn command(&self, spec: &CommandSpec) -> HostResult<Outcome> {
        let mut child = self.spawn(spec, false)?;
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        tokio::select! {
            outcome = self.wait_process(&mut child, stdout, stderr) => Ok(outcome),
            () = self.stopped.cancelled() => {
                crate::process::kill_tree(&mut child, std::time::Duration::from_millis(1500)).await;
                Ok(Outcome::default())
            }
        }
    }

    /// Stop admission and every process tree; terminals end too.
    pub async fn close(&self) {
        if self.closing.swap(true, Ordering::Relaxed) {
            return;
        }
        self.stopped.cancel();
        let pids: Vec<u32> = std::mem::take(&mut *self.processes.lock().unwrap());
        let terminals: Vec<Arc<Pty>> = std::mem::take(&mut *self.terminals.lock().unwrap());
        let processes = futures::future::join_all(pids.into_iter().map(kill_pid_tree));
        let terminals = futures::future::join_all(terminals.iter().map(|terminal| terminal.terminate()));
        tokio::join!(processes, terminals);
    }
}
