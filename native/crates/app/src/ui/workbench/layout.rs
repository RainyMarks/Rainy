//! IDE and Agent mode pane layouts, resizable groups, responsive left sidebar tabs with spring indicator,
//! and Agent mode's `Presence`-animated narrow-window sidebar drawer.

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::base::GlobalState;
use gpui_kit::base::motion::{Presence, Spring, Transition, spring};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, ResizableState, Selectable as _, Sizable as _, StyledExt as _, h_flex, h_resizable, resizable_panel, v_flex, v_resizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::store::ide::PaneLayout;
use crate::ui::agent::{self, Fit, Tool};
use crate::ui::settings::Section;
use crate::ui::tr;

use super::Workbench;

/// What the left pane shows.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Side {
    Files,
    Search,
    Git,
    History,
    Outline,
    Relations,
    /// The board and members of the selected chat's swarm.
    Swarm,
}

/// Below this width, in logical pixels, the left pane's tabs show only their icons.
const SIDE_TAB_NAMES_WIDTH: f32 = 360.;

/// Measured horizontal geometry of the IDE sidebar tab bar and each tab button,
/// used to drive the bottom 2px spring accent bar without extra re-layouts.
#[derive(Default)]
pub(super) struct SideTabBounds {
    pub bar_left: f32,
    pub tabs: HashMap<Side, (f32, f32)>,
}

/// Pane sizes in logical pixels: the file pane's width, the AI pane's (0 follows the window) and the bottom panel's height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Sizes {
    pub sidebar: f32,
    pub agent: f32,
    pub bottom: f32,
}

/// What a new project starts with, as the Host stores it.
pub(super) const DEFAULT_SIZES: Sizes = Sizes { sidebar: 240., agent: 0., bottom: 220. };
/// The narrowest the editor between the panes gets while the window allows, and the narrowest AI pane.
pub(super) const EDITOR_MIN: f32 = 300.;
pub(super) const AGENT_MIN: f32 = 300.;

impl Sizes {
    /// The sizes to lay out in a window of `width` × `height`: the file pane 180–360 px, the AI pane
    /// following the window (28%, 340–600 px) until it was resized and never under 300 px, the bottom panel from 120
    /// px to the window height less 230, and the editor not under 300 px while the window allows.
    pub fn clamped(self, width: f32, height: f32, left: bool, agent: bool) -> Sizes {
        let sidebar = self.sidebar.clamp(180., 360.).min(sidebar_max(width, agent)).round();
        let left_width = if left { sidebar } else { 0. };
        let preferred = if self.agent > 0. && self.agent != 400. { self.agent } else { (width * 0.28).clamp(340., 600.) };
        let agent = preferred.min(width - left_width - EDITOR_MIN).max(AGENT_MIN).round();
        Sizes { sidebar, agent, bottom: self.bottom.min(height - 230.).max(120.).round() }
    }
}

/// The widest file pane that leaves the editor and the AI pane their least.
pub(super) fn sidebar_max(width: f32, agent: bool) -> f32 {
    (width - EDITOR_MIN - if agent { AGENT_MIN } else { 0. }).clamp(180., 360.)
}

/// The state of a group of resizable panes, and the sizes last put into it.
pub(super) struct PaneGroup {
    key: String,
    state: Entity<ResizableState>,
    applied: Vec<(usize, f32)>,
}

/// The state of a group of panes, new when the panes changed, and given the `targets` sizes (panel index, size) when
/// they changed since the last frame.
pub(super) fn pane_group(slot: &mut Option<PaneGroup>, key: String, targets: Vec<(usize, f32)>, window: &mut Window, cx: &mut App) -> Entity<ResizableState> {
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
            let state = cx.new(|_| ResizableState::default());
            *slot = Some(PaneGroup { key, state: state.clone(), applied: targets });
            state
        }
    }
}

impl Workbench {
    /// A tab of the left pane wrapped with a zero-overhead bounds tracker for the spring bottom indicator.
    fn side_tab(
        &self,
        id: &'static str,
        icon: Icon,
        name: SharedString,
        shown: Option<SharedString>,
        value: Side,
        cx: &mut Context<Self>,
    ) -> Div {
        let selected = self.side == value;
        let named = shown.as_ref() == Some(&name);
        let button = Button::new(id)
            .ghost()
            .when(selected, |this| this.custom(crate::theme::selected_button(cx)))
            .xsmall()
            .icon(icon)
            .when_some(shown, |this, shown| this.label(shown))
            .when(!named, |this| this.tooltip(name))
            .selected(selected)
            .on_click(cx.listener(move |this, _, window, cx| this.show_side(value, window, cx)));
        let tracker = self.side_tab_bounds.clone();
        div()
            .relative()
            .flex_shrink_0()
            .child(button)
            .child(
                canvas(
                    move |bounds, _, _| {
                        tracker.borrow_mut().tabs.insert(value, (f32::from(bounds.origin.x), f32::from(bounds.size.width)));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
    }

    /// `width`: the pane's, in logical pixels; compact layout keeps all 7 sidebar icons inside 180px without overflow.
    fn render_side(&self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let compact = width < SIDE_TAB_NAMES_WIDTH;
        let very_narrow = width < 220.;
        let named = |name: SharedString| (!compact).then(|| name.clone());
        let explorer = tr(cx, "ide.ideExplorer");
        let files = self.side_tab("side-files", Icon::new(IconName::Folder), explorer.clone(), named(explorer), Side::Files, cx);
        let search_name = tr(cx, "search.searchTitle");
        let search = self.side_tab("side-search", Icon::new(IconName::Search), search_name.clone(), named(search_name), Side::Search, cx);
        let changes = self.git.read(cx).change_count();
        let git_name: SharedString = if changes > 0 { format!("Git {}", changes.min(999)).into() } else { tr(cx, "git.gitTab") };
        let git_shown = if very_narrow {
            None
        } else if compact {
            (changes > 0).then(|| SharedString::from(changes.min(999).to_string()))
        } else {
            Some(git_name.clone())
        };
        let git = self.side_tab("side-git", Icon::new(gpui_kit::assets::IconName::GitBranch), git_name, git_shown, Side::Git, cx);
        let history_name = tr(cx, "ide.ideHistory");
        let history = self.side_tab("side-history", Icon::new(gpui_kit::assets::IconName::Clock), history_name.clone(), named(history_name), Side::History, cx);
        let outline = self.side_tab("side-outline", Icon::new(gpui_kit::assets::IconName::ListTree), tr(cx, "ide.ideOutline"), None, Side::Outline, cx);
        let relations = self.side_tab("side-relations", Icon::new(gpui_kit::assets::IconName::Network), tr(cx, "ide.ideRelations"), None, Side::Relations, cx);
        let swarm = self
            .selected_swarm
            .as_ref()
            .is_some_and(|(_, coordinator)| coordinator.is_some())
            .then(|| self.side_tab("side-swarm", Icon::new(gpui_kit::assets::IconName::Users), tr(cx, "agent.toolSwarm"), None, Side::Swarm, cx));

        let indicator = {
            let bounds = self.side_tab_bounds.borrow();
            bounds.tabs.get(&self.side).copied().filter(|(_, w)| *w > 0.).map(|(tab_x, tab_w)| {
                let target_x = (tab_x - bounds.bar_left).max(0.);
                let policy = Spring::new(Duration::from_millis(200)).with_epsilon(0.5);
                let anim_x: f32 = spring(("ide-side-tab", "x"), target_x, policy, window, cx);
                let anim_w: f32 = spring(("ide-side-tab", "w"), tab_w, policy, window, cx);
                (anim_x, anim_w.max(0.))
            })
        };

        let content: AnyView = match self.side {
            Side::Swarm => self.swarm.clone().into(),
            Side::Files => self.files.clone().into(),
            Side::Search => self.search.clone().into(),
            Side::Git => self.git.clone().into(),
            Side::History => self.history.clone().into(),
            Side::Outline => self.outline.clone().into(),
            Side::Relations => self.relations.clone().into(),
        };
        let bar_tracker = self.side_tab_bounds.clone();
        let theme = cx.theme();
        v_flex()
            .size_full()
            .min_w_0()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                div()
                    .relative()
                    .h(px(34.))
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .overflow_hidden()
                    .border_b_1()
                    .border_color(theme.sidebar_border)
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                bar_tracker.borrow_mut().bar_left = f32::from(bounds.origin.x);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .child(
                        h_flex()
                            .size_full()
                            .min_w_0()
                            .overflow_hidden()
                            .px(if very_narrow { px(2.) } else { px(6.) })
                            .gap(if very_narrow { px(0.) } else { px(2.) })
                            .items_center()
                            .child(files)
                            .child(search)
                            .child(git)
                            .child(history)
                            .when(!very_narrow, |this| this.child(div().flex_1().min_w_0()))
                            .children(swarm)
                            .child(relations)
                            .child(outline),
                    )
                    .when_some(indicator, |this, (x, w)| {
                        this.child(div().absolute().bottom_0().left(px(x)).w(px(w)).h(px(2.)).rounded_t(px(1.)).bg(theme.primary))
                    }),
            )
            .child(div().flex_1().min_h_0().child(content))
    }

    /// The pane sizes to lay out: the shown project's, else those of this window, fitted to the window.
    pub(super) fn pane_sizes(&self, window: &Window, cx: &App) -> Sizes {
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

    pub(super) fn render_body(&mut self, sizes: Sizes, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
                resizable_panel()
                    .size(px(sizes.sidebar))
                    .size_range(px(180.)..px(sidebar_max(width, agent)))
                    .fixed()
                    .child(self.render_side(sizes.sidebar, window, cx)),
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

    /// Agent mode's columns: the sidebar, the conversation and the panel, each resizable; the sidebar slides smoothly
    /// over the conversation in a narrow window via `Presence` (zero frames when settled, respects `reduce_motion`).
    pub(super) fn render_agent_body(&mut self, fit: Fit, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let width = f32::from(window.viewport_size().width);
        let this = cx.entity().downgrade();
        let dragged = |now: Option<&Pixels>, before: f32| now.map(|size| f32::from(*size).round()).filter(|size| (size - before).abs() >= 1.);
        let left = self.layout.panel_left;
        let panel_index = if left { usize::from(fit.sidebar.is_some()) } else { usize::from(fit.sidebar.is_some()) + 1 };
        let mut targets = Vec::new();
        if let Some(sidebar) = fit.sidebar {
            targets.push((0, sidebar));
        }
        if let Some(panel) = fit.panel {
            targets.push((panel_index, panel));
        }
        let state = pane_group(&mut self.agent_panes, format!("agent-{}-{}-{left}", fit.sidebar.is_some(), fit.panel.is_some()), targets, window, cx);
        let mut panes = h_resizable("agent").with_state(&state).on_resize(move |state, _, cx| {
            let now = state.read(cx).sizes().clone();
            let sidebar = fit.sidebar.and_then(|before| dragged(now.first(), before));
            let panel = fit.panel.and_then(|before| dragged(now.get(panel_index), before));
            if sidebar.is_some() || panel.is_some() {
                let _ = this.update(cx, |this, cx| this.agent_resized(sidebar, panel, cx));
            }
        });
        if let Some(sidebar) = fit.sidebar {
            panes = panes.child(
                resizable_panel().size(px(sidebar)).size_range(px(agent::SIDEBAR_MIN)..px(agent::SIDEBAR_MAX)).fixed().child(self.render_agent_sidebar(cx)),
            );
        }
        let panel = fit.panel.map(|panel| {
            let most = (width - fit.sidebar.unwrap_or(0.) - agent::CONVERSATION_MIN).max(agent::PANEL_MIN);
            let content = self.render_tool_panel(panel, cx);
            resizable_panel().size(px(panel)).size_range(px(agent::PANEL_MIN)..px(most)).fixed().child(content)
        });
        let rest = width - fit.sidebar.unwrap_or(0.) - fit.panel.unwrap_or(0.);
        let conversation = resizable_panel().size_range(px(rest.clamp(100., agent::CONVERSATION_MIN))..Pixels::MAX).child(self.chat.clone());
        if left {
            if let Some(panel) = panel {
                panes = panes.child(panel);
            }
            panes = panes.child(conversation);
        } else {
            panes = panes.child(conversation);
            if let Some(panel) = panel {
                panes = panes.child(panel);
            }
        }
        if fit.sidebar.is_some() {
            self.sidebar_peek = false;
        }
        let (cover_left, cover_right) = match fit.panel {
            Some(panel) if left => (panel, 0.),
            Some(panel) => (0., panel),
            None => (0., 0.),
        };
        let peek_presence = Presence::new("agent-sidebar-peek", self.sidebar_peek)
            .transition(Transition::new(Duration::from_millis(180)))
            .sample(window, cx);
        let peek = peek_presence.should_render().then(|| {
            let t = peek_presence.progress.clamp(0.0, 1.0);
            let drawer_w = self.layout.sidebar_width;
            let slide_offset = -(1.0 - t) * drawer_w;
            let theme = cx.theme();
            let backdrop = theme.background.opacity(0.4 * t);
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(cover_left))
                .right(px(cover_right))
                .overflow_hidden()
                .child(
                    div()
                        .id("sidebar-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(backdrop)
                        .occlude()
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                            // Suppress window-level text selection so dismissing the drawer never selects chat messages underneath.
                            GlobalState::suppress_text_selection(cx);
                            cx.stop_propagation();
                            this.sidebar_peek = false;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left(px(slide_offset))
                        .h_full()
                        .w(px(drawer_w))
                        .occlude()
                        .when(t > 0.5, |this| this.shadow_lg())
                        .child(self.render_agent_sidebar(cx)),
                )
        });
        div().relative().size_full().child(panes).children(peek).into_any_element()
    }

    /// The chat sidebar: the list (`HistoryPanel`), then settings and the execution environment at the bottom.
    fn render_agent_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (dot, target) = self.render_target("sidebar-target", cx);
        let is_ssh = self.store.read(cx).is_ssh();
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
                    .when(is_ssh, |this| this.child(self.tunnels.clone()))
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1()
                            .items_center()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(dot))
                            .child(target),
                    ),
            )
    }

    /// Agent mode's panel beside the conversation, `width` wide.
    fn render_tool_panel(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(tool) = self.tool else { return div().into_any_element() };
        let (icon, label) = match tool {
            Tool::Files => (Icon::new(IconName::FolderOpen), "agent.toolFiles"),
            Tool::Terminal => (Icon::new(IconName::SquareTerminal), "agent.toolTerminal"),
            Tool::Changes => (Icon::new(gpui_kit::assets::IconName::GitCompare), "agent.toolChanges"),
            Tool::Swarm => (Icon::new(gpui_kit::assets::IconName::Users), "agent.toolSwarm"),
        };
        let left = self.layout.panel_left;
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
        let side = Button::new("tool-side")
            .ghost()
            .xsmall()
            .icon(if left { IconName::PanelRight } else { IconName::PanelLeft })
            .tooltip(tr(cx, if left { "agent.panelToRight" } else { "agent.panelToLeft" }))
            .on_click(cx.listener(|this, _, _, cx| this.swap_panel(cx)));
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
            Tool::Swarm => self.swarm.clone().into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .when(left, |this| this.border_r_1())
            .when(!left, |this| this.border_l_1())
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
                    .child(side)
                    .child(close),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}
