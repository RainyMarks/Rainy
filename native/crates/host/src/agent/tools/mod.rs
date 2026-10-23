//! The model's tools (3.x `tools/`): what every tool shares — its interface, path resolution, read-before-write
//! tracking and output limits — and the built-in set of a chat.

pub mod browser;
pub mod encoding;
pub mod jobs;
pub mod read;
pub mod schema;
pub mod search;
pub mod shell;
pub mod task;
pub mod web;
pub mod write;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use rainy_protocol::chat::ResultContent;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::budget::estimate_text;
use super::llm::ToolSpec;
use crate::runtime::Runtime;

/// What a tool call produced: content for the model, details for the page's card.
#[derive(Clone, Debug, Default)]
pub struct ToolOutput {
    pub content: Vec<ResultContent>,
    pub details: Option<Value>,
    pub is_error: bool,
    /// End the run after this step when every call of the step asks for it.
    pub terminate: bool,
}

impl ToolOutput {
    /// A text-only result.
    pub fn text(text: impl Into<String>, details: Value) -> Self {
        Self { content: vec![ResultContent::Text { text: text.into() }], details: Some(details), is_error: false, terminate: false }
    }

    /// A failed call, as the model reads it.
    pub fn error(message: impl Into<String>) -> Self {
        Self { content: vec![ResultContent::Text { text: message.into() }], details: Some(Value::Object(Default::default())), is_error: true, terminate: false }
    }
}

/// Partial results of a running call.
pub type Updates = Arc<dyn Fn(ToolOutput) + Send + Sync>;

/// Updates nobody reads.
pub fn no_updates() -> Updates {
    Arc::new(|_| {})
}

/// One tool the model can call.
pub trait Tool: Send + Sync {
    fn spec(&self) -> &ToolSpec;
    /// Run alone: a step holding such a call runs its calls one after another. Only `task` runs alongside others.
    fn sequential(&self) -> bool {
        true
    }
    /// Run the call; an `Err` is reported to the model as a failed call.
    fn execute(&self, call_id: String, args: Value, cancel: CancellationToken, updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>>;
}

/// Parse validated arguments into a tool's parameter type.
pub fn params<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|error| format!("invalid arguments: {error}"))
}

/// A tool's declaration.
pub fn spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec { name: name.into(), description: description.into(), parameters }
}

/// A file's identity at one observation: device, file id, size and times, as the editor's file versions.
pub type FileVersion = String;

/// The current version of a file; `None` when it does not exist.
pub fn version_of(path: &Path) -> Result<Option<FileVersion>, String> {
    crate::ide::fs::stat(path).map(|info| info.map(|info| info.version)).map_err(|error| error.to_string())
}

fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    if cfg!(windows) { text.to_lowercase() } else { text }
}

/// Files a chat has seen; write and edit refuse to change a file the chat has not read in its current state.
#[derive(Default)]
pub struct Observations {
    versions: Mutex<HashMap<String, Option<FileVersion>>>,
}

impl Observations {
    /// Record what the chat last saw; `None` when the file did not exist.
    pub fn record(&self, path: &Path, version: Option<FileVersion>) {
        self.versions.lock().unwrap().insert(path_key(path), version);
    }

    /// Check that a change is based on the file's current state.
    pub fn assert_current(&self, path: &Path, current: Option<FileVersion>, display: &str, verb: &str) -> Result<(), String> {
        let Some(current) = current else { return Ok(()) };
        match self.versions.lock().unwrap().get(&path_key(path)) {
            None | Some(None) => Err(format!("cannot {} \"{display}\": file has not been read — read the file, then retry", if verb == "write" { "modify" } else { verb })),
            Some(Some(seen)) if seen != &current => Err(format!("cannot {verb} \"{display}\": file changed since it was read — re-read the file, then retry")),
            Some(Some(_)) => Ok(()),
        }
    }
}

/// Keeps what write and edit replaced, so the user can undo the change; answers whether it can be undone.
pub type RecordChange = Arc<dyn Fn(String, PathBuf, Option<Vec<u8>>, Vec<u8>) -> BoxFuture<'static, bool> + Send + Sync>;

/// What every tool of one chat needs.
#[derive(Clone)]
pub struct ToolContext {
    /// The chat's working directory (the project's primary directory).
    pub cwd: PathBuf,
    /// Exported to commands as `RAINY_SESSION_ID`.
    pub session_id: String,
    pub runtime: Arc<Runtime>,
    /// Full tool outputs that were cut for the model.
    pub spill_dir: PathBuf,
    /// Token budget of one tool result for the current model.
    pub tool_tokens: Arc<dyn Fn() -> u64 + Send + Sync>,
    pub observations: Arc<Observations>,
    /// PowerShell chosen with `RAINY_PWSH_PATH`, on Windows.
    pub pwsh_path: Option<PathBuf>,
    /// Called after a tool reads or changes a file.
    pub on_file_touched: Option<Arc<dyn Fn(PathBuf) + Send + Sync>>,
    pub record_change: Option<RecordChange>,
    /// Commands of this chat that keep running after their call; absent where background commands are unavailable.
    pub jobs: Option<Arc<jobs::Jobs>>,
}

impl ToolContext {
    /// A tool path against the working directory, and the spelling the model is shown.
    pub fn resolve(&self, requested: &str) -> Result<(PathBuf, String), String> {
        if requested.trim().is_empty() || requested.contains('\0') {
            return Err("invalid file_path: expected a non-empty path".into());
        }
        let absolute = crate::paths::normalize(&self.cwd.join(requested));
        Ok((absolute.clone(), self.display(&absolute)))
    }

    /// A path as the model knows it: relative to the working directory when inside it.
    pub fn display(&self, absolute: &Path) -> String {
        match crate::paths::relative_to(&self.cwd, absolute) {
            Some(inside) if !inside.is_empty() => inside,
            _ => absolute.to_string_lossy().into_owned(),
        }
    }

    pub fn touched(&self, path: &Path) {
        if let Some(touched) = &self.on_file_touched {
            touched(path.to_path_buf());
        }
    }

    /// Keep a large output within the model's tool budget: the full text goes to a file and the model receives its head
    /// and tail with a pointer to that file.
    pub fn limit_output(&self, tool: &str, text: String) -> String {
        let cap = (self.tool_tokens)();
        if estimate_text(&text) <= cap {
            return text;
        }
        let path = self.spill_dir.join(format!("{}-{tool}.txt", &uuid::Uuid::new_v4().simple().to_string()[..12]));
        let saved = crate::store::create_private_dir(&self.spill_dir).and_then(|()| crate::store::write_atomic(&path, text.as_bytes()));
        let note = match saved {
            Ok(()) => format!("\n[Output limited; full result: {}]\nUse read with offset/limit, or grep this path to search within it.\n", path.display()),
            Err(_) => "\n[Output limited]\n".to_owned(),
        };
        let chars: Vec<char> = text.chars().collect();
        let mut window = (cap * 3) as usize;
        loop {
            let head: String = chars[..(window * 3 / 4).min(chars.len())].iter().collect();
            let tail: String = chars[chars.len().saturating_sub(window / 4)..].iter().collect();
            let candidate = format!("{head}{note}{tail}");
            if estimate_text(&candidate) <= cap || window < 64 {
                return candidate;
            }
            window = window * 4 / 5;
        }
    }
}

/// Built-in tools: read, grep, glob, write, edit, the platform shell, background commands, and web and sub-agents
/// when given.
pub fn core_tools(context: &ToolContext, windows: bool, web: Option<web::WebAccess>, subagents: Option<Arc<dyn task::Subagents>>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(read::Read::new(context.clone())),
        Arc::new(search::Grep::new(context.clone())),
        Arc::new(search::Glob::new(context.clone())),
        Arc::new(write::Write::new(context.clone())),
        Arc::new(write::Edit::new(context.clone())),
        Arc::new(shell::Shell::new(context.clone(), windows)),
    ];
    if context.jobs.is_some() {
        tools.push(Arc::new(shell::CommandOutput::new(context.clone())));
        tools.push(Arc::new(shell::StopCommand::new(context.clone())));
    }
    if let Some(web) = web {
        tools.push(Arc::new(web::WebSearch::new(web.clone())));
        tools.push(Arc::new(web::WebFetch::new(web, context.clone())));
    }
    if let Some(subagents) = subagents {
        tools.push(Arc::new(task::Task::new(context.clone(), subagents)));
    }
    tools
}

/// Tools a sub-agent may use: it reads and searches, so no call of its own waits for the user.
pub fn subagent_tools(context: &ToolContext, web: Option<web::WebAccess>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = vec![Arc::new(read::Read::new(context.clone())), Arc::new(search::Grep::new(context.clone())), Arc::new(search::Glob::new(context.clone()))];
    if let Some(web) = web {
        tools.push(Arc::new(web::WebSearch::new(web)));
    }
    tools
}

/// A tool context over a directory, for tests.
#[cfg(test)]
pub fn test_context(cwd: &Path) -> ToolContext {
    let env = crate::env::HostEnv::for_tests(cwd);
    let projects = Arc::new(crate::projects::Projects::load(cwd.join(".rainy-test-projects.json")).unwrap());
    let registry = Arc::new(crate::registry::Registry::new(cwd.join(".rainy-test-state"), "test".into()));
    let runtime = Runtime::new(env, projects, registry, Arc::new(crate::activity::Activity::default())).unwrap();
    ToolContext {
        cwd: cwd.to_path_buf(),
        session_id: "test".into(),
        runtime,
        spill_dir: cwd.join(".spill"),
        tool_tokens: Arc::new(|| 4000),
        observations: Arc::default(),
        pwsh_path: None,
        on_file_touched: None,
        record_change: None,
        jobs: None,
    }
}
