//! The window (`Workbench.tsx`) in its two layouts. IDE mode: the title bar with the project menu, the file and
//! history pane, the editor area, the AI pane and the status bar. Agent mode (`ui::agent`): the chat sidebar, the
//! conversation in the middle and, on demand, a panel on the right with the files, the terminals or the chat's
//! changes. Both show the same entities.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::dialog::DialogFooter;
use rainy_carrier::{ExecutionTarget, targets};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, ResizableState, Sizable as _, TitleBar, h_flex, h_resizable, resizable_panel, v_flex, v_resizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::{Disableable as _, Selectable as _, StyledExt as _};
use serde_json::Value;
use rainy_carrier::ConnectionState;
use rainy_carrier::trust::TrustError;

use crate::placement::Layout;
use crate::store::ide::{Ide, IdeEvent, PaneLayout};
use crate::boot::RuntimeChoice;
use crate::store::{Phase, RuntimePrompt, Store, StoreEvent};
use crate::theme;
use crate::ui::agent::{self, AgentMode, Fit, IdeMode, Mode, ToggleMode, ToggleSidebar, Tool};
use crate::ui::chat::ChatPane;
use crate::ui::history::{ChatActions, HistoryPanel};
use crate::ui::ide::editor_area::{EditorArea, close_tab};
use crate::ui::ide::file_tree::FileTree;
use crate::ui::ide::git::GitView;
use crate::ui::ide::search::SearchView;
use crate::ui::ide::cursor_status::CursorStatus;
use crate::store::execution::Execution;
use crate::store::ide::{RUN_LANGUAGES, RunMode, run_language};
use crate::ui::ide::debug_marks::DebugMarks;
use crate::ui::ide::outline::OutlineView;
use crate::ui::ide::{
    AttachFolder, CheckUpdates, ExportHtml, ExportPdf, FocusEditor, GitRefresh, MarkdownFocus, MarkdownSource, MarkdownTypewriter, SaveAll, SearchHistory,
    ShowOutline, ShowProblems, ToggleAgent, ToggleFiles,
};
use crate::ui::ide::terminal_panel::{BottomTab, TerminalPanel};
use crate::ui::ide::{
    CloseFile, CommandPalette, DebugFile, FindInFiles, NewChat, OpenFolder, OpenRuntimeSettings, OpenSettings, QuickOpen, ReplaceInFiles, RunFile, RunProfiles,
    SaveFile, ShowChat, ShowExplorer, ShowMap, ShowSourceControl, StepInto, StepOut, StepOver, StopRun, ToggleBreakpoint, ToggleTerminal, choose_folder,
    quick_open, run_dialog, editor_commands, GoToLine, FormatDocument, CompareWithSaved, SendSelection, FindReferences, RenameSymbol,
};
use crate::ui::settings::{CloseSettings, Section, SettingsView};
use crate::ui::{tr, trf};

/// What the left pane shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Files,
    Search,
    Git,
    History,
    Outline,
}

/// Pane sizes in logical pixels: the file pane's width, the AI pane's (0 follows the window) and the bottom panel's
/// height.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sizes {
    sidebar: f32,
    agent: f32,
    bottom: f32,
}

/// What a new project starts with, as the Host stores it.
const DEFAULT_SIZES: Sizes = Sizes { sidebar: 240., agent: 0., bottom: 220. };
/// The narrowest the editor between the panes gets while the window allows, and the narrowest AI pane (3.x
/// `clampAgent`).
const EDITOR_MIN: f32 = 300.;
const AGENT_MIN: f32 = 300.;
/// GPUI Kit's title bar starts its children this far from the window's left edge.
const TITLE_BAR_INSET: f32 = 12.;

impl Sizes {
    /// The sizes to lay out in a window of `width` × `height` (3.x `layout.ts`): the file pane 180–360 px, the AI pane
    /// following the window (28%, 340–600 px) until it was resized and never under 300 px, the bottom panel from 120
    /// px to the window height less 230, and the editor not under 300 px while the window allows.
    fn clamped(self, width: f32, height: f32, left: bool, agent: bool) -> Sizes {
        let sidebar = self.sidebar.clamp(180., 360.).min(sidebar_max(width, agent)).round();
        let left_width = if left { sidebar } else { 0. };
        // 400 is the fixed width projects stored before the pane followed the window; a pane never resized still has it.
        let preferred = if self.agent > 0. && self.agent != 400. { self.agent } else { (width * 0.28).clamp(340., 600.) };
        let agent = preferred.min(width - left_width - EDITOR_MIN).max(AGENT_MIN).round();
        Sizes { sidebar, agent, bottom: self.bottom.min(height - 230.).max(120.).round() }
    }
}

/// The widest file pane that leaves the editor and the AI pane their least.
fn sidebar_max(width: f32, agent: bool) -> f32 {
    (width - EDITOR_MIN - if agent { AGENT_MIN } else { 0. }).clamp(180., 360.)
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
    _marks: Entity<DebugMarks>,
    bottom_visible: bool,
    settings: Option<Entity<SettingsView>>,
    side: Side,
    left_visible: bool,
    agent_visible: bool,
    /// The pane sizes dragged in this window, for a window without a project and a project that has none saved; a
    /// project's own are kept with it (`Ide::layout`).
    sizes: Sizes,
    /// The states of the pane groups: the window's columns, and the editor above the bottom panel.
    panes: Option<PaneGroup>,
    bottom_panes: Option<PaneGroup>,
    /// The layout and what Agent mode keeps across launches (`layout.json`).
    layout: Layout,
    /// What the panel on the right of Agent mode shows; hidden while `None`.
    tool: Option<Tool>,
    /// The file tree beside the editor in the files panel, as the user set it; until then it is shown in a panel wide
    /// enough for both, or while no file is open.
    tool_tree: Option<bool>,
    /// The sidebar shown over the conversation, in a window too narrow to dock it.
    sidebar_peek: bool,
    /// The state of Agent mode's columns.
    agent_panes: Option<PaneGroup>,
    /// The tabs open when the editor was last looked at, and whether the next tabs are the ones a project shown just
    /// now restores: in Agent mode, a tab opened for the user brings up the files panel, a restored one does not.
    known_tabs: HashSet<String>,
    restoring_tabs: bool,
    /// Whether the projects were loaded since the Host became ready.
    ide_started: bool,
    /// Chats known to wait for an approval, so each new wait flashes the taskbar once.
    awaiting: HashSet<String>,
    /// The window's own focus, so shortcuts work while nothing inside it has the focus (after a dialog closed, say).
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Workbench {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ide = Ide::global(cx);
        let layout = crate::placement::read_layout(&store.read(cx).paths.user_data);
        let history = cx.new(|cx| HistoryPanel::new(store.clone(), window, cx));
        let chat = cx.new(|cx| ChatPane::new(store.clone(), window, cx));
        let files = cx.new(|cx| FileTree::new(ide.clone(), cx));
        let search = cx.new(|cx| SearchView::new(ide.clone(), window, cx));
        let git = cx.new(|cx| GitView::new(ide.clone(), window, cx));
        let terminals = cx.new(|cx| TerminalPanel::new(window, cx));
        let cursor = cx.new(|cx| CursorStatus::new(ide.clone(), cx));
        let editor = cx.new(|cx| EditorArea::new(ide.clone(), cx));
        let marks = cx.new(DebugMarks::new);
        let outline = cx.new(|cx| OutlineView::new(ide.clone(), editor.clone(), cx));
        let subscriptions = vec![
            cx.observe(&Execution::global(cx), |_, _, cx| cx.notify()),
            cx.observe_in(&store, window, |this, store, window, cx| {
                if !this.ide_started && matches!(store.read(cx).phase, Phase::Ready) {
                    this.ide_started = true;
                    this.ide.update(cx, |ide, cx| ide.initialize(window, cx));
                }
                let awaiting: HashSet<String> =
                    store.read(cx).summaries.iter().filter(|summary| summary.awaiting_approval == Some(true)).map(|summary| summary.id.clone()).collect();
                if awaiting.iter().any(|id| !this.awaiting.contains(id)) {
                    crate::ui::attention::flash(window);
                }
                this.awaiting = awaiting;
                crate::ui::update::taskbar(window, &store.read(cx).updates.phase);
                cx.notify();
            }),
            cx.observe(&ide, |_, _, cx| cx.notify()),
            cx.subscribe(&ide, |this, ide, event: &IdeEvent, cx| match event {
                IdeEvent::WorkspaceChanged => this.restoring_tabs = true,
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
                }
                _ => {}
            }),
            // An image viewer's "添加到对话" lands in the composer, which is shown for it.
            cx.subscribe_in(&editor, window, |this, _, event: &crate::ui::ide::editor_area::EditorAreaEvent, window, cx| match event {
                crate::ui::ide::editor_area::EditorAreaEvent::AttachImage { name, bytes } => {
                    this.agent_visible = true;
                    let (name, bytes) = (name.clone(), bytes.clone());
                    let added = tr(cx, "viewers.imageAttached").to_string();
                    this.chat.update(cx, |chat, cx| chat.attach_bytes(name, bytes, Some(added), window, cx));
                    cx.notify();
                }
            }),
            cx.observe(&crate::store::language::LanguageServers::global(cx), |_, _, cx| cx.notify()),
            cx.observe(&git, |_, _, cx| cx.notify()),
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| {
                match event {
                    StoreEvent::PrefsChanged => {
                        let prefs = this.store.read(cx).prefs.clone();
                        theme::apply(&prefs, Some(window), cx);
                    }
                    // A chat chosen in the sidebar shown over the conversation closes it.
                    StoreEvent::SelectionChanged if this.sidebar_peek => {
                        this.sidebar_peek = false;
                        cx.notify();
                    }
                    StoreEvent::HostExited(code) => crate::ui::ssh::host_exited(this.store.clone(), *code, window, cx),
                    StoreEvent::ResourcesDamaged(error) => resources_damaged(error, window, cx),
                    StoreEvent::Update(notice) => crate::ui::update::show(notice, window, cx),
                    _ => {}
                }
            }),
            // "Follow the system" follows a change of the Windows light or dark mode.
            cx.observe_window_appearance(window, |this, window, cx| {
                let prefs = this.store.read(cx).prefs.clone();
                if prefs.theme == "system" {
                    theme::apply(&prefs, Some(window), cx);
                }
            }),
        ];
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
            _marks: marks,
            bottom_visible: false,
            settings: None,
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
            known_tabs: HashSet::new(),
            restoring_tabs: false,
            ide_started: false,
            awaiting: HashSet::new(),
            focus: {
                let focus = cx.focus_handle();
                cx.set_global(WorkbenchFocus(focus.clone()));
                cx.set_global(WorkbenchHandle(cx.entity().downgrade()));
                focus
            },
            _subscriptions: subscriptions,
        };
        workbench.apply_mode(cx);
        workbench
    }

    /// The layout shown.
    pub fn mode(&self) -> Mode {
        self.layout.mode
    }

    fn agent(&self) -> bool {
        self.layout.mode == Mode::Agent
    }

    /// Switch the layout. Everything stays as it is: the chat and its draft, running work, open files and terminals.
    fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.mode == mode {
            return;
        }
        self.layout.mode = mode;
        // The pane groups of the other layout start again at their sizes when it comes back.
        self.panes = None;
        self.bottom_panes = None;
        self.agent_panes = None;
        self.sidebar_peek = false;
        self.apply_mode(cx);
        // Git reads its status more often only while its view is shown.
        let git_shown = mode == Mode::Ide && self.left_visible && self.side == Side::Git;
        self.git.update(cx, |git, cx| git.set_visible(git_shown, window, cx));
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

    /// Keep the layout for the next start. The record is small and changes only on a click or at the end of a drag.
    fn save_layout(&self, cx: &App) {
        if let Err(error) = crate::placement::write_layout(&self.store.read(cx).paths.user_data, &self.layout) {
            log::warn!("The window layout was not saved: {error:#}");
        }
    }

    /// Show a tool: in Agent mode in the panel on the right; in IDE mode the editor is always there, and the
    /// terminals are in the bottom panel.
    pub fn show_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        if !self.agent() {
            if tool == Tool::Terminal {
                self.bottom_visible = true;
                cx.notify();
            }
            return;
        }
        if self.tool == Some(tool) {
            return;
        }
        self.tool = Some(tool);
        self.dock_changes(cx);
        cx.notify();
    }

    /// Open the panel with a tool, or close it when it shows that tool already; the terminal takes the keyboard when
    /// it opens, the composer when the panel closes.
    fn toggle_tool(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        if self.tool == Some(tool) {
            self.close_tool(window, cx);
            return;
        }
        self.show_tool(tool, cx);
        if tool == Tool::Terminal {
            self.terminals.update(cx, |terminals, cx| terminals.focus(window, cx));
        }
    }

    fn close_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tool = None;
        self.dock_changes(cx);
        self.chat.update(cx, |chat, cx| chat.focus_composer(window, cx));
        cx.notify();
    }

    /// Where Agent mode's sidebar and panel go in this window.
    fn agent_fit(&self, window: &Window) -> Fit {
        let width = f32::from(window.viewport_size().width);
        agent::fit(width, self.layout.sidebar.then_some(self.layout.sidebar_width), self.tool.map(|_| self.layout.panel_width))
    }

    /// Show or hide the sidebar of Agent mode, or the file pane of IDE mode (Ctrl+B). A window too narrow to dock
    /// Agent mode's sidebar shows it over the conversation until a chat is chosen.
    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.agent() {
            self.left_visible = !self.left_visible;
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
    fn agent_resized(&mut self, sidebar: Option<f32>, panel: Option<f32>, cx: &mut Context<Self>) {
        if let Some(sidebar) = sidebar {
            self.layout.sidebar_width = sidebar.clamp(agent::SIDEBAR_MIN, agent::SIDEBAR_MAX);
        }
        if let Some(panel) = panel {
            self.layout.panel_width = panel;
        }
        self.save_layout(cx);
        cx.notify();
    }

    /// Open the settings window at `section`.
    pub fn open_settings(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| SettingsView::new(self.store.clone(), section, window, cx));
        cx.subscribe(&view, |this, _, _: &CloseSettings, cx| {
            this.settings = None;
            cx.notify();
        })
        .detach();
        self.settings = Some(view);
        cx.notify();
    }

    fn render_title_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // In a narrow window the quick-open and run buttons show only their icons and the project name is cut shorter.
        let narrow = window.viewport_size().width < px(1100.);
        let ready = matches!(self.store.read(cx).phase, Phase::Ready);
        let ide = self.ide.read(cx);
        let title: SharedString =
            ide.workspace.as_ref().map(|workspace| SharedString::from(workspace.title.clone())).unwrap_or_else(|| tr(cx, "ide.ideOpenFolder"));
        let path = ide.workspace.as_ref().map(|workspace| workspace.path.clone()).unwrap_or_default();
        let has_project = ide.workspace.is_some();
        let (left, agent, bottom) = (self.left_visible, self.agent_visible, self.bottom_visible);
        let tooltip: SharedString = if path.is_empty() { tr(cx, "native.projects") } else { path.into() };
        // The name is cut with an ellipsis; the tooltip keeps the whole path.
        let projects = agent::with_project_menu(
            Button::new("projects")
                .ghost()
                .small()
                .min_w_0()
                .flex_shrink_1()
                .max_w(px(if narrow { 180. } else { 280. }))
                .icon(IconName::Folder)
                .label(title)
                .tooltip(tooltip),
            cx,
        );
        let quick_open = Button::new("quick-open")
            .ghost()
            .small()
            .icon(IconName::Search)
            .when(!narrow, |this| this.label(tr(cx, "ide.ideSearchFiles")))
            .tooltip(tr(cx, "ide.ideQuickOpen"))
            .on_click(|_, window, cx| quick_open::open(window, cx));
        let toggle = |on: bool, button: Button| button.toggled(on).selected(on).when(on, |this| this.custom(crate::theme::selected_button(cx)));
        let toggle_files = toggle(
            left,
            Button::new("toggle-files")
                .ghost()
                .small()
                .icon(IconName::PanelLeft)
                .tooltip_with_action(tr(cx, "ide.ideToggleFiles"), &ToggleSidebar, None)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.left_visible = !this.left_visible;
                    cx.notify();
                })),
        );
        let toggle_bottom = toggle(
            bottom,
            Button::new("toggle-bottom")
                .ghost()
                .small()
                .icon(IconName::PanelBottom)
                .tooltip(tr(cx, "ide.ideToggleBottom"))
                .on_click(cx.listener(|this, _, window, cx| this.toggle_terminal(window, cx))),
        );
        let toggle_agent = toggle(
            agent,
            Button::new("toggle-agent")
                .ghost()
                .small()
                .icon(IconName::PanelRight)
                .tooltip(tr(cx, "ide.ideToggleAgent"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.agent_visible = !this.agent_visible;
                    cx.notify();
                })),
        );
        let settings = Button::new("open-settings")
            .ghost()
            .small()
            .icon(IconName::Settings)
            .tooltip(tr(cx, "app.settings"))
            .on_click(cx.listener(|this, _, window, cx| this.open_settings(Section::General, window, cx)));
        let run_controls = (ready && has_project).then(|| self.render_run_controls(narrow, cx));
        TitleBar::new().child(
            // The bar keeps at least the width its children need, so a row wider than the window would push the window
            // buttons out of it: this row asks for none (no width of its own, what does not fit is cut), and its middle
            // part gives way first, the project name before the buttons.
            h_flex()
                .flex_1()
                .w_0()
                .min_w_0()
                .overflow_hidden()
                .pr_2()
                .gap_2()
                .items_center()
                .child(img("brand/rainy-mark.svg").size(px(18.)).flex_shrink_0())
                .child(div().flex_shrink_0().text_sm().font_semibold().child("Rainy"))
                .child(title_control(agent::mode_switch(Mode::Ide, cx)))
                .child(div().flex_1())
                .child(
                    h_flex()
                        .min_w_0()
                        .overflow_hidden()
                        .gap_2()
                        .items_center()
                        .when(ready, |this| this.child(title_control(projects).min_w_0()).when(has_project, |this| this.child(title_control(quick_open))))
                        .children(run_controls),
                )
                .child(div().flex_1())
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_2()
                        .items_center()
                        .child(title_control(toggle_files))
                        .child(title_control(toggle_bottom))
                        .child(title_control(toggle_agent))
                        .child(title_control(settings)),
                ),
        )
    }

    /// Agent mode's title bar: the sidebar toggle and the mode switch over the sidebar, the chat's title with its
    /// menu, and the toggles of the panel on the right.
    fn render_agent_title_bar(&self, fit: Fit, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let store = self.store.read(cx);
        let summary = store.selected.as_ref().and_then(|id| store.summaries.iter().find(|summary| summary.id == *id)).cloned();
        let toggle = |on: bool, button: Button| button.toggled(on).selected(on).when(on, |this| this.custom(crate::theme::selected_button(cx)));
        let sidebar = toggle(
            fit.sidebar.is_some() || self.sidebar_peek,
            Button::new("toggle-sidebar")
                .ghost()
                .small()
                .icon(IconName::PanelLeft)
                .tooltip_with_action(tr(cx, "agent.toggleSidebar"), &ToggleSidebar, None)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx))),
        );
        let title = match summary {
            Some(summary) => {
                let title: SharedString = if summary.title.is_empty() { tr(cx, "chat.newChat") } else { summary.title.clone().into() };
                let actions = ChatActions::of(&summary, title.clone());
                // The title is cut with an ellipsis before the buttons give way.
                title_control(
                    Button::new("chat-title").ghost().small().min_w_0().flex_shrink_1().max_w(px(560.)).label(title).dropdown_caret(true).dropdown_menu(
                        move |menu, _, cx| {
                            actions
                                .menu(menu, cx)
                                .separator()
                                .item(PopupMenuItem::new(tr(cx, "agent.newChat")).on_click(|_, window, cx| window.dispatch_action(Box::new(NewChat), cx)))
                        },
                    ),
                )
                .min_w_0()
                .into_any_element()
            }
            None => div().min_w_0().px_2().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().child(tr(cx, "chat.newChat")).into_any_element(),
        };
        let changes = self.chat.read(cx).changes().read(cx).count();
        let tool = |id: &'static str, icon: Icon, label: &str, value: Tool| {
            toggle(
                self.tool == Some(value),
                Button::new(id).ghost().small().icon(icon).tooltip(tr(cx, label)).on_click(cx.listener(move |this, _, window, cx| this.toggle_tool(value, window, cx))),
            )
        };
        let files = tool("tool-files", Icon::new(IconName::FolderOpen), "agent.toolFiles", Tool::Files);
        let terminal = tool("tool-terminal", Icon::new(IconName::SquareTerminal), "agent.toolTerminal", Tool::Terminal);
        let theme = cx.theme();
        let changes = tool("tool-changes", Icon::new(gpui_kit::assets::IconName::GitCompare), "agent.toolChanges", Tool::Changes).when(changes > 0, |this| {
            this.child(div().flex_shrink_0().px_1().rounded(theme.radius).bg(theme.muted).text_xs().text_color(theme.foreground).child(changes.min(99).to_string()))
        });
        let more = Button::new("agent-more").ghost().small().icon(IconName::Ellipsis).tooltip(tr(cx, "agent.more")).dropdown_menu_with_anchor(Anchor::TopRight, |menu, _, cx| {
            menu.item(PopupMenuItem::new(tr(cx, "agent.openInIde")).on_click(|_, window, cx| window.dispatch_action(Box::new(IdeMode), cx)))
                .item(PopupMenuItem::new(tr(cx, "native.projectMap")).on_click(|_, window, cx| window.dispatch_action(Box::new(ShowMap), cx)))
                .separator()
                .item(PopupMenuItem::new(tr(cx, "app.settings")).on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)))
        });
        TitleBar::new().child(
            // As in IDE mode, the row asks for no width of its own; the title gives way first.
            h_flex()
                .flex_1()
                .w_0()
                .min_w_0()
                .overflow_hidden()
                .pr_2()
                .gap_2()
                .items_center()
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_1()
                        .items_center()
                        // Over a docked sidebar the group spans it, so the title starts over the conversation.
                        .when_some(fit.sidebar, |this, width| this.w(px(width - TITLE_BAR_INSET - 8.)))
                        .child(img("brand/rainy-mark.svg").size(px(18.)).flex_shrink_0().mr_1())
                        .child(title_control(sidebar))
                        .child(div().flex_1())
                        .child(title_control(agent::mode_switch(Mode::Agent, cx))),
                )
                .child(title)
                .child(div().flex_1())
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_1()
                        .items_center()
                        .child(title_control(files))
                        .child(title_control(terminal))
                        .child(title_control(changes))
                        .child(title_control(more)),
                ),
        )
    }

    /// The execution environment, as a label whose menu switches it, and the connection's color.
    fn render_target(&self, id: &'static str, cx: &mut Context<Self>) -> (Hsla, impl IntoElement + use<>) {
        let store = self.store.read(cx);
        let target: SharedString = store
            .target
            .as_ref()
            .map(|target| target.label().to_owned())
            .or_else(|| store.info.as_ref().map(|info| info.environment.clone()))
            .unwrap_or_else(|| "Windows".into())
            .into();
        let current = store.target.as_ref().map(|target| target.id().to_owned());
        let handle = self.store.clone();
        let theme = cx.theme();
        let dot = match store.connection {
            ConnectionState::Open => theme.success,
            ConnectionState::Connecting => theme.warning,
            ConnectionState::Closed => theme.danger,
        };
        let button = Button::new(id)
            .ghost()
            .xsmall()
            .max_w(px(200.))
            .label(target)
            .tooltip(tr(cx, "native.switchTarget"))
            .dropdown_menu(move |menu, _, _| target_menu(menu, handle.clone(), current.clone()));
        (dot, button)
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let version = env!("CARGO_PKG_VERSION");
        // The language service of the file shown, while it starts or after it failed; a working one says nothing.
        let active = crate::store::ide::Ide::global(cx).read(cx).active.clone();
        let language = active.and_then(|key| {
            let servers = crate::store::language::LanguageServers::global(cx);
            let servers = servers.read(cx);
            let (name, phase) = servers.phase_of(&key)?;
            let (label, reason, missing) = match phase {
                crate::store::language::ServerPhase::Starting => (trf(cx, "native.languageStarting", &[("language", &name)]), None, false),
                // Its tools are not downloaded: a click opens the page that downloads them.
                crate::store::language::ServerPhase::Failed(reason) if reason.as_ref() == "tools-missing" => (trf(cx, "native.languageToolsMissing", &[("language", &name)]), None, true),
                crate::store::language::ServerPhase::Failed(reason) => (trf(cx, "native.languageFailed", &[("language", &name)]), Some(reason.clone()), false),
                crate::store::language::ServerPhase::Ready => return None,
            };
            Some(
                div()
                    .id("language-state")
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(label)
                    .when(missing, |this| this.cursor_pointer().on_click(cx.listener(|this, _, window, cx| this.open_settings(Section::Tools, window, cx))))
                    .when_some(reason, |this, reason| this.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(reason.clone()).build(window, cx))),
            )
        });
        // The branch of the project's repository and its changed files (3.x status bar); a click shows them.
        let changes = self.git.read(cx).change_count();
        let branch = self.ide.read(cx).workspace.as_ref().and(self.git.read(cx).branch()).map(|branch| {
            let label: SharedString = branch.map(SharedString::from).unwrap_or_else(|| tr(cx, "git.gitNoCommits"));
            let theme = cx.theme();
            Button::new("status-branch")
                .ghost()
                .xsmall()
                .min_w_0()
                .flex_shrink_1()
                .max_w(px(220.))
                .icon(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::GitBranch))
                .label(label)
                .when(changes > 0, |this| {
                    this.child(div().flex_shrink_0().px_1().rounded(theme.radius).bg(theme.muted).text_color(theme.foreground).child(changes.min(999).to_string()))
                })
                .tooltip(tr(cx, "git.gitPanel"))
                .on_click(cx.listener(|this, _, window, cx| this.show_side(Side::Git, window, cx)))
        });
        let (dot, target) = self.render_target("target", cx);
        let theme = cx.theme();
        // What does not fit in a narrow window is cut: the branch and the language state give way first.
        h_flex()
            .h(px(24.))
            .flex_shrink_0()
            .overflow_hidden()
            .px_3()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(theme.muted_foreground)
            .bg(theme.status_bar)
            .border_t_1()
            .border_color(theme.status_bar_border)
            .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(dot))
            .child(target)
            .children(branch)
            .child(div().flex_1())
            .children(language)
            .child(div().flex_shrink_0().child(self.cursor.clone()))
            .child(div().flex_shrink_0().whitespace_nowrap().child(format!("Rainy {version}")))
    }

    fn render_phase(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let phase = self.store.read(cx).phase.clone();
        if matches!(phase, Phase::Ready) {
            return None;
        }
        // A failed WSL or SSH start can continue on Windows, as 3.x offered.
        let on_wsl = matches!(self.store.read(cx).target, Some(ExecutionTarget::Wsl { .. } | ExecutionTarget::Ssh { .. }));
        let theme = cx.theme();
        match phase {
            Phase::Ready => None,
            Phase::Starting(step, fraction) => Some(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(img("brand/rainy-mark.svg").size(px(56.)))
                    .child(h_flex().gap_2().items_center().text_sm().text_color(theme.muted_foreground).child(Spinner::new().small()).child(tr(cx, "native.starting")))
                    .when(step != tr(cx, "native.starting"), |this| this.child(div().text_xs().text_color(theme.muted_foreground).child(step)))
                    .when_some(fraction, |this, fraction| this.child(Progress::new("startup-progress").xsmall().w(px(240.)).value(fraction * 100.)))
                    .into_any_element(),
            ),
            Phase::Failed(error) => Some(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .p_8()
                    .child(img("brand/rainy-mark.svg").size(px(56.)).flex_shrink_0())
                    // A long error scrolls, and gives way in a short window: the buttons below it stay in sight.
                    .child(
                        div()
                            .id("startup-error")
                            .w_full()
                            .max_w(px(640.))
                            .max_h(px(360.))
                            .min_h_0()
                            .overflow_y_scroll()
                            .text_sm()
                            .text_color(theme.danger)
                            .child(error),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_2()
                            .child(Button::new("retry").primary().label(tr(cx, "native.retry")).on_click(cx.listener(|this, _, _, cx| {
                                this.store.update(cx, |store, cx| store.restart(cx));
                            })))
                            .when(on_wsl, |this| {
                                this.child(Button::new("use-windows").outline().label(tr(cx, "native.useWindows")).on_click(cx.listener(
                                    |this, _, window, cx| {
                                        let handle = window.window_handle();
                                        this.store.update(cx, |store, cx| store.switch_target(ExecutionTarget::windows(), handle, cx));
                                    },
                                )))
                            })
                            .child(Button::new("quit").outline().label(tr(cx, "native.quit")).on_click(|_, _, cx| cx.quit())),
                    )
                    .into_any_element(),
            ),
        }
    }
    fn side_tab(&self, id: &'static str, icon: gpui_kit::component::Icon, label: SharedString, value: Side, cx: &mut Context<Self>) -> Button {
        let selected = self.side == value;
        Button::new(id)
            .ghost()
            .when(selected, |this| this.custom(crate::theme::selected_button(cx)))
            .xsmall()
            .icon(icon)
            .label(label)
            .selected(selected)
            .on_click(cx.listener(move |this, _, window, cx| this.show_side(value, window, cx)))
    }

    /// Run, the run menu, Debug and Stop (3.x title-bar run group); in a narrow window Run shows only its icon.
    fn render_run_controls(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ide = self.ide.read(cx);
        let active = ide.active.clone().filter(|key| ide.files.get(key).is_some_and(|file| file.editor.is_some()));
        let target = ide.run_target(active.as_deref());
        let execution = ide.execution();
        let profiles: Vec<Value> = execution["profiles"].as_array().cloned().unwrap_or_default();
        let place = active.as_deref().map(|key| {
            let (root, path) = crate::store::ide::split_key(key);
            (root.map(str::to_owned), path.to_owned())
        });
        let runs_open = |profile: &Value| {
            place.as_ref().is_some_and(|(root, path)| profile["program"] == path.as_str() && profile["rootId"].as_str() == root.as_deref())
        };
        let others: Vec<String> = profiles.iter().filter(|profile| !runs_open(profile)).take(8).filter_map(|profile| profile["name"].as_str().map(str::to_owned)).collect();
        let pinned = target.as_ref().filter(|(_, mode)| *mode == RunMode::Pinned).and_then(|(configuration, _)| configuration["name"].as_str().map(str::to_owned));
        let file_language =
            target.as_ref().filter(|(_, mode)| *mode != RunMode::Pinned).and_then(|(configuration, _)| configuration["language"].as_str().map(str::to_owned));
        let inferred = active.as_deref().and_then(run_language);
        let language_name = |id: &str| RUN_LANGUAGES.iter().find(|(known, _)| *known == id).map_or(id.to_owned(), |(_, name)| (*name).to_owned());
        let label: SharedString = match &target {
            None => tr(cx, "ide.ideRun"),
            Some((configuration, RunMode::Pinned)) => trf(cx, "ide.ideRunWith", &[("target", &configuration["name"].as_str().unwrap_or_default())]),
            Some((configuration, _)) => trf(cx, "ide.ideRunWith", &[("target", &language_name(configuration["language"].as_str().unwrap_or_default()))]),
        };
        let php = target.as_ref().is_some_and(|(configuration, _)| configuration["language"] == "php");
        let running = self.terminals.read(cx).running_selection(cx).is_some()
            || Execution::global(cx).read(cx).debug().is_some_and(|debug| debug.running());
        let handle = self.ide.clone();
        let (auto_label, current_label, pinned_label, edit_label) = (
            match inferred {
                Some(language) => trf(cx, "ide.ideRunAuto", &[("language", &language_name(language))]),
                None => tr(cx, "ide.ideRunAutoUnknown"),
            },
            tr(cx, "ide.ideRunCurrentFile"),
            tr(cx, "ide.ideRunPinned"),
            tr(cx, "ide.ideRunEdit"),
        );
        h_flex()
            .gap_0p5()
            .items_center()
            .child(title_control(
                Button::new("run-file")
                    .ghost()
                    .small()
                    .icon(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Play))
                    .when(!narrow, |this| this.label(label.clone()))
                    .tooltip(if narrow { label } else { tr(cx, "app.cmdRunFile") })
                    .on_click(cx.listener(|this, _, window, cx| this.launch(false, window, cx))),
            ))
            .child(title_control(
                Button::new("run-menu").ghost().small().icon(IconName::ChevronDown).tooltip(tr(cx, "ide.ideRunMode")).dropdown_menu(
                    move |mut menu, _, _| {
                        menu = menu.label(current_label.clone());
                        let choose = |choice: Option<&'static str>| {
                            let (handle, active) = (handle.clone(), active.clone());
                            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                                if let Some(key) = &active {
                                    handle.update(cx, |ide, cx| ide.choose_file_language(key, choice, cx));
                                }
                            }
                        };
                        menu = menu.item(
                            PopupMenuItem::new(auto_label.clone())
                                .disabled(inferred.is_none())
                                .checked(file_language.is_some() && file_language.as_deref() == inferred)
                                .on_click(choose(None)),
                        );
                        for (id, name) in RUN_LANGUAGES {
                            menu = menu.item(
                                PopupMenuItem::new(name)
                                    .disabled(active.is_none())
                                    .checked(file_language.as_deref() == Some(id) && inferred != Some(id))
                                    .on_click(choose(Some(id))),
                            );
                        }
                        if !others.is_empty() {
                            menu = menu.separator().label(pinned_label.clone());
                            for name in &others {
                                let (handle, pin) = (handle.clone(), name.clone());
                                menu = menu.item(PopupMenuItem::new(name.clone()).checked(pinned.as_deref() == Some(name.as_str())).on_click(move |_, _, cx| {
                                    let pin = pin.clone();
                                    handle.update(cx, |ide, cx| ide.pin_profile(Some(pin), cx));
                                }));
                            }
                        }
                        menu.separator().item(PopupMenuItem::new(edit_label.clone()).on_click(|_, window, cx| run_dialog::open(window, cx)))
                    },
                ),
            ))
            .child(title_control(
                Button::new("debug-file")
                    .ghost()
                    .small()
                    .icon(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Bug))
                    .tooltip(tr(cx, "app.cmdDebugFile"))
                    .disabled(php)
                    .on_click(cx.listener(|this, _, window, cx| this.launch(true, window, cx))),
            ))
            .when(running, |this| {
                this.child(title_control(
                    Button::new("stop-run")
                        .ghost()
                        .small()
                        .icon(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Square))
                        .tooltip(tr(cx, "ide.ideStop"))
                        .on_click(cx.listener(|this, _, _, cx| this.stop_run(cx))),
                ))
            })
    }

    /// Save everything, then run or debug what the run button names (3.x `launch`); F5 continues a paused session.
    fn launch(&mut self, debugging: bool, window: &mut Window, cx: &mut Context<Self>) {
        let execution = Execution::global(cx);
        if debugging && execution.read(cx).debug().is_some_and(|debug| debug.running() && debug.phase == "paused") {
            execution.update(cx, |execution, cx| execution.control("continue", cx));
            return;
        }
        let ide = self.ide.read(cx);
        if ide.workspace.is_none() {
            return;
        }
        let active = ide.active.clone().filter(|key| ide.files.get(key).is_some_and(|file| file.editor.is_some()));
        let Some((configuration, _)) = ide.run_target(active.as_deref()) else {
            run_dialog::open(window, cx);
            return;
        };
        if debugging && configuration["language"] == "php" {
            return;
        }
        let breakpoints = debugging.then(|| ide.breakpoints_for_root(configuration["rootId"].as_str()));
        let save = self.ide.update(cx, |ide, cx| ide.save_all(window, cx));
        self.show_tool(Tool::Terminal, cx);
        self.terminals.update(cx, |terminals, cx| terminals.show(if debugging { BottomTab::Debug } else { BottomTab::Terminal }, cx));
        cx.notify();
        cx.spawn_in(window, async move |_, cx| {
            if !save.await {
                return;
            }
            let Ok(started) = cx.update(|_, cx| {
                execution.update(cx, |execution, cx| match breakpoints {
                    Some(breakpoints) => execution.start_debug(configuration, breakpoints, cx),
                    None => execution.run(configuration, cx),
                })
            }) else {
                return;
            };
            if let Err(error) = started.await {
                let _ = cx.update(|window, cx| window.push_notification(crate::ui::error_notification(cx, &error), cx));
            }
        })
        .detach();
    }

    /// Stop the shown operation, else the debug session, else the last run still going.
    fn stop_run(&mut self, cx: &mut Context<Self>) {
        let execution = Execution::global(cx);
        let id = {
            let execution = execution.read(cx);
            self.terminals
                .read(cx)
                .running_selection(cx)
                .or_else(|| execution.debug().filter(|debug| debug.running()).map(|debug| debug.id.clone()))
                .or_else(|| execution.runs.iter().rev().find(|run| run.running()).map(|run| run.id.clone()))
        };
        if let Some(id) = id {
            execution.read(cx).stop(&id, cx);
        }
    }

    /// Step the paused debug session.
    fn step(&mut self, action: &'static str, cx: &mut Context<Self>) {
        let execution = Execution::global(cx);
        if execution.read(cx).debug().is_some_and(|debug| debug.running() && debug.phase == "paused") {
            execution.update(cx, |execution, cx| execution.control(action, cx));
        }
    }

    /// Set or clear the breakpoint on the cursor's line, in the running session too.
    fn toggle_breakpoint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ide = self.ide.read(cx);
        let Some(key) = ide.active.clone() else { return };
        // In Markdown, F9 is Typora's typewriter mode.
        if crate::ui::ide::viewers::preview_kind(&key) == Some("markdown") {
            window.dispatch_action(Box::new(MarkdownTypewriter), cx);
            return;
        }
        let Some(editor) = ide.files.get(&key).and_then(|file| file.editor.clone()) else { return };
        let line = editor.read(cx).cursor_position().line + 1;
        let source = self.ide.update(cx, |ide, cx| ide.toggle_breakpoint(&key, line, cx));
        Execution::global(cx).read(cx).set_breakpoints(source, cx);
    }

    /// Show or hide the terminals; showing them focuses the current one. Agent mode shows them in its panel.
    fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent() {
            self.toggle_tool(Tool::Terminal, window, cx);
            return;
        }
        self.bottom_visible = !self.bottom_visible;
        if self.bottom_visible {
            self.terminals.update(cx, |terminals, cx| terminals.focus(window, cx));
        }
        cx.notify();
    }

    /// Show one view in the left pane; Git reads its status more often while it is shown.
    fn show_side(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        self.side = side;
        self.left_visible = true;
        self.git.update(cx, |git, cx| git.set_visible(side == Side::Git, window, cx));
        cx.notify();
    }

    fn render_side(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let files = self.side_tab("side-files", gpui_kit::component::Icon::new(IconName::Folder), tr(cx, "ide.ideExplorer"), Side::Files, cx);
        let search = self.side_tab(
            "side-search",
            gpui_kit::component::Icon::new(IconName::Search),
            tr(cx, "search.searchTitle"),
            Side::Search,
            cx,
        );
        let changes = self.git.read(cx).change_count();
        let git_label: SharedString = if changes > 0 { format!("Git {}", changes.min(999)).into() } else { tr(cx, "git.gitTab") };
        let git = self.side_tab("side-git", gpui_kit::component::Icon::new(gpui_kit::assets::IconName::GitBranch), git_label, Side::Git, cx);
        let history = self.side_tab("side-history", gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Clock), tr(cx, "ide.ideHistory"), Side::History, cx);
        // The outline is only an icon: the pane is narrow, and the four tabs before it have their names.
        let outline = Button::new("side-outline")
            .ghost()
            .when(self.side == Side::Outline, |this| this.custom(crate::theme::selected_button(cx)))
            .xsmall()
            .icon(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::ListTree))
            .tooltip(tr(cx, "ide.ideOutline"))
            .selected(self.side == Side::Outline)
            .on_click(cx.listener(|this, _, window, cx| this.show_side(Side::Outline, window, cx)));
        let content: AnyView = match self.side {
            Side::Files => self.files.clone().into(),
            Side::Search => self.search.clone().into(),
            Side::Git => self.git.clone().into(),
            Side::History => self.history.clone().into(),
            Side::Outline => self.outline.clone().into(),
        };
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex()
                    .h(px(34.))
                    .flex_shrink_0()
                    .px_2()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.sidebar_border)
                    .child(files)
                    .child(search)
                    .child(git)
                    .child(history)
                    .child(div().flex_1())
                    .child(outline),
            )
            .child(div().flex_1().min_h_0().child(content))
    }

    /// The pane sizes to lay out: the shown project's, else those of this window, fitted to the window.
    fn pane_sizes(&self, window: &Window, cx: &App) -> Sizes {
        let saved = self.ide.read(cx).layout().unwrap_or_default();
        let stored = Sizes {
            sidebar: saved.sidebar_width.unwrap_or(self.sizes.sidebar),
            agent: saved.agent_width.unwrap_or(self.sizes.agent),
            bottom: saved.bottom_height.unwrap_or(self.sizes.bottom),
        };
        let viewport = window.viewport_size();
        stored.clamped(f32::from(viewport.width), f32::from(viewport.height), self.left_visible, self.agent_visible)
    }

    /// Keep the sizes a pane was dragged to, in this window and in the shown project.
    fn resized(&mut self, patch: PaneLayout, cx: &mut Context<Self>) {
        self.sizes = Sizes {
            sidebar: patch.sidebar_width.unwrap_or(self.sizes.sidebar),
            agent: patch.agent_width.unwrap_or(self.sizes.agent),
            bottom: patch.bottom_height.unwrap_or(self.sizes.bottom),
        };
        self.ide.update(cx, |ide, cx| ide.set_layout(patch, cx));
        cx.notify();
    }

    fn render_body(&mut self, sizes: Sizes, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (left, agent) = (self.left_visible, self.agent_visible);
        let viewport = window.viewport_size();
        let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
        let left_width = if left { sizes.sidebar } else { 0. };
        let agent_width = if agent { sizes.agent } else { 0. };
        let this = cx.entity().downgrade();
        let dragged = |now: Option<&Pixels>, before: f32| now.map(|size| f32::from(*size).round()).filter(|size| (size - before).abs() >= 1.);
        let mut targets = Vec::new();
        if left {
            targets.push((0, sizes.sidebar));
        }
        if agent {
            targets.push((usize::from(left) + 1, sizes.agent));
        }
        let state = pane_group(&mut self.panes, format!("{left}-{agent}"), targets, window, cx);
        let mut panes = h_resizable("workbench").with_state(&state).on_resize({
            let this = this.clone();
            move |state, _, cx| {
                let now = state.read(cx).sizes().clone();
                let patch = PaneLayout {
                    sidebar_width: left.then(|| dragged(now.first(), sizes.sidebar)).flatten(),
                    agent_width: agent.then(|| dragged(now.last(), sizes.agent)).flatten(),
                    bottom_height: None,
                };
                if patch != PaneLayout::default() {
                    let _ = this.update(cx, |this, cx| this.resized(patch, cx));
                }
            }
        });
        if left {
            panes = panes.child(
                resizable_panel().size(px(sizes.sidebar)).size_range(px(180.)..px(sidebar_max(width, agent))).fixed().child(self.render_side(cx)),
            );
        }
        let center = if self.bottom_visible && self.ide.read(cx).workspace.is_some() {
            let state = pane_group(&mut self.bottom_panes, String::new(), vec![(1, sizes.bottom)], window, cx);
            v_resizable("center")
                .with_state(&state)
                .on_resize(move |state, _, cx| {
                    if let Some(height) = dragged(state.read(cx).sizes().last(), sizes.bottom) {
                        let _ = this.update(cx, |this, cx| this.resized(PaneLayout { bottom_height: Some(height), ..PaneLayout::default() }, cx));
                    }
                })
                .child(resizable_panel().child(self.editor.clone()))
                .child(resizable_panel().size(px(sizes.bottom)).size_range(px(120.)..px((height - 230.).max(120.))).fixed().child(self.terminals.clone()))
                .into_any_element()
        } else {
            self.bottom_panes = None;
            self.editor.clone().into_any_element()
        };
        let editor_min = (width - left_width - agent_width).clamp(100., EDITOR_MIN);
        panes = panes.child(resizable_panel().size_range(px(editor_min)..Pixels::MAX).child(center));
        if agent {
            let agent_max = (width - left_width - EDITOR_MIN).max(AGENT_MIN);
            panes = panes.child(resizable_panel().size(px(sizes.agent)).size_range(px(AGENT_MIN)..px(agent_max)).fixed().child(self.chat.clone()));
        }
        panes.into_any_element()
    }

    /// Agent mode's columns: the sidebar, the conversation and the panel, each resizable; the sidebar shown over the
    /// conversation in a narrow window.
    fn render_agent_body(&mut self, fit: Fit, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let width = f32::from(window.viewport_size().width);
        let this = cx.entity().downgrade();
        let dragged = |now: Option<&Pixels>, before: f32| now.map(|size| f32::from(*size).round()).filter(|size| (size - before).abs() >= 1.);
        let mut targets = Vec::new();
        if let Some(sidebar) = fit.sidebar {
            targets.push((0, sidebar));
        }
        if let Some(panel) = fit.panel {
            targets.push((usize::from(fit.sidebar.is_some()) + 1, panel));
        }
        let state = pane_group(&mut self.agent_panes, format!("agent-{}-{}", fit.sidebar.is_some(), fit.panel.is_some()), targets, window, cx);
        let mut panes = h_resizable("agent").with_state(&state).on_resize(move |state, _, cx| {
            let now = state.read(cx).sizes().clone();
            let sidebar = fit.sidebar.and_then(|before| dragged(now.first(), before));
            let panel = fit.panel.and_then(|before| dragged(now.last(), before));
            if sidebar.is_some() || panel.is_some() {
                let _ = this.update(cx, |this, cx| this.agent_resized(sidebar, panel, cx));
            }
        });
        if let Some(sidebar) = fit.sidebar {
            panes = panes.child(
                resizable_panel().size(px(sidebar)).size_range(px(agent::SIDEBAR_MIN)..px(agent::SIDEBAR_MAX)).fixed().child(self.render_agent_sidebar(cx)),
            );
        }
        let rest = width - fit.sidebar.unwrap_or(0.) - fit.panel.unwrap_or(0.);
        panes = panes.child(resizable_panel().size_range(px(rest.clamp(100., agent::CONVERSATION_MIN))..Pixels::MAX).child(self.chat.clone()));
        if let Some(panel) = fit.panel {
            let most = (width - fit.sidebar.unwrap_or(0.) - agent::CONVERSATION_MIN).max(agent::PANEL_MIN);
            let content = self.render_tool_panel(panel, cx);
            panes = panes.child(resizable_panel().size(px(panel)).size_range(px(agent::PANEL_MIN)..px(most)).fixed().child(content));
        }
        if fit.sidebar.is_some() {
            self.sidebar_peek = false;
        }
        // Over the conversation only: a page in the panel is a window of its own, which nothing GPUI draws can cover.
        let peek = self.sidebar_peek.then(|| {
            let theme = cx.theme();
            let backdrop = theme.background.opacity(0.4);
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(
                    div()
                        .id("sidebar-backdrop")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .right(px(fit.panel.unwrap_or(0.)))
                        .bg(backdrop)
                        .occlude()
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                            this.sidebar_peek = false;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .h_full()
                        .w(px(self.layout.sidebar_width))
                        .occlude()
                        .shadow_lg()
                        .child(self.render_agent_sidebar(cx)),
                )
        });
        div().relative().size_full().child(panes).children(peek).into_any_element()
    }

    /// The chat sidebar: the list (`HistoryPanel`), then settings and the execution environment at the bottom.
    fn render_agent_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (dot, target) = self.render_target("sidebar-target", cx);
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(div().flex_1().min_h_0().child(self.history.clone()))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h(px(40.))
                    .px_2()
                    .gap_1()
                    .items_center()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .child(
                        Button::new("sidebar-settings")
                            .ghost()
                            .small()
                            .icon(IconName::Settings)
                            .label(tr(cx, "app.settings"))
                            .on_click(cx.listener(|this, _, window, cx| this.open_settings(Section::General, window, cx))),
                    )
                    .child(div().flex_1())
                    .child(h_flex().min_w_0().gap_1().items_center().text_xs().text_color(theme.muted_foreground).child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(dot)).child(target)),
            )
    }

    /// Agent mode's panel on the right, `width` wide: a header with the tool's name and close, then the tool.
    fn render_tool_panel(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(tool) = self.tool else { return div().into_any_element() };
        let (icon, label) = match tool {
            Tool::Files => (Icon::new(IconName::FolderOpen), "agent.toolFiles"),
            Tool::Terminal => (Icon::new(IconName::SquareTerminal), "agent.toolTerminal"),
            Tool::Changes => (Icon::new(gpui_kit::assets::IconName::GitCompare), "agent.toolChanges"),
        };
        let ide = self.ide.read(cx);
        let tree = tool == Tool::Files && ide.workspace.is_some() && self.tool_tree.unwrap_or(width >= 560. || ide.tabs.is_empty());
        let tree_toggle = (tool == Tool::Files).then(|| {
            Button::new("tool-tree")
                .ghost()
                .xsmall()
                .icon(IconName::PanelLeft)
                .selected(tree)
                .tooltip(tr(cx, "agent.toggleTree"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tool_tree = Some(!tree);
                    cx.notify();
                }))
        });
        let close = Button::new("tool-close")
            .ghost()
            .xsmall()
            .icon(IconName::Close)
            .tooltip(tr(cx, "agent.closePanel"))
            .on_click(cx.listener(|this, _, window, cx| this.close_tool(window, cx)));
        let theme = cx.theme().clone();
        let body: AnyElement = match tool {
            Tool::Files => h_flex()
                .size_full()
                .when(tree, |this| {
                    this.child(
                        div()
                            .w(px((width * 0.36).clamp(160., 260.)))
                            .h_full()
                            .flex_shrink_0()
                            .bg(theme.sidebar)
                            .border_r_1()
                            .border_color(theme.sidebar_border)
                            .child(self.files.clone()),
                    )
                })
                .child(div().flex_1().min_w_0().h_full().child(self.editor.clone()))
                .into_any_element(),
            Tool::Terminal => self.terminals.clone().into_any_element(),
            Tool::Changes => self.chat.read(cx).changes().into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .border_l_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .h(px(34.))
                    .flex_shrink_0()
                    .pl_3()
                    .pr_1()
                    .gap_1p5()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon.small().text_color(theme.muted_foreground))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().font_semibold().child(tr(cx, label)))
                    .children(tree_toggle)
                    .child(close),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

/// The state of a group of resizable panes, and the sizes last put into it.
struct PaneGroup {
    /// Which panes the group holds; its state is only good for those.
    key: String,
    state: Entity<ResizableState>,
    applied: Vec<(usize, f32)>,
}

/// The state of a group of panes, new when the panes changed, and given the `targets` sizes (panel index, size) when
/// they changed since the last frame: another project, or a window too small for them. A drag changes no target until
/// it ends, so it is not fought, and leaves the sizes it made. The group's element id stays the same, so what the panes
/// keep in element state, such as scroll positions, survives all this.
fn pane_group(slot: &mut Option<PaneGroup>, key: String, targets: Vec<(usize, f32)>, window: &mut Window, cx: &mut App) -> Entity<ResizableState> {
    match slot {
        Some(group) if group.key == key => {
            if group.applied != targets {
                group.state.update(cx, |state, cx| {
                    for &(ix, size) in &targets {
                        let now = state.sizes().get(ix).map(|now| f32::from(*now));
                        if state.container_size() > px(0.) && now.is_some_and(|now| (now - size).abs() >= 1.) {
                            state.resize_panel(ix, px(size), window, cx);
                        }
                    }
                });
                group.applied = targets;
            }
            group.state.clone()
        }
        _ => {
            // A new state lays the panes out at their initial sizes, which are the targets.
            let state = cx.new(|_| ResizableState::default());
            *slot = Some(PaneGroup { key, state: state.clone(), applied: targets });
            state
        }
    }
}

/// The execution environments to switch to, the current one checked; switching asks, then restarts.
fn target_menu(mut menu: PopupMenu, store: Entity<Store>, current: Option<String>) -> PopupMenu {
    let mut choices = vec![ExecutionTarget::windows()];
    choices.extend(targets::wsl_distributions());
    for choice in choices {
        let checked = current.as_deref() == Some(choice.id());
        let store = store.clone();
        menu = menu.item(PopupMenuItem::new(choice.label().to_owned()).checked(checked).on_click(move |_, window, cx| {
            if !checked {
                confirm_switch(store.clone(), choice.clone(), window, cx);
            }
        }));
    }
    menu
}

/// The workbench's own focus, for commands that run after a dialog closed.
pub struct WorkbenchFocus(pub FocusHandle);
impl Global for WorkbenchFocus {}

/// The workbench, for commands that reach it from outside its own tree (the palette).
pub struct WorkbenchHandle(pub WeakEntity<Workbench>);
impl Global for WorkbenchHandle {}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::web::begin_frame(cx);
        // An entity's async updates (`update_in`) go to the window that last drew it, and the projects load through
        // them from their first request on: every frame reads the Ide, so this window is its own before any view
        // that shows a project has been drawn (Agent mode starts without one).
        let _ = self.ide.read(cx);
        let agent_mode = self.agent();
        let phase = self.render_phase(cx);
        let (body, title_bar, status_bar, toast_width) = if agent_mode {
            let fit = self.agent_fit(window);
            let body = phase.unwrap_or_else(|| self.render_agent_body(fit, window, cx));
            (body, self.render_agent_title_bar(fit, cx).into_any_element(), None, 382.)
        } else {
            let sizes = self.pane_sizes(window, cx);
            // Toasts stack in the top right corner, over the AI pane: while it is shown they are narrowed to fit inside
            // it, so no page, which is only ever in the editor area, has to step aside for them (`web::end_frame`).
            let toast_width = if phase.is_none() && self.agent_visible { (sizes.agent - 32.).clamp(260., 382.) } else { 382. };
            let body = phase.unwrap_or_else(|| self.render_body(sizes, window, cx));
            (body, self.render_title_bar(window, cx).into_any_element(), Some(self.render_status_bar(cx)), toast_width)
        };
        if cx.theme().notification.width != px(toast_width) {
            gpui_kit::component::Theme::global_mut(cx).notification.width = px(toast_width);
        }
        if window.focused(cx).is_none() {
            let focus = self.focus.clone();
            window.defer(cx, move |window, cx| {
                if window.focused(cx).is_none() {
                    focus.focus(window, cx);
                }
            });
        }
        let theme = cx.theme();
        let settings = self.settings.clone();
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
                // Not a tab out of sight: Agent mode closes tabs while its files panel shows them.
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
            // Not through the workbench: the palette reads it to list the commands.
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
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleAgent, _, cx| {
                this.agent_visible = !this.agent_visible;
                cx.notify();
            }))
            // 3.x "focus editor": the AI pane and the bottom panel step aside.
            .on_action(cx.listener(|this, _: &FocusEditor, window, cx| {
                this.ide_mode(window, cx);
                this.agent_visible = false;
                this.bottom_visible = false;
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
                // The file chosen opens in the files panel of Agent mode.
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
            .child(div().flex_1().min_h_0().flex().overflow_hidden().child(body))
            .children(status_bar)
            .children(settings.clone())
            // Painted after everything else: pages show only where their area was painted and nothing covers them.
            .child(canvas(|_, _, _| {}, move |_, _, window, cx| crate::web::end_frame(settings.is_some(), window, cx)).absolute().size_0())
    }
}

/// The daily recheck found installed files changed or damaged. Rainy keeps running; the next start checks every file
/// again.
fn resources_damaged(error: &TrustError, window: &mut Window, cx: &mut App) {
    let message = crate::boot::trust_message(crate::ui::locale(cx), error);
    let description = trf(cx, "native.resourcesRecheck", &[("message", &message)]);
    window.open_alert_dialog(cx, move |alert, _, cx| alert.title(tr(cx, "native.resourcesChanged")).description(description.clone()).ok_text(tr(cx, "native.ok")));
}

/// The Linux runtime could not be downloaded during startup: retry, run on Windows instead, or quit (3.x
/// `downloadLinuxRuntime`). Closing the dialog quits, as 3.x's cancel button did.
fn runtime_download_failed(prompt: RuntimePrompt, window: &mut Window, cx: &mut App) {
    let reply = Rc::new(RefCell::new(Some(prompt.reply)));
    let answer = move |choice: RuntimeChoice| {
        if let Some(reply) = reply.borrow_mut().take() {
            let _ = reply.send(choice);
        }
    };
    let description = trf(cx, "native.linuxDownloadDetail", &[("error", &prompt.error)]);
    window.open_dialog(cx, move |dialog, window, cx| {
        let (retry, windows, quit, closed) = (answer.clone(), answer.clone(), answer.clone(), answer.clone());
        // A long error scrolls inside the dialog, so its buttons stay in the window.
        let room = (f32::from(window.viewport_size().height) - 260.).clamp(120., 420.);
        dialog
            .title(tr(cx, "native.linuxDownloadFailed"))
            .w(px(520.))
            .child(
                div()
                    .id("runtime-download-error")
                    .max_h(px(room))
                    .overflow_y_scroll()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(description.clone()),
            )
            .on_close(move |_, _, _| closed(RuntimeChoice::Quit))
            // A stray click beside the dialog must not stop startup; Esc and the close button still mean Quit.
            .overlay_closable(false)
            .footer(
                DialogFooter::new()
                    .child(Button::new("runtime-quit").outline().label(tr(cx, "native.quit")).on_click(move |_, window, cx| {
                        quit(RuntimeChoice::Quit);
                        window.close_dialog(cx);
                    }))
                    .child(Button::new("runtime-windows").outline().label(tr(cx, "native.useWindows")).on_click(move |_, window, cx| {
                        windows(RuntimeChoice::Windows);
                        window.close_dialog(cx);
                    }))
                    .child(Button::new("runtime-retry").primary().label(tr(cx, "native.retry")).on_click(move |_, window, cx| {
                        retry(RuntimeChoice::Retry);
                        window.close_dialog(cx);
                    })),
            )
    });
}

/// Ask, then switch the execution environment and restart.
pub(crate) fn confirm_switch(store: Entity<Store>, target: ExecutionTarget, window: &mut Window, cx: &mut App) {
    let label = target.label().to_owned();
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let (store, target) = (store.clone(), target.clone());
        alert
            .title(tr(cx, "native.switchTarget"))
            .description(trf(cx, "native.switchConfirm", &[("target", &label)]))
            .ok_text(tr(cx, "native.switch"))
            .cancel_text(tr(cx, "chat.cancel"))
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                let handle = window.window_handle();
                store.update(cx, |store, cx| store.switch_target(target.clone(), handle, cx));
                true
            })
    });
}

/// A control in the title bar. The whole bar is the window's drag area, which Windows hit-tests as the caption; the
/// control blocks that below it, and keeps its mouse-down from the bar's own drag, so a click is not taken as a move.
fn title_control(control: impl IntoElement) -> Div {
    div().occlude().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(control)
}
