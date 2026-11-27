//! IDE and Agent mode title bars, target switcher button, and SSH reconnect/swap status banner.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, Size, StyledExt as _, TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::{ConnectionState, ExecutionTarget};

use crate::store::{Phase, SshAutoReconnect};
use crate::ui::agent::{self, Fit, IdeMode, Mode, ToggleSidebar, Tool};
use crate::ui::history::ChatActions;
use crate::ui::ide::{NewChat, OpenSettings, ShowMap, quick_open};
use crate::ui::settings::Section;
use crate::ui::{motion, tr, trf};

use super::Workbench;
use super::dialogs::target_menu;

/// GPUI Kit's title bar starts its children this far from the window's left edge.
const TITLE_BAR_INSET: f32 = 12.;

/// A control in the title bar. The whole bar is the window's drag area, which Windows hit-tests as the caption; the
/// control blocks that below it, and keeps its mouse-down from the bar's own drag, so a click is not taken as a move.
pub(super) fn title_control(control: impl IntoElement) -> Div {
    div().occlude().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(control)
}

impl Workbench {
    pub(super) fn render_title_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let narrow = window.viewport_size().width < px(1100.);
        let ready = matches!(self.store.read(cx).phase, Phase::Ready);
        let ide = self.ide.read(cx);
        let title: SharedString =
            ide.workspace.as_ref().map(|workspace| SharedString::from(workspace.title.clone())).unwrap_or_else(|| tr(cx, "ide.ideOpenFolder"));
        let path = ide.workspace.as_ref().map(|workspace| workspace.path.clone()).unwrap_or_default();
        let has_project = ide.workspace.is_some();
        let (left, agent, bottom) = (self.left_visible, self.agent_visible, self.bottom_visible);
        let tooltip: SharedString = if path.is_empty() { tr(cx, "native.projects") } else { path.into() };
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
                .on_click(cx.listener(|this, _, window, cx| {
                    this.left_visible = !this.left_visible;
                    this.sync_panel_visibility(Some(window), cx);
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
    pub(super) fn render_agent_title_bar(&self, fit: Fit, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let store = self.store.read(cx);
        let summary = store.selected.as_ref().and_then(|id| store.summaries.iter().find(|summary| summary.id == *id)).cloned();
        let summary_swarm = summary.as_ref().is_some_and(|summary| summary.swarm.is_some() || store.has_team(&summary.id));
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
        let swarm = summary_swarm.then(|| tool("tool-swarm", Icon::new(gpui_kit::assets::IconName::Users), "agent.toolSwarm", Tool::Swarm));
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
                        .children(swarm.map(title_control))
                        .child(title_control(files))
                        .child(title_control(terminal))
                        .child(title_control(changes))
                        .child(title_control(more)),
                ),
        )
    }

    /// The execution environment, as a label whose menu switches it, and the connection's color.
    pub(super) fn render_target(&self, id: &'static str, cx: &mut Context<Self>) -> (Hsla, impl IntoElement + use<>) {
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
            .dropdown_menu(move |menu, _, cx| target_menu(menu, handle.clone(), current.clone(), cx));
        (dot, button)
    }

    /// Low-key non-blocking top banner shown while an in-place target swap or silent SSH backoff reconnect is active.
    pub(super) fn render_ssh_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let store = self.store.read(cx);
        let target_label = store
            .target
            .as_ref()
            .map(|t| t.label().to_owned())
            .unwrap_or_default();
        let theme = cx.theme().clone();

        if let Some(swapping) = store.swapping_target.clone() {
            return Some(
                h_flex()
                    .h(px(28.))
                    .flex_shrink_0()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .bg(theme.secondary)
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.foreground)
                    .child(motion::spinner_with_id("ssh-swap-spin", Size::XSmall, None, cx))
                    .child(div().flex_1().min_w_0().truncate().child(trf(cx, "ssh.swapping", &[("target", &swapping)])))
                    .into_any_element(),
            );
        }

        match &store.ssh_reconnect {
            SshAutoReconnect::Idle => None,
            SshAutoReconnect::Waiting { attempt, max, retry_at, .. } => {
                let secs = retry_at.saturating_duration_since(std::time::Instant::now()).as_secs().max(1);
                let text = trf(
                    cx,
                    "ssh.reconnectingWait",
                    &[("target", &target_label), ("seconds", &secs), ("attempt", attempt), ("max", max)],
                );
                Some(self.ssh_reconnect_strip(text, false, true, theme.warning, cx))
            }
            SshAutoReconnect::Attempting { attempt, max } => {
                let text = trf(
                    cx,
                    "ssh.reconnectingBanner",
                    &[("target", &target_label), ("attempt", attempt), ("max", max)],
                );
                Some(self.ssh_reconnect_strip(text, true, false, theme.warning, cx))
            }
            SshAutoReconnect::Exhausted { .. } => {
                let text = trf(cx, "ssh.reconnectFailedBanner", &[("target", &target_label)]);
                Some(self.ssh_reconnect_strip(text, false, true, theme.danger, cx))
            }
        }
    }

    fn ssh_reconnect_strip(
        &self,
        text: SharedString,
        spinning: bool,
        show_retry: bool,
        accent: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        h_flex()
            .h(px(28.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .items_center()
            .bg(accent.opacity(0.10))
            .border_b_1()
            .border_color(accent.opacity(0.35))
            .text_xs()
            .child(if spinning {
                motion::spinner_with_id("ssh-reconnect-spin", Size::XSmall, None, cx)
            } else {
                div().size(px(7.)).flex_shrink_0().rounded_full().bg(accent).into_any_element()
            })
            .child(div().flex_1().min_w_0().truncate().text_color(theme.foreground).child(text))
            .when(show_retry, |this| {
                this.child(
                    Button::new("banner-ssh-retry")
                        .outline()
                        .xsmall()
                        .label(tr(cx, "ssh.retryNow"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            let handle = window.window_handle();
                            this.store.update(cx, |store, cx| store.retry_ssh_now(handle, cx));
                        })),
                )
            })
            .child(
                Button::new("banner-ssh-local")
                    .ghost()
                    .xsmall()
                    .label(tr(cx, "ssh.switchLocal"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        let handle = window.window_handle();
                        this.store.update(cx, |store, cx| {
                            store.swap_target_in_place(ExecutionTarget::windows(), handle, cx);
                        });
                    })),
            )
            .into_any_element()
    }
}
