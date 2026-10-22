//! Projects, their file trees and the open files (`src/renderer/ide/ide-model.ts`).
//!
//! File keys follow the renderer: a path relative to the primary root as it is, and `"\0<rootId>/<path>"` for a
//! path in an attached root, so the editor state saved per project stays readable by Rainy 3.x.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use gpui_kit::component::diff::{DiffFile, DiffState};
use gpui_kit::component::input::{EditorState, InputEvent, Position};
use gpui_kit::*;
use rainy_protocol::{
    FileEntry, IdeDirectory, IdeFileChange, IdeFileDiff, IdeFileDocument, IdeWorkspace, IdeWorkspaceState, RpcError,
    WorkspaceRoot, WorkspaceSelection,
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::store::Store;

const PREFIX: char = '\0';

/// How often open files and listings are checked against the disk (3.x `editorPollMs`).
const POLL: Duration = Duration::from_secs(10);

/// A root-qualified file key.
pub fn file_key(path: &str, root: Option<&WorkspaceRoot>) -> String {
    match root {
        Some(root) if !root.primary => format!("{PREFIX}{}/{path}", root.root_id),
        _ => path.to_owned(),
    }
}

/// The root id (none for the primary root) and relative path of a key.
pub fn split_key(key: &str) -> (Option<&str>, &str) {
    match key.strip_prefix(PREFIX) {
        Some(rest) => match rest.split_once('/') {
            Some((root, path)) => (Some(root), path),
            None => (Some(rest), ""),
        },
        None => (None, key),
    }
}

/// The key of a child of a directory key.
pub fn child_key(directory: &str, name: &str) -> String {
    let (root, path) = split_key(directory);
    let path = if path.is_empty() { name.to_owned() } else { format!("{path}/{name}") };
    match root {
        Some(root) => format!("{PREFIX}{root}/{path}"),
        None => path,
    }
}

/// The key of the directory holding a key.
pub fn parent_key(key: &str) -> String {
    let (root, path) = split_key(key);
    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    match root {
        Some(root) => format!("{PREFIX}{root}/{parent}"),
        None => parent.to_owned(),
    }
}

/// The file name of a key.
pub fn file_name(key: &str) -> &str {
    let (_, path) = split_key(key);
    path.rsplit('/').next().unwrap_or(path)
}

/// The editor language of a path, in GPUI Kit's names; plain text when unknown.
pub fn language_of(path: &str) -> &'static str {
    let extension = path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match extension.as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "json" | "jsonc" => "json",
        "md" | "markdown" => "markdown",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "sh" | "bash" | "zsh" => "bash",
        "ps1" | "psm1" => "powershell",
        "sql" => "sql",
        "php" => "php",
        "rb" => "ruby",
        "lua" => "lua",
        "swift" => "swift",
        "scala" => "scala",
        "zig" => "zig",
        "xml" | "svg" => "xml",
        "diff" | "patch" => "diff",
        _ => "text",
    }
}

/// One open file.
pub struct OpenFile {
    pub document: IdeFileDocument,
    /// The editor of a text file; read-only files have none.
    pub editor: Option<Entity<EditorState>>,
    /// The text as last saved or read.
    pub saved: String,
    pub dirty: bool,
    /// The file changed on disk while it had unsaved edits.
    pub conflict: bool,
    /// The version on disk when it differs from the one read; saving over it overwrites that version.
    pub disk_version: Option<String>,
    _subscription: Option<Subscription>,
}

/// A tab showing a file against its Git base.
#[derive(Default)]
pub struct DiffTab {
    pub state: Option<Entity<DiffState>>,
    pub error: Option<SharedString>,
    /// `modified`, `added`, `deleted` or `unchanged`.
    pub status: String,
}

/// A unified diff of two texts with every line as context, so the viewer can fold unchanged runs itself.
pub fn unified_patch(path: &str, base: &str, current: &str) -> String {
    let diff = similar::TextDiff::from_lines(base, current);
    let mut out = String::new();
    let mut unified = diff.unified_diff();
    unified.context_radius(usize::MAX / 4).header(&format!("a/{path}"), &format!("b/{path}"));
    out.push_str(&unified.to_string());
    out
}

/// What a directory listing holds.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    pub entries: Vec<FileEntry>,
    pub error: Option<SharedString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdeEvent {
    WorkspaceChanged,
    FilesChanged,
    ActiveChanged,
    /// A file was written from the editor.
    Saved,
    /// The projects were loaded and the one selected last is shown again; launched paths may open now.
    Restored,
}

/// A file or folder handed to Rainy, as a path on the Host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub path: String,
    pub directory: bool,
}

/// The folder of a Host path in the path's own spelling (`parentDirectory`); a file at the root of a drive gives the
/// drive's root.
pub fn parent_directory(path: &str) -> String {
    let Some(separator) = path.rfind(['/', '\\']) else { return path.to_owned() };
    let parent = &path[..separator];
    let drive = parent.len() == 2 && parent.as_bytes()[0].is_ascii_alphabetic() && parent.as_bytes()[1] == b':';
    if drive {
        format!("{parent}\\")
    } else if parent.is_empty() {
        "/".to_owned()
    } else {
        parent.to_owned()
    }
}

/// The key of an absolute Host path inside one of a project's folders, the most specific folder first.
pub fn key_in(workspace: &IdeWorkspace, path: &str) -> Option<String> {
    let candidate = path.replace('\\', "/");
    let windows = candidate.as_bytes().get(1) == Some(&b':') || candidate.starts_with("//");
    let fold = |value: &str| if windows { value.to_lowercase() } else { value.to_owned() };
    let mut roots = workspace.roots();
    roots.sort_by_key(|root| std::cmp::Reverse(root.path.len()));
    roots.into_iter().find_map(|root| {
        let prefix = format!("{}/", root.path.replace('\\', "/").trim_end_matches('/'));
        fold(&candidate).starts_with(&fold(&prefix)).then(|| file_key(&candidate[prefix.len()..], Some(&root)))
    })
}

/// The project whose folder holds a path most closely, with the path's key there.
pub fn holding_workspace(workspaces: &[IdeWorkspace], path: &str) -> Option<(IdeWorkspace, String)> {
    workspaces
        .iter()
        .filter_map(|workspace| Some((workspace.clone(), key_in(workspace, path)?)))
        .min_by_key(|(_, key)| split_key(key).1.len())
}

pub struct Ide {
    store: Entity<Store>,
    pub workspaces: Vec<IdeWorkspace>,
    pub workspace: Option<IdeWorkspace>,
    /// Listings by directory key; the root of a mount is its key with an empty path.
    pub listings: HashMap<String, Listing>,
    pub expanded: BTreeSet<String>,
    pub tabs: Vec<String>,
    /// Tabs showing a diff instead of the editor.
    pub diff_tabs: BTreeSet<String>,
    /// How a tab shows its file when it is not the default (`source`, `preview`, `split`, `rich`), as 3.x stores it.
    pub views: HashMap<String, String>,
    pub diffs: HashMap<String, DiffTab>,
    pub active: Option<String>,
    pub files: HashMap<String, OpenFile>,
    pub loading: bool,
    pub error: Option<SharedString>,
    /// Whether `initialize` has shown the project selected last (`IdeEvent::Restored`).
    pub restored: bool,
    state: Option<IdeWorkspaceState>,
    save_task: Option<Task<()>>,
    /// The window editors are created in, for files reloaded after the Host reported a change.
    window: Option<AnyWindowHandle>,
    /// Tabs that close once their save succeeds.
    closing: BTreeSet<String>,
    /// The chat to show for the project just opened, until the chat list has loaded.
    pending_session: Option<Option<String>>,
    /// Lists the shown directories again a moment after the Host reports changed files.
    relist: Option<Task<()>>,
    /// Checks open files and listings every few seconds; asking also keeps the Host watching their folders.
    poll: Option<Task<()>>,
    /// Unsaved text kept in the project state and not applied yet, by file key, as stored.
    recovery: HashMap<String, Value>,
    /// Where to put the cursor once a file being opened has loaded: zero-based line and UTF-16 column.
    reveals: HashMap<String, (u32, u32)>,
    /// The file whose editor takes the cursor once loaded.
    focus_next: Option<String>,
    /// The latest `files.read` of each key; the answer to an earlier one, or to one sent before another project was
    /// shown, is dropped.
    reads: HashMap<String, u64>,
    read_count: u64,
}

impl EventEmitter<IdeEvent> for Ide {}

struct GlobalIde(Entity<Ide>);
impl Global for GlobalIde {}

fn ide_error(error: RpcError) -> SharedString {
    error.message.into()
}

impl Ide {
    pub fn global(cx: &App) -> Entity<Ide> {
        cx.global::<GlobalIde>().0.clone()
    }

    pub fn init(store: Entity<Store>, cx: &mut App) -> Entity<Ide> {
        let ide = cx.new(|cx| {
            cx.subscribe(&store, |this: &mut Ide, _, event: &crate::store::StoreEvent, cx| match event {
                crate::store::StoreEvent::Host { name, data } => this.on_host_event(name, data, cx),
                // The chat shown last in a project comes back with it.
                crate::store::StoreEvent::SelectionChanged if this.workspace.is_some() => this.schedule_state_save(cx),
                crate::store::StoreEvent::SummariesChanged => this.restore_session(cx),
                _ => {}
            })
            .detach();
            Ide {
                store,
                workspaces: Vec::new(),
                workspace: None,
                listings: HashMap::new(),
                expanded: BTreeSet::new(),
                tabs: Vec::new(),
                diff_tabs: BTreeSet::new(),
                views: HashMap::new(),
                diffs: HashMap::new(),
                active: None,
                files: HashMap::new(),
                loading: false,
                error: None,
                restored: false,
                state: None,
                save_task: None,
                window: None,
                closing: BTreeSet::new(),
                pending_session: None,
                reveals: HashMap::new(),
                relist: None,
                poll: None,
                recovery: HashMap::new(),
                focus_next: None,
                reads: HashMap::new(),
                read_count: 0,
            }
        });
        cx.set_global(GlobalIde(ide.clone()));
        ide
    }

    /// One `ide` request.
    fn request<T: DeserializeOwned + 'static>(&self, params: Value, cx: &App) -> impl Future<Output = Result<T, RpcError>> + use<T> {
        let call = self.store.read(cx).call("ide", params);
        async move {
            let value = call.await?;
            serde_json::from_value(value).map_err(|error| RpcError { code: "invalid".into(), message: error.to_string(), data: None })
        }
    }

    fn workspace_id(&self) -> Option<String> {
        self.workspace.as_ref().map(|workspace| workspace.workspace_id.clone())
    }

    /// The request fields locating a key: the workspace, the root of an attached directory, and the path.
    fn location(&self, key: &str) -> Map<String, Value> {
        let (root, path) = split_key(key);
        let mut fields = Map::new();
        fields.insert("workspaceId".into(), json!(self.workspace_id()));
        if let Some(root) = root {
            fields.insert("rootId".into(), json!(root));
        }
        fields.insert("path".into(), json!(path));
        fields
    }

    /// Load the projects and reopen the one selected last (`initialize`).
    pub fn initialize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window = Some(window.window_handle());
        let handle = window.window_handle();
        self.poll = Some(cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(POLL).await;
                let Some(ide) = this.upgrade() else { return };
                let _ = handle.update(cx, |_, window, cx| {
                    ide.update(cx, |ide, cx| {
                        ide.check_files(cx);
                        ide.refresh(window, cx);
                    })
                });
            }
        }));
        let list = self.request::<Vec<IdeWorkspace>>(json!({ "op": "workspaces.list" }), cx);
        let selection = self.request::<WorkspaceSelection>(json!({ "op": "state.selection.read" }), cx);
        cx.spawn_in(window, async move |this, cx| {
            let (list, selection) = (list.await, selection.await);
            let shown = this.update_in(cx, |this, window, cx| {
                match list {
                    Ok(workspaces) => this.workspaces = workspaces,
                    Err(error) => this.error = Some(ide_error(error)),
                }
                let selected = selection.ok().and_then(|selection| selection.workspace_id);
                let shown = selected
                    .and_then(|id| this.workspaces.iter().find(|w| w.workspace_id == id).cloned())
                    .map(|workspace| this.select_task(workspace, window, cx));
                cx.notify();
                shown
            });
            if let Ok(Some(shown)) = shown {
                shown.await;
            }
            let _ = this.update(cx, |this, cx| {
                this.restored = true;
                cx.emit(IdeEvent::Restored);
            });
        })
        .detach();
    }

    /// Register a folder as a project and open it.
    pub fn open_folder(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_folder_task(path, window, cx).detach();
    }

    /// `open_folder`, done once the project's saved tabs are shown; `false` when it could not be opened.
    fn open_folder_task(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        let request = self.request::<IdeWorkspace>(json!({ "op": "workspaces.open", "path": path }), cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let shown = this.update_in(cx, |this, window, cx| match result {
                Ok(workspace) => {
                    this.workspaces.retain(|w| w.workspace_id != workspace.workspace_id);
                    this.workspaces.push(workspace.clone());
                    Some(this.select_task(workspace, window, cx))
                }
                Err(error) => {
                    this.error = Some(ide_error(error));
                    cx.notify();
                    None
                }
            });
            match shown {
                Ok(Some(shown)) => {
                    shown.await;
                    true
                }
                _ => false,
            }
        })
    }

    /// Show a project: its saved tabs and expanded folders (`selectWorkspace`).
    pub fn select(&mut self, workspace: IdeWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        self.select_task(workspace, window, cx).detach();
    }

    /// `select`, done once the project's saved tabs are shown and their files read.
    fn select_task(&mut self, workspace: IdeWorkspace, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        if self.workspace.as_ref().is_some_and(|current| current.workspace_id == workspace.workspace_id) {
            return Task::ready(());
        }
        self.flush_state(cx);
        self.workspace = Some(workspace.clone());
        self.listings.clear();
        self.expanded.clear();
        self.recovery.clear();
        self.reads.clear();
        self.tabs.clear();
        self.diff_tabs.clear();
        self.views.clear();
        self.diffs.clear();
        self.files.clear();
        self.active = None;
        self.loading = true;
        self.error = None;
        cx.emit(IdeEvent::WorkspaceChanged);
        cx.notify();
        let id = workspace.workspace_id.clone();
        let state = self.request::<IdeWorkspaceState>(json!({ "op": "state.read", "workspaceId": id }), cx);
        let selection = self.request::<Value>(json!({ "op": "state.selection.save", "workspaceId": id }), cx);
        cx.spawn_in(window, async move |this, cx| {
            let state = state.await;
            let _ = selection.await;
            let reads = this.update_in(cx, |this, window, cx| {
                if this.workspace_id().as_deref() != Some(id.as_str()) {
                    return Vec::new();
                }
                this.loading = false;
                // Stored paths name their folder by `rootId`; the primary folder's paths stand alone.
                let mut views = HashMap::new();
                let (tabs, active, expanded, recovery) = match &state {
                    Ok(state) => {
                        let data = &state.data;
                        let keyed = |entry: &Value| entry["path"].as_str().map(|path| this.key_of_stored(path, entry["rootId"].as_str()));
                        let tabs: Vec<String> = data["tabs"].as_array().into_iter().flatten().filter_map(keyed).collect();
                        views = data["tabs"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|tab| Some((keyed(tab)?, tab["view"].as_str()?.to_owned())))
                            .collect();
                        let mut expanded: Vec<String> =
                            data["expandedPaths"].as_array().into_iter().flatten().filter_map(|path| path.as_str().map(str::to_owned)).collect();
                        expanded.extend(data["expandedRoots"].as_array().into_iter().flatten().filter_map(keyed));
                        let active = data["activePath"].as_str().map(|path| this.key_of_stored(path, data["activeRootId"].as_str()));
                        let recovery: HashMap<String, Value> =
                            data["buffers"].as_array().into_iter().flatten().filter_map(|buffer| Some((keyed(buffer)?, buffer.clone()))).collect();
                        (tabs, active, expanded, recovery)
                    }
                    Err(_) => (Vec::new(), None, Vec::new(), HashMap::new()),
                };
                this.recovery = recovery;
                this.views = views;
                this.state = state.ok();
                for root in workspace.roots() {
                    this.list(file_key("", Some(&root)), window, cx);
                }
                for path in expanded {
                    this.expanded.insert(path.clone());
                    this.list(path, window, cx);
                }
                // Tabs come back as files, as in 3.x; a comparison is opened again from Git. All are read at once.
                let reads: Vec<Task<()>> = tabs.iter().map(|key| this.open_task(key.clone(), false, window, cx)).collect();
                this.active = active.filter(|key| tabs.contains(key)).or_else(|| tabs.first().cloned());
                let last = this.state.as_ref().and_then(|state| state.data["lastSessionId"].as_str().map(str::to_owned));
                // The chat list may still be loading; the chat is chosen once it has come.
                this.pending_session = Some(last);
                this.restore_session(cx);
                cx.emit(IdeEvent::ActiveChanged);
                cx.notify();
                reads
            });
            for read in reads.into_iter().flatten() {
                read.await;
            }
        })
    }

    /// Open launched files and folders one after another (`openLaunched`): a folder as a project; a file in the
    /// current project, else in the project whose folder holds it most closely, else with its folder as a new project.
    pub fn open_launched(&mut self, launched: Vec<Launched>, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            for entry in launched {
                if entry.directory {
                    let Ok(opened) = this.update_in(cx, |ide, window, cx| ide.open_folder_task(entry.path.clone(), window, cx)) else { return };
                    opened.await;
                    continue;
                }
                let Ok((current, holding)) = this.read_with(cx, |ide, _| (ide.key_from_absolute(&entry.path), holding_workspace(&ide.workspaces, &entry.path)))
                else {
                    return;
                };
                // Each file is read before the next entry, which may show another project.
                let read = if let Some(key) = current {
                    this.update_in(cx, |ide, window, cx| ide.open_task(key, true, window, cx)).ok()
                } else if let Some((workspace, key)) = holding {
                    let id = workspace.workspace_id.clone();
                    let Ok(shown) = this.update_in(cx, |ide, window, cx| ide.select_task(workspace, window, cx)) else { return };
                    shown.await;
                    this.update_in(cx, |ide, window, cx| (ide.workspace_id().as_deref() == Some(id.as_str())).then(|| ide.open_task(key, true, window, cx)))
                        .ok()
                        .flatten()
                } else {
                    let Ok(opened) = this.update_in(cx, |ide, window, cx| ide.open_folder_task(parent_directory(&entry.path), window, cx)) else { return };
                    if !opened.await {
                        continue;
                    }
                    this.update_in(cx, |ide, window, cx| {
                        // A folder reached through a link has another canonical path; the file is still directly inside it.
                        let name = entry.path.rsplit(['/', '\\']).next().unwrap_or(&entry.path).to_owned();
                        let key = ide.key_from_absolute(&entry.path).unwrap_or(name);
                        ide.open_task(key, true, window, cx)
                    })
                    .ok()
                };
                if let Some(read) = read {
                    read.await;
                }
            }
        })
    }

    /// List one directory.
    pub fn list(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let mut params = self.location(&key);
        params.insert("op".into(), json!("files.list"));
        let request = self.request::<IdeDirectory>(Value::Object(params), cx);
        let workspace = self.workspace_id();
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                if this.workspace_id() != workspace {
                    return;
                }
                let listing = match result {
                    Ok(directory) => {
                        let mut entries = directory.entries;
                        entries.sort_by(|a, b| b.is_directory().cmp(&a.is_directory()).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                        Listing { entries, error: None }
                    }
                    Err(error) => Listing { entries: Vec::new(), error: Some(ide_error(error)) },
                };
                this.listings.insert(key, listing);
                cx.emit(IdeEvent::FilesChanged);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn toggle_directory(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key.clone());
            self.list(key, window, cx);
        }
        self.schedule_state_save(cx);
        cx.notify();
    }

    /// Open a file in a tab, or show its tab; `focus` makes it the active tab.
    pub fn open(&mut self, key: String, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open_task(key, focus, window, cx).detach();
    }

    /// `open`, done once the file has been read.
    fn open_task(&mut self, key: String, focus: bool, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        if !self.tabs.contains(&key) {
            self.tabs.push(key.clone());
        }
        if focus {
            self.active = Some(key.clone());
            cx.emit(IdeEvent::ActiveChanged);
            self.schedule_state_save(cx);
        }
        cx.notify();
        if let Some(file) = self.files.get(&key) {
            if focus && let Some(editor) = file.editor.clone() {
                editor.update(cx, |state, cx| state.focus(window, cx));
            }
            return Task::ready(());
        }
        if focus {
            self.focus_next = Some(key.clone());
        }
        self.read_count += 1;
        let read = self.read_count;
        self.reads.insert(key.clone(), read);
        let mut params = self.location(&key);
        params.insert("op".into(), json!("files.read"));
        let request = self.request::<IdeFileDocument>(Value::Object(params), cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update_in(cx, |this, window, cx| {
                // The Host answers in any order; an older read must not replace a newer one or reach another project.
                if this.reads.get(&key) != Some(&read) {
                    return;
                }
                this.reads.remove(&key);
                match result {
                    Ok(document) => this.accept_document(key, document, window, cx),
                    Err(error) => {
                        this.tabs.retain(|tab| *tab != key);
                        if this.active.as_deref() == Some(key.as_str()) {
                            this.active = this.tabs.last().cloned();
                            cx.emit(IdeEvent::ActiveChanged);
                        }
                        this.error = Some(ide_error(error));
                        cx.notify();
                    }
                }
            });
        })
    }

    fn accept_document(&mut self, key: String, mut document: IdeFileDocument, window: &mut Window, cx: &mut Context<Self>) {
        // Unsaved text from before a restart comes back; edits based on an older version are in conflict with the disk.
        let recovered = self
            .recovery
            .remove(&key)
            .and_then(|buffer| Some((buffer["content"].as_str()?.to_owned(), buffer["baseVersion"].as_str().map(str::to_owned))))
            .filter(|(content, _)| document.read_only_reason.is_none() && Some(content) != document.content.as_ref());
        let mut conflict = None;
        if let Some((_, Some(base))) = &recovered
            && *base != document.version
        {
            conflict = Some(std::mem::replace(&mut document.version, base.clone()));
        }
        let (editor, subscription, saved) = match document.content.clone().filter(|_| document.read_only_reason.is_none()) {
            Some(disk) => {
                let text = recovered.as_ref().map_or(disk.clone(), |(content, _)| content.clone());
                let language = language_of(&document.path);
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(language)
                        .line_number(true)
                        .searchable(true)
                        .default_value(text.clone())
                });
                let watched = key.clone();
                let subscription = cx.subscribe(&editor, move |this: &mut Ide, editor, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        let text = editor.read(cx).value().to_string();
                        if let Some(file) = this.files.get_mut(&watched) {
                            let dirty = text != file.saved;
                            let changed = dirty != file.dirty;
                            if changed {
                                file.dirty = dirty;
                                cx.notify();
                            }
                            // Unsaved text is kept in the project state, so it survives a crash or restart.
                            if dirty || changed {
                                this.schedule_state_save(cx);
                            }
                        }
                    }
                });
                (Some(editor), Some(subscription), disk)
            }
            None => (None, None, String::new()),
        };
        if let (Some(editor), Some(target)) = (&editor, self.language_target(&key)) {
            let servers = crate::store::language::LanguageServers::global(cx);
            servers.update(cx, |servers, cx| servers.attach(&key, editor, target, cx));
        }
        let revealed = key.clone();
        let dirty = recovered.is_some() && editor.is_some();
        self.files.insert(
            key.clone(),
            OpenFile { document, editor, saved, dirty, conflict: conflict.is_some(), disk_version: conflict, _subscription: subscription },
        );
        self.apply_reveal(&revealed, window, cx);
        if self.focus_next.as_deref() == Some(revealed.as_str()) {
            self.focus_next = None;
            if let Some(editor) = self.files.get(&revealed).and_then(|file| file.editor.clone()) {
                editor.update(cx, |state, cx| state.focus(window, cx));
            }
        }
        cx.notify();
    }

    /// Show a file against its Git base in its tab.
    pub fn open_diff(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.contains(&key) {
            self.tabs.push(key.clone());
        }
        self.diff_tabs.insert(key.clone());
        self.active = Some(key.clone());
        cx.emit(IdeEvent::ActiveChanged);
        self.schedule_state_save(cx);
        self.load_diff(key, window, cx);
        cx.notify();
    }

    /// Show the editor of a tab that showed a diff.
    pub fn show_file(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        self.diff_tabs.remove(&key);
        self.diffs.remove(&key);
        self.open(key, true, window, cx);
    }

    /// Read a diff again (`files.diff`).
    pub fn load_diff(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let mut params = self.location(&key);
        params.insert("op".into(), json!("files.diff"));
        let request = self.request::<IdeFileDiff>(Value::Object(params), cx);
        self.diffs.entry(key.clone()).or_default();
        let workspace = self.workspace_id();
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                if this.workspace_id() != workspace || !this.diff_tabs.contains(&key) {
                    return;
                }
                let mut tab = DiffTab::default();
                match result {
                    Ok(diff) if diff.status == "unavailable" => tab.error = Some(diff.reason.unwrap_or_default().into()),
                    Ok(diff) => {
                        let patch = unified_patch(&diff.path, diff.base.as_deref().unwrap_or_default(), diff.current.as_deref().unwrap_or_default());
                        tab.status = diff.status;
                        match DiffFile::parse(&patch) {
                            Ok(files) => {
                                let language = language_of(&diff.path);
                                let files: Vec<DiffFile> = files.into_iter().map(|file| file.with_language(language)).collect();
                                tab.state = Some(cx.new(|cx| DiffState::new(files, cx)));
                            }
                            Err(error) => tab.error = Some(error.to_string().into()),
                        }
                    }
                    Err(error) => tab.error = Some(ide_error(error)),
                }
                this.diffs.insert(key, tab);
                cx.notify();
            });
        })
        .detach();
    }

    /// The key of a path as the stored state names it, with the folder's id unless it is the primary folder.
    fn key_of_stored(&self, path: &str, root: Option<&str>) -> String {
        let primary = self.workspace.as_ref().is_none_or(|workspace| workspace.roots().iter().any(|known| Some(known.root_id.as_str()) == root && known.primary));
        match root {
            Some(root) if !primary => format!("{PREFIX}{root}/{path}"),
            _ => path.to_owned(),
        }
    }

    /// A key as the stored state names it: `{ path, rootId? }`.
    fn stored(key: &str) -> Value {
        match split_key(key) {
            (Some(root), path) => json!({ "path": path, "rootId": root }),
            (None, path) => json!({ "path": path }),
        }
    }

    /// Where the language server of a file runs, and the file as the Host and the model see it.
    pub fn language_target(&self, key: &str) -> Option<crate::store::language::Target> {
        let workspace = self.workspace.as_ref()?;
        let (root_id, relative) = split_key(key);
        let roots = workspace.roots();
        let root = match root_id {
            Some(id) => roots.iter().find(|root| root.root_id == id)?,
            None => roots.iter().find(|root| root.primary).or(roots.first())?,
        };
        let separator = if root.path.contains('\\') { "\\" } else { "/" };
        let base = root.path.trim_end_matches(['/', '\\']);
        let path = format!("{base}{separator}{}", relative.replace('/', separator));
        Some(crate::store::language::Target {
            workspace_id: workspace.workspace_id.clone(),
            root_id: root.root_id.clone(),
            root_path: root.path.clone(),
            root_title: root.title.clone(),
            path,
            relative: relative.to_owned(),
            python_path: self.python_path(),
        })
    }

    /// The Python of the active run profile, or of the first Python profile with one (`selectedPythonExecutable`).
    pub fn python_path(&self) -> Option<String> {
        let execution = self.state.as_ref()?.data.get("execution")?;
        let profiles = execution.get("profiles")?.as_array()?;
        let active = execution.get("activeProfile").and_then(Value::as_str);
        let python = |profile: &&Value| profile.get("language").and_then(Value::as_str) == Some("python");
        let executable = |profile: &Value| profile.get("executable").and_then(Value::as_str).map(str::to_owned);
        match profiles.iter().filter(python).find(|profile| profile.get("name").and_then(Value::as_str) == active) {
            Some(profile) => executable(profile),
            None => profiles.iter().filter(python).find_map(executable),
        }
    }

    /// The key of an absolute Host path inside one of the project's folders.
    pub fn key_from_absolute(&self, path: &str) -> Option<String> {
        key_in(self.workspace.as_ref()?, path)
    }

    /// Close the comparison tab of a file, as after its changes were undone.
    pub fn close_review(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.diff_tabs.contains(key) {
            self.close(key, cx);
        }
    }

    /// Show the agent's changes to a file as a comparison of the text before them with the text now.
    pub fn show_review(&mut self, key: String, base: String, current: String, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.contains(&key) {
            self.tabs.push(key.clone());
        }
        self.diff_tabs.insert(key.clone());
        self.active = Some(key.clone());
        let (_, path) = split_key(&key);
        let patch = unified_patch(path, &base, &current);
        let mut tab = DiffTab { status: "modified".into(), ..DiffTab::default() };
        match DiffFile::parse(&patch) {
            Ok(files) => {
                let language = language_of(path);
                let files: Vec<DiffFile> = files.into_iter().map(|file| file.with_language(language)).collect();
                tab.state = Some(cx.new(|cx| DiffState::new(files, cx)));
            }
            Err(error) => tab.error = Some(error.to_string().into()),
        }
        self.diffs.insert(key, tab);
        cx.emit(IdeEvent::ActiveChanged);
        self.schedule_state_save(cx);
        cx.notify();
    }

    /// Open a file with the cursor at a zero-based line and UTF-16 column, as search results give them.
    pub fn reveal(&mut self, key: String, line: u32, column: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.diff_tabs.remove(&key);
        self.reveals.insert(key.clone(), (line, column));
        self.open(key.clone(), true, window, cx);
        self.apply_reveal(&key, window, cx);
    }

    fn apply_reveal(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.files.get(key).and_then(|file| file.editor.clone()) else { return };
        let Some((line, column)) = self.reveals.remove(key) else { return };
        editor.update(cx, |state, cx| {
            let text = state.value();
            // The editor counts characters; search columns count UTF-16 units.
            let character = text.lines().nth(line as usize).map_or(0, |text| {
                let mut units = 0;
                text.chars().take_while(|c| {
                    units += c.len_utf16() as u32;
                    units <= column
                })
                .count() as u32
            });
            state.set_cursor_position(Position { line, character }, window, cx);
        });
    }

    /// Save the active file, or `key`; `overwrite` replaces a version changed on disk since it was read.
    pub fn save(&mut self, key: Option<String>, overwrite: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = key.or_else(|| self.active.clone()) else { return };
        let Some(file) = self.files.get(&key) else { return };
        let Some(editor) = &file.editor else { return };
        if !file.dirty && !overwrite {
            return;
        }
        let text = editor.read(cx).value().to_string();
        let expected = match (&file.disk_version, overwrite) {
            (Some(disk), true) => disk.clone(),
            _ => file.document.version.clone(),
        };
        let mut params = self.location(&key);
        params.insert("op".into(), json!("files.save"));
        params.insert("content".into(), json!(text));
        params.insert("expectedVersion".into(), json!(expected));
        let call = self.store.read(cx).call("ide", Value::Object(params));
        let workspace = self.workspace_id();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                // After a switch the open files with this key are another project's.
                let shown = this.workspace_id() == workspace;
                match result.and_then(|value| serde_json::from_value::<IdeFileDocument>(value).map_err(|e| RpcError { code: "invalid".into(), message: e.to_string(), data: None })) {
                    Ok(document) => {
                        if shown && let Some(file) = this.files.get_mut(&key) {
                            file.saved = text;
                            file.document = document;
                            file.dirty = file.editor.as_ref().is_some_and(|editor| editor.read(cx).value() != file.saved.as_str());
                            file.conflict = false;
                            file.disk_version = None;
                        }
                        cx.emit(IdeEvent::Saved);
                        if shown {
                            let servers = crate::store::language::LanguageServers::global(cx);
                            servers.update(cx, |servers, _| servers.saved(&key));
                        }
                        if this.closing.remove(&key) && shown {
                            this.close(&key, cx);
                        }
                    }
                    Err(error) => {
                        this.closing.remove(&key);
                        if shown
                            && error.code == "version-conflict"
                            && let Some(file) = this.files.get_mut(&key)
                        {
                            file.conflict = true;
                            file.disk_version = error.data.as_ref().and_then(|data| data["currentVersion"].as_str()).map(str::to_owned);
                        }
                        use gpui_kit::component::WindowExt as _;
                        window.push_notification(gpui_kit::component::notification::Notification::error(error.message), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Save a tab, then close it.
    pub fn save_and_close(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        self.closing.insert(key.clone());
        self.save(Some(key), false, window, cx);
    }

    /// Read a file again from disk, dropping unsaved edits.
    pub fn reload(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.files.remove(key);
        self.open(key.to_owned(), false, window, cx);
    }

    /// Choose how a tab shows its file; `None` is the default for the file type.
    pub fn set_view(&mut self, key: &str, view: Option<&str>, cx: &mut Context<Self>) {
        match view {
            Some(view) => drop(self.views.insert(key.to_owned(), view.to_owned())),
            None => drop(self.views.remove(key)),
        }
        self.schedule_state_save(cx);
        cx.notify();
    }

    /// Replace the unsaved text of an open file, as a viewer that edits it (rich Markdown) does.
    pub fn set_text(&mut self, key: &str, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.files.get(key).and_then(|file| file.editor.clone()) else { return };
        if editor.read(cx).value() == text {
            return;
        }
        editor.update(cx, |state, cx| state.set_value(text.to_owned(), window, cx));
        // Setting the value does not report a change, so the tab's unsaved state and the kept copy follow here.
        if let Some(file) = self.files.get_mut(key) {
            file.dirty = text != file.saved;
        }
        self.schedule_state_save(cx);
        cx.notify();
    }

    /// Close a tab; unsaved edits are the caller's to confirm first.
    pub fn close(&mut self, key: &str, cx: &mut Context<Self>) {
        let index = self.tabs.iter().position(|tab| tab == key);
        self.tabs.retain(|tab| tab != key);
        if self.files.remove(key).is_some() {
            let servers = crate::store::language::LanguageServers::global(cx);
            servers.update(cx, |servers, cx| servers.detach(key, cx));
        }
        self.reads.remove(key);
        self.diff_tabs.remove(key);
        self.diffs.remove(key);
        self.views.remove(key);
        if self.active.as_deref() == Some(key) {
            self.active = index.and_then(|index| self.tabs.get(index.min(self.tabs.len().saturating_sub(1))).cloned());
            cx.emit(IdeEvent::ActiveChanged);
        }
        self.schedule_state_save(cx);
        cx.notify();
    }

    /// The listed entry of a key.
    pub fn entry(&self, key: &str) -> Option<&FileEntry> {
        let (_, path) = split_key(key);
        self.listings.get(&parent_key(key))?.entries.iter().find(|entry| entry.path == path)
    }

    /// List every shown directory again.
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.clone() else { return };
        for root in workspace.roots() {
            self.list(file_key("", Some(&root)), window, cx);
        }
        for key in self.expanded.clone() {
            self.list(key, window, cx);
        }
    }

    /// Run a file operation, show its failure, and list the touched directory again.
    fn file_operation(
        &mut self,
        op: &str,
        key: &str,
        extra: Value,
        then: impl FnOnce(&mut Self, Value, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut params = self.location(key);
        params.insert("op".into(), json!(op));
        if let Value::Object(extra) = extra {
            params.extend(extra);
        }
        let call = self.store.read(cx).call("ide", Value::Object(params));
        let parent = parent_key(key);
        let workspace = self.workspace_id();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let shown = this.workspace_id() == workspace;
                match result {
                    Ok(value) if shown => then(this, value, window, cx),
                    Ok(_) => {}
                    Err(error) => {
                        use gpui_kit::component::WindowExt as _;
                        let message = match error.code.as_str() {
                            "already-exists" => crate::ui::tr(cx, "ide.ideAlreadyExists").to_string(),
                            _ => error.message,
                        };
                        window.push_notification(gpui_kit::component::notification::Notification::error(message), cx);
                    }
                }
                if shown {
                    this.list(parent, window, cx);
                }
            });
        })
        .detach();
    }

    /// Create a file or folder in a directory; a new file opens.
    pub fn create(&mut self, directory: String, name: String, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        let key = child_key(&directory, name.trim().trim_matches('/'));
        // A folder that gets something new opens to show it.
        if !split_key(&directory).1.is_empty() && self.expanded.insert(directory.clone()) {
            self.schedule_state_save(cx);
        }
        let (op, extra) = if folder { ("files.mkdir", json!({})) } else { ("files.create", json!({ "content": "" })) };
        let opened = key.clone();
        self.file_operation(op, &key, extra, move |this, _, window, cx| {
            if !folder {
                this.open(opened, true, window, cx);
            }
        }, window, cx);
    }

    /// Give a file or folder a new name in the same directory.
    pub fn rename(&mut self, key: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry(&key).cloned() else { return };
        let (_, path) = split_key(&key);
        let destination = match path.rsplit_once('/') {
            Some((parent, _)) => format!("{parent}/{}", name.trim()),
            None => name.trim().to_owned(),
        };
        let renamed = child_key(&parent_key(&key), name.trim());
        let was_open = self.tabs.contains(&key);
        let old = key.clone();
        self.file_operation("files.rename", &key, json!({ "destination": destination, "expectedVersion": entry.version }), move |this, _, window, cx| {
            // Tabs under the old name close; an open file comes back under the new one.
            let gone: Vec<String> = this.tabs.iter().filter(|tab| **tab == old || tab.starts_with(&format!("{old}/"))).cloned().collect();
            for tab in gone {
                this.close(&tab, cx);
            }
            if was_open {
                this.open(renamed, true, window, cx);
            }
        }, window, cx);
    }

    /// What deleting a path would remove, with the token that confirms it (`files.deletePreview`).
    pub fn delete_preview(&self, key: &str, cx: &App) -> impl Future<Output = Result<Value, RpcError>> + use<> {
        let mut params = self.location(key);
        params.insert("op".into(), json!("files.deletePreview"));
        self.store.read(cx).call("ide", Value::Object(params))
    }

    /// Delete a path confirmed by its preview token; its tabs close.
    pub fn delete(&mut self, key: String, token: String, window: &mut Window, cx: &mut Context<Self>) {
        let old = key.clone();
        self.file_operation("files.delete", &key, json!({ "token": token }), move |this, _, _, cx| {
            let gone: Vec<String> = this.tabs.iter().filter(|tab| **tab == old || tab.starts_with(&format!("{old}/"))).cloned().collect();
            for tab in gone {
                this.close(&tab, cx);
            }
        }, window, cx);
    }

    /// Show the project's last chat, unless the chat shown already belongs to the project.
    fn restore_session(&mut self, cx: &mut Context<Self>) {
        let (Some(workspace), Some(last)) = (self.workspace_id(), self.pending_session.clone()) else { return };
        let store = self.store.clone();
        if store.read(cx).summaries.is_empty() && last.is_some() {
            return;
        }
        self.pending_session = None;
        store.update(cx, |store, cx| {
            let belongs = |chat: &String| store.summaries.iter().any(|s| s.id == *chat && s.workspace_id.as_deref() == Some(workspace.as_str()));
            if !store.selected.as_ref().is_some_and(belongs) {
                let chosen = last.filter(belongs);
                store.select(chosen, cx);
            }
        });
    }

    /// Whether a tab has unsaved edits.
    pub fn is_dirty(&self, key: &str) -> bool {
        self.files.get(key).is_some_and(|file| file.dirty)
    }

    /// Forget the selected project, as when it was removed from the list.
    pub fn remove_workspace(&mut self, workspace_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let request = self.request::<Value>(json!({ "op": "workspaces.remove", "workspaceId": workspace_id }), cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        this.workspaces.retain(|w| w.workspace_id != workspace_id);
                        if this.workspace_id().as_deref() == Some(workspace_id.as_str()) {
                            this.workspace = None;
                            this.state = None;
                            this.listings.clear();
                            this.expanded.clear();
                            this.tabs.clear();
                            this.files.clear();
                            this.reads.clear();
                            this.active = None;
                            cx.emit(IdeEvent::WorkspaceChanged);
                        }
                    }
                    Err(error) => this.error = Some(ide_error(error)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn activate(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.tabs.contains(&key) {
            return;
        }
        self.active = Some(key);
        cx.emit(IdeEvent::ActiveChanged);
        self.schedule_state_save(cx);
        cx.notify();
    }

    /// The Host noticed a change of an open file or a run (`ide.hint`).
    fn on_host_event(&mut self, name: &str, data: &Value, cx: &mut Context<Self>) {
        let files_hint = name == "ide.hint"
            && data.get("workspaceId").and_then(Value::as_str) == self.workspace_id().as_deref()
            && data.get("kind").and_then(Value::as_str) == Some("files");
        // The agent changed files: they appear in the tree without waiting for the next check.
        if files_hint || name == "session.changes" {
            self.check_files(cx);
            // Files the agent or a tool created or removed appear in the tree.
            let window = self.window;
            self.relist = Some(cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
                cx.background_executor().timer(Duration::from_millis(300)).await;
                let (Some(window), Some(ide)) = (window, this.upgrade()) else { return };
                let _ = window.update(cx, |_, window, cx| ide.update(cx, |ide, cx| ide.refresh(window, cx)));
            }));
        }
    }

    /// Compare open files with the disk: an unchanged buffer follows the file, an edited one is marked in conflict.
    pub fn check_files(&mut self, cx: &mut Context<Self>) {
        let mut by_root: HashMap<Option<String>, Vec<String>> = HashMap::new();
        for key in self.files.keys() {
            let (root, path) = split_key(key);
            by_root.entry(root.map(str::to_owned)).or_default().push(path.to_owned());
        }
        let workspace = self.workspace_id();
        for (root, paths) in by_root {
            let mut params = json!({ "op": "files.changes", "workspaceId": workspace, "paths": paths });
            if let Some(root) = &root {
                params["rootId"] = json!(root);
            }
            let request = self.request::<Vec<IdeFileChange>>(params, cx);
            let window = self.window;
            let workspace = workspace.clone();
            cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
                let Ok(changes) = request.await else { return };
                let changed = this.update(cx, |this, cx| {
                    if this.workspace_id() != workspace {
                        return Vec::new();
                    }
                    let changed: Vec<String> = changes
                        .into_iter()
                        .filter_map(|change| {
                            let key = match &root {
                                Some(root) => format!("{PREFIX}{root}/{}", change.path),
                                None => change.path.clone(),
                            };
                            let file = this.files.get_mut(&key)?;
                            if change.version.as_deref() == Some(file.document.version.as_str()) {
                                return None;
                            }
                            if file.dirty {
                                file.conflict = true;
                                file.disk_version = change.version.clone();
                                None
                            } else {
                                Some(key)
                            }
                        })
                        .collect();
                    cx.emit(IdeEvent::FilesChanged);
                    cx.notify();
                    changed
                });
                // An unedited buffer follows the file on disk.
                let (Ok(changed), Some(window), Some(ide)) = (changed, window, this.upgrade()) else { return };
                for key in changed {
                    let _ = window.update(cx, |_, window, cx| ide.update(cx, |ide, cx| ide.reload(&key, window, cx)));
                }
            })
            .detach();
        }
    }

    /// Save tabs, the active file and expanded folders a moment after the last change.
    fn schedule_state_save(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
            cx.background_executor().timer(Duration::from_millis(600)).await;
            let _ = this.update(cx, |this, cx| this.flush_state(cx));
        }));
    }

    /// Write the editor state now (`state.save`), keeping every field this interface does not use.
    fn flush_state(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.state_request(cx) else { return };
        cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
            let result = request.await;
            let _ = this.update(cx, |this, _| this.state_saved(result));
        })
        .detach();
    }

    /// Write the editor state with its unsaved text now and resolve once the Host stored it, as before Rainy restarts
    /// into an update (3.x `rainy:flush-workbench`). A save that lost to another window's is written again on top of it.
    pub fn flush_now(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.save_task = None;
        cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
            let mut attempts = 0;
            loop {
                let request = this.update(cx, |this, cx| this.state_request(cx)).map_err(|_| "The editor is closed.".to_owned())?;
                let Some(request) = request else { return Ok(()) };
                let result = request.await;
                let saved = this.update(cx, |this, _| this.state_saved(result)).map_err(|_| "The editor is closed.".to_owned())?;
                attempts += 1;
                match saved {
                    Ok(()) => return Ok(()),
                    Err((error, true)) if attempts < 2 => log::info!("The editor state is saved again after a conflict: {}", error.message),
                    Err((error, _)) => return Err(error.message),
                }
            }
        })
    }

    /// Take the Host's answer to `state.save`: the saved state, or the error and whether the state another window saved
    /// replaced this one's base.
    fn state_saved(&mut self, result: Result<IdeWorkspaceState, RpcError>) -> Result<(), (RpcError, bool)> {
        match result {
            Ok(saved) => {
                self.state = Some(saved);
                Ok(())
            }
            // Another window saved first: take its revision; the next change writes again.
            Err(error) => {
                let current = error.data.as_ref().and_then(|data| data.get("currentState")).cloned().and_then(|current| serde_json::from_value::<IdeWorkspaceState>(current).ok());
                let replaced = current.is_some();
                if let Some(current) = current {
                    self.state = Some(current);
                }
                Err((error, replaced))
            }
        }
    }

    /// The `state.save` request for what differs from the saved state, if anything does.
    fn state_request(&mut self, cx: &mut Context<Self>) -> Option<impl Future<Output = Result<IdeWorkspaceState, RpcError>> + use<>> {
        let (Some(id), Some(state)) = (self.workspace_id(), self.state.clone()) else { return None };
        let mut data = state.data.clone();
        // Tabs keep the fields this interface does not set, such as a cursor saved by 3.x.
        let previous: HashMap<String, Value> = data["tabs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tab| Some((self.key_of_stored(tab["path"].as_str()?, tab["rootId"].as_str()), tab.clone())))
            .collect();
        data["tabs"] = Value::Array(
            self.tabs
                .iter()
                .map(|key| {
                    let kind = if self.diff_tabs.contains(key) { "diff" } else { "file" };
                    let mut tab = previous.get(key).cloned().unwrap_or_else(|| json!({}));
                    let place = Self::stored(key);
                    tab["path"] = place["path"].clone();
                    match place.get("rootId") {
                        Some(root) => tab["rootId"] = root.clone(),
                        None => drop(tab.as_object_mut().and_then(|tab| tab.remove("rootId"))),
                    }
                    tab["kind"] = json!(kind);
                    match self.views.get(key) {
                        Some(view) => tab["view"] = json!(view),
                        None => drop(tab.as_object_mut().and_then(|tab| tab.remove("view"))),
                    }
                    tab
                })
                .collect(),
        );
        let object = data.as_object_mut().expect("state data is an object");
        match &self.active {
            Some(key) => {
                let place = Self::stored(key);
                object.insert("activePath".into(), place["path"].clone());
                match place.get("rootId") {
                    Some(root) => drop(object.insert("activeRootId".into(), root.clone())),
                    None => drop(object.remove("activeRootId")),
                }
            }
            None => {
                object.insert("activePath".into(), Value::Null);
                object.remove("activeRootId");
            }
        }
        // Unsaved text of open files, and of files whose text has not come back yet.
        let mut buffers: Vec<Value> = self
            .files
            .iter()
            .filter(|(_, file)| file.dirty)
            .filter_map(|(key, file)| {
                let content = file.editor.as_ref()?.read(cx).value().to_string();
                let mut buffer = Self::stored(key);
                buffer["content"] = json!(content);
                buffer["baseVersion"] = json!(file.document.version);
                buffer["bom"] = json!(file.document.bom);
                buffer["eol"] = json!(file.document.eol);
                Some(buffer)
            })
            .collect();
        buffers.extend(self.recovery.iter().filter(|(key, _)| !self.files.contains_key(*key) && self.tabs.contains(key)).map(|(_, buffer)| buffer.clone()));
        object.insert("buffers".into(), Value::Array(buffers));
        let (primary, attached): (Vec<&String>, Vec<&String>) = self.expanded.iter().partition(|key| split_key(key).0.is_none());
        object.insert("expandedPaths".into(), json!(primary));
        if attached.is_empty() {
            object.remove("expandedRoots");
        } else {
            object.insert("expandedRoots".into(), Value::Array(attached.iter().map(|key| Self::stored(key)).collect()));
        }
        let store = self.store.read(cx);
        let in_project = |chat: &&String| store.summaries.iter().any(|s| s.id == **chat && s.workspace_id.as_deref() == Some(id.as_str()));
        if let Some(selected) = store.selected.as_ref().filter(in_project) {
            data["lastSessionId"] = json!(selected);
        }
        if data == state.data {
            return None;
        }
        Some(self.request::<IdeWorkspaceState>(json!({ "op": "state.save", "workspaceId": id, "baseRevision": state.revision, "data": data }), cx))
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn keys_round_trip() {
        let attached = WorkspaceRoot { root_id: "r2".into(), path: "/q".into(), title: "q".into(), primary: false };
        let primary = WorkspaceRoot { root_id: "r1".into(), path: "/p".into(), title: "p".into(), primary: true };
        assert_eq!(file_key("src/a.rs", Some(&primary)), "src/a.rs");
        let key = file_key("src/a.rs", Some(&attached));
        assert_eq!(split_key(&key), (Some("r2"), "src/a.rs"));
        assert_eq!(split_key("src/a.rs"), (None, "src/a.rs"));
        assert_eq!(file_name(&key), "a.rs");
        assert_eq!(language_of("x/Main.PY"), "python");
        assert_eq!(language_of("README"), "text");
    }

    #[test]
    fn finds_the_parent_folder_of_windows_unc_and_posix_paths() {
        assert_eq!(parent_directory(r"C:\notes.md"), r"C:\");
        assert_eq!(parent_directory(r"C:\Users\me\a.md"), r"C:\Users\me");
        assert_eq!(parent_directory(r"\\wsl.localhost\Ubuntu\home\me\a.md"), r"\\wsl.localhost\Ubuntu\home\me");
        assert_eq!(parent_directory("/home/me/a.md"), "/home/me");
        assert_eq!(parent_directory("/a.md"), "/");
        assert_eq!(parent_directory("a.md"), "a.md");
    }

    #[test]
    fn opens_a_launched_file_in_the_project_that_holds_it_most_closely() {
        let project = |id: &str, path: &str, roots: Option<Vec<WorkspaceRoot>>| IdeWorkspace { workspace_id: id.into(), path: path.into(), title: id.into(), roots };
        let notes = project("notes", r"C:\Users\me\Notes", None);
        let daily = project("daily", r"C:\Users\me\Notes\daily", None);
        let linux = project("linux", "/home/me/src", None);
        let attached = project(
            "work",
            r"D:\Work",
            Some(vec![
                WorkspaceRoot { root_id: "r1".into(), path: r"D:\Work".into(), title: "Work".into(), primary: true },
                WorkspaceRoot { root_id: "r2".into(), path: r"E:\Shared".into(), title: "Shared".into(), primary: false },
            ]),
        );
        assert_eq!(key_in(&notes, r"c:\users\ME\notes\daily\today.md").as_deref(), Some("daily/today.md"));
        assert_eq!(key_in(&notes, r"C:\Users\me\Notesbook\x.md"), None);
        assert_eq!(key_in(&linux, "/home/me/src/main.rs").as_deref(), Some("main.rs"));
        assert_eq!(key_in(&linux, "/home/me/SRC/main.rs"), None);
        assert_eq!(key_in(&attached, r"E:\Shared\a\b.md"), Some(file_key("a/b.md", attached.roots().get(1))));
        let all = [notes.clone(), daily.clone(), linux, attached];
        let (found, key) = holding_workspace(&all, r"C:\Users\me\Notes\daily\today.md").unwrap();
        assert_eq!((found.workspace_id.as_str(), key.as_str()), ("daily", "today.md"));
        let (found, key) = holding_workspace(&all, r"C:\Users\me\Notes\ideas.md").unwrap();
        assert_eq!((found.workspace_id.as_str(), key.as_str()), ("notes", "ideas.md"));
        assert_eq!(holding_workspace(&all, r"F:\elsewhere.md"), None);
    }
}
