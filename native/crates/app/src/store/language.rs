//! Language servers and the editor's AI suggestions for open files (`src/editor/lsp.ts`, the language-server half of
//! `src/editor/editor.ts`, `src/renderer/ide/ai-assist.ts`).
//!
//! One server runs per project folder and server language, through the Host's `/rainy/ide/lsp` WebSocket. Every
//! editor gets a [`Provider`]: completions, hover and Go to Definition come from the server of its file, when it has
//! one; ghost-text suggestions come from the Host's `inline.complete` for any file. Documents are sent whole: on open,
//! a moment after each edit, and before every request whose text differs from what the server has.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{
    CompletionProvider, DefinitionProvider, EditorState, HoverProvider, InputEvent, Rope, RopeExt as _, ShowDocumentHandler,
    TextDecoration, TextDecorationCollection, highlight_characters,
};
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionResponse, CompletionTextEdit, CompletionTriggerKind, GotoDefinitionResponse, Hover,
    InlineCompletionContext, InlineCompletionItem, InlineCompletionResponse, LocationLink, Position, TextEdit,
};
use rainy_carrier::lsp::{self, Answer, LspClient, LspIncoming, file_uri, uri_key, uri_path};
use serde_json::{Value, json};

use crate::store::Store;
use crate::store::ide::{Ide, IdeEvent};

/// Pause after an edit before the server gets the new text.
const SYNC_DELAY: Duration = Duration::from_millis(300);
/// How long a server that failed is left alone before a document asks for it again (3.x `SERVER_RETRY_MS`).
const SERVER_RETRY: Duration = Duration::from_secs(30);
/// Pause in typing before a suggestion is requested (3.x `SUGGEST_DELAY_MS`).
const SUGGEST_DELAY: Duration = Duration::from_millis(180);
/// Text sent around the cursor for a suggestion (3.x `SUGGEST_BEFORE`, `SUGGEST_AFTER`), in characters.
const SUGGEST_BEFORE: usize = 4000;
const SUGGEST_AFTER: usize = 1500;
/// Completion rows shown at most.
const MAX_COMPLETIONS: usize = 200;

/// The `/rainy/ide/lsp` language and the LSP language id of a path (`serverLanguage`, `lspLanguageId`).
pub fn server_language(path: &str) -> Option<(&'static str, &'static str)> {
    let extension = path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    Some(match extension.as_str() {
        "py" | "pyi" => ("python", "python"),
        "js" | "mjs" | "cjs" => ("javascript", "javascript"),
        "jsx" => ("javascript", "javascriptreact"),
        "ts" | "mts" | "cts" => ("typescript", "typescript"),
        "tsx" => ("typescript", "typescriptreact"),
        "c" | "h" => ("c", "c"),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => ("cpp", "cpp"),
        _ => return None,
    })
}

/// Where a document's server runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub workspace_id: String,
    /// The folder's root id; empty for the primary folder of a project saved before folders had ids.
    pub root_id: String,
    pub root_path: String,
    pub root_title: String,
    /// The file as the Host sees it.
    pub path: String,
    /// Workspace-relative path, shown to the model with suggestions.
    pub relative: String,
    pub python_path: Option<String>,
}

/// A problem a language server reported, for the problems list.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub key: String,
    /// One-based line and UTF-16 column.
    pub line: u32,
    pub column: u32,
    pub error: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerPhase {
    Starting,
    Ready,
    Failed(SharedString),
}

struct Connection {
    client: LspClient,
    target: Target,
    language: &'static str,
    ready: bool,
    triggers: Vec<String>,
    _listen: Task<()>,
}

struct Document {
    target: Target,
    /// The server connection (`<rootId>:<language>`) and LSP language id, for files a server serves.
    server: Option<(String, &'static str)>,
    uri: String,
    version: i32,
    /// The text the server has; none until it was opened there.
    synced: Option<String>,
    editor: WeakEntity<EditorState>,
    sync: Option<Task<()>>,
    /// What the server reported last, for the hover text.
    diagnostics: Vec<lsp_types::Diagnostic>,
    /// The wavy underlines under them.
    underlines: Option<TextDecorationCollection>,
    _subscription: Subscription,
}

pub struct LanguageServers {
    store: Entity<Store>,
    connections: HashMap<String, Connection>,
    retry_at: HashMap<String, Instant>,
    documents: HashMap<String, Document>,
    pub phases: HashMap<String, ServerPhase>,
    /// The suggestion failure shown last; each message shows once until a suggestion works again.
    reported: Option<String>,
    next_request: u64,
}

struct GlobalLanguageServers(Entity<LanguageServers>);
impl Global for GlobalLanguageServers {}

impl LanguageServers {
    pub fn global(cx: &App) -> Entity<LanguageServers> {
        cx.global::<GlobalLanguageServers>().0.clone()
    }

    pub fn init(store: Entity<Store>, ide: &Entity<Ide>, cx: &mut App) -> Entity<LanguageServers> {
        let servers = cx.new(|cx| {
            cx.subscribe(ide, |this: &mut Self, _, event: &IdeEvent, cx| {
                if *event == IdeEvent::WorkspaceChanged {
                    this.reset(cx);
                }
            })
            .detach();
            LanguageServers {
            store,
            connections: HashMap::new(),
            retry_at: HashMap::new(),
            documents: HashMap::new(),
            phases: HashMap::new(),
            reported: None,
            next_request: 0,
            }
        });
        cx.set_global(GlobalLanguageServers(servers.clone()));
        servers
    }

    /// Give a new editor its providers and, for a file a server serves, open it there.
    pub fn attach(&mut self, key: &str, editor: &Entity<EditorState>, target: Target, cx: &mut Context<Self>) {
        self.detach(key, cx);
        let provider = Rc::new(Provider { key: key.to_owned(), uri: file_uri(&target.path) });
        let show: ShowDocumentHandler = {
            let own = uri_key(&provider.uri);
            Rc::new(move |params, window, cx| {
                let uri = params.uri.to_string();
                if uri_key(&uri) == own {
                    return false;
                }
                let Some(path) = uri_path(&uri) else { return false };
                let selection = params.selection.map(|range| range.start);
                let ide = Ide::global(cx);
                ide.update(cx, |ide, cx| {
                    if let Some(key) = ide.key_from_absolute(&path) {
                        let start = selection.unwrap_or_default();
                        ide.reveal(key, start.line, start.character, window, cx);
                    }
                });
                true
            })
        };
        let server = server_language(&target.path).map(|(language, id)| (identity(&target.root_id, language), id));
        let has_server = server.is_some();
        editor.update(cx, |state, _| {
            let lsp = state.lsp_mut();
            lsp.completion_provider = Some(provider.clone());
            lsp.set_completion_highlighter(Rc::new(highlight_characters));
            if has_server {
                lsp.hover_provider = Some(provider.clone());
                lsp.definition_provider = Some(provider.clone());
                lsp.show_document = Some(show);
            }
        });
        let watched = key.to_owned();
        let subscription = cx.subscribe(editor, move |this: &mut Self, editor, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.schedule_sync(&watched, editor, cx);
            }
        });
        let uri = provider.uri.clone();
        self.documents.insert(
            key.to_owned(),
            Document {
                target,
                server,
                uri,
                version: 0,
                synced: None,
                editor: editor.downgrade(),
                sync: None,
                diagnostics: Vec::new(),
                underlines: None,
                _subscription: subscription,
            },
        );
        self.open_document(key, cx);
    }

    /// Close a file's document on its server.
    pub fn detach(&mut self, key: &str, _cx: &mut Context<Self>) {
        let Some(document) = self.documents.remove(key) else { return };
        if let (Some((server, _)), Some(_)) = (&document.server, &document.synced)
            && let Some(connection) = self.connections.get(server)
        {
            connection.client.notify("textDocument/didClose", json!({ "textDocument": { "uri": document.uri } }));
        }
    }

    /// Stop every server: another project is shown, or the Host restarted.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        for (_, connection) in self.connections.drain() {
            connection.client.close();
        }
        self.documents.clear();
        self.retry_at.clear();
        self.phases.clear();
        cx.notify();
    }

    /// Tools were downloaded: servers that failed for want of them start again for their open files.
    pub fn retry_missing_tools(&mut self, cx: &mut Context<Self>) {
        let waiting: Vec<String> = self.phases.iter().filter(|(_, phase)| matches!(phase, ServerPhase::Failed(reason) if reason.as_ref() == "tools-missing")).map(|(server, _)| server.clone()).collect();
        for server in &waiting {
            self.phases.remove(server);
            self.retry_at.remove(server);
        }
        let keys: Vec<String> = self.documents.iter().filter(|(_, document)| document.server.as_ref().is_some_and(|(name, _)| waiting.contains(name))).map(|(key, _)| key.clone()).collect();
        for key in keys {
            self.open_document(&key, cx);
        }
        cx.notify();
    }

    /// Tell the server a file was saved.
    pub fn saved(&mut self, key: &str) {
        let Some(document) = self.documents.get(key) else { return };
        if let (Some((server, _)), Some(_)) = (&document.server, &document.synced)
            && let Some(connection) = self.connections.get(server)
            && connection.ready
        {
            connection.client.notify("textDocument/didSave", json!({ "textDocument": { "uri": document.uri } }));
        }
    }

    /// Every problem the servers reported in the open files, by file then line (3.x `problems`).
    pub fn problems(&self) -> Vec<Problem> {
        let mut problems: Vec<Problem> = self
            .documents
            .iter()
            .flat_map(|(key, document)| {
                document.diagnostics.iter().filter(|diagnostic| diagnostic.severity != Some(lsp_types::DiagnosticSeverity::HINT)).map(|diagnostic| Problem {
                    key: key.clone(),
                    line: diagnostic.range.start.line + 1,
                    column: diagnostic.range.start.character + 1,
                    error: diagnostic.severity.is_none_or(|severity| severity == lsp_types::DiagnosticSeverity::ERROR),
                    message: diagnostic.message.clone(),
                })
            })
            .collect();
        problems.sort_by(|left, right| (&left.key, left.line, left.column).cmp(&(&right.key, right.line, right.column)));
        problems
    }

    /// The phase of the server of a file, for the status bar.
    pub fn phase_of(&self, key: &str) -> Option<(&'static str, &ServerPhase)> {
        let document = self.documents.get(key)?;
        let (server, _) = document.server.as_ref()?;
        let language = server.rsplit(':').next()?;
        let language = match language {
            "python" => "Python",
            "javascript" => "JavaScript",
            "typescript" => "TypeScript",
            "c" => "C",
            _ => "C++",
        };
        Some((language, self.phases.get(server)?))
    }

    fn schedule_sync(&mut self, key: &str, editor: Entity<EditorState>, cx: &mut Context<Self>) {
        let Some(document) = self.documents.get_mut(key) else { return };
        if document.server.is_none() {
            return;
        }
        let key = key.to_owned();
        let editor = editor.downgrade();
        document.sync = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SYNC_DELAY).await;
            let Some(editor) = editor.upgrade() else { return };
            let _ = this.update(cx, |this, cx| {
                let text = editor.read(cx).value().to_string();
                if this.documents.get(&key).is_some_and(|document| document.synced.is_none()) {
                    this.open_document(&key, cx);
                } else {
                    this.sync_text(&key, text);
                }
            });
        }));
    }

    /// Send the text of a document when the server has another; returns the server once the document is open there.
    fn sync_text(&mut self, key: &str, text: String) -> Option<LspClient> {
        let document = self.documents.get_mut(key)?;
        let (server, _) = document.server.as_ref()?;
        let connection = self.connections.get(server).filter(|connection| connection.ready)?;
        let synced = document.synced.as_ref()?;
        if *synced != text {
            document.version += 1;
            connection.client.notify(
                "textDocument/didChange",
                json!({ "textDocument": { "uri": document.uri, "version": document.version }, "contentChanges": [{ "text": text }] }),
            );
            document.synced = Some(text);
        }
        Some(connection.client.clone())
    }

    /// Open a document on its server, starting the server first when it is not running.
    fn open_document(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(document) = self.documents.get(key) else { return };
        let Some((server, language_id)) = document.server.clone() else { return };
        if !self.connections.contains_key(&server) {
            if self.retry_at.get(&server).is_some_and(|at| *at > Instant::now()) {
                return;
            }
            let target = document.target.clone();
            self.connect(server.clone(), target, cx);
            // The document is opened once the server has answered `initialize`.
            return;
        }
        let connection = &self.connections[&server];
        if !connection.ready {
            return;
        }
        let Some(editor) = document.editor.upgrade() else { return };
        let text = editor.read(cx).value().to_string();
        let document = self.documents.get_mut(key).expect("document checked above");
        if document.synced.is_some() {
            return;
        }
        document.version += 1;
        connection.client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": document.uri, "languageId": language_id, "version": document.version, "text": text } }),
        );
        document.synced = Some(text);
    }

    fn connect(&mut self, server: String, target: Target, cx: &mut Context<Self>) {
        let Some(rpc) = self.store.read(cx).rpc.clone() else { return };
        let Some(language) = server.rsplit(':').next().and_then(static_language) else { return };
        let root_uri = file_uri(&target.root_path);
        let python = (language == "python").then(|| target.python_path.clone()).flatten();
        let answer: Answer = {
            let (root_uri, title, python) = (root_uri.clone(), target.root_title.clone(), python.clone());
            Box::new(move |method, params| match method {
                "workspace/configuration" => {
                    let items = params.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
                    Ok(Value::Array(
                        items
                            .iter()
                            .map(|item| match (item.get("section").and_then(Value::as_str), &python) {
                                (Some("python"), Some(path)) => json!({ "pythonPath": path }),
                                (Some("python.pythonPath"), Some(path)) => json!(path),
                                _ => Value::Null,
                            })
                            .collect(),
                    ))
                }
                "workspace/workspaceFolders" => Ok(json!([{ "uri": root_uri, "name": title }])),
                "client/registerCapability" | "client/unregisterCapability" | "window/workDoneProgress/create" | "window/showMessageRequest" => {
                    Ok(Value::Null)
                }
                "workspace/applyEdit" => Ok(json!({ "applied": false })),
                _ => Err(format!("Method not implemented: {method}")),
            })
        };
        let root_id = (!target.root_id.is_empty()).then_some(target.root_id.as_str());
        let (client, events) = LspClient::connect(&rpc, lsp::route(&target.workspace_id, language, root_id), answer);
        let mut initialize = json!({
            "processId": null,
            "rootUri": root_uri,
            "workspaceFolders": [{ "uri": root_uri, "name": target.root_title }],
            "capabilities": client_capabilities(),
        });
        if let Some(path) = &python {
            initialize["initializationOptions"] = json!({ "pythonPath": path });
        }
        let initialized = client.request("initialize", initialize);
        let listen = cx.spawn({
            let server = server.clone();
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let result = initialized.await;
                let ok = this
                    .update(cx, |this, cx| this.initialized(&server, result, cx))
                    .unwrap_or(false);
                if !ok {
                    return;
                }
                while let Ok(event) = events.recv().await {
                    let closed = matches!(event, LspIncoming::Closed { .. });
                    if this.update(cx, |this, cx| this.incoming(&server, event, cx)).is_err() || closed {
                        break;
                    }
                }
            }
        });
        self.phases.insert(server.clone(), ServerPhase::Starting);
        self.connections.insert(server, Connection { client, target, language, ready: false, triggers: Vec::new(), _listen: listen });
        cx.notify();
    }

    /// The server answered `initialize`: finish the handshake and open the documents waiting for it.
    fn initialized(&mut self, server: &str, result: Result<Value>, cx: &mut Context<Self>) -> bool {
        let Some(connection) = self.connections.get_mut(server) else { return false };
        match result {
            Ok(value) => {
                connection.triggers = value
                    .pointer("/capabilities/completionProvider/triggerCharacters")
                    .and_then(Value::as_array)
                    .map(|chars| chars.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                    .unwrap_or_default();
                connection.client.notify("initialized", json!({}));
                if connection.language == "python"
                    && let Some(path) = &connection.target.python_path
                {
                    connection.client.notify("workspace/didChangeConfiguration", json!({ "settings": { "python": { "pythonPath": path } } }));
                }
                connection.ready = true;
                self.retry_at.remove(server);
                self.phases.insert(server.to_owned(), ServerPhase::Ready);
                let waiting: Vec<String> = self
                    .documents
                    .iter()
                    .filter(|(_, document)| document.server.as_ref().is_some_and(|(name, _)| name == server) && document.synced.is_none())
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in waiting {
                    self.open_document(&key, cx);
                }
                cx.notify();
                true
            }
            Err(error) => {
                log::warn!("Language server {server} did not start: {error:#}");
                self.failed(server, error.to_string(), cx);
                false
            }
        }
    }

    fn incoming(&mut self, server: &str, event: LspIncoming, cx: &mut Context<Self>) {
        match event {
            LspIncoming::Notification { method, params } if method == "textDocument/publishDiagnostics" => {
                let Some(uri) = params.get("uri").and_then(Value::as_str) else { return };
                let diagnostics: Vec<lsp_types::Diagnostic> =
                    params.get("diagnostics").cloned().and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default();
                let wanted = uri_key(uri);
                let keys: Vec<String> =
                    self.documents.iter().filter(|(_, document)| uri_key(&document.uri) == wanted).map(|(key, _)| key.clone()).collect();
                for key in keys {
                    self.show_diagnostics(&key, diagnostics.clone(), cx);
                }
            }
            LspIncoming::Notification { .. } => {}
            LspIncoming::Closed { reason, restart } => {
                let Some(connection) = self.connections.remove(server) else { return };
                let served: Vec<String> = self
                    .documents
                    .iter_mut()
                    .filter(|(_, document)| document.server.as_ref().is_some_and(|(name, _)| name == server))
                    .map(|(key, document)| {
                        document.synced = None;
                        key.clone()
                    })
                    .collect();
                if restart {
                    // The Host replaced the server for a changed interpreter environment; reopen the documents.
                    self.connect(server.to_owned(), connection.target, cx);
                } else {
                    log::warn!("Language server {server} stopped: {reason}");
                    self.retry_at.insert(server.to_owned(), Instant::now() + SERVER_RETRY);
                    self.phases.insert(server.to_owned(), ServerPhase::Failed(reason.into()));
                    let _ = served;
                }
                cx.notify();
            }
        }
    }

    /// Underline what the server reported in a file. GPUI Kit's own diagnostic underline has no color with the syntect
    /// highlighter Rainy uses, so the underlines are text decorations, and the messages join the hover text.
    fn show_diagnostics(&mut self, key: &str, diagnostics: Vec<lsp_types::Diagnostic>, cx: &mut Context<Self>) {
        let Some(document) = self.documents.get_mut(key) else { return };
        let Some(editor) = document.editor.upgrade() else { return };
        let text = editor.read(cx).text().clone();
        let theme = cx.theme();
        // The most severe first; a weaker one over the same text is left out, so one color shows.
        let mut ordered: Vec<&lsp_types::Diagnostic> = diagnostics.iter().collect();
        ordered.sort_by_key(|diagnostic| diagnostic.severity.map_or(1, |severity| match severity {
            lsp_types::DiagnosticSeverity::ERROR => 1,
            lsp_types::DiagnosticSeverity::WARNING => 2,
            lsp_types::DiagnosticSeverity::INFORMATION => 3,
            _ => 4,
        }));
        let mut kept: Vec<std::ops::Range<usize>> = Vec::new();
        let decorations: Vec<TextDecoration> = ordered
            .into_iter()
            .filter_map(|diagnostic| {
                let color = match diagnostic.severity {
                    Some(lsp_types::DiagnosticSeverity::WARNING) => theme.yellow,
                    Some(lsp_types::DiagnosticSeverity::INFORMATION) => theme.blue,
                    Some(lsp_types::DiagnosticSeverity::HINT) => return None,
                    _ => theme.red,
                };
                let start = byte_offset(&text, diagnostic.range.start);
                let mut end = byte_offset(&text, diagnostic.range.end);
                // A diagnostic at a point still gets a mark: the character after it, or before it at a line end.
                if end <= start {
                    end = text.slice(start..text.len()).chars().next().filter(|ch| *ch != '\n').map_or(start, |ch| start + ch.len_utf8());
                }
                let start = if end <= start { text.slice(0..start).chars().last().map_or(start, |ch| start - ch.len_utf8()) } else { start };
                if end <= start || kept.iter().any(|range| range.start < end && start < range.end) {
                    return None;
                }
                kept.push(start..end);
                Some({
                    TextDecoration::new(start..end, HighlightStyle {
                        underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.), wavy: true }),
                        ..Default::default()
                    })
                })
            })
            .collect();
        document.diagnostics = diagnostics;
        match &document.underlines {
            Some(underlines) => underlines.set(decorations, cx),
            None => {
                let underlines = editor.update(cx, |state, cx| state.create_decorations_collection(decorations, cx));
                document.underlines = Some(underlines);
            }
        }
    }

    fn failed(&mut self, server: &str, message: String, cx: &mut Context<Self>) {
        if let Some(connection) = self.connections.remove(server) {
            connection.client.close();
        }
        for document in self.documents.values_mut() {
            if document.server.as_ref().is_some_and(|(name, _)| name == server) {
                document.synced = None;
            }
        }
        self.retry_at.insert(server.to_owned(), Instant::now() + SERVER_RETRY);
        self.phases.insert(server.to_owned(), ServerPhase::Failed(message.into()));
        cx.notify();
    }

    /// Whether a server serves the file.
    pub fn has_server(&self, key: &str) -> bool {
        self.documents.get(key).is_some_and(|document| document.server.is_some())
    }

    /// Ask the server of a document something about it, with the text synced first; `params` gets the document.
    pub fn document_request(
        &mut self,
        key: &str,
        text: &Rope,
        method: &str,
        mut params: Value,
    ) -> Option<impl std::future::Future<Output = Result<Value>> + Send + 'static + use<>> {
        let (client, uri, _) = self.ready_server(key, text)?;
        params["textDocument"] = json!({ "uri": uri });
        Some(client.request(method, params))
    }

    /// The server of a document with the text synced, and the document's URI and trigger characters.
    fn ready_server(&mut self, key: &str, text: &Rope) -> Option<(LspClient, String, Vec<String>)> {
        let client = self.sync_text(key, text.to_string())?;
        let document = self.documents.get(key)?;
        let (server, _) = document.server.as_ref()?;
        let triggers = self.connections.get(server).map(|connection| connection.triggers.clone()).unwrap_or_default();
        Some((client, document.uri.clone(), triggers))
    }
}

fn identity(root_id: &str, language: &str) -> String {
    format!("{root_id}:{language}")
}

fn static_language(language: &str) -> Option<&'static str> {
    ["python", "javascript", "typescript", "c", "cpp"].into_iter().find(|known| *known == language)
}

fn client_capabilities() -> Value {
    json!({
        "general": { "positionEncodings": ["utf-16"] },
        "workspace": { "configuration": true, "workspaceFolders": true, "workspaceEdit": { "documentChanges": true } },
        "textDocument": {
            "synchronization": { "didSave": true, "dynamicRegistration": false },
            "completion": {
                "contextSupport": true,
                "completionItem": { "snippetSupport": false, "documentationFormat": ["markdown", "plaintext"], "deprecatedSupport": true },
            },
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "definition": { "linkSupport": true },
            "references": {},
            "rename": { "prepareSupport": false },
            "formatting": {},
            "publishDiagnostics": { "relatedInformation": false },
        },
    })
}

/// The LSP position (UTF-16 columns) of a byte offset.
pub fn utf16_position(text: &Rope, offset: usize) -> Position {
    let offset = offset.min(text.len());
    let point = text.offset_to_point(offset);
    let start = text.line_start_offset(point.row);
    let column = text.slice(start..offset).to_string().encode_utf16().count();
    Position::new(point.row as u32, column as u32)
}

/// The byte offset of an LSP position (UTF-16 column), clamped to its line.
fn byte_offset(text: &Rope, position: Position) -> usize {
    let row = position.line as usize;
    if row >= text.lines_len() {
        return text.len();
    }
    let start = text.line_start_offset(row);
    let line = text.slice(start..text.line_end_offset(row)).to_string();
    let mut units = 0u32;
    for (index, ch) in line.char_indices() {
        if units >= position.character {
            return start + index;
        }
        units += ch.len_utf16() as u32;
    }
    start + line.len()
}

/// The word typed before the cursor.
fn word_before(text: &Rope, offset: usize) -> String {
    let start = text.line_start_offset(text.offset_to_point(offset.min(text.len())).row);
    let line = text.slice(start..offset.min(text.len())).to_string();
    let word: String = line.chars().rev().take_while(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '$').collect();
    word.chars().rev().collect()
}

/// Whether the typed word matches a completion: its characters in order, case-insensitively; prefixes rank first.
fn matches(word: &str, item: &CompletionItem) -> Option<u8> {
    if word.is_empty() {
        return Some(1);
    }
    let candidate = item.filter_text.as_deref().unwrap_or(&item.label).to_lowercase();
    let word = word.to_lowercase();
    if candidate.starts_with(&word) {
        return Some(0);
    }
    let mut rest = candidate.chars();
    word.chars().all(|ch| rest.any(|other| other == ch)).then_some(1)
}

/// A completion that replaces the word typed before the cursor, `start..end`. The editor puts an item's `insertText`
/// at the cursor beside that word (`pri` became `priPRIMARYLANGID`), where servers mean it to take the word's place
/// as its label does; Python's automatic imports come that way.
fn replacing_word(mut item: CompletionItem, start: Position, end: Position) -> CompletionItem {
    if item.text_edit.is_some() {
        return item;
    }
    match item.insert_text.take() {
        // The label already goes in over the word.
        Some(text) if text == item.label => {}
        Some(text) => item.text_edit = Some(CompletionTextEdit::Edit(TextEdit::new(lsp_types::Range::new(start, end), text))),
        None => {}
    }
    item
}

/// What an editor asks its file's server and the Host.
struct Provider {
    key: String,
    uri: String,
}

impl Provider {
    fn servers(cx: &App) -> Entity<LanguageServers> {
        LanguageServers::global(cx)
    }
}

impl CompletionProvider for Provider {
    fn completions(&self, text: &Rope, offset: usize, trigger: CompletionContext, _: &mut Window, cx: &mut App) -> Task<Result<CompletionResponse>> {
        let servers = Self::servers(cx);
        let Some((client, uri, triggers)) = servers.update(cx, |servers, _| servers.ready_server(&self.key, text)) else {
            return Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        };
        let typed = trigger.trigger_character.unwrap_or_default();
        let last: String = typed.chars().last().map(String::from).unwrap_or_default();
        let context = if trigger.trigger_kind == CompletionTriggerKind::TRIGGER_CHARACTER && triggers.contains(&last) {
            json!({ "triggerKind": 2, "triggerCharacter": last })
        } else {
            json!({ "triggerKind": 1 })
        };
        let position = utf16_position(text, offset);
        let word = word_before(text, offset);
        let word_start = Position::new(position.line, position.character.saturating_sub(word.encode_utf16().count() as u32));
        let request = client.request(
            "textDocument/completion",
            json!({ "textDocument": { "uri": uri }, "position": position, "context": context }),
        );
        cx.background_spawn(async move {
            let value = request.await?;
            let response: Option<CompletionResponse> = serde_json::from_value(value)?;
            let items = match response {
                Some(CompletionResponse::Array(items)) => items,
                Some(CompletionResponse::List(list)) => list.items,
                None => Vec::new(),
            };
            let mut ranked: Vec<(u8, CompletionItem)> = items.into_iter().filter_map(|item| Some((matches(&word, &item)?, item))).collect();
            ranked.sort_by(|(left_rank, left), (right_rank, right)| {
                left_rank.cmp(right_rank).then_with(|| {
                    left.sort_text.as_deref().unwrap_or(&left.label).cmp(right.sort_text.as_deref().unwrap_or(&right.label))
                })
            });
            let items = ranked.into_iter().take(MAX_COMPLETIONS).map(|(_, item)| replacing_word(item, word_start, position));
            Ok(CompletionResponse::Array(items.collect()))
        })
    }

    fn inline_completion(
        &self,
        text: &Rope,
        offset: usize,
        _: InlineCompletionContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<InlineCompletionResponse>> {
        let none = || Task::ready(Ok(InlineCompletionResponse::Array(Vec::new())));
        let servers = Self::servers(cx);
        let (store, target) = {
            let servers = servers.read(cx);
            let Some(document) = servers.documents.get(&self.key) else { return none() };
            (servers.store.clone(), document.target.clone())
        };
        if !store.read(cx).prefs.inline_completion {
            return none();
        }
        // Only where the rest of the line is closing punctuation, so a suggestion never splits code the user wrote.
        let offset = offset.min(text.len());
        let row = text.offset_to_point(offset).row;
        let rest = text.slice(offset..text.line_end_offset(row)).to_string();
        if !rest.chars().all(|ch| ch.is_whitespace() || ")]}>'\"`;,:".contains(ch)) {
            return none();
        }
        let before = text.slice(0..offset).to_string();
        let after = text.slice(offset..text.len()).to_string();
        let prefix: String = {
            let count = before.chars().count();
            before.chars().skip(count.saturating_sub(SUGGEST_BEFORE)).collect()
        };
        let suffix: String = after.chars().take(SUGGEST_AFTER).collect();
        let request_id = servers.update(cx, |servers, _| {
            servers.next_request += 1;
            format!("suggest-native-{}", servers.next_request)
        });
        let call = store.read(cx).call(
            "inline.complete",
            json!({ "requestId": request_id, "path": target.relative, "language": crate::store::ide::language_of(&target.path), "prefix": prefix, "suffix": suffix }),
        );
        let position = utf16_position(text, offset);
        let handle = window.window_handle();
        cx.spawn(async move |cx| {
            // The editor already waits before asking; this keeps 3.x's shorter pause from mattering less than typing.
            let value = call.await.map_err(|error| anyhow!(error.message))?;
            let suggestion = value.get("text").and_then(Value::as_str).unwrap_or_default().to_owned();
            let error = value.get("error").and_then(Value::as_str).map(str::to_owned);
            servers.update(cx, |servers, cx| match &error {
                Some(message) if servers.reported.as_ref() != Some(message) => {
                    servers.reported = Some(message.clone());
                    let message = message.clone();
                    let _ = handle.update(cx, |_, window, cx| {
                        window.push_notification(gpui_kit::component::notification::Notification::error(message), cx)
                    });
                }
                Some(_) => {}
                None => servers.reported = None,
            });
            if suggestion.is_empty() {
                return Ok(InlineCompletionResponse::Array(Vec::new()));
            }
            Ok(InlineCompletionResponse::Array(vec![InlineCompletionItem {
                insert_text: suggestion,
                filter_text: None,
                range: Some(lsp_types::Range::new(position, position)),
                command: None,
                insert_text_format: None,
            }]))
        })
    }

    fn inline_completion_debounce(&self) -> Duration {
        SUGGEST_DELAY
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, cx: &mut App) -> bool {
        let servers = Self::servers(cx).read(cx);
        let Some(document) = servers.documents.get(&self.key) else { return false };
        let Some((server, _)) = &document.server else { return false };
        let Some(connection) = servers.connections.get(server).filter(|connection| connection.ready) else { return false };
        let Some(last) = new_text.chars().last() else { return false };
        last.is_alphanumeric() || last == '_' || connection.triggers.iter().any(|trigger| trigger.ends_with(last))
    }
}

impl HoverProvider for Provider {
    fn hover(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<Result<Option<Hover>>> {
        let servers = Self::servers(cx);
        let position = utf16_position(text, offset);
        // The problems reported where the mouse is come first.
        let problems: Vec<String> = servers
            .read(cx)
            .documents
            .get(&self.key)
            .map(|document| {
                document
                    .diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.range.start <= position && position <= diagnostic.range.end)
                    .map(|diagnostic| match &diagnostic.source {
                        Some(source) => format!("{} ({source})", diagnostic.message),
                        None => diagnostic.message.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let Some((client, uri, _)) = servers.update(cx, |servers, _| servers.ready_server(&self.key, text)) else {
            return Task::ready(Ok(None));
        };
        let request = client.request("textDocument/hover", json!({ "textDocument": { "uri": uri }, "position": position }));
        cx.background_spawn(async move {
            let hover: Option<Hover> = request.await.ok().and_then(|value| serde_json::from_value(value).ok()).flatten();
            if problems.is_empty() {
                return Ok(hover);
            }
            let mut parts = vec![problems.join("\n\n")];
            if let Some(hover) = &hover {
                let info = hover_markdown(&hover.contents);
                if !info.trim().is_empty() {
                    parts.push(info);
                }
            }
            Ok(Some(Hover {
                contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                    kind: lsp_types::MarkupKind::Markdown,
                    value: parts.join("\n\n---\n\n"),
                }),
                range: hover.and_then(|hover| hover.range),
            }))
        })
    }
}

/// Hover contents as Markdown.
fn hover_markdown(contents: &lsp_types::HoverContents) -> String {
    let marked = |text: &lsp_types::MarkedString| match text {
        lsp_types::MarkedString::String(text) => text.clone(),
        lsp_types::MarkedString::LanguageString(code) => format!("```{}\n{}\n```", code.language, code.value),
    };
    match contents {
        lsp_types::HoverContents::Scalar(text) => marked(text),
        lsp_types::HoverContents::Array(texts) => texts.iter().map(marked).collect::<Vec<_>>().join("\n\n"),
        lsp_types::HoverContents::Markup(markup) => markup.value.clone(),
    }
}

impl DefinitionProvider for Provider {
    fn definitions(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<Result<Vec<LocationLink>>> {
        let servers = Self::servers(cx);
        let Some((client, uri, _)) = servers.update(cx, |servers, _| servers.ready_server(&self.key, text)) else {
            return Task::ready(Ok(Vec::new()));
        };
        let request =
            client.request("textDocument/definition", json!({ "textDocument": { "uri": uri }, "position": utf16_position(text, offset) }));
        cx.background_spawn(async move {
            let response: Option<GotoDefinitionResponse> = serde_json::from_value(request.await?)?;
            Ok(match response {
                None => Vec::new(),
                Some(GotoDefinitionResponse::Scalar(location)) => vec![link(location)],
                Some(GotoDefinitionResponse::Array(locations)) => locations.into_iter().map(link).collect(),
                Some(GotoDefinitionResponse::Link(links)) => links,
            })
        })
    }
}

fn link(location: lsp_types::Location) -> LocationLink {
    LocationLink { origin_selection_range: None, target_uri: location.uri, target_range: location.range, target_selection_range: location.range }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn counts_columns_in_utf16_units_for_servers() {
        let text = Rope::from("a😀b\nxy");
        assert_eq!(utf16_position(&text, "a😀".len()), Position::new(0, 3));
        assert_eq!(utf16_position(&text, "a😀b\nx".len()), Position::new(1, 1));
        assert_eq!(byte_offset(&text, Position::new(0, 3)), "a😀".len());
        assert_eq!(byte_offset(&text, Position::new(1, 9)), text.len());
    }

    #[test]
    fn ranks_prefix_matches_before_scattered_ones() {
        let item = |label: &str| CompletionItem { label: label.into(), ..Default::default() };
        assert_eq!(matches("pri", &item("print")), Some(0));
        assert_eq!(matches("pnt", &item("print")), Some(1));
        assert_eq!(matches("xyz", &item("print")), None);
        assert_eq!(word_before(&Rope::from("  foo.ba"), 8), "ba");
    }

    #[test]
    fn completions_replace_the_typed_word() {
        let (start, end) = (Position::new(3, 4), Position::new(3, 7));
        let item = |label: &str, insert: Option<&str>| CompletionItem { label: label.into(), insert_text: insert.map(Into::into), ..Default::default() };
        // Text of its own takes the word's place instead of going in beside it.
        let replaced = replacing_word(item("PRIMARYLANGID", Some("PRIMARYLANGID_X")), start, end);
        assert_eq!(replaced.text_edit, Some(CompletionTextEdit::Edit(TextEdit::new(lsp_types::Range::new(start, end), "PRIMARYLANGID_X".into()))));
        assert_eq!(replaced.insert_text, None);
        // The same text as the label goes in as the label does.
        let same = replacing_word(item("print", Some("print")), start, end);
        assert_eq!((same.insert_text, same.text_edit), (None, None));
        // A server's own edit and a bare label are left as they are.
        assert_eq!(replacing_word(item("print", None), start, end), item("print", None));
        let edit = CompletionTextEdit::Edit(TextEdit::new(lsp_types::Range::new(end, end), "x".into()));
        let edited = CompletionItem { text_edit: Some(edit.clone()), ..item("x", Some("y")) };
        assert_eq!(replacing_word(edited.clone(), start, end), edited);
    }

    #[test]
    fn knows_which_files_have_a_server() {
        assert_eq!(server_language("src/a.tsx"), Some(("typescript", "typescriptreact")));
        assert_eq!(server_language("a.h"), Some(("c", "c")));
        assert_eq!(server_language("README.md"), None);
    }
}
