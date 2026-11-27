//! Language servers over the `/rainy/ide/lsp` WebSocket (3.x `ide/language.ts`) plus a shared lightweight diagnostics
//! pool keyed by `(scope, root_id, language)` for Agent tools and post-edit feedback.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures::{SinkExt as _, StreamExt as _};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::files::Files;
use super::framing::{Decoder, encode};
use super::lsp_client::{LspStdioClient, document_language_id};
use super::state::StateStore;
use super::tools::{Kit, Tools};
use crate::activity::Activity;
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::paths::{contains, realpath};
use crate::process::{ProcessJob, child_env, contain_child};
use crate::runtime::Runtime;

const MAX_CONNECTIONS: usize = 8;
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_DIAG_POOL: usize = 4;
const MAX_OPENED_DOCS: usize = 32;
const MAX_DIAG_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CACHED_DIAGNOSTICS: usize = 512;
const DIAG_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const DIAG_CACHE_TTL: Duration = Duration::from_secs(60);
const DIAG_INIT_TIMEOUT: Duration = Duration::from_secs(8);
const DIAG_QUERY_TIMEOUT: Duration = Duration::from_secs(6);
const DIAG_PUSH_WAIT: Duration = Duration::from_millis(1500);
const CLOSE_WAIT_TIMEOUT: Duration = Duration::from_secs(3);

/// Close code telling the editor to connect again: the project's interpreter environment changed.
pub const RESTART_CODE: u16 = 1012;
/// Close code and reason prefix of a connection whose language server is not installed in the execution target (clangd
/// is not bundled); the reason is `program-missing:<program>` and the interface points at Settings → Runtime.
pub const PROGRAM_MISSING_CODE: u16 = 4405;
pub const PROGRAM_MISSING: &str = "program-missing";
/// The longest reason a WebSocket close frame carries (RFC 6455: 125 payload bytes, 2 of them the code).
const MAX_CLOSE_REASON: usize = 123;

/// One diagnostic entry returned by the shared LSP diagnostics pool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticItem {
    /// 1-based line number.
    pub line: u32,
    /// 1-based UTF-16 column.
    pub column: u32,
    /// `"error"`, `"warning"`, `"info"`, or `"hint"`.
    pub severity: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Structured result of querying diagnostics for a file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsReport {
    pub language: Option<String>,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    pub diagnostics: Vec<DiagnosticItem>,
}

/// Infer the Host LSP language identifier from a file extension.
pub fn language_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "py" | "pyi" | "pyw" => Some("python"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "tsx" | "mts" | "cts" => Some("typescript"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Some("cpp"),
        _ => None,
    }
}

/// Parse an LSP `Diagnostic[]` JSON array into sorted [`DiagnosticItem`]s.
pub fn parse_lsp_diagnostics(items: &[Value]) -> Vec<DiagnosticItem> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(message) = item.get("message").and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty()) else {
            continue;
        };
        let line = item.pointer("/range/start/line").and_then(Value::as_u64).unwrap_or(0).saturating_add(1).min(u64::from(u32::MAX)) as u32;
        let column = item.pointer("/range/start/character").and_then(Value::as_u64).unwrap_or(0).saturating_add(1).min(u64::from(u32::MAX)) as u32;
        let severity = match item.get("severity").and_then(Value::as_u64) {
            Some(1) => "error",
            Some(2) => "warning",
            Some(3) => "info",
            Some(4) => "hint",
            _ => "error",
        }
        .to_owned();
        let code = match item.get("code") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        let source = item.get("source").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned);
        out.push(DiagnosticItem { line, column, severity, message: message.to_owned(), code, source });
    }
    out.sort_by(|a, b| a.line.cmp(&b.line).then(a.column.cmp(&b.column)).then(a.message.cmp(&b.message)));
    out
}

/// `text` cut to a close frame's reason at a character boundary.
fn close_reason(text: &str) -> String {
    let mut end = text.len().min(MAX_CLOSE_REASON);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

struct Spec {
    root: PathBuf,
    argv: Vec<String>,
    environment: BTreeMap<String, String>,
}

struct Connection {
    id: u64,
    workspace_id: String,
    root_id: Option<String>,
    language: String,
    argv: Vec<String>,
    environment: BTreeMap<String, String>,
    stop: CancellationToken,
    done: watch::Receiver<bool>,
    /// The close code and reason the socket gets when the Host stops the server.
    close: Mutex<Option<(u16, &'static str)>>,
}

#[derive(Clone)]
struct CachedDiagnostics {
    file_version: String,
    diagnostics: Vec<DiagnosticItem>,
    updated_at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PoolKey {
    /// Either `ws:<workspace_id>` or `dir:<canonical_root>`.
    scope: String,
    root_id: String,
    language: String,
}

struct DiagProcess {
    client: LspStdioClient,
    published: HashMap<String, Vec<DiagnosticItem>>,
    opened: HashMap<String, (String, i32)>,
    opened_order: VecDeque<String>,
    supports_pull: Option<bool>,
}

impl DiagProcess {
    async fn start(spec: &Spec) -> HostResult<Self> {
        let mut env = child_env(&spec.environment);
        env.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
        let client = LspStdioClient::spawn(&spec.argv[0], &spec.argv[1..], &spec.root, &env, MAX_MESSAGE_BYTES, true)
            .map_err(|error| HostError::other(format!("{}: {error}", spec.argv[0])))?;
        let mut process = Self {
            client,
            published: HashMap::new(),
            opened: HashMap::new(),
            opened_order: VecDeque::new(),
            supports_pull: None,
        };
        let uri = file_uri(&spec.root);
        let name = spec.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| spec.root.display().to_string());
        let init_params = json!({
            "processId": Value::Null,
            "rootPath": spec.root,
            "rootUri": uri,
            "workspaceFolders": [{ "uri": uri, "name": name }],
            "capabilities": {
                "textDocument": {
                    "publishDiagnostics": { "relatedInformation": false },
                    "diagnostic": { "dynamicRegistration": false }
                }
            }
        });
        let init_result = tokio::time::timeout(DIAG_INIT_TIMEOUT, process.request("initialize", init_params)).await;
        match init_result {
            Ok(Ok(_)) => {
                process.client.notify("initialized", json!({})).await?;
                Ok(process)
            }
            Ok(Err(error)) => {
                process.shutdown().await;
                Err(error)
            }
            Err(_) => {
                process.shutdown().await;
                Err(HostError::other(l("语言服务器初始化超时。", "Language server initialization timed out.")))
            }
        }
    }

    fn record_notification(published: &mut HashMap<String, Vec<DiagnosticItem>>, message: &Value) -> Option<String> {
        if message.get("method").and_then(Value::as_str) != Some("textDocument/publishDiagnostics") {
            return None;
        }
        let params = message.get("params")?;
        let uri = params.get("uri").and_then(Value::as_str)?;
        let items = params.get("diagnostics").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let parsed = parse_lsp_diagnostics(items);
        let key = uri_key(uri);
        published.insert(key.clone(), parsed);
        Some(key)
    }

    async fn request(&mut self, method: &str, params: Value) -> HostResult<Result<Value, (i64, String)>> {
        let published = &mut self.published;
        self.client.request(method, params, |message| {
            let _ = Self::record_notification(published, message);
        }).await
    }

    async fn evict_oldest_opened(&mut self) {
        while self.opened.len() >= MAX_OPENED_DOCS {
            let Some(old_key) = self.opened_order.pop_front() else { break };
            if let Some((old_uri, _)) = self.opened.remove(&old_key) {
                self.published.remove(&old_key);
                let _ = self.client.notify("textDocument/didClose", json!({ "textDocument": { "uri": old_uri } })).await;
            }
        }
    }

    async fn query_file(&mut self, file: &Path, language: &str, text: &str) -> HostResult<Vec<DiagnosticItem>> {
        let uri = file_uri(file);
        let key = uri_key(&uri);
        self.published.remove(&key);
        if let Some((_, version)) = self.opened.get_mut(&key) {
            *version += 1;
            let ver = *version;
            self.client
                .notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri, "version": ver },
                        "contentChanges": [{ "text": text }]
                    }),
                )
                .await?;
        } else {
            self.evict_oldest_opened().await;
            self.opened.insert(key.clone(), (uri.clone(), 1));
            self.opened_order.push_back(key.clone());
            let doc_lang = document_language_id(file, language);
            self.client
                .notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": { "uri": uri, "languageId": doc_lang, "version": 1, "text": text }
                    }),
                )
                .await?;
        }

        // Prefer LSP 3.17 pull diagnostics when supported (ty, tsgo/tsc --lsp).
        if self.supports_pull != Some(false) {
            match self.request("textDocument/diagnostic", json!({ "textDocument": { "uri": uri } })).await? {
                Ok(result) => {
                    self.supports_pull = Some(true);
                    if let Some(items) = result.get("items").and_then(Value::as_array) {
                        return Ok(parse_lsp_diagnostics(items));
                    }
                    if let Some(cached) = self.published.get(&key) {
                        return Ok(cached.clone());
                    }
                    return Ok(Vec::new());
                }
                Err((-32601, _)) => {
                    // MethodNotFound: server uses push-only `publishDiagnostics` (e.g. clangd).
                    self.supports_pull = Some(false);
                }
                Err(_) => {
                    // Fall through to check whether diagnostics were pushed meanwhile.
                }
            }
        }

        if let Some(pushed) = self.published.get(&key) {
            return Ok(pushed.clone());
        }
        // Wait briefly for `textDocument/publishDiagnostics` notification from push-only servers.
        let deadline = tokio::time::Instant::now() + DIAG_PUSH_WAIT;
        loop {
            while let Some(message) = self.client.pop_received() {
                if message.get("method").is_some() {
                    if let Some(req_id) = message.get("id") {
                        let _ = self.client.send(&json!({ "jsonrpc": "2.0", "id": req_id, "result": Value::Null })).await;
                    } else if Self::record_notification(&mut self.published, &message).as_deref() == Some(key.as_str()) {
                        return Ok(self.published.get(&key).cloned().unwrap_or_default());
                    }
                }
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(self.published.get(&key).cloned().unwrap_or_default());
            }
            match tokio::time::timeout(remaining, self.client.read_more()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => return Ok(self.published.get(&key).cloned().unwrap_or_default()),
            }
        }
    }

    async fn shutdown(self) {
        self.client.shutdown().await;
    }
}

struct DiagEntry {
    workspace_id: Option<String>,
    argv: Vec<String>,
    environment: BTreeMap<String, String>,
    process: tokio::sync::Mutex<Option<DiagProcess>>,
    last_used: Mutex<Instant>,
}

impl DiagEntry {
    fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.last_used.lock().unwrap().elapsed()
    }

    async fn close(&self) {
        let proc = self.process.lock().await.take();
        if let Some(proc) = proc {
            proc.shutdown().await;
        }
    }
}

pub struct Languages {
    tools: Tools,
    files: Arc<Files>,
    runtime: Arc<Runtime>,
    state: Arc<StateStore>,
    activity: Arc<Activity>,
    connections: Mutex<Vec<Arc<Connection>>>,
    pool: Mutex<HashMap<PoolKey, Arc<DiagEntry>>>,
    published_cache: Mutex<HashMap<String, CachedDiagnostics>>,
    next: AtomicU64,
    closing: AtomicBool,
    reaper_stop: CancellationToken,
    tokio_handle: Option<tokio::runtime::Handle>,
}

fn file_uri(path: &Path) -> String {
    Url::from_file_path(path).map(String::from).unwrap_or_else(|_| format!("file://{}", path.display()))
}

fn uri_key(uri: &str) -> String {
    if cfg!(windows) { uri.to_lowercase() } else { uri.to_owned() }
}

fn path_cache_key(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    if cfg!(windows) { text.to_lowercase() } else { text }
}

/// Refuse a document URI that is not a file inside the project, also through links.
fn check_uri(root: &Path, spelling: &str) -> HostResult<()> {
    let refused = || HostError::invalid("Language document is outside its workspace");
    let url = Url::parse(spelling).map_err(|_| refused())?;
    if url.scheme() != "file" {
        return Err(HostError::invalid("Editor language documents must use file URIs"));
    }
    let path = url.to_file_path().map_err(|_| refused())?;
    let path = crate::paths::normalize(&path);
    if !contains(root, &path) {
        return Err(refused());
    }
    let mut existing = path.as_path();
    loop {
        match realpath(existing) {
            Ok(real) => return if contains(root, &real) { Ok(()) } else { Err(refused()) },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => existing = existing.parent().ok_or_else(refused)?,
            Err(error) => return Err(error.into()),
        }
    }
}

/// Check an editor message and bind initialization to the project folder.
fn prepare(mut message: Value, root: &Path) -> HostResult<Value> {
    let invalid = || HostError::invalid("Invalid editor language message");
    let object = message.as_object().ok_or_else(invalid)?;
    let id_ok = match object.get("id") {
        None => object.get("method").is_some_and(Value::is_string),
        Some(id) => id.is_number() || id.is_string() || id.is_null(),
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || !id_ok {
        return Err(invalid());
    }
    match object.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let params = message.get_mut("params").and_then(Value::as_object_mut).ok_or_else(|| HostError::invalid("Language initialization parameters are missing"))?;
            let uri = file_uri(root);
            let name = root.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
            params.insert("processId".into(), Value::Null);
            params.insert("rootPath".into(), json!(root));
            params.insert("rootUri".into(), json!(uri));
            params.insert("workspaceFolders".into(), json!([{ "uri": uri, "name": name }]));
            return Ok(message);
        }
        Some("workspace/didChangeWorkspaceFolders") => return Err(HostError::invalid("Open another workspace language connection to change the project")),
        _ => {}
    }
    let mut pending = vec![&message["params"]];
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(items) => pending.extend(items.iter()),
            Value::Object(fields) => {
                for (key, child) in fields {
                    match child {
                        Value::String(spelling) if ["uri", "oldUri", "newUri", "rootUri"].contains(&key.as_str()) => check_uri(root, spelling)?,
                        Value::Array(_) | Value::Object(_) => pending.push(child),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(message)
}

impl Languages {
    pub fn new(tools: Tools, files: Arc<Files>, runtime: Arc<Runtime>, state: Arc<StateStore>, activity: Arc<Activity>) -> Arc<Self> {
        let tokio_handle = tokio::runtime::Handle::try_current().ok();
        let languages = Arc::new(Self {
            tools,
            files,
            runtime,
            state,
            activity,
            connections: Mutex::default(),
            pool: Mutex::default(),
            published_cache: Mutex::default(),
            next: AtomicU64::new(1),
            closing: AtomicBool::new(false),
            reaper_stop: CancellationToken::new(),
            tokio_handle: tokio_handle.clone(),
        });
        // Reap idle pooled diagnostic LSP sessions every 30s so they never stay resident unnecessarily.
        if let Some(handle) = tokio_handle {
            let weak = Arc::downgrade(&languages);
            let stop = languages.reaper_stop.clone();
            handle.spawn(async move {
                loop {
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(30)) => {}
                        () = stop.cancelled() => break,
                    }
                    let Some(languages) = weak.upgrade() else { break };
                    languages.reap_idle().await;
                }
            });
        }
        languages
    }

    async fn reap_idle(&self) {
        let stale: Vec<Arc<DiagEntry>> = {
            let mut pool = self.pool.lock().unwrap();
            let expired: Vec<PoolKey> = pool.iter().filter(|(_, entry)| entry.idle_for() >= DIAG_IDLE_TIMEOUT).map(|(k, _)| k.clone()).collect();
            expired.into_iter().filter_map(|k| pool.remove(&k)).collect()
        };
        futures::future::join_all(stale.iter().map(|entry| entry.close())).await;
    }

    fn resolve_for_dir(&self, root: PathBuf, language: &str, workspace_id: Option<&str>) -> HostResult<Spec> {
        self.activity.assert_can_start()?;
        if self.closing.load(Ordering::Relaxed) {
            return Err(HostError::other(l("语言服务正在关闭。", "Editor language services are closing.")));
        }
        let environment = match workspace_id {
            Some(id) => self.runtime.resolve_workspace(id).map(|resolved| resolved.environment).unwrap_or_default(),
            None => self.runtime.resolve(&root).map(|resolved| resolved.environment).unwrap_or_default(),
        };
        let argv = match language {
            "python" => vec![Tools::require(self.tools.ty(), Kit::Python)?.display().to_string(), "server".into()],
            "javascript" | "typescript" => vec![Tools::require(self.tools.tsc(), Kit::Web)?.display().to_string(), "--lsp".into(), "--stdio".into()],
            "c" | "cpp" => {
                // The error's message is the program, which the close reason names.
                let clangd = crate::process::resolve_executable("clangd", Some(&child_env(&environment))).ok_or_else(|| HostError::new(PROGRAM_MISSING, "clangd"))?;
                let mut argv = vec![clangd.display().to_string(), "--background-index=0".into()];
                if let Some(directory) = self.compile_commands(workspace_id, &root) {
                    argv.push(format!("--compile-commands-dir={}", directory.display()));
                }
                argv
            }
            _ => return Err(HostError::invalid("Unknown editor language")),
        };
        Ok(Spec { root, argv, environment })
    }

    fn resolve(&self, workspace_id: &str, language: &str, root_id: Option<&str>) -> HostResult<Spec> {
        let root = self.files.resolve_workspace(workspace_id, root_id)?.root;
        self.resolve_for_dir(root, language, Some(workspace_id))
    }

    /// The build folder of the project's active CMake run profile, or an auto-detected `compile_commands.json`
    /// directory (`root` or `root/build`) for non-CMake C/C++ projects.
    fn compile_commands(&self, workspace_id: Option<&str>, root: &Path) -> Option<PathBuf> {
        if let Some(workspace_id) = workspace_id {
            if let Ok(state) = self.state.get(workspace_id) {
                let configuration = &state["data"]["execution"];
                if let Some(profiles) = configuration["profiles"].as_array() {
                    let native = |profile: &&Value| profile["language"] == "c" || profile["language"] == "cpp";
                    let active = profiles.iter().find(|profile| profile["name"] == configuration["activeProfile"]).filter(native);
                    if let Some(profile) = active.or_else(|| profiles.iter().find(native)) {
                        if profile["build"]["kind"] == "cmake" {
                            if let Some(dir) = profile["build"]["buildDirectory"]
                                .as_str()
                                .and_then(|dir| super::resolve::workspace_path(root, dir, None).ok())
                            {
                                return Some(dir);
                            }
                        }
                    }
                }
            }
        }
        for candidate in [root.to_path_buf(), root.join("build")] {
            if candidate.join("compile_commands.json").is_file() {
                return Some(candidate);
            }
        }
        None
    }

    fn insert_published_cache(&self, key: String, file_version: String, diagnostics: Vec<DiagnosticItem>) {
        let mut cache = self.published_cache.lock().unwrap();
        if !cache.contains_key(&key) && cache.len() >= MAX_CACHED_DIAGNOSTICS {
            if let Some(oldest) = cache.iter().min_by_key(|(_, v)| v.updated_at).map(|(k, _)| k.clone()) {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, CachedDiagnostics { file_version, diagnostics, updated_at: Instant::now() });
    }

    fn cache_published(&self, root: &Path, message: &Value, dirty_uris: &Mutex<HashSet<String>>) {
        if message.get("method").and_then(Value::as_str) != Some("textDocument/publishDiagnostics") {
            return;
        }
        let Some(params) = message.get("params") else { return };
        let Some(uri) = params.get("uri").and_then(Value::as_str) else { return };
        let Ok(url) = Url::parse(uri) else { return };
        if url.scheme() != "file" {
            return;
        }
        let Ok(path) = url.to_file_path() else { return };
        let Ok(real) = realpath(&path) else { return };
        if !contains(root, &real) {
            return;
        }
        let cache_key = path_cache_key(&real);
        // Never cache diagnostics computed over an unsaved dirty editor buffer as if they came from disk.
        if dirty_uris.lock().unwrap().contains(&uri_key(uri)) {
            self.published_cache.lock().unwrap().remove(&cache_key);
            return;
        }
        let Ok(Some(info)) = super::fs::stat(&real) else { return };
        let items = params.get("diagnostics").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let diagnostics = parse_lsp_diagnostics(items);
        self.insert_published_cache(cache_key, info.version, diagnostics);
    }

    /// Quiet diagnostics query for a file inside a workspace (returns `Ok(vec![])` when the file has no LSP or the tool
    /// is not installed). Suitable for post-edit hooks.
    pub async fn diagnostics(self: &Arc<Self>, workspace_id: &str, file_path: &Path, root_id: Option<&str>) -> HostResult<Vec<DiagnosticItem>> {
        let report = self.query_diagnostics(workspace_id, file_path, root_id).await?;
        Ok(report.diagnostics)
    }

    /// Structured diagnostics query for a workspace file (reports `available: false` with a localized reason when the
    /// language is unsupported or its language server is not installed).
    pub async fn query_diagnostics(self: &Arc<Self>, workspace_id: &str, file_path: &Path, root_id: Option<&str>) -> HostResult<DiagnosticsReport> {
        let root = self.files.resolve_workspace(workspace_id, root_id)?.root;
        let root_key = root_id.unwrap_or("primary").to_owned();
        self.query_diagnostics_impl(&root, file_path, Some(workspace_id), root_key).await
    }

    /// Structured diagnostics query for a file under an arbitrary directory (e.g. a Git worktree or standalone chat).
    pub async fn query_diagnostics_in_dir(self: &Arc<Self>, root: &Path, file_path: &Path, workspace_id: Option<&str>) -> HostResult<DiagnosticsReport> {
        let canonical_root = realpath(root)?;
        self.query_diagnostics_impl(&canonical_root, file_path, workspace_id, "primary".into()).await
    }

    async fn query_diagnostics_impl(self: &Arc<Self>, root: &Path, file_path: &Path, workspace_id: Option<&str>, root_id: String) -> HostResult<DiagnosticsReport> {
        let resolved = if file_path.is_absolute() { file_path.to_path_buf() } else { root.join(file_path) };
        let canonical_root = realpath(root)?;
        let canonical_file = realpath(&resolved)?;
        if !contains(&canonical_root, &canonical_file) {
            return Err(HostError::invalid(l("文件不在当前项目目录内。", "The file is outside the current project directory.")));
        }
        let Some(language) = language_for_path(&canonical_file) else {
            return Ok(DiagnosticsReport {
                language: None,
                available: false,
                unavailable_reason: Some(l("该文件类型暂无内置语言服务器支持。", "No language server is configured for this file type.")),
                diagnostics: Vec::new(),
            });
        };
        let info = super::fs::stat(&canonical_file)?
            .ok_or_else(|| HostError::new("not-found", l("文件不存在。", "The file does not exist.")))?;
        if info.kind != super::fs::Kind::File {
            return Err(HostError::invalid(l("目标路径不是普通文件。", "The target path is not a regular file.")));
        }
        if info.size > MAX_DIAG_FILE_BYTES {
            return Ok(DiagnosticsReport {
                language: Some(language.to_owned()),
                available: false,
                unavailable_reason: Some(l("文件超过诊断大小上限。", "The file exceeds the diagnostics size limit.")),
                diagnostics: Vec::new(),
            });
        }

        // Fast path: reuse up-to-date diagnostics published by an active editor LSP connection.
        {
            let cache = self.published_cache.lock().unwrap();
            if let Some(hit) = cache.get(&path_cache_key(&canonical_file)) {
                if hit.file_version == info.version && hit.updated_at.elapsed() <= DIAG_CACHE_TTL {
                    return Ok(DiagnosticsReport {
                        language: Some(language.to_owned()),
                        available: true,
                        unavailable_reason: None,
                        diagnostics: hit.diagnostics.clone(),
                    });
                }
            }
        }

        let spec = match self.resolve_for_dir(canonical_root.clone(), language, workspace_id) {
            Ok(spec) => spec,
            Err(error) if error.code == super::tools::MISSING_REASON => {
                return Ok(DiagnosticsReport {
                    language: Some(language.to_owned()),
                    available: false,
                    unavailable_reason: Some(error.message),
                    diagnostics: Vec::new(),
                });
            }
            Err(error) if error.code == PROGRAM_MISSING => {
                return Ok(DiagnosticsReport {
                    language: Some(language.to_owned()),
                    available: false,
                    unavailable_reason: Some(l(
                        &format!("未安装语言服务器 {}。", error.message),
                        &format!("Language server {} is not installed.", error.message),
                    )),
                    diagnostics: Vec::new(),
                });
            }
            Err(error) => return Err(error),
        };

        let bytes = tokio::fs::read(&canonical_file).await?;
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                return Ok(DiagnosticsReport {
                    language: Some(language.to_owned()),
                    available: false,
                    unavailable_reason: Some(l("仅支持对 UTF-8 文本文件进行诊断。", "Diagnostics require a UTF-8 text file.")),
                    diagnostics: Vec::new(),
                });
            }
        };

        let key = PoolKey {
            scope: match workspace_id {
                Some(id) => format!("ws:{id}"),
                None => format!("dir:{}", path_cache_key(&canonical_root)),
            },
            root_id,
            language: language.to_owned(),
        };

        let (entry, evicted) = {
            let mut pool = self.pool.lock().unwrap();
            let mut evicted: Vec<Arc<DiagEntry>> = Vec::new();
            let needs_new = match pool.get(&key) {
                Some(existing) if existing.argv == spec.argv && existing.environment == spec.environment => false,
                Some(_) => {
                    if let Some(old) = pool.remove(&key) {
                        evicted.push(old);
                    }
                    true
                }
                None => true,
            };
            if needs_new {
                while pool.len() >= MAX_DIAG_POOL {
                    let Some(lru_key) = pool.iter().min_by_key(|(_, e)| *e.last_used.lock().unwrap()).map(|(k, _)| k.clone()) else {
                        break;
                    };
                    if let Some(old) = pool.remove(&lru_key) {
                        evicted.push(old);
                    }
                }
                let created = Arc::new(DiagEntry {
                    workspace_id: workspace_id.map(str::to_owned),
                    argv: spec.argv.clone(),
                    environment: spec.environment.clone(),
                    process: tokio::sync::Mutex::new(None),
                    last_used: Mutex::new(Instant::now()),
                });
                pool.insert(key.clone(), created.clone());
                (created, evicted)
            } else {
                let existing = pool.get(&key).cloned().expect("checked above");
                existing.touch();
                (existing, evicted)
            }
        };
        if !evicted.is_empty() {
            tokio::spawn(async move {
                futures::future::join_all(evicted.iter().map(|old| old.close())).await;
            });
        }

        let mut guard = entry.process.lock().await;
        entry.touch();
        if guard.is_none() {
            match DiagProcess::start(&spec).await {
                Ok(proc) => *guard = Some(proc),
                Err(error) => {
                    return Ok(DiagnosticsReport {
                        language: Some(language.to_owned()),
                        available: false,
                        unavailable_reason: Some(error.message),
                        diagnostics: Vec::new(),
                    });
                }
            }
        }
        let proc = guard.as_mut().expect("started above");
        let query_result = tokio::time::timeout(DIAG_QUERY_TIMEOUT, proc.query_file(&canonical_file, language, &text)).await;
        match query_result {
            Ok(Ok(diagnostics)) => {
                entry.touch();
                // Cache under file version with bounded capacity so immediate repeated queries hit fast path.
                self.insert_published_cache(path_cache_key(&canonical_file), info.version, diagnostics.clone());
                Ok(DiagnosticsReport {
                    language: Some(language.to_owned()),
                    available: true,
                    unavailable_reason: None,
                    diagnostics,
                })
            }
            Ok(Err(error)) => {
                if let Some(broken) = guard.take() {
                    broken.shutdown().await;
                }
                Ok(DiagnosticsReport {
                    language: Some(language.to_owned()),
                    available: false,
                    unavailable_reason: Some(error.message),
                    diagnostics: Vec::new(),
                })
            }
            Err(_) => {
                if let Some(timed_out) = guard.take() {
                    timed_out.shutdown().await;
                }
                Ok(DiagnosticsReport {
                    language: Some(language.to_owned()),
                    available: false,
                    unavailable_reason: Some(l("语言服务器诊断超时。", "Language server diagnostics timed out.")),
                    diagnostics: Vec::new(),
                })
            }
        }
    }

    /// Serve one accepted socket whose query names the project, folder and language.
    pub async fn accept(self: Arc<Self>, socket: WebSocket, url: Url) {
        let query: HashMap<String, String> = url.query_pairs().map(|(name, value)| (name.into_owned(), value.into_owned())).collect();
        let (mut sink, mut stream) = socket.split();
        let close = |code: u16, reason: &str| Message::Close(Some(CloseFrame { code, reason: close_reason(reason).into() }));
        let valid_keys = query.keys().all(|key| ["workspaceId", "language", "rootId"].contains(&key.as_str()));
        let (Some(workspace_id), Some(language)) = (query.get("workspaceId").filter(|_| valid_keys), query.get("language")) else {
            let _ = sink.send(close(1008, "Invalid editor language request")).await;
            return;
        };
        let root_id = query.get("rootId").cloned();
        let spec = match self.resolve(workspace_id, language, root_id.as_deref()) {
            Ok(spec) => spec,
            // The interface offers the download of tools that are not there.
            Err(error) if error.code == super::tools::MISSING_REASON => {
                let _ = sink.send(close(super::tools::MISSING_CODE, super::tools::MISSING_REASON)).await;
                return;
            }
            // The interface names the program and points at Settings → Runtime.
            Err(error) if error.code == PROGRAM_MISSING => {
                let _ = sink.send(close(PROGRAM_MISSING_CODE, &format!("{PROGRAM_MISSING}:{}", error.message))).await;
                return;
            }
            // The status bar shows the reason, in the interface language.
            Err(error) => {
                log::info!("IDE language request rejected: {}", error.message);
                let _ = sink.send(close(1008, &error.message)).await;
                return;
            }
        };
        // Atomically reserve a connection slot under the lock so concurrent connects cannot exceed MAX_CONNECTIONS
        // or race past `close()`.
        let (done_tx, done_rx) = watch::channel(false);
        let connection = Arc::new(Connection {
            id: self.next.fetch_add(1, Ordering::Relaxed),
            workspace_id: workspace_id.clone(),
            root_id,
            language: language.clone(),
            argv: spec.argv.clone(),
            environment: spec.environment.clone(),
            stop: CancellationToken::new(),
            done: done_rx,
            close: Mutex::new(None),
        });
        let rejected = {
            let mut connections = self.connections.lock().unwrap();
            if self.closing.load(Ordering::Acquire) {
                Some((1001, "Editor language services are closing"))
            } else if connections.len() >= MAX_CONNECTIONS {
                Some((1013, "Editor language capacity reached"))
            } else {
                connections.push(connection.clone());
                None
            }
        };
        if let Some((code, reason)) = rejected {
            let _ = done_tx.send(true);
            let _ = sink.send(close(code, reason)).await;
            return;
        }
        let mut env = child_env(&spec.environment);
        env.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
        let mut command = crate::process::command(&spec.argv[0], &spec.argv[1..], &spec.root, &env);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.connections.lock().unwrap().retain(|known| known.id != connection.id);
                let _ = done_tx.send(true);
                log::warn!("IDE language startup failed: {}: {error}", spec.argv[0]);
                let _ = sink.send(Message::Text(json!({ "jsonrpc": "2.0", "method": "window/logMessage", "params": { "type": 1, "message": format!("{}: {error}", spec.argv[0]) } }).to_string().into())).await;
                let _ = sink.send(close(1011, "Language server could not start")).await;
                return;
            }
        };
        let job = ProcessJob::new();
        if !job.as_ref().is_some_and(|job| job.assign_child(&child)) {
            contain_child(&child);
        }
        let dirty_uris: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let (mut stdin, mut stdout, mut stderr) = (child.stdin.take().expect("piped"), child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
        tokio::spawn(async move {
            let mut sink = [0u8; 8192];
            while matches!(stderr.read(&mut sink).await, Ok(read) if read > 0) {}
        });
        let (to_socket, mut socket_queue) = tokio::sync::mpsc::unbounded_channel::<Message>();
        let writer = tokio::spawn(async move {
            while let Some(message) = socket_queue.recv().await {
                let closing = matches!(message, Message::Close(_));
                if sink.send(message).await.is_err() || closing {
                    break;
                }
            }
        });
        // Server → editor (also taps `publishDiagnostics` into the shared cache when the buffer is clean).
        let server_output = {
            let to_socket = to_socket.clone();
            let stop = connection.stop.clone();
            let languages = self.clone();
            let root = spec.root.clone();
            let dirty_uris = dirty_uris.clone();
            tokio::spawn(async move {
                let mut decoder = Decoder::new(MAX_MESSAGE_BYTES);
                let mut buffer = vec![0u8; 65536];
                loop {
                    let read = tokio::select! {
                        read = stdout.read(&mut buffer) => read,
                        () = stop.cancelled() => break,
                    };
                    match read {
                        Ok(0) | Err(_) => break,
                        Ok(read) => match decoder.push(&buffer[..read]) {
                            Ok(messages) => {
                                for message in messages {
                                    languages.cache_published(&root, &message, &dirty_uris);
                                    let _ = to_socket.send(Message::Text(message.to_string().into()));
                                }
                            }
                            Err(error) => {
                                log::warn!("Editor language output failed: {error}");
                                stop.cancel();
                                break;
                            }
                        },
                    }
                }
            })
        };
        // Editor → server, in order, with bounded write timeout so a wedged server cannot hang input forever.
        let root = spec.root.clone();
        let input_stop = connection.stop.clone();
        let editor_input = {
            let to_socket = to_socket.clone();
            let dirty_uris = dirty_uris.clone();
            tokio::spawn(async move {
                loop {
                    let message = tokio::select! {
                        message = stream.next() => message,
                        () = input_stop.cancelled() => break,
                    };
                    let text = match message {
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(Message::Binary(_))) => {
                            let _ = to_socket.send(Message::Close(Some(CloseFrame { code: 1011, reason: "Editor language input exceeds its message budget".into() })));
                            break;
                        }
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        Some(Ok(_)) => continue,
                    };
                    if text.len() > MAX_MESSAGE_BYTES {
                        break;
                    }
                    let admitted = serde_json::from_str::<Value>(text.as_str()).map_err(HostError::from).and_then(|message| prepare(message, &root));
                    match admitted {
                        Ok(message) => {
                            if let Some(method) = message.get("method").and_then(Value::as_str)
                                && let Some(uri) = message.pointer("/params/textDocument/uri").and_then(Value::as_str)
                            {
                                let key = uri_key(uri);
                                match method {
                                    "textDocument/didChange" => {
                                        dirty_uris.lock().unwrap().insert(key);
                                    }
                                    "textDocument/didSave" | "textDocument/didClose" => {
                                        dirty_uris.lock().unwrap().remove(&key);
                                    }
                                    _ => {}
                                }
                            }
                            let encoded = encode(&message);
                            let wrote = tokio::time::timeout(Duration::from_secs(5), async {
                                stdin.write_all(&encoded).await?;
                                stdin.flush().await
                            })
                            .await;
                            if !matches!(wrote, Ok(Ok(()))) {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = to_socket.send(Message::Text(json!({ "jsonrpc": "2.0", "method": "window/logMessage", "params": { "type": 1, "message": error.message } }).to_string().into()));
                            let _ = to_socket.send(Message::Close(Some(CloseFrame { code: 1011, reason: "Editor language connection failed".into() })));
                            break;
                        }
                    }
                }
                input_stop.cancel();
            })
        };
        let status = tokio::select! {
            status = child.wait() => Some(status),
            () = connection.stop.cancelled() => None,
        };
        connection.stop.cancel();
        if status.is_none() {
            if let Some(job) = &job {
                job.terminate();
            }
            crate::process::kill_tree(&mut child, Duration::from_secs(2)).await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), server_output).await;
        editor_input.abort();
        let requested = *connection.close.lock().unwrap();
        let (code, reason) = match (requested, status) {
            (Some(requested), _) => requested,
            (None, Some(Ok(status))) if status.success() => (1000, "Language server exited"),
            (None, Some(_)) => (1011, "Language server stopped"),
            (None, None) => (1000, "Editor language connection closed"),
        };
        let _ = to_socket.send(close(code, reason));
        drop(to_socket);
        let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
        self.connections.lock().unwrap().retain(|known| known.id != connection.id);
        let _ = done_tx.send(true);
    }

    /// Stop the language servers and pooled diagnostic sessions of one project.
    pub fn close_workspace(&self, workspace_id: &str) {
        for connection in self.connections.lock().unwrap().iter().filter(|connection| connection.workspace_id == workspace_id) {
            connection.stop.cancel();
        }
        let pooled: Vec<Arc<DiagEntry>> = {
            let mut pool = self.pool.lock().unwrap();
            let keys: Vec<PoolKey> = pool.iter().filter(|(_, e)| e.workspace_id.as_deref() == Some(workspace_id)).map(|(k, _)| k.clone()).collect();
            keys.into_iter().filter_map(|k| pool.remove(&k)).collect()
        };
        if !pooled.is_empty() {
            let handle = self.tokio_handle.clone().or_else(|| tokio::runtime::Handle::try_current().ok());
            if let Some(handle) = handle {
                handle.spawn(async move {
                    futures::future::join_all(pooled.iter().map(|entry| entry.close())).await;
                });
            }
        }
    }

    /// Restart the project's servers whose process choice changed, such as the PATH after another interpreter was
    /// selected; their sockets close with [`RESTART_CODE`] so the editor connects again.
    pub fn refresh_workspace(&self, workspace_id: &str) {
        let connections: Vec<Arc<Connection>> = self.connections.lock().unwrap().iter().filter(|connection| connection.workspace_id == workspace_id).cloned().collect();
        for connection in connections {
            let Ok(next) = self.resolve(workspace_id, &connection.language, connection.root_id.as_deref()) else { continue };
            if next.argv != connection.argv || next.environment != connection.environment {
                *connection.close.lock().unwrap() = Some((RESTART_CODE, "Language environment changed"));
                connection.stop.cancel();
            }
        }
        // Drop pooled diagnostic sessions for this workspace so the next query picks up the new interpreter/PATH.
        let pooled: Vec<Arc<DiagEntry>> = {
            let mut pool = self.pool.lock().unwrap();
            let keys: Vec<PoolKey> = pool.iter().filter(|(_, e)| e.workspace_id.as_deref() == Some(workspace_id)).map(|(k, _)| k.clone()).collect();
            keys.into_iter().filter_map(|k| pool.remove(&k)).collect()
        };
        if !pooled.is_empty() {
            let handle = self.tokio_handle.clone().or_else(|| tokio::runtime::Handle::try_current().ok());
            if let Some(handle) = handle {
                handle.spawn(async move {
                    futures::future::join_all(pooled.iter().map(|entry| entry.close())).await;
                });
            }
        }
    }

    /// Stop all editor connections and pooled diagnostic language servers, awaiting their process-tree cleanup with a
    /// bounded timeout so no orphan `ty`/`tsc`/`clangd` processes outlive Host shutdown.
    pub async fn close(&self) {
        self.closing.store(true, Ordering::Relaxed);
        self.reaper_stop.cancel();
        let wait_connections: Vec<watch::Receiver<bool>> = {
            let connections = self.connections.lock().unwrap();
            connections
                .iter()
                .map(|connection| {
                    connection.stop.cancel();
                    connection.done.clone()
                })
                .collect()
        };
        let pooled: Vec<Arc<DiagEntry>> = self.pool.lock().unwrap().drain().map(|(_, entry)| entry).collect();
        self.published_cache.lock().unwrap().clear();

        let conn_wait = futures::future::join_all(wait_connections.into_iter().map(|mut rx| async move {
            let _ = rx.wait_for(|done| *done).await;
        }));
        let pool_wait = futures::future::join_all(pooled.iter().map(|entry| entry.close()));
        let _ = tokio::time::timeout(CLOSE_WAIT_TIMEOUT, async { tokio::join!(conn_wait, pool_wait) }).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_reasons_fit_a_frame_and_keep_whole_characters() {
        assert_eq!(close_reason("program-missing:clangd"), "program-missing:clangd");
        let chinese = "找不到当前执行环境中的项目".repeat(10);
        let cut = close_reason(&chinese);
        assert!(cut.len() <= MAX_CLOSE_REASON && cut.len() > MAX_CLOSE_REASON - 3);
        assert!(chinese.starts_with(&cut));
    }

    #[test]
    fn binds_initialization_and_refuses_outside_documents() {
        let dir = tempfile::tempdir().unwrap();
        let root = realpath(dir.path()).unwrap();
        std::fs::write(root.join("a.py"), "").unwrap();
        let initialized = prepare(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "processId": 7, "capabilities": {} } }), &root).unwrap();
        assert_eq!(initialized["params"]["processId"], Value::Null);
        assert_eq!(initialized["params"]["rootUri"], file_uri(&root));
        let inside = json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": { "textDocument": { "uri": file_uri(&root.join("a.py")), "text": "" } } });
        assert!(prepare(inside, &root).is_ok());
        let outside = json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": { "textDocument": { "uri": file_uri(&root.parent().unwrap().join("x.py")) } } });
        assert!(prepare(outside, &root).is_err());
        assert!(prepare(json!({ "jsonrpc": "1.0", "method": "x" }), &root).is_err());
        assert!(prepare(json!({ "jsonrpc": "2.0", "method": "workspace/didChangeWorkspaceFolders", "params": {} }), &root).is_err());
    }

    #[test]
    fn infers_languages_and_parses_lsp_diagnostics() {
        assert_eq!(language_for_path(Path::new("src/app.py")), Some("python"));
        assert_eq!(language_for_path(Path::new("src/view.tsx")), Some("typescript"));
        assert_eq!(language_for_path(Path::new("src/index.mjs")), Some("javascript"));
        assert_eq!(language_for_path(Path::new("src/main.cpp")), Some("cpp"));
        assert_eq!(language_for_path(Path::new("README.md")), None);

        let parsed = parse_lsp_diagnostics(&[
            json!({
                "range": { "start": { "line": 9, "character": 4 }, "end": { "line": 9, "character": 8 } },
                "severity": 2,
                "code": "W001",
                "source": "ty",
                "message": "Unused variable"
            }),
            json!({
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 5 } },
                "severity": 1,
                "code": 2322,
                "source": "ts",
                "message": "Type mismatch"
            }),
        ]);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], DiagnosticItem { line: 2, column: 1, severity: "error".into(), message: "Type mismatch".into(), code: Some("2322".into()), source: Some("ts".into()) });
        assert_eq!(parsed[1], DiagnosticItem { line: 10, column: 5, severity: "warning".into(), message: "Unused variable".into(), code: Some("W001".into()), source: Some("ty".into()) });
    }
}
