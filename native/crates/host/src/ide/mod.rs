//! The editor's backend (3.x `ide/index.ts`): project files, editor recovery, formatting, language servers, runs,
//! terminals and debugging, served through the `ide` method, `/rainy/ide/raw` and the `/rainy/ide/lsp` socket.

pub mod dap;
pub mod debug;
pub mod execution;
pub mod files;
pub mod format;
pub mod framing;
pub mod fs;
pub mod hints;
pub mod language;
pub mod owner;
pub mod preview;
pub mod pty;
pub mod raw;
pub mod resolve;
pub mod state;
pub mod tools;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::Host;
use crate::error::{HostError, HostResult};

/// Largest `ide` request, escaped file text and recovery buffers included.
const MAX_REQUEST_BYTES: usize = 40 * 1024 * 1024;

pub struct Ide {
    pub files: Arc<files::Files>,
    pub state: Arc<state::StateStore>,
    pub execution: Arc<execution::Execution>,
    pub languages: Arc<language::Languages>,
    pub hints: hints::Hints,
    pub tools: tools::Tools,
}

impl Ide {
    pub fn new(host: &Arc<Host>) -> Arc<Self> {
        let rpc = host.rpc.clone();
        let hints = hints::Hints::new(move |workspace_id, kind| rpc.emit("ide.hint", json!({ "workspaceId": workspace_id, "kind": kind })), Duration::from_secs(30));
        let watcher = hints.clone();
        let runtime = host.runtime.clone();
        let files = Arc::new(files::Files::new(
            host.projects.clone(),
            move |workspace_id, root_id, folders| watcher.watch(workspace_id, root_id, folders),
            move |folder: &Path| runtime.resolve(folder).map(|resolved| resolved.environment).unwrap_or_default(),
        ));
        let state = Arc::new(state::StateStore::open(host.env.home.join("ide"), host.projects.clone()));
        let tools = tools::Tools::new(&host.env.resources);
        let executing = hints.clone();
        let execution = execution::Execution::new(host.runtime.clone(), tools.root.clone(), host.env.pwsh_path.clone(), move |workspace_id| executing.notify(workspace_id, "execution"));
        let probe = execution.clone();
        host.activity.add(move || probe.has_activity());
        let languages = language::Languages::new(tools.clone(), files.clone(), host.runtime.clone(), state.clone(), host.activity.clone());
        // A server started before another interpreter was selected would keep resolving imports on the old PATH.
        let refreshing = Arc::downgrade(&languages);
        host.runtime.on_selection_change(move |workspace_id| {
            if let Some(languages) = refreshing.upgrade() {
                languages.refresh_workspace(workspace_id);
            }
        });
        let ide = Arc::new(Self { files, state, execution, languages, hints, tools });
        let (raw_files, activity) = (ide.files.clone(), host.activity.clone());
        host.server.route(
            "/rainy/ide/raw",
            Arc::new(move |request, url| Box::pin(raw::serve(raw_files.clone(), activity.clone(), request, url))),
        );
        let languages = ide.languages.clone();
        host.server.socket("/rainy/ide/lsp", Arc::new(move |socket, url| Box::pin(languages.clone().accept(socket, url))));
        ide
    }

    /// One request of the `ide` method.
    pub async fn handle(self: &Arc<Self>, host: &Arc<Host>, request: Value) -> HostResult<Value> {
        if request.to_string().len() > MAX_REQUEST_BYTES {
            return Err(HostError::new("too-large", "The IDE request exceeds its configured byte limit"));
        }
        let op = request["op"].as_str().ok_or_else(|| HostError::invalid("The IDE operation is missing"))?.to_owned();
        if op == "workspaces.remove" {
            let workspace_id = request["workspaceId"].as_str().unwrap_or_default().to_owned();
            let selected = self.state.selection().as_deref() == Some(workspace_id.as_str());
            self.execution.stop_workspace(&workspace_id).await;
            self.languages.close_workspace(&workspace_id);
            let ide = self.clone();
            let result = tokio::task::spawn_blocking(move || ide.files.handle(&request)).await.map_err(|_| HostError::other("The file operation stopped."))??;
            if selected {
                self.state.set_selection(None)?;
            }
            return Ok(result);
        }
        if op.starts_with("workspaces.") || op.starts_with("files.") || op.starts_with("directories.") {
            let ide = self.clone();
            return tokio::task::spawn_blocking(move || ide.files.handle(&request)).await.map_err(|_| HostError::other("The file operation stopped."))?;
        }
        if op.starts_with("state.") {
            let ide = self.clone();
            return tokio::task::spawn_blocking(move || ide.state.handle(&request)).await.map_err(|_| HostError::other("The state operation stopped."))?;
        }
        if op == "tools.status" {
            host.activity.assert_can_start()?;
            let tools = self.tools.clone();
            return tokio::task::spawn_blocking(move || tools.status()).await.map_err(|_| HostError::other("Tool inspection stopped."));
        }
        if op == "format" {
            host.activity.assert_can_start()?;
            let workspace_id = request["workspaceId"].as_str().ok_or_else(|| HostError::invalid("Invalid formatter request"))?;
            let root = self.files.resolve_workspace(workspace_id, None)?.root;
            let environment = host.runtime.resolve_workspace(workspace_id).map(|resolved| resolved.environment).unwrap_or_default();
            return format::format(&request, &root, &self.tools, &environment).await;
        }
        if op.starts_with("execution.") || op.starts_with("run.") || op.starts_with("terminal.") || op.starts_with("debug.") {
            if op.ends_with(".start") || op.ends_with(".input") || op == "debug.evaluate" {
                host.activity.assert_can_start()?;
            }
            return self.execution.handle(&request, &self.files).await.map_err(|error| {
                if error.code == "invalid-request" || error.code == "dependency-unavailable" { error } else { HostError::new("execution-error", error.message) }
            });
        }
        Err(HostError::invalid(format!("Unknown IDE operation {op}")))
    }

    pub async fn close(&self) {
        self.hints.close();
        self.languages.close();
        self.execution.dispose().await;
    }
}
