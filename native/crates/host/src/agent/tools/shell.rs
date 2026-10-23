//! `bash` (WSL, Linux, SSH) and `pwsh` (Windows): one fresh shell per command, plus `command_output` and
//! `stop_command` for commands left running in the background.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;
use tokio_util::sync::CancellationToken;

use super::jobs::{Job, Jobs, Spec};
use super::{Tool, ToolContext, ToolOutput, Updates, params, spec};
use crate::agent::llm::ToolSpec;

/// Timeout when the model sets none.
pub const DEFAULT_TIMEOUT_MS: u64 = 60_000;
/// Longest timeout a command may ask for.
pub const MAX_TIMEOUT_MS: u64 = 600_000;
/// Bytes of each stream kept in memory (the tail).
const TAIL_BYTES: usize = 64_000;
/// Bytes of each stream saved to the full-output file.
const SPILL_BYTES: usize = 64 * 1024 * 1024;
const ENCODING_PREAMBLE: &str = "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [System.Text.UTF8Encoding]::new($false); ";
/// A background command is watched this long before its call returns, so an immediate failure is reported at once.
const BACKGROUND_SETTLE: Duration = Duration::from_millis(600);

/// One output stream: a bounded tail in memory, and the full text in a file once it grows large.
struct Collector {
    tail: Vec<u8>,
    total: usize,
    file: Option<(PathBuf, std::fs::File)>,
    spill: Box<dyn Fn() -> Option<PathBuf> + Send>,
}

impl Collector {
    fn new(spill: impl Fn() -> Option<PathBuf> + Send + 'static) -> Self {
        Self { tail: Vec::new(), total: 0, file: None, spill: Box::new(spill) }
    }

    fn push(&mut self, chunk: &[u8]) {
        let before = self.total;
        self.total += chunk.len();
        if self.file.is_none() && self.total > TAIL_BYTES {
            if let Some(path) = (self.spill)()
                && let Ok(mut file) = std::fs::File::create(&path)
            {
                let _ = file.write_all(&self.tail);
                self.file = Some((path, file));
            }
        }
        if let Some((_, file)) = &mut self.file
            && before < SPILL_BYTES
        {
            let _ = file.write_all(&chunk[..chunk.len().min(SPILL_BYTES - before)]);
        }
        self.tail.extend_from_slice(chunk);
        if self.tail.len() > TAIL_BYTES * 2 {
            let excess = self.tail.len() - TAIL_BYTES;
            self.tail.drain(..excess);
        }
    }

    fn text(&self) -> String {
        let mut bytes: &[u8] = &self.tail;
        if bytes.len() > TAIL_BYTES {
            bytes = &bytes[bytes.len() - TAIL_BYTES..];
            // The cut may fall inside a UTF-8 character; its remaining bytes are dropped.
            let skip = bytes.iter().take(3).take_while(|byte| *byte & 0xc0 == 0x80).count();
            bytes = &bytes[skip..];
        }
        let text = crate::process::console_text(bytes);
        match &self.file {
            Some((path, _)) => format!("{text}\n[output truncated; full output: {}]", path.display()),
            None => text,
        }
    }
}

/// The model-facing result: the output (or `(no output)`), then timeout and exit markers on their own lines.
pub fn render(stdout: &str, stderr: &str, exit_code: Option<i32>, signal: Option<&str>, timed_out: bool, timeout_ms: u64) -> String {
    let mut body = stdout.to_owned();
    if !stderr.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&format!("[stderr]\n{stderr}"));
    }
    if body.is_empty() {
        body = "(no output)".into();
    }
    let mut markers = Vec::new();
    if timed_out {
        markers.push(format!("[timed out after {timeout_ms}ms]"));
    }
    match (signal, exit_code) {
        (Some(signal), _) => markers.push(format!("[killed by signal: {signal}]")),
        (None, Some(0)) => {}
        (None, code) => markers.push(format!("[exit code: {}]", code.map_or_else(|| "unknown".to_owned(), |code| code.to_string()))),
    }
    if markers.is_empty() {
        return body;
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body + &markers.join("\n")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShellParams {
    command: String,
    description: String,
    timeout_ms: Option<f64>,
    workdir: Option<String>,
    background: Option<bool>,
}

pub struct Shell {
    context: ToolContext,
    spec: ToolSpec,
    windows: bool,
}

impl Shell {
    pub fn new(context: ToolContext, windows: bool) -> Self {
        let (name, label) = if windows { ("pwsh", "PowerShell") } else { ("bash", "Bash") };
        let spec = spec(
            name,
            &format!("Run a command in the project {label} shell and inspect its exit status. Search with the grep and glob tools rather than shell commands."),
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": if windows { "The PowerShell command to execute." } else { "The bash command to execute." } },
                    "description": { "type": "string", "description": "Clear, concise description of what this command does in active voice, 5-10 words (shown in the UI). Examples: \"ls\" → \"List files in current directory\"; \"git status\" → \"Show working tree status\"; \"npm install\" → \"Install package dependencies\"." },
                    "timeoutMs": { "type": "number", "description": "Timeout in milliseconds. The executor applies its configured default and cap, and kills the command on expiry." },
                    "workdir": { "type": "string", "description": "Working directory for this command. Defaults to the session workspace; a relative path is resolved against it." },
                    "background": { "type": "boolean", "description": "Keep the command running after this call returns, for a development server, a watcher or another command that does not end by itself. The call answers with a command id; read its new output with command_output and stop it with stop_command. The timeout does not apply." },
                },
                "required": ["command", "description"],
            }),
        );
        Self { context, spec, windows }
    }

    /// The program, its arguments and its environment for one command.
    fn prepare(&self, command: &str, cwd: &std::path::Path) -> Result<(PathBuf, Vec<String>, std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>), String> {
        let runtime = self.context.runtime.resolve(cwd).map_err(|error| error.message)?;
        let mut env = crate::process::child_env(&runtime.environment);
        for (name, value) in [("NO_COLOR", "1"), ("TERM", "dumb"), ("PAGER", "cat"), ("GIT_PAGER", "cat")] {
            env.insert(name.into(), value.into());
        }
        env.insert("RAINY_SESSION_ID".into(), self.context.session_id.clone().into());
        if self.windows {
            let (path, _) = crate::process::resolve_powershell(self.context.pwsh_path.as_deref(), runtime.powershell.as_deref());
            let args = ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"].iter().map(|arg| (*arg).to_owned()).chain([format!("{ENCODING_PREAMBLE}{command}")]).collect();
            Ok((path, args, env))
        } else {
            Ok((PathBuf::from("bash"), vec!["-c".into(), command.into()], env))
        }
    }

    async fn run(&self, args: Value, cancel: CancellationToken, updates: Updates) -> Result<ToolOutput, String> {
        let params: ShellParams = params(args)?;
        if params.command.trim().is_empty() {
            return Err("invalid command: expected a non-empty string".into());
        }
        if params.description.trim().is_empty() {
            return Err("invalid description: expected a non-empty string".into());
        }
        let timeout_ms = params.timeout_ms.map_or(DEFAULT_TIMEOUT_MS, |ms| ms.floor().clamp(1.0, MAX_TIMEOUT_MS as f64) as u64);
        let cwd = match &params.workdir {
            Some(workdir) => crate::paths::normalize(&self.context.cwd.join(workdir)),
            None => self.context.cwd.clone(),
        };
        let (program, program_args, env) = self.prepare(&params.command, &cwd)?;
        let started = Instant::now();
        let cwd_text = cwd.to_string_lossy().into_owned();
        let details = |exit_code: Option<i32>, signal: Option<&str>, timed_out: bool, job: Option<&str>| {
            let mut details = json!({
                "command": params.command, "description": params.description, "cwd": cwd_text, "exitCode": exit_code, "signal": signal,
                "timedOut": timed_out, "durationMs": started.elapsed().as_millis() as u64,
            });
            if let Some(job) = job {
                details["jobId"] = json!(job);
            }
            details
        };
        if params.background == Some(true) {
            let jobs = self.context.jobs.as_ref().ok_or("background commands are unavailable in this context; run the command without background")?;
            let job = jobs.start(Spec { program, args: program_args, cwd: cwd.clone(), env, command: params.command.clone(), description: params.description.clone() })?;
            tokio::time::sleep(BACKGROUND_SETTLE).await;
            let (settled, text) = jobs.read(&job.id)?;
            let head = if settled.state == "exited" {
                let how = settled.signal.as_ref().map_or_else(|| format!("with code {}", settled.exit_code.map_or_else(|| "unknown".to_owned(), |code| code.to_string())), |signal| format!("on signal {signal}"));
                format!("[{} exited {how} right after starting]", job.id)
            } else {
                format!("[{} is running in the background; read its new output with command_output, stop it with stop_command]", job.id)
            };
            let body = if text.trim().is_empty() { "(no output yet)".to_owned() } else { text };
            let tool = self.spec.name.clone();
            return Ok(ToolOutput::text(self.context.limit_output(&tool, format!("{head}\n{body}")), details(settled.exit_code, settled.signal.as_deref(), false, Some(&job.id))));
        }
        let mut child = crate::process::spawn(&program, &program_args, &cwd, &env, false).map_err(|error| format!("could not start {}: {error}", program.display()))?;
        let spill_dir = self.context.spill_dir.clone();
        let tool = self.spec.name.clone();
        let spill = move |stream: &'static str| {
            let (dir, tool) = (spill_dir.clone(), tool.clone());
            move || {
                crate::store::create_private_dir(&dir).ok()?;
                Some(dir.join(format!("{}-{tool}-{stream}.txt", &uuid::Uuid::new_v4().simple().to_string()[..12])))
            }
        };
        let stdout = Arc::new(Mutex::new(Collector::new(spill("stdout"))));
        let stderr = Arc::new(Mutex::new(Collector::new(spill("stderr"))));
        let last_update = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1)));
        let pump = |stream: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>, target: Arc<Mutex<Collector>>| {
            let (stdout, stderr, updates, last_update, details) = (stdout.clone(), stderr.clone(), updates.clone(), last_update.clone(), details(None, None, false, None));
            async move {
                let Some(mut stream) = stream else { return };
                let mut buffer = [0u8; 16384];
                while let Ok(read) = stream.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    target.lock().unwrap().push(&buffer[..read]);
                    let mut last = last_update.lock().unwrap();
                    if last.elapsed() >= Duration::from_millis(200) {
                        *last = Instant::now();
                        let text = render(&stdout.lock().unwrap().text(), &stderr.lock().unwrap().text(), Some(0), None, false, timeout_ms);
                        updates(ToolOutput::text(text, details.clone()));
                    }
                }
            }
        };
        let out = pump(child.stdout.take().map(|out| Box::new(out) as Box<dyn tokio::io::AsyncRead + Unpin + Send>), stdout.clone());
        let err = pump(child.stderr.take().map(|err| Box::new(err) as Box<dyn tokio::io::AsyncRead + Unpin + Send>), stderr.clone());
        let mut timed_out = false;
        let status = {
            let work = async { tokio::join!(out, err, child.wait()).2 };
            tokio::pin!(work);
            tokio::select! {
                status = &mut work => status.ok(),
                () = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                    timed_out = true;
                    None
                }
                () = cancel.cancelled() => None,
            }
        };
        let status = match status {
            Some(status) => Some(status),
            None => {
                crate::process::kill_tree(&mut child, Duration::from_secs(3)).await;
                child.wait().await.ok()
            }
        };
        if cancel.is_cancelled() {
            return Err("tool call aborted".into());
        }
        let (exit_code, signal) = status.as_ref().map_or((None, None), crate::process::outcome);
        let text = render(&stdout.lock().unwrap().text(), &stderr.lock().unwrap().text(), exit_code, signal.as_deref(), timed_out, timeout_ms);
        let tool = self.spec.name.clone();
        Ok(ToolOutput::text(self.context.limit_output(&tool, text), details(exit_code, signal.as_deref(), timed_out, None)))
    }
}

impl Tool for Shell {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, cancel: CancellationToken, updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(self.run(args, cancel, updates))
    }
}

fn job_details(job: &Job) -> Value {
    json!({ "id": job.id, "command": job.command, "description": job.description, "cwd": job.cwd, "state": job.state, "exitCode": job.exit_code, "signal": job.signal })
}

fn jobs_of(context: &ToolContext) -> Result<Arc<Jobs>, String> {
    context.jobs.clone().ok_or_else(|| "background commands are unavailable in this context".into())
}

#[derive(Deserialize)]
struct IdParams {
    id: String,
}

pub struct CommandOutput {
    context: ToolContext,
    spec: ToolSpec,
}

impl CommandOutput {
    pub fn new(context: ToolContext) -> Self {
        let spec = spec(
            "command_output",
            "Read the new output of a command started in the background, and whether it is still running. Output already read is not repeated. Wait between reads rather than polling in a loop.",
            json!({
                "type": "object",
                "properties": { "id": { "type": "string", "description": "Id of the background command, as the command that started it reported (for example \"cmd-1\")." } },
                "required": ["id"],
            }),
        );
        Self { context, spec }
    }
}

impl Tool for CommandOutput {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: IdParams = params(args)?;
            let (job, text) = jobs_of(&self.context)?.read(&params.id)?;
            let state = if job.state == "running" {
                format!("[{} is still running]", job.id)
            } else {
                match &job.signal {
                    Some(signal) => format!("[{} was stopped on signal {signal}]", job.id),
                    None => format!("[{} exited with code {}]", job.id, job.exit_code.map_or_else(|| "unknown".to_owned(), |code| code.to_string())),
                }
            };
            let body = if text.trim().is_empty() { "(no new output)".to_owned() } else { text };
            Ok(ToolOutput::text(self.context.limit_output("command_output", format!("{body}\n{state}")), job_details(&job)))
        })
    }
}

pub struct StopCommand {
    context: ToolContext,
    spec: ToolSpec,
}

impl StopCommand {
    pub fn new(context: ToolContext) -> Self {
        let spec = spec(
            "stop_command",
            "Stop a command running in the background. Stop every background command you started once it is no longer needed.",
            json!({ "type": "object", "properties": { "id": { "type": "string", "description": "Id of the background command to stop." } }, "required": ["id"] }),
        );
        Self { context, spec }
    }
}

impl Tool for StopCommand {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: IdParams = params(args)?;
            let jobs = jobs_of(&self.context)?;
            let job = jobs.stop(&params.id).await?;
            let (_, text) = jobs.read(&job.id)?;
            let tail = if text.trim().is_empty() { String::new() } else { format!("\nIts last output:\n{text}") };
            Ok(ToolOutput::text(format!("[{} stopped]{tail}", job.id), job_details(&job)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_like_3x() {
        assert_eq!(render("", "", Some(0), None, false, 1000), "(no output)");
        assert_eq!(render("out", "bad", Some(2), None, false, 1000), "out\n[stderr]\nbad\n[exit code: 2]");
        assert_eq!(render("x\n", "", None, None, true, 5), "x\n[timed out after 5ms]\n[exit code: unknown]");
    }

    #[tokio::test]
    async fn runs_a_command() {
        let root = std::env::temp_dir().join(format!("rainy-shell-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let shell = Shell::new(crate::agent::tools::test_context(&root), cfg!(windows));
        let command = if cfg!(windows) { "Write-Output hello; exit 3" } else { "echo hello; exit 3" };
        let output = shell.run(json!({ "command": command, "description": "Say hello" }), CancellationToken::new(), super::super::no_updates()).await.unwrap();
        let rainy_protocol::chat::ResultContent::Text { text } = &output.content[0] else { panic!() };
        assert!(text.starts_with("hello") && text.ends_with("[exit code: 3]"), "{text}");
        let _ = std::fs::remove_dir_all(root);
    }
}
