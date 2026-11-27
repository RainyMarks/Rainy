//! The window (`Workbench.tsx`) in its two layouts. IDE mode: the title bar with the project menu, the file and
//! history pane, the editor area, the AI pane and the status bar. Agent mode (`ui::agent`): the chat sidebar, the
//! conversation in the middle and, on demand, a panel on the right with the files, the terminals or the chat's
//! changes. Both show the same entities.

mod dialogs;
mod layout;
mod run_controls;
mod status_bar;
mod title_bar;

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use gpui_kit::base::{TextSelection, apply_system_reduce_motion};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;
use rainy_carrier::ConnectionState;

use crate::i18n::Locale;
use crate::placement::Layout;
use crate::store::execution::Execution;
use crate::store::ide::{Ide, IdeEvent};
use crate::store::language::{LanguageServers, ServerPhase};
use crate::store::update::UpdatePhase;
use crate::store::{Phase, SshAutoReconnect, Store, StoreEvent};
use crate::theme;
use crate::ui::agent::{self, AgentMode, Fit, IdeMode, Mode, ToggleMode, ToggleSidebar, Tool};
use crate::ui::chat::ChatPane;
use crate::ui::history::HistoryPanel;
use crate::ui::ide::cursor_status::CursorStatus;
use crate::ui::ide::debug_marks::DebugMarks;
use crate::ui::ide::editor_area::{EditorArea, EditorAreaEvent, close_tab};
use crate::ui::ide::file_tree::FileTree;
use crate::ui::ide::git::GitView;
use crate::ui::ide::outline::OutlineView;
use crate::ui::ide::relations::RelationsView;
use crate::ui::ide::search::SearchView;
use crate::ui::ide::terminal_panel::{BottomTab, TerminalPanel};
use crate::ui::ide::{
    AttachFolder, CheckUpdates, CloseFile, CommandPalette, CompareWithSaved, DebugFile, ExportHtml, ExportPdf, FindInFiles, FindReferences, FocusEditor,
    FormatDocument, GitRefresh, GoToLine, MarkdownFocus, MarkdownSource, MarkdownTypewriter, NewChat, OpenFolder, OpenRuntimeSettings, OpenSettings,
    QuickOpen, RenameSymbol, ReplaceInFiles, RunFile, RunProfiles, SaveAll, SaveFile, SearchHistory, SendSelection, ShowChat, ShowExplorer, ShowMap,
    ShowOutline, ShowProblems, ShowSourceControl, StepInto, StepOut, StepOver, StopRun, ToggleAgent, ToggleBreakpoint, ToggleFiles, ToggleTerminal,
    choose_folder, editor_commands, quick_open, run_dialog,
};
use crate::ui::metrics::MetricsView;
use crate::ui::settings::{CloseSettings, Section, SettingsView};
use crate::ui::tr;
use crate::ui::tunnels::TunnelsView;

pub(crate) use dialogs::confirm_switch;
use dialogs::{credential_alert, resources_damaged, runtime_download_failed};
use layout::{DEFAULT_SIZES, PaneGroup, Side, SideTabBounds, Sizes};

/// Snapshot of the subset of `Store` state rendered directly by `Workbench`.
/// Diffing this fingerprint stops per-token streaming updates from triggering full-window Workbench repaints
/// or redundant Win32 foreground/taskbar calls.
#[derive(PartialEq, Eq)]
struct StoreFingerprint {
    phase: PhaseKey,
    target_id: Option<String>,
    target_label: Option<String>,
    connection: ConnectionState,
    is_ssh: bool,
    is_remote: bool,
    swapping_target: Option<SharedString>,
    ssh_reconnect: (u8, u8, u8, u64),
    selected: Option<String>,
    selected_title: Option<String>,
    selected_has_swarm: bool,
    locale: Locale,
}

#[derive(PartialEq, Eq)]
enum PhaseKey {
    Ready,
    Starting(SharedString, Option<u16>),
    Failed(SharedString),
}

impl StoreFingerprint {
    fn capture(store: &Store) -> Self {
        let phase = match &store.phase {
            Phase::Ready => PhaseKey::Ready,
            Phase::Starting(step, frac) => PhaseKey::Starting(step.clone(), frac.map(|f| (f * 1000.0).round() as u16)),
            Phase::Failed(err) => PhaseKey::Failed(err.clone()),
        };
        let target_id = store.target.as_ref().map(|t| t.id().to_owned());
        let target_label = store
            .target
            .as_ref()
            .map(|t| t.label().to_owned())
            .or_else(|| store.info.as_ref().map(|info| info.environment.clone()));
        let ssh_reconnect = match &store.ssh_reconnect {
            SshAutoReconnect::Idle => (0, 0, 0, 0),
            SshAutoReconnect::Waiting { attempt, max, retry_at, .. } => {
                let secs = retry_at.saturating_duration_since(std::time::Instant::now()).as_secs();
                (1, *attempt, *max, secs)
            }
            SshAutoReconnect::Attempting { attempt, max } => (2, *attempt, *max, 0),
            SshAutoReconnect::Exhausted { .. } => (3, 0, 0, 0),
        };
        let summary = store.selected.as_ref().and_then(|id| store.summaries.iter().find(|s| s.id == *id));
        let selected_title = summary.map(|s| s.title.clone());
        let selected_has_swarm = summary.is_some_and(|s| s.swarm.is_some() || store.has_team(&s.id));
        Self {
            phase,
            target_id,
            target_label,
            connection: store.connection,
            is_ssh: store.is_ssh(),
            is_remote: store.is_remote(),
            swapping_target: store.swapping_target.clone(),
            ssh_reconnect,
            selected: store.selected.clone(),
            selected_title,
            selected_has_swarm,
            locale: store.locale(),
        }
    }
}

/// Snapshot of the subset of `Ide` state rendered directly by `Workbench`.
#[derive(PartialEq, Eq)]
struct IdeFingerprint {
    workspace_id: Option<String>,
    workspace_title: Option<String>,
    workspace_path: Option<String>,
    active: Option<String>,
    active_has_editor: bool,
    tabs_empty: bool,
}

impl IdeFingerprint {
    fn capture(ide: &Ide) -> Self {
        let active_has_editor = ide.active.as_ref().is_some_and(|k| ide.files.get(k).is_some_and(|f| f.editor.is_some()));
        Self {
            workspace_id: ide.workspace.as_ref().map(|w| w.workspace_id.clone()),
            workspace_title: ide.workspace.as_ref().map(|w| w.title.clone()),
            workspace_path: ide.workspace.as_ref().map(|w| w.path.clone()),
            active: ide.active.clone(),
            active_has_editor,
            tabs_empty: ide.tabs.is_empty(),
        }
    }
}

pub struct Workbench {
    store: Entity<Store>,
    ide: Entity<Ide>,
    history: Entity<HistoryPanel>,
    chat: Entity<ChatPane>,
    files: Entity<FileTree>,
    search: Entity<SearchView>,
    git: Entity<GitView>,
    editor: Entity<EditorArea>,
    terminals: Entity<TerminalPanel>,
    cursor: Entity<CursorStatus>,
    outline: Entity<OutlineView>,
    relations: Entity<RelationsView>,
    swarm: Entity<agent::swarm::SwarmView>,
    metrics: Entity<MetricsView>,
    tunnels: Entity<TunnelsView>,
    /// The swarm of the selected chat at the last look, so a chat that just became a coordinator opens its board.
    selected_swarm: Option<(String, Option<String>)>,
    _marks: Entity<DebugMarks>,
    bottom_visible: bool,
    settings: Option<Entity<SettingsView>>,
    settings_section: Option<Section>,
    side: Side,
    left_visible: bool,
    agent_visible: bool,
    sizes: Sizes,
    panes: Option<PaneGroup>,
    bottom_panes: Option<PaneGroup>,
    layout: Layout,
    tool: Option<Tool>,
    tool_tree: Option<bool>,
    sidebar_peek: bool,
    agent_panes: Option<PaneGroup>,
    side_tab_bounds: Rc<RefCell<SideTabBounds>>,
    known_tabs: HashSet<String>,
    restoring_tabs: bool,
    ide_started: bool,
    awaiting: HashSet<String>,
    running: HashSet<String>,
    store_sig: StoreFingerprint,
    ide_sig: IdeFingerprint,
    last_update_phase: UpdatePhase,
    last_git_sig: (Option<Option<String>>, usize),
    last_lsp_sig: Option<(String, ServerPhase)>,
    last_debug_running: bool,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

/// The workbench's own focus, for commands that run after a dialog closed.
pub struct WorkbenchFocus(pub FocusHandle);
impl Global for WorkbenchFocus {}

/// The workbench, for commands that reach it from outside its own tree (the palette).
pub struct WorkbenchHandle(pub WeakEntity<Workbench>);
impl Global for WorkbenchHandle {}

impl Workbench {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ide = Ide::global(cx);
        let layout = crate::placement::read_layout(&store.read(cx).paths.user_data);
        let history = cx.new(|cx| HistoryPanel::new(store.clone(), window, cx));
        let chat = cx.new(|cx| ChatPane::new(store.clone(), window, cx));
        let files = cx.new(|cx| FileTree::new(ide.clone(), cx));
        let search = cx.new(|cx| SearchView::new(ide.clone(), window, cx));
        let git = cx.new(|cx| {
            let mut view = GitView::new(ide.clone(), window, cx);
            view.set_polling(layout.mode == Mode::Ide, window, cx);
            view
        });
        let terminals = cx.new(|cx| TerminalPanel::new(window, cx));
        let cursor = cx.new(|cx| CursorStatus::new(ide.clone(), cx));
        let editor = cx.new(|cx| EditorArea::new(ide.clone(), cx));
        let marks = cx.new(DebugMarks::new);
        let outline = cx.new(|cx| OutlineView::new(ide.clone(), editor.clone(), cx));
        let relations = cx.new(|cx| RelationsView::new(ide.clone(), cx));
        let swarm = cx.new(|cx| agent::swarm::SwarmView::new(store.clone(), cx));
        let metrics = cx.new(|cx| MetricsView::new(store.clone(), ide.clone(), window, cx));
        let tunnels = cx.new(|cx| TunnelsView::new(store.clone(), window, cx));

        let store_sig = StoreFingerprint::capture(store.read(cx));
        let ide_sig = IdeFingerprint::capture(ide.read(cx));
        let last_update_phase = store.read(cx).updates.phase.clone();
        let last_git_sig = (git.read(cx).branch(), git.read(cx).change_count());
        let last_debug_running = Execution::global(cx).read(cx).debug().is_some_and(|d| d.running());

        let subscriptions = vec![
            cx.observe(&Execution::global(cx), |this, exec, cx| {
                let debug_running = exec.read(cx).debug().is_some_and(|d| d.running());
                if debug_running != this.last_debug_running {
                    this.last_debug_running = debug_running;
                    cx.notify();
                }
            }),
            cx.observe_in(&store, window, |this, store, window, cx| {
                let store_ref = store.read(cx);
                if !this.ide_started && matches!(store_ref.phase, Phase::Ready) {
                    this.ide_started = true;
                    this.ide.update(cx, |ide, cx| ide.initialize(window, cx));
                }
                let store_ref = store.read(cx);
                let awaiting: HashSet<String> = store_ref
                    .summaries
                    .iter()
                    .filter(|summary| summary.awaiting_approval == Some(true))
                    .map(|summary| summary.id.clone())
                    .collect();
                let running: HashSet<String> = store_ref
                    .summaries
                    .iter()
                    .filter(|summary| matches!(summary.status.as_str(), "running" | "compacting"))
                    .map(|summary| summary.id.clone())
                    .collect();
                // Only invoke Win32 foreground/flash/toast checks when awaiting or running chats actually changed.
                if awaiting != this.awaiting || running != this.running {
                    if awaiting.iter().any(|id| !this.awaiting.contains(id)) {
                        crate::ui::attention::flash(window);
                    }
                    if !crate::ui::notify::in_front() && store_ref.prefs.notify {
                        let zh = crate::ui::locale(cx) == Locale::Zh;
                        for summary in &store_ref.summaries {
                            let was_running = this.running.contains(&summary.id);
                            let newly_waiting = awaiting.contains(&summary.id) && !this.awaiting.contains(&summary.id);
                            if let Some((title, text)) = crate::ui::notify::note_for(
                                &summary.title,
                                was_running,
                                summary.status == "idle",
                                newly_waiting,
                                zh,
                            ) {
                                crate::ui::notify::show(crate::ui::notify::Note {
                                    title,
                                    text,
                                    chat: summary.id.clone(),
                                });
                            }
                        }
                    }
                    this.awaiting = awaiting;
                    this.running = running;
                }
                if store_ref.updates.phase != this.last_update_phase {
                    this.last_update_phase = store_ref.updates.phase.clone();
                    crate::ui::update::taskbar(window, &this.last_update_phase);
                }
                let next_sig = StoreFingerprint::capture(store_ref);
                if next_sig != this.store_sig {
                    this.store_sig = next_sig;
                    cx.notify();
                }
            }),
            cx.observe(&ide, |this, ide, cx| {
                let next_sig = IdeFingerprint::capture(ide.read(cx));
                if next_sig != this.ide_sig {
                    let ws_changed = next_sig.workspace_id != this.ide_sig.workspace_id;
                    this.ide_sig = next_sig;
                    if ws_changed {
                        this.sync_panel_visibility(None, cx);
                    }
                    cx.notify();
                }
            }),
            cx.subscribe(&ide, |this, ide, event: &IdeEvent, cx| match event {
                IdeEvent::WorkspaceChanged => {
                    this.restoring_tabs = true;
                    this.sync_panel_visibility(None, cx);
                    cx.notify();
                }
                IdeEvent::ActiveChanged => {
                    let (opened, restored, tabs) = {
                        let ide = ide.read(cx);
                        (ide.active.clone().filter(|key| !this.known_tabs.contains(key)), ide.restored, ide.tabs.iter().cloned().collect())
                    };
                    let restoring = std::mem::take(&mut this.restoring_tabs) || !restored;
                    this.known_tabs = tabs;
                    if opened.is_some() && !restoring {
                        this.show_tool(Tool::Files, cx);
                    }
                    cx.notify();
                }
                IdeEvent::ExecutionChanged => cx.notify(),
                _ => {}
            }),
            // An image viewer's "添加到对话" lands in the composer, which is shown for it.
            cx.subscribe_in(&editor, window, |this, _, event: &EditorAreaEvent, window, cx| match event {
                EditorAreaEvent::AttachImage { name, bytes } => {
                    this.agent_visible = true;
                    let (name, bytes) = (name.clone(), bytes.clone());
                    let added = tr(cx, "viewers.imageAttached").to_string();
                    this.chat.update(cx, |chat, cx| chat.attach_bytes(name, bytes, Some(added), window, cx));
                    cx.notify();
                }
            }),
            cx.observe(&LanguageServers::global(cx), |this, servers, cx| {
                let next = this
                    .ide
                    .read(cx)
                    .active
                    .as_deref()
                    .and_then(|key| servers.read(cx).phase_of(key))
                    .map(|(name, phase)| (name.to_owned(), phase.clone()));
                if next != this.last_lsp_sig {
                    this.last_lsp_sig = next;
                    cx.notify();
                }
            }),
            cx.observe(&git, |this, git, cx| {
                let git = git.read(cx);
                let sig = (git.branch(), git.change_count());
                if sig != this.last_git_sig {
                    this.last_git_sig = sig;
                    cx.notify();
                }
            }),
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| match event {
                StoreEvent::PrefsChanged => {
                    let prefs = this.store.read(cx).prefs.clone();
                    theme::apply(&prefs, Some(window), cx);
                }
                StoreEvent::SelectionChanged if this.sidebar_peek => {
                    this.sidebar_peek = false;
                    cx.notify();
                }
                StoreEvent::SummariesChanged | StoreEvent::SelectionChanged | StoreEvent::TeamChanged(_) => this.follow_swarm(window, cx),
                StoreEvent::HostExited(code) => crate::ui::ssh::host_exited(this.store.clone(), *code, window, cx),
                StoreEvent::ResourcesDamaged(error) => resources_damaged(error, window, cx),
                StoreEvent::Update(notice) => crate::ui::update::show(notice, window, cx),
                StoreEvent::CredentialAlert(alert) => {
                    let _ = this.store.update(cx, |store, _| store.take_credential_alerts());
                    credential_alert(alert, window, cx);
                }
                _ => {}
            }),
            // Back in front: dismiss stale notifications, re-sync OS reduced-motion preference, and resume remote metrics polling.
            cx.observe_window_activation(window, |this, window, cx| {
                let active = window.is_window_active();
                if active {
                    crate::ui::notify::dismiss();
                    if !crate::reduced_motion_env() {
                        apply_system_reduce_motion(cx);
                    }
                }
                this.metrics.update(cx, |metrics, cx| metrics.set_window_active(active, cx));
            }),
            // "Follow the system" follows a change of the Windows light or dark mode.
            cx.observe_window_appearance(window, |this, window, cx| {
                let prefs = this.store.read(cx).prefs.clone();
                if prefs.theme == "system" {
                    theme::apply(&prefs, Some(window), cx);
                }
            }),
        ];

        // A click on a notification brings the window to the front with that chat.
        let (clicks_tx, clicks) = async_channel::unbounded::<String>();
        crate::ui::notify::start(clicks_tx);
        cx.spawn_in(window, async move |_, cx| {
            while let Ok(chat) = clicks.recv().await {
                let _ = cx.update(|window, cx| {
                    window.activate_window();
                    crate::ui::history::show_chat(chat, window, cx);
                });
            }
        })
        .detach();

        // Surface any DPAPI credential warning/error that arrived before the window opened.
        let early_alerts = store.update(cx, |store, _| store.take_credential_alerts());
        if let Some(first) = early_alerts.into_iter().next() {
            window.defer(cx, move |window, cx| credential_alert(&first, window, cx));
        }

        // The questions `ssh` asks are shown here, one after another.
        let questions = store.read(cx).questions.clone();
        cx.spawn_in(window, async move |_, cx| {
            while let Ok(request) = questions.recv().await {
                let _ = cx.update(|window, cx| crate::ui::ssh::ask(request, window, cx));
            }
        })
        .detach();
        let prompts = store.read(cx).runtime_prompts.clone();
        cx.spawn_in(window, async move |_, cx| {
            while let Ok(prompt) = prompts.recv().await {
                let _ = cx.update(|window, cx| runtime_download_failed(prompt, window, cx));
            }
        })
        .detach();

        let mut workbench = Self {
            store,
            ide,
            history,
            chat,
            files,
            search,
            git,
            editor,
            terminals,
            cursor,
            outline,
            relations,
            swarm,
            metrics,
            tunnels,
            selected_swarm: None,
            _marks: marks,
            bottom_visible: false,
            settings: None,
            settings_section: None,
            side: Side::Files,
            left_visible: true,
            agent_visible: true,
            sizes: DEFAULT_SIZES,
            panes: None,
            bottom_panes: None,
            layout,
            tool: None,
            tool_tree: None,
            sidebar_peek: false,
            agent_panes: None,
            side_tab_bounds: Rc::new(RefCell::new(SideTabBounds::default())),
            known_tabs: HashSet::new(),
            restoring_tabs: false,
            ide_started: false,
            awaiting: HashSet::new(),
            running: HashSet::new(),
            store_sig,
            ide_sig,
            last_update_phase,
            last_git_sig,
            last_lsp_sig: None,
            last_debug_running,
            focus: {
                let focus = cx.focus_handle();
                cx.set_global(WorkbenchFocus(focus.clone()));
                cx.set_global(WorkbenchHandle(cx.entity().downgrade()));
                focus
            },
            _subscriptions: subscriptions,
        };
        workbench.apply_mode(cx);
        workbench.sync_panel_visibility(Some(window), cx);
        workbench
    }

    /// The layout shown.
    pub fn mode(&self) -> Mode {
        self.layout.mode
    }

    pub(super) fn agent(&self) -> bool {
        self.layout.mode == Mode::Agent
    }

    /// Propagate current panel visibility to background-polling views so hidden panels never burn CPU or RPCs.
    pub(super) fn sync_panel_visibility(&self, window: Option<&mut Window>, cx: &mut App) {
        let ide_mode = !self.agent();
        if let Some(window) = window {
            let git_shown = ide_mode && self.left_visible && self.side == Side::Git;
            self.git.update(cx, |git, cx| {
                git.set_visible(git_shown, window, cx);
                git.set_polling(ide_mode, window, cx);
            });
        }
        let outline_shown = ide_mode && self.left_visible && self.side == Side::Outline;
        self.outline.update(cx, |outline, cx| outline.set_visible(outline_shown, cx));

        let terminals_shown = (ide_mode && self.bottom_visible && self.ide.read(cx).workspace.is_some())
            || (!ide_mode && self.tool == Some(Tool::Terminal));
        self.terminals.update(cx, |terminals, cx| terminals.set_panel_visible(terminals_shown, cx));

        // Metrics live in IDE mode's status bar; pause 5s polling when Agent mode hides the status bar.
        self.metrics.update(cx, |metrics, cx| metrics.set_visible(ide_mode, cx));
    }

    /// Reveal the chat pane and inject a quoted snippet into the composer so the user can add context and send.
    pub fn inject_into_chat(&mut self, snippet: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.agent() {
            self.agent_visible = true;
        }
        self.chat.update(cx, |chat, cx| chat.inject_into_composer(snippet, window, cx));
        cx.notify();
    }

    /// Switch the layout. Everything stays as it is: the chat and its draft, running work, open files and terminals.
    fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.mode == mode {
            return;
        }
        self.layout.mode = mode;
        self.panes = None;
        self.bottom_panes = None;
        self.agent_panes = None;
        self.sidebar_peek = false;
        self.apply_mode(cx);
        self.sync_panel_visibility(Some(window), cx);
        self.save_layout(cx);
        match mode {
            Mode::Agent => self.chat.update(cx, |chat, cx| chat.focus_composer(window, cx)),
            Mode::Ide => {
                if let Some((_, editor)) = editor_commands::active_editor(cx) {
                    editor.update(cx, |state, cx| state.focus(window, cx));
                }
            }
        }
        cx.notify();
    }

    /// Draw the shared views the way the layout shows them.
    fn apply_mode(&mut self, cx: &mut Context<Self>) {
        let agent = self.agent();
        self.chat.update(cx, |chat, cx| chat.set_wide(agent, cx));
        self.history.update(cx, |history, cx| history.set_sidebar(agent, cx));
        self.dock_changes(cx);
    }

    /// The chat's changes are in the panel while it shows them, else above the composer.
    fn dock_changes(&mut self, cx: &mut Context<Self>) {
        let docked = self.agent() && self.tool == Some(Tool::Changes);
        let changes = self.chat.read(cx).changes();
        changes.update(cx, |changes, cx| changes.set_docked(docked, cx));
    }

    /// Keep the layout for the next start.
    fn save_layout(&self, cx: &App) {
        if let Err(error) = crate::placement::write_layout(&self.store.read(cx).paths.user_data, &self.layout) {
            log::warn!("The window layout was not saved: {error:#}");
        }
    }

    /// Show a tool: in Agent mode in the panel beside the conversation; in IDE mode the editor is always there, and the
    /// terminals are in the bottom panel.
    pub fn show_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        if !self.agent() {
            if tool == Tool::Terminal {
                self.bottom_visible = true;
                self.sync_panel_visibility(None, cx);
                cx.notify();
            }
            return;
        }
        if self.tool == Some(tool) {
            return;
        }
        self.tool = Some(tool);
        self.dock_changes(cx);
        self.sync_panel_visibility(None, cx);
        cx.notify();
    }

    /// Open the panel with a tool, or close it when it shows that tool already.
    pub(super) fn toggle_tool(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        if self.tool == Some(tool) {
            self.close_tool(window, cx);
            return;
        }
        self.show_tool(tool, cx);
        if tool == Tool::Terminal {
            self.terminals.update(cx, |terminals, cx| terminals.focus(window, cx));
        }
    }

    pub(super) fn close_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tool = None;
        self.dock_changes(cx);
        self.sync_panel_visibility(Some(window), cx);
        self.chat.update(cx, |chat, cx| chat.focus_composer(window, cx));
        cx.notify();
    }

    /// Open the swarm's board when the selected chat just started a swarm; close it when the chat shown has none.
    fn follow_swarm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let summary = store.selected.as_ref().and_then(|id| store.summaries.iter().find(|summary| summary.id == *id));
        let now = store.selected.as_ref().map(|id| {
            let coordinator = summary.and_then(agent::swarm::coordinator_of).or_else(|| store.has_team(id).then(|| id.clone()));
            (id.clone(), coordinator)
        });
        let opens = summary.is_some_and(|summary| summary.swarm.is_some() || summary.status == "running");
        let before = std::mem::replace(&mut self.selected_swarm, now.clone());
        let (id, coordinator) = now.unwrap_or_default();
        if coordinator.is_some() && opens && before.as_ref().is_some_and(|(was, had)| *was == id && had.is_none()) {
            if self.agent() {
                self.show_tool(Tool::Swarm, cx);
            } else {
                self.show_side(Side::Swarm, window, cx);
            }
        } else if coordinator.is_none() {
            if self.tool == Some(Tool::Swarm) {
                self.tool = None;
                self.sync_panel_visibility(Some(window), cx);
                cx.notify();
            }
            if self.side == Side::Swarm {
                self.side = Side::History;
                self.sync_panel_visibility(Some(window), cx);
                cx.notify();
            }
        }
    }

    /// Put Agent mode's panel on the other side of the conversation.
    pub(super) fn swap_panel(&mut self, cx: &mut Context<Self>) {
        self.layout.panel_left = !self.layout.panel_left;
        self.agent_panes = None;
        self.save_layout(cx);
        cx.notify();
    }

    /// Where Agent mode's sidebar and panel go in this window.
    fn agent_fit(&self, window: &Window) -> Fit {
        let width = f32::from(window.viewport_size().width);
        agent::fit(width, self.layout.sidebar.then_some(self.layout.sidebar_width), self.tool.map(|_| self.layout.panel_width))
    }

    /// Show or hide the sidebar of Agent mode, or the file pane of IDE mode (Ctrl+B).
    pub(super) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.agent() {
            self.left_visible = !self.left_visible;
            self.sync_panel_visibility(Some(window), cx);
            cx.notify();
            return;
        }
        if self.sidebar_peek {
            self.sidebar_peek = false;
        } else if self.agent_fit(window).sidebar.is_some() {
            self.layout.sidebar = false;
            self.save_layout(cx);
        } else if !self.layout.sidebar {
            self.layout.sidebar = true;
            self.sidebar_peek = self.agent_fit(window).sidebar.is_none();
            self.save_layout(cx);
        } else {
            self.sidebar_peek = true;
        }
        cx.notify();
    }

    /// Bring up the chat sidebar with the cursor in its search field (Ctrl+Alt+K).
    fn search_chats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent() {
            if self.agent_fit(window).sidebar.is_none() {
                self.sidebar_peek = true;
            }
        } else {
            self.agent_visible = true;
            self.show_side(Side::History, window, cx);
        }
        self.history.update(cx, |history, cx| history.focus_search(window, cx));
        cx.notify();
    }

    /// Commands of IDE mode alone switch to it first.
    fn ide_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(Mode::Ide, window, cx);
    }

    /// A drag in Agent mode ended: keep the widths it made.
    pub(super) fn agent_resized(&mut self, sidebar: Option<f32>, panel: Option<f32>, cx: &mut Context<Self>) {
        if let Some(sidebar) = sidebar {
            self.layout.sidebar_width = sidebar.clamp(agent::SIDEBAR_MIN, agent::SIDEBAR_MAX);
        }
        if let Some(panel) = panel {
            self.layout.panel_width = panel;
        }
        self.save_layout(cx);
        cx.notify();
    }

    /// Show or hide the terminals; showing them focuses the current one. Agent mode shows them in its panel.
    pub(super) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent() {
            self.toggle_tool(Tool::Terminal, window, cx);
            return;
        }
        self.bottom_visible = !self.bottom_visible;
        self.sync_panel_visibility(Some(window), cx);
        if self.bottom_visible {
            self.terminals.update(cx, |terminals, cx| terminals.focus(window, cx));
        }
        cx.notify();
    }

    /// Show one view in the left pane; updates Git and Outline visibility immediately.
    fn show_side(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        self.side = side;
        self.left_visible = true;
        self.sync_panel_visibility(Some(window), cx);
        cx.notify();
    }

    /// Open the settings overlay at `section`.
    /// Clears window text selection on both open and close, and reuses an already-open settings view for the same
    /// section so repeated `Ctrl+,` / gear clicks never discard unsaved form edits.
    pub fn open_settings(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        TextSelection::clear(window, cx);
        if self.settings.is_some() && self.settings_section == Some(section) {
            cx.notify();
            return;
        }
        let view = cx.new(|cx| SettingsView::new(self.store.clone(), section, window, cx));
        cx.subscribe_in(&view, window, |this, _, _: &CloseSettings, window, cx| {
            TextSelection::clear(window, cx);
            this.settings = None;
            this.settings_section = None;
            cx.notify();
        })
        .detach();
        self.settings = Some(view);
        self.settings_section = Some(section);
        cx.notify();
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::web::begin_frame(cx);
        let _ = self.ide.read(cx);
        let agent_mode = self.agent();
        let phase = self.render_phase(cx);
        let (body, title_bar, status_bar, toast_width) = if agent_mode {
            let fit = self.agent_fit(window);
            let body = phase.unwrap_or_else(|| self.render_agent_body(fit, window, cx));
            (body, self.render_agent_title_bar(fit, cx).into_any_element(), None, 382.)
        } else {
            let sizes = self.pane_sizes(window, cx);
            let toast_width = if phase.is_none() && self.agent_visible { (sizes.agent - 32.).clamp(260., 382.) } else { 382. };
            let body = phase.unwrap_or_else(|| self.render_body(sizes, window, cx));
            (body, self.render_title_bar(window, cx).into_any_element(), Some(self.render_status_bar(cx)), toast_width)
        };
        if cx.theme().notification.width != px(toast_width) {
            cx.defer(move |cx| {
                let theme = gpui_kit::component::Theme::global_mut(cx);
                if theme.notification.width != px(toast_width) {
                    theme.notification.width = px(toast_width);
                }
            });
        }
        if window.focused(cx).is_none() {
            let focus = self.focus.clone();
            window.defer(cx, move |window, cx| {
                if window.focused(cx).is_none() {
                    focus.focus(window, cx);
                }
            });
        }
        let settings = self.settings.clone();
        let ssh_banner = self.render_ssh_banner(cx);
        let theme = cx.theme();
        v_flex()
            .relative()
            .size_full()
            .key_context("Workbench")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &AgentMode, window, cx| this.set_mode(Mode::Agent, window, cx)))
            .on_action(cx.listener(|this, _: &IdeMode, window, cx| this.set_mode(Mode::Ide, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleMode, window, cx| {
                let next = if this.agent() { Mode::Ide } else { Mode::Agent };
                this.set_mode(next, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| this.toggle_sidebar(window, cx)))
            .on_action(cx.listener(|this, _: &SaveFile, window, cx| {
                this.ide.update(cx, |ide, cx| ide.save(None, false, window, cx));
            }))
            .on_action(cx.listener(|this, _: &CloseFile, window, cx| {
                if this.agent() && this.tool != Some(Tool::Files) {
                    return;
                }
                if let Some(key) = this.ide.read(cx).active.clone() {
                    close_tab(this.ide.clone(), key, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ReplaceInFiles, window, cx| {
                this.ide_mode(window, cx);
                this.show_side(Side::Search, window, cx);
                this.search.update(cx, |search, cx| search.show_replace(cx));
                let query = this.search.read(cx).query();
                query.update(cx, |state, cx| state.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &ShowExplorer, window, cx| {
                if this.agent() {
                    this.tool_tree = Some(true);
                    this.show_tool(Tool::Files, cx);
                } else {
                    this.show_side(Side::Files, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ShowSourceControl, window, cx| {
                this.ide_mode(window, cx);
                this.show_side(Side::Git, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(Section::General, window, cx)))
            .on_action(cx.listener(|this, _: &OpenRuntimeSettings, window, cx| this.open_settings(Section::Runtime, window, cx)))
            .on_action(|_: &CommandPalette, window, cx| crate::ui::palette::open(window, cx))
            .on_action(cx.listener(|this, _: &ShowMap, window, cx| {
                this.show_tool(Tool::Files, cx);
                this.editor.update(cx, |editor, cx| editor.toggle_map(window, cx));
            }))
            .on_action(cx.listener(|this, _: &ShowChat, window, cx| {
                if this.agent() {
                    this.chat.update(cx, |chat, cx| chat.focus_composer(window, cx));
                } else {
                    this.agent_visible = true;
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NewChat, window, cx| {
                this.agent_visible = true;
                this.sidebar_peek = false;
                this.store.update(cx, |store, cx| store.select(None, cx));
                if this.agent() {
                    this.chat.update(cx, |chat, cx| chat.focus_composer(window, cx));
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FindInFiles, window, cx| {
                this.ide_mode(window, cx);
                this.show_side(Side::Search, window, cx);
                let query = this.search.read(cx).query();
                query.update(cx, |state, cx| state.focus(window, cx));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| this.toggle_terminal(window, cx)))
            .on_action(cx.listener(|this, _: &RunFile, window, cx| this.launch(false, window, cx)))
            .on_action(cx.listener(|this, _: &DebugFile, window, cx| this.launch(true, window, cx)))
            .on_action(cx.listener(|this, _: &StopRun, _, cx| this.stop_run(cx)))
            .on_action(cx.listener(|this, _: &StepOver, _, cx| this.step("next", cx)))
            .on_action(cx.listener(|this, _: &StepInto, _, cx| this.step("stepIn", cx)))
            .on_action(cx.listener(|this, _: &StepOut, _, cx| this.step("stepOut", cx)))
            .on_action(cx.listener(|this, _: &ToggleBreakpoint, window, cx| this.toggle_breakpoint(window, cx)))
            .on_action(cx.listener(|_, _: &RunProfiles, window, cx| run_dialog::open(window, cx)))
            .on_action(|_: &GoToLine, window, cx| editor_commands::go_to_line(window, cx))
            .on_action(cx.listener(|this, _: &ExportPdf, window, cx| this.editor.update(cx, |editor, cx| editor.export_markdown("pdf", window, cx))))
            .on_action(cx.listener(|this, _: &ExportHtml, window, cx| this.editor.update(cx, |editor, cx| editor.export_markdown("html", window, cx))))
            .on_action(cx.listener(|this, _: &ShowOutline, window, cx| {
                this.ide_mode(window, cx);
                this.show_side(Side::Outline, window, cx);
            }))
            .on_action(cx.listener(|this, _: &MarkdownSource, _, cx| this.editor.update(cx, |editor, cx| editor.toggle_markdown_source(cx))))
            .on_action(cx.listener(|this, _: &MarkdownFocus, window, cx| {
                let on = this.store.read(cx).prefs.markdown_focus;
                crate::ui::settings::set_prefs(serde_json::json!({ "markdownFocus": !on }), window, cx);
            }))
            .on_action(cx.listener(|this, _: &MarkdownTypewriter, window, cx| {
                let on = this.store.read(cx).prefs.markdown_typewriter;
                crate::ui::settings::set_prefs(serde_json::json!({ "markdownTypewriter": !on }), window, cx);
            }))
            .on_action(cx.listener(|this, _: &SaveAll, window, cx| {
                this.ide.update(cx, |ide, cx| ide.save_all(window, cx)).detach();
            }))
            .on_action(cx.listener(|this, _: &ToggleFiles, window, cx| {
                if this.agent() {
                    this.toggle_tool(Tool::Files, window, cx);
                } else {
                    this.left_visible = !this.left_visible;
                    this.sync_panel_visibility(Some(window), cx);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleAgent, _, cx| {
                this.agent_visible = !this.agent_visible;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusEditor, window, cx| {
                this.ide_mode(window, cx);
                this.agent_visible = false;
                this.bottom_visible = false;
                this.sync_panel_visibility(Some(window), cx);
                this.editor.update(cx, |editor, cx| editor.hide_map(cx));
                if let Some((_, editor)) = editor_commands::active_editor(cx) {
                    editor.update(cx, |state, cx| state.focus(window, cx));
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ShowProblems, _, cx| {
                this.show_tool(Tool::Terminal, cx);
                this.terminals.update(cx, |terminals, cx| terminals.show(BottomTab::Problems, cx));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SearchHistory, window, cx| this.search_chats(window, cx)))
            .on_action(|_: &AttachFolder, window, cx| crate::ui::ide::choose_folder_to_attach(window, cx))
            .on_action(cx.listener(|this, _: &GitRefresh, window, cx| this.git.update(cx, |git, cx| git.refresh(window, cx))))
            .on_action(cx.listener(|this, _: &CheckUpdates, _, cx| this.store.update(cx, |store, cx| store.check_updates(true, cx))))
            .on_action(|_: &FormatDocument, window, cx| editor_commands::format_document(window, cx))
            .on_action(|_: &CompareWithSaved, window, cx| editor_commands::compare_with_saved(window, cx))
            .on_action(|_: &SendSelection, window, cx| editor_commands::send_selection(window, cx))
            .on_action(|_: &FindReferences, window, cx| editor_commands::find_references(window, cx))
            .on_action(|_: &RenameSymbol, window, cx| editor_commands::rename_symbol(window, cx))
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| {
                this.show_tool(Tool::Files, cx);
                quick_open::open(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenFolder, window, cx| {
                if matches!(this.store.read(cx).phase, Phase::Ready) {
                    choose_folder(window, cx);
                }
            }))
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .text_size(theme.font_size)
            .child(title_bar)
            .children(ssh_banner)
            .child(div().flex_1().min_h_0().flex().overflow_hidden().child(body))
            .children(status_bar)
            .children(settings.clone())
            .child(canvas(|_, _, _| {}, move |_, _, window, cx| crate::web::end_frame(settings.is_some(), window, cx)).absolute().size_0())
    }
}
