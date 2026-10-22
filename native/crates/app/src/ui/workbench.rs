//! The window (`Workbench.tsx`): title bar with the project menu, the file and history pane, the editor area, the AI
//! pane, and the status bar.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::dialog::DialogFooter;
use rainy_carrier::{ExecutionTarget, targets};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, TitleBar, h_flex, h_resizable, resizable_panel, v_flex, v_resizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::{Selectable as _, StyledExt as _};
use rainy_carrier::ConnectionState;
use rainy_carrier::trust::TrustError;

use crate::store::ide::Ide;
use crate::boot::RuntimeChoice;
use crate::store::{Phase, RuntimePrompt, Store, StoreEvent};
use crate::theme;
use crate::ui::chat::ChatPane;
use crate::ui::history::HistoryPanel;
use crate::ui::ide::editor_area::{EditorArea, close_tab};
use crate::ui::ide::file_tree::FileTree;
use crate::ui::ide::git::GitView;
use crate::ui::ide::search::SearchView;
use crate::ui::ide::cursor_status::CursorStatus;
use crate::ui::ide::terminal_panel::TerminalPanel;
use crate::ui::ide::{
    CloseFile, FindInFiles, NewChat, OpenFolder, OpenSettings, QuickOpen, ReplaceInFiles, SaveFile, ShowExplorer, ShowSourceControl, ToggleTerminal,
    choose_folder, quick_open,
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
    bottom_visible: bool,
    settings: Option<Entity<SettingsView>>,
    side: Side,
    left_visible: bool,
    agent_visible: bool,
    /// Whether the projects were loaded since the Host became ready.
    ide_started: bool,
    /// Chats known to wait for an approval, so each new wait flashes the taskbar once.
    awaiting: std::collections::HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

impl Workbench {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ide = Ide::global(cx);
        let history = cx.new(|cx| HistoryPanel::new(store.clone(), cx));
        let chat = cx.new(|cx| ChatPane::new(store.clone(), window, cx));
        let files = cx.new(|cx| FileTree::new(ide.clone(), cx));
        let search = cx.new(|cx| SearchView::new(ide.clone(), window, cx));
        let git = cx.new(|cx| GitView::new(ide.clone(), window, cx));
        let terminals = cx.new(TerminalPanel::new);
        let cursor = cx.new(|cx| CursorStatus::new(ide.clone(), cx));
        let editor = cx.new(|cx| EditorArea::new(ide.clone(), cx));
        let subscriptions = vec![
            cx.observe_in(&store, window, |this, store, window, cx| {
                if !this.ide_started && matches!(store.read(cx).phase, Phase::Ready) {
                    this.ide_started = true;
                    this.ide.update(cx, |ide, cx| ide.initialize(window, cx));
                }
                let awaiting: std::collections::HashSet<String> =
                    store.read(cx).summaries.iter().filter(|summary| summary.awaiting_approval == Some(true)).map(|summary| summary.id.clone()).collect();
                if awaiting.iter().any(|id| !this.awaiting.contains(id)) {
                    crate::ui::attention::flash(window);
                }
                this.awaiting = awaiting;
                crate::ui::update::taskbar(window, &store.read(cx).updates.phase);
                cx.notify();
            }),
            cx.observe(&ide, |_, _, cx| cx.notify()),
            cx.observe(&git, |_, _, cx| cx.notify()),
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| {
                match event {
                    StoreEvent::PrefsChanged => {
                        let prefs = this.store.read(cx).prefs.clone();
                        theme::apply(&prefs, Some(window), cx);
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
        Self {
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
            bottom_visible: false,
            settings: None,
            side: Side::Files,
            left_visible: true,
            agent_visible: true,
            ide_started: false,
            awaiting: std::collections::HashSet::new(),
            _subscriptions: subscriptions,
        }
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

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ready = matches!(self.store.read(cx).phase, Phase::Ready);
        let ide = self.ide.read(cx);
        let title: SharedString =
            ide.workspace.as_ref().map(|workspace| SharedString::from(workspace.title.clone())).unwrap_or_else(|| tr(cx, "ide.ideOpenFolder"));
        let path = ide.workspace.as_ref().map(|workspace| workspace.path.clone()).unwrap_or_default();
        let current = ide.workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        let has_project = current.is_some();
        let workspaces = ide.workspaces.clone();
        let handle = self.ide.clone();
        let (left, agent, bottom) = (self.left_visible, self.agent_visible, self.bottom_visible);
        let tooltip: SharedString = if path.is_empty() { tr(cx, "native.projects") } else { path.into() };
        TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_2()
                .gap_2()
                .items_center()
                .child(img("brand/rainy-mark.svg").size(px(18.)))
                .child(div().text_sm().font_semibold().child("Rainy"))
                .child(div().flex_1())
                .when(ready, |this| {
                    this.child(title_control(
                        Button::new("projects")
                            .ghost()
                            .small()
                            .icon(gpui_kit::component::IconName::Folder)
                            .label(title)
                            .tooltip(tooltip)
                            .dropdown_menu(move |mut menu, _, cx| {
                                for workspace in workspaces.iter().rev() {
                                    let checked = current.as_deref() == Some(workspace.workspace_id.as_str());
                                    let (handle, workspace) = (handle.clone(), workspace.clone());
                                    menu = menu.item(PopupMenuItem::new(workspace.title.clone()).checked(checked).on_click(move |_, window, cx| {
                                        let workspace = workspace.clone();
                                        handle.update(cx, |ide, cx| ide.select(workspace, window, cx));
                                    }));
                                }
                                if !workspaces.is_empty() {
                                    menu = menu.separator();
                                }
                                menu = menu.item(PopupMenuItem::new(tr(cx, "ide.ideOpenFolder")).on_click(|_, window, cx| choose_folder(window, cx)));
                                if let Some(current) = current.clone() {
                                    let handle = handle.clone();
                                    let title = workspaces.iter().find(|w| w.workspace_id == current).map(|w| w.title.clone()).unwrap_or_default();
                                    menu = menu.item(PopupMenuItem::new(tr(cx, "ide.ideRemoveWorkspace")).on_click(move |_, window, cx| {
                                        confirm_remove(handle.clone(), current.clone(), title.clone(), window, cx);
                                    }));
                                }
                                menu
                            }),
                    ))
                    .when(has_project, |this| {
                        this.child(title_control(
                            Button::new("quick-open")
                                .ghost()
                                .small()
                                .icon(gpui_kit::component::IconName::Search)
                                .label(tr(cx, "ide.ideSearchFiles"))
                                .tooltip(tr(cx, "ide.ideQuickOpen"))
                                .on_click(|_, window, cx| quick_open::open(window, cx)),
                        ))
                    })
                })
                .child(div().flex_1())
                .child(title_control(
                    Button::new("toggle-files")
                        .ghost()
                        .small()
                        .icon(gpui_kit::component::IconName::PanelLeft)
                        .selected(left)
                        .tooltip(tr(cx, "ide.ideToggleFiles"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.left_visible = !this.left_visible;
                            cx.notify();
                        })),
                ))
                .child(title_control(
                    Button::new("toggle-bottom")
                        .ghost()
                        .small()
                        .icon(gpui_kit::component::IconName::PanelBottom)
                        .selected(bottom)
                        .tooltip(tr(cx, "ide.ideToggleBottom"))
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_terminal(window, cx))),
                ))
                .child(title_control(
                    Button::new("toggle-agent")
                        .ghost()
                        .small()
                        .icon(gpui_kit::component::IconName::PanelRight)
                        .selected(agent)
                        .tooltip(tr(cx, "ide.ideToggleAgent"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.agent_visible = !this.agent_visible;
                            cx.notify();
                        })),
                ))
                .child(title_control(
                    Button::new("open-settings")
                        .ghost()
                        .small()
                        .icon(gpui_kit::component::IconName::Settings)
                        .tooltip(tr(cx, "app.settings"))
                        .on_click(cx.listener(|this, _, window, cx| this.open_settings(Section::General, window, cx))),
                )),
        )
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
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
        let version = env!("CARGO_PKG_VERSION");
        let connection = store.connection;
        let theme = cx.theme();
        let dot = match connection {
            ConnectionState::Open => theme.success,
            ConnectionState::Connecting => theme.warning,
            ConnectionState::Closed => theme.danger,
        };
        h_flex()
            .h(px(24.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(theme.muted_foreground)
            .bg(theme.status_bar)
            .border_t_1()
            .border_color(theme.status_bar_border)
            .child(div().size(px(7.)).rounded_full().bg(dot))
            .child(
                Button::new("target")
                    .ghost()
                    .xsmall()
                    .label(target)
                    .tooltip(tr(cx, "native.switchTarget"))
                    .dropdown_menu(move |mut menu, _, _| {
                        let mut choices = vec![ExecutionTarget::windows()];
                        choices.extend(targets::wsl_distributions());
                        for choice in choices {
                            let checked = current.as_deref() == Some(choice.id());
                            let handle = handle.clone();
                            menu = menu.item(PopupMenuItem::new(choice.label().to_owned()).checked(checked).on_click(move |_, window, cx| {
                                if !checked {
                                    confirm_switch(handle.clone(), choice.clone(), window, cx);
                                }
                            }));
                        }
                        menu
                    }),
            )
            .child(div().flex_1())
            .child(self.cursor.clone())
            .child(format!("Rainy {version}"))
    }

    fn render_phase(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let phase = self.store.read(cx).phase.clone();
        if matches!(phase, Phase::Ready) {
            return None;
        }
        // A failed WSL or SSH start can continue on Windows, as 3.x offered.
        let on_wsl = targets::read_preferences(&self.store.read(cx).paths.desktop_settings())
            .ok()
            .and_then(|preferences| targets::saved_target(&preferences).ok())
            .is_some_and(|target| matches!(target, ExecutionTarget::Wsl { .. } | ExecutionTarget::Ssh { .. }));
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
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .p_8()
                    .child(img("brand/rainy-mark.svg").size(px(56.)))
                    .child(div().text_sm().text_color(theme.danger).max_w(px(640.)).child(error))
                    .child(
                        h_flex()
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
        Button::new(id).ghost().xsmall().icon(icon).label(label).selected(self.side == value).on_click(cx.listener(move |this, _, window, cx| {
            this.show_side(value, window, cx);
        }))
    }

    /// Show or hide the terminals; showing them focuses the current one.
    fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let files = self.side_tab("side-files", gpui_kit::component::Icon::new(gpui_kit::component::IconName::Folder), tr(cx, "ide.ideExplorer"), Side::Files, cx);
        let search = self.side_tab(
            "side-search",
            gpui_kit::component::Icon::new(gpui_kit::component::IconName::Search),
            tr(cx, "search.searchTitle"),
            Side::Search,
            cx,
        );
        let changes = self.git.read(cx).change_count();
        let git_label: SharedString = if changes > 0 { format!("Git {}", changes.min(999)).into() } else { tr(cx, "git.gitTab") };
        let git = self.side_tab("side-git", gpui_kit::component::Icon::new(gpui_kit::assets::IconName::GitBranch), git_label, Side::Git, cx);
        let history = self.side_tab("side-history", gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Clock), tr(cx, "ide.ideHistory"), Side::History, cx);
        let content: AnyView = match self.side {
            Side::Files => self.files.clone().into(),
            Side::Search => self.search.clone().into(),
            Side::Git => self.git.clone().into(),
            Side::History => self.history.clone().into(),
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
                    .child(history),
            )
            .child(div().flex_1().min_h_0().child(content))
    }

    fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (left, agent) = (self.left_visible, self.agent_visible);
        // The AI pane takes 28% of the window, between 340 and 600 px, as in 3.x.
        let agent_width = (f32::from(window.viewport_size().width) * 0.28).clamp(340., 600.);
        let mut panes = h_resizable(SharedString::from(format!("workbench-{left}-{agent}")));
        if left {
            panes = panes.child(resizable_panel().size(px(260.)).size_range(px(180.)..px(360.)).child(self.render_side(cx)));
        }
        let center = if self.bottom_visible && self.ide.read(cx).workspace.is_some() {
            v_resizable("center")
                .child(resizable_panel().child(self.editor.clone()))
                .child(resizable_panel().size(px(280.)).size_range(px(120.)..px(900.)).child(self.terminals.clone()))
                .into_any_element()
        } else {
            self.editor.clone().into_any_element()
        };
        panes = panes.child(resizable_panel().child(center));
        if agent {
            panes = panes.child(resizable_panel().size(px(agent_width)).size_range(px(300.)..px(900.)).child(self.chat.clone()));
        }
        panes.into_any_element()
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::web::begin_frame(cx);
        let body = self.render_phase(cx).unwrap_or_else(|| self.render_body(window, cx));
        let title_bar = self.render_title_bar(cx);
        let status_bar = self.render_status_bar(cx);
        let theme = cx.theme();
        let settings = self.settings.clone();
        v_flex()
            .relative()
            .size_full()
            .key_context("Workbench")
            .on_action(cx.listener(|this, _: &SaveFile, window, cx| {
                this.ide.update(cx, |ide, cx| ide.save(None, false, window, cx));
            }))
            .on_action(cx.listener(|this, _: &CloseFile, window, cx| {
                if let Some(key) = this.ide.read(cx).active.clone() {
                    close_tab(this.ide.clone(), key, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ReplaceInFiles, window, cx| {
                this.show_side(Side::Search, window, cx);
                this.search.update(cx, |search, cx| search.show_replace(cx));
                let query = this.search.read(cx).query();
                query.update(cx, |state, cx| state.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &ShowExplorer, window, cx| this.show_side(Side::Files, window, cx)))
            .on_action(cx.listener(|this, _: &ShowSourceControl, window, cx| this.show_side(Side::Git, window, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(Section::General, window, cx)))
            .on_action(cx.listener(|this, _: &NewChat, window, cx| {
                this.agent_visible = true;
                this.store.update(cx, |store, cx| store.select(None, cx));
                cx.notify();
                let _ = window;
            }))
            .on_action(cx.listener(|this, _: &FindInFiles, window, cx| {
                this.show_side(Side::Search, window, cx);
                let query = this.search.read(cx).query();
                query.update(cx, |state, cx| state.focus(window, cx));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| this.toggle_terminal(window, cx)))
            .on_action(cx.listener(|_, _: &QuickOpen, window, cx| quick_open::open(window, cx)))
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
            .child(div().flex_1().min_h_0().flex().child(body))
            .child(status_bar)
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
    window.open_dialog(cx, move |dialog, _, cx| {
        let (retry, windows, quit, closed) = (answer.clone(), answer.clone(), answer.clone(), answer.clone());
        dialog
            .title(tr(cx, "native.linuxDownloadFailed"))
            .w(px(520.))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(description.clone()))
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

/// Confirm, then remove a project from the list; its files stay on disk.
fn confirm_remove(ide: Entity<Ide>, workspace_id: String, title: String, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let (ide, workspace_id) = (ide.clone(), workspace_id.clone());
        alert
            .title(tr(cx, "ide.ideRemoveWorkspace"))
            .description(trf(cx, "ide.ideRemoveWorkspaceDescription", &[("title", &title)]))
            .ok_text(tr(cx, "ide.ideConfirm"))
            .cancel_text(tr(cx, "ide.ideCancel"))
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                let id = workspace_id.clone();
                ide.update(cx, |ide, cx| ide.remove_workspace(id, window, cx));
                true
            })
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
fn title_control(control: impl IntoElement) -> impl IntoElement {
    div().occlude().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(control)
}
