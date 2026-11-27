//! Projects, their file trees and the open files (`src/renderer/ide/ide-model.ts`).
//!
//! File keys follow the renderer: a path relative to the primary root as it is, and `"\0<rootId>/<path>"` for a
//! path in an attached root, so the editor state saved per project stays readable by Rainy 3.x.

mod diff;
mod execution_cfg;
mod files;
mod keys;
mod state;

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::component::diff::DiffState;
use gpui_kit::component::input::EditorState;
use gpui_kit::*;
use rainy_protocol::{
    FileEntry, IdeFileDocument, IdeWorkspace, IdeWorkspaceState, RpcError, WorkspaceSelection,
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

pub use execution_cfg::{RUN_LANGUAGES, RunMode, run_language};
pub use keys::{
    file_key, file_name, holding_workspace, key_in, language_of, parent_directory, parent_key, root_key, split_key,
};

use crate::i18n::Locale;
use crate::store::Store;

/// Set when the last run panicked soon after it started: the first project shown then leaves its last chat closed, since
/// restoring it may be what ended that run (4.10.0 panicked on a tool path in one chat, on every start).
pub static SKIP_LAST_CHAT: AtomicBool = AtomicBool::new(false);

/// How often open files and listings are checked against the disk while the window is active (3.x `editorPollMs`).
const POLL: Duration = Duration::from_secs(10);
/// While the window is inactive/backgrounded, skip this many poll ticks between checks (30s effective interval).
const BACKGROUND_POLL_STRIDE: u8 = 3;

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
    pub(super) _subscription: Option<Subscription>,
}

/// A tab showing a file against its Git base.
#[derive(Default)]
pub struct DiffTab {
    pub state: Option<Entity<DiffState>>,
    pub error: Option<SharedString>,
    /// `modified`, `added`, `deleted` or `unchanged`.
    pub status: String,
    /// Raw baseline text for hunk-level accept/revert review.
    pub base: String,
    /// Raw current text for hunk-level accept/revert review.
    pub current: String,
    /// Interactive diff hunks extracted with `similar`.
    pub hunks: Vec<crate::ui::ide::hunk_diff::DiffHunk>,
}

/// What a directory listing holds.
#[derive(Debug, Clone, Default, PartialEq)]
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
    /// The run settings changed: profiles, breakpoints or watches.
    ExecutionChanged,
}

/// Pane sizes as 3.x keeps them in a project's `data.layout`, in logical pixels: the file pane's width, the AI pane's
/// (0 follows the window) and the bottom panel's height. As a patch, `None` leaves a size as it is.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PaneLayout {
    pub sidebar_width: Option<f32>,
    pub agent_width: Option<f32>,
    pub bottom_height: Option<f32>,
}

/// A file or folder handed to Rainy, as a path on the Host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub path: String,
    pub directory: bool,
}

pub struct Ide {
    pub(super) store: Entity<Store>,
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
    pub(super) state: Option<IdeWorkspaceState>,
    pub(super) save_task: Option<Task<()>>,
    /// The window editors are created in, for files reloaded after the Host reported a change.
    pub(super) window: Option<AnyWindowHandle>,
    /// Tabs that close once their save succeeds.
    pub(super) closing: BTreeSet<String>,
    /// The chat to show for the project just opened, until the chat list has loaded.
    pub(super) pending_session: Option<Option<String>>,
    /// The project is being shown for a new chat: its last chat is not brought back when its state arrives.
    pub(super) fresh: bool,
    /// Lists the shown directories again a moment after the Host reports changed files.
    pub(super) relist: Option<Task<()>>,
    /// Checks open files and listings every few seconds; asking also keeps the Host watching their folders.
    pub(super) poll: Option<Task<()>>,
    /// Unsaved text kept in the project state and not applied yet, by file key, as stored.
    pub(super) recovery: HashMap<String, Value>,
    /// Where to put the cursor once a file being opened has loaded: zero-based line and UTF-16 column.
    pub(super) reveals: HashMap<String, (u32, u32)>,
    /// The file whose editor takes the cursor once loaded.
    pub(super) focus_next: Option<String>,
    /// The latest `files.read` of each key; the answer to an earlier one, or to one sent before another project was
    /// shown, is dropped.
    pub(super) reads: HashMap<String, u64>,
    pub(super) read_count: u64,
    /// Pane sizes changed in this window and not yet in the saved state, with the project they belong to.
    pub(super) layout: Option<(String, PaneLayout)>,
}

impl EventEmitter<IdeEvent> for Ide {}

struct GlobalIde(Entity<Ide>);
impl Global for GlobalIde {}

/// A Host failure as the project view shows it: in the interface language, with a raw system message kept on the next line.
pub(super) fn ide_error(locale: Locale, error: &RpcError) -> SharedString {
    crate::i18n::error_text(locale, error).into()
}

impl Ide {
    pub fn global(cx: &App) -> Entity<Ide> {
        cx.global::<GlobalIde>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<Ide>> {
        cx.try_global::<GlobalIde>().map(|global| global.0.clone())
    }

    pub fn init(store: Entity<Store>, cx: &mut App) -> Entity<Ide> {
        let ide = cx.new(|cx| {
            cx.subscribe(&store, |this: &mut Ide, _, event: &crate::store::StoreEvent, cx| match event {
                crate::store::StoreEvent::Host { name, data } => this.on_host_event(name, data, cx),
                crate::store::StoreEvent::SelectionChanged if this.workspace.is_some() => this.schedule_state_save(cx),
                crate::store::StoreEvent::SummariesChanged => this.restore_session(cx),
                crate::store::StoreEvent::HostReconnected(rpc) => this.on_host_reconnected(rpc, cx),
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
                fresh: false,
                reveals: HashMap::new(),
                relist: None,
                poll: None,
                recovery: HashMap::new(),
                focus_next: None,
                reads: HashMap::new(),
                read_count: 0,
                layout: None,
            }
        });
        cx.set_global(GlobalIde(ide.clone()));
        ide
    }

    /// One `ide` request.
    pub(super) fn request<T: DeserializeOwned + 'static>(&self, params: Value, cx: &App) -> impl Future<Output = Result<T, RpcError>> + use<T> {
        let call = self.store.read(cx).call("ide", params);
        async move {
            let value = call.await?;
            serde_json::from_value(value).map_err(|error| RpcError { code: "invalid".into(), message: error.to_string(), data: None })
        }
    }

    pub(super) fn workspace_id(&self) -> Option<String> {
        self.workspace.as_ref().map(|workspace| workspace.workspace_id.clone())
    }

    /// The interface language, for failures the Host reports.
    pub(super) fn locale(&self, cx: &App) -> Locale {
        self.store.read(cx).locale()
    }

    /// The request fields locating a key: the workspace, the root of an attached directory, and the path.
    pub(super) fn location(&self, key: &str) -> Map<String, Value> {
        let (root, path) = split_key(key);
        let mut fields = Map::new();
        fields.insert("workspaceId".into(), json!(self.workspace_id()));
        if let Some(root) = root {
            fields.insert("rootId".into(), json!(root));
        }
        fields.insert("path".into(), json!(path));
        fields
    }

    /// Clear every per-workspace field so switching, removing, or reconnecting never leaves stale tabs, reveals or locks.
    pub(super) fn clear_workspace_state(&mut self) {
        self.save_task = None;
        self.relist = None;
        self.listings.clear();
        self.expanded.clear();
        self.recovery.clear();
        self.reads.clear();
        self.tabs.clear();
        self.diff_tabs.clear();
        self.views.clear();
        self.diffs.clear();
        self.files.clear();
        self.closing.clear();
        self.reveals.clear();
        self.focus_next = None;
        self.active = None;
        self.layout = None;
    }

    /// Load the projects and reopen the one selected last (`initialize`).
    pub fn initialize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window = Some(window.window_handle());
        let handle = window.window_handle();
        self.poll = Some(cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
            let mut idle_ticks: u8 = 0;
            loop {
                cx.background_executor().timer(POLL).await;
                let Some(ide) = this.upgrade() else { return };
                let _ = handle.update(cx, |_, window, cx| {
                    // Slow polling down while the window is unfocused/backgrounded to avoid wasting IPC & CPU.
                    if window.is_window_active() {
                        idle_ticks = 0;
                    } else {
                        idle_ticks = idle_ticks.wrapping_add(1);
                        if !idle_ticks.is_multiple_of(BACKGROUND_POLL_STRIDE) {
                            return;
                        }
                    }
                    ide.update(cx, |ide, cx| {
                        ide.check_files(cx);
                        ide.refresh(window, cx);
                    })
                });
            }
        }));
        let list = self.request::<Vec<IdeWorkspace>>(json!({ "op": "workspaces.list" }), cx);
        let selection = self.request::<WorkspaceSelection>(json!({ "op": "state.selection.read" }), cx);
        let default_workspace = self
            .store
            .read(cx)
            .current_ssh_profile()
            .and_then(|profile| profile.default_workspace)
            .filter(|path| !path.trim().is_empty());
        cx.spawn_in(window, async move |this, cx| {
            let (list, selection) = (list.await, selection.await);
            let shown = this.update_in(cx, |this, window, cx| {
                match list {
                    Ok(workspaces) => this.workspaces = workspaces,
                    Err(error) => this.error = Some(ide_error(this.locale(cx), &error)),
                }
                let selected = selection.ok().and_then(|selection| selection.workspace_id);
                let shown = selected
                    .as_ref()
                    .and_then(|id| this.workspaces.iter().find(|w| w.workspace_id == *id).cloned())
                    .map(|workspace| this.select_task(workspace, window, cx));
                cx.notify();
                (shown, selected.is_none() && this.workspaces.is_empty())
            });
            if let Ok((Some(shown), _)) = shown {
                shown.await;
            } else if let Ok((None, true)) = shown
                && let Some(default_path) = default_workspace
            {
                let open = this.update_in(cx, |this, window, cx| this.open_folder_task(default_path, window, cx));
                if let Ok(open) = open {
                    let _ = open.await;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.restored = true;
                cx.emit(IdeEvent::Restored);
            });
        })
        .detach();
    }

    /// Re-synchronize workspaces, directory listings, and open files after an in-place Host swap or SSH reconnect.
    fn on_host_reconnected(&mut self, rpc: &rainy_carrier::RpcClient, cx: &mut Context<Self>) {
        self.workspaces.clear();
        self.workspace = None;
        self.state = None;
        self.clear_workspace_state();
        self.error = None;
        self.loading = false;
        cx.emit(IdeEvent::WorkspaceChanged);
        cx.notify();

        let Some(handle) = self.window else { return };
        let list = rpc.request::<Vec<IdeWorkspace>>("ide", json!({ "op": "workspaces.list" }));
        let selection = rpc.request::<WorkspaceSelection>("ide", json!({ "op": "state.selection.read" }));
        let default_workspace = self
            .store
            .read(cx)
            .current_ssh_profile()
            .and_then(|profile| profile.default_workspace)
            .filter(|path| !path.trim().is_empty());
        cx.spawn(async move |this: WeakEntity<Ide>, cx: &mut AsyncApp| {
            let (list, selection) = (list.await, selection.await);
            let Some(ide) = this.upgrade() else { return };
            let next = handle.update(cx, |_, window, cx| {
                ide.update(cx, |this, cx| {
                    match list {
                        Ok(workspaces) => this.workspaces = workspaces,
                        Err(error) => this.error = Some(ide_error(this.locale(cx), &error)),
                    }
                    let selected = selection.ok().and_then(|selection| selection.workspace_id);
                    let chosen = selected.and_then(|id| this.workspaces.iter().find(|w| w.workspace_id == id).cloned());
                    let shown = chosen.map(|workspace| this.select_task(workspace, window, cx));
                    cx.notify();
                    (shown, this.workspace.is_none() && this.workspaces.is_empty())
                })
            });
            if let Ok((Some(shown), _)) = next {
                shown.await;
            } else if let Ok((None, true)) = next
                && let Some(default_path) = default_workspace
            {
                let open = handle.update(cx, |_, window, cx| {
                    ide.update(cx, |this, cx| this.open_folder_task(default_path, window, cx))
                });
                if let Ok(open) = open {
                    let _ = open.await;
                }
            }
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
                    this.error = Some(ide_error(this.locale(cx), &error));
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

    /// Show a project for a new chat in it (the sidebar's "+" of a project): its last chat stays where it is.
    pub fn select_for_new_chat(&mut self, workspace: IdeWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.as_ref().is_some_and(|current| current.workspace_id == workspace.workspace_id) {
            return;
        }
        self.fresh = true;
        self.select_task(workspace, window, cx).detach();
    }

    /// `select`, done once the project's saved tabs are shown and their files read.
    fn select_task(&mut self, workspace: IdeWorkspace, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        if self.workspace.as_ref().is_some_and(|current| current.workspace_id == workspace.workspace_id) {
            return Task::ready(());
        }
        self.flush_state(cx);
        self.workspace = Some(workspace.clone());
        self.clear_workspace_state();
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
                let reads: Vec<Task<()>> = tabs.iter().map(|key| this.open_task(key.clone(), false, window, cx)).collect();
                this.active = active.filter(|key| tabs.contains(key)).or_else(|| tabs.first().cloned());
                let last = this.state.as_ref().and_then(|state| state.data["lastSessionId"].as_str().map(str::to_owned));
                let skip = std::mem::take(&mut this.fresh) | SKIP_LAST_CHAT.swap(false, Ordering::Relaxed);
                this.pending_session = if skip { None } else { Some(last) };
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

    /// Open launched files and folders one after another (`openLaunched`).
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

    /// The key of a path as the stored state names it, with the folder's id unless it is the primary folder.
    pub fn key_of_stored(&self, path: &str, root: Option<&str>) -> String {
        let primary = self.workspace.as_ref().is_none_or(|workspace| workspace.roots().iter().any(|known| Some(known.root_id.as_str()) == root && known.primary));
        match root {
            Some(root) if !primary && !root.is_empty() && root != "primary" => root_key(Some(root), path),
            _ => path.to_owned(),
        }
    }

    /// A key as the stored state names it: `{ path, rootId? }`.
    pub(super) fn stored(key: &str) -> Value {
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

    /// The window the project is shown in.
    pub fn window_handle(&self) -> Option<AnyWindowHandle> {
        self.window
    }

    /// The key of an absolute Host path inside one of the project's folders.
    pub fn key_from_absolute(&self, path: &str) -> Option<String> {
        key_in(self.workspace.as_ref()?, path)
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

    /// Add a folder to the shown project without changing its chats or main folder (3.x `attachRoot`).
    pub fn attach_root(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace_id) = self.workspace_id() else {
            self.open_folder(path, window, cx);
            return;
        };
        self.flush_state(cx);
        let request = self.request::<IdeWorkspace>(json!({ "op": "workspaces.attach", "workspaceId": workspace_id, "path": path }), cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(workspace) => this.replace_workspace(workspace, window, cx),
                Err(error) => {
                    this.error = Some(ide_error(this.locale(cx), &error));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Take a folder other than the main one out of the shown project; its files stay on disk (3.x `removeRoot`).
    pub fn remove_root(&mut self, root_id: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(workspace_id) = self.workspace_id() else { return false };
        let inside = |key: &str| split_key(key).0 == Some(root_id.as_str());
        if self.files.iter().any(|(key, file)| inside(key) && file.dirty) {
            return false;
        }
        self.flush_state(cx);
        let request = self.request::<IdeWorkspace>(json!({ "op": "workspaces.removeRoot", "workspaceId": workspace_id, "rootId": root_id }), cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(workspace) => {
                    let inside = |key: &str| split_key(key).0 == Some(root_id.as_str());
                    let open: Vec<String> = this.tabs.iter().filter(|key| inside(key)).cloned().collect();
                    for key in open {
                        this.close(&key, cx);
                    }
                    this.listings.retain(|key, _| !inside(key));
                    this.expanded.retain(|key| !inside(key));
                    this.replace_workspace(workspace, window, cx);
                    this.schedule_state_save(cx);
                }
                Err(error) => {
                    this.error = Some(ide_error(this.locale(cx), &error));
                    cx.notify();
                }
            });
        })
        .detach();
        true
    }

    /// A project as the Host now has it, after its folders changed.
    fn replace_workspace(&mut self, workspace: IdeWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        for known in &mut self.workspaces {
            if known.workspace_id == workspace.workspace_id {
                *known = workspace.clone();
            }
        }
        if self.workspace_id().as_deref() == Some(workspace.workspace_id.as_str()) {
            self.workspace = Some(workspace);
            self.error = None;
            self.refresh(window, cx);
            cx.emit(IdeEvent::FilesChanged);
        }
        cx.notify();
    }

    /// Rename a project, or keep it at the top of the sidebar: `workspaces.rename` or `workspaces.pin`.
    pub fn change_workspace(&mut self, request: Value, window: &mut Window, cx: &mut Context<Self>) {
        let request = self.request::<IdeWorkspace>(request, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(changed) => {
                        for known in this.workspaces.iter_mut().chain(this.workspace.as_mut()) {
                            if known.workspace_id == changed.workspace_id {
                                known.title = changed.title.clone();
                                known.pinned = changed.pinned;
                            }
                        }
                        if this.workspace_id().as_deref() == Some(changed.workspace_id.as_str()) {
                            cx.emit(IdeEvent::FilesChanged);
                        }
                    }
                    Err(error) => this.error = Some(ide_error(this.locale(cx), &error)),
                }
                cx.notify();
            });
        })
        .detach();
    }

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
                            this.clear_workspace_state();
                            cx.emit(IdeEvent::WorkspaceChanged);
                        }
                    }
                    Err(error) => this.error = Some(ide_error(this.locale(cx), &error)),
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
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;
    use rainy_protocol::WorkspaceRoot;

    #[test]
    fn run_languages_follow_3x_extensions() {
        assert_eq!(run_language("src/main.py"), Some("python"));
        assert_eq!(run_language("a.PYI"), Some("python"));
        assert_eq!(run_language("web/app.tsx"), Some("typescript"));
        assert_eq!(run_language("x.h"), Some("c"));
        assert_eq!(run_language("x.hpp"), Some("cpp"));
        assert_eq!(run_language("notes.md"), None);
        assert_eq!(run_language("Makefile"), None);
    }

    #[test]
    fn a_profile_runs_the_file_of_its_folder() {
        let primary = json!({ "path": "src/main.py" });
        let other = json!({ "path": "src/main.py", "rootId": "r2" });
        assert!(execution_cfg::runs_file(&json!({ "program": "src/main.py" }), &primary));
        assert!(execution_cfg::runs_file(&json!({ "program": "src/main.py", "rootId": null }), &primary));
        assert!(!execution_cfg::runs_file(&json!({ "program": "src/main.py" }), &other));
        assert!(execution_cfg::runs_file(&json!({ "program": "src/main.py", "rootId": "r2" }), &other));
        assert!(!execution_cfg::runs_file(&json!({ "program": "main.py" }), &primary));
    }

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
        let project = |id: &str, path: &str, roots: Option<Vec<WorkspaceRoot>>| IdeWorkspace { workspace_id: id.into(), path: path.into(), title: id.into(), roots, pinned: false };
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
        assert_eq!(key_in(&notes, r"C:\迅雷下载\撕裂者\a.md"), None);
        assert_eq!(key_in(&project("k", r"C:\K", None), "C:\\\u{212A}\\a.md"), None);
        assert_eq!(key_in(&project("z", r"C:\中文", None), r"c:\中文\笔记.md").as_deref(), Some("笔记.md"));
        assert_eq!(key_in(&attached, r"E:\Shared\a\b.md"), Some(file_key("a/b.md", attached.roots().get(1))));
        let all = [notes.clone(), daily.clone(), linux, attached];
        let (found, key) = holding_workspace(&all, r"C:\Users\me\Notes\daily\today.md").unwrap();
        assert_eq!((found.workspace_id.as_str(), key.as_str()), ("daily", "today.md"));
        let (found, key) = holding_workspace(&all, r"C:\Users\me\Notes\ideas.md").unwrap();
        assert_eq!((found.workspace_id.as_str(), key.as_str()), ("notes", "ideas.md"));
        assert_eq!(holding_workspace(&all, r"F:\elsewhere.md"), None);
    }
}
