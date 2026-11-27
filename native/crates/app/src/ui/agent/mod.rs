//! Agent mode: the conversation is the window, as in the Claude and ChatGPT desktop apps. The chats are in a sidebar
//! on the left, and the project's files, its terminals and the chat's changes open in a panel on the right on demand.
//! IDE mode is the workbench of 4.2. The window switches between the two at any time; both show the same entities, so
//! the chat, its draft, running work, open files and terminals carry over.

use std::time::Duration;

use gpui_kit::base::ElementExt as _;
use gpui_kit::base::motion::{Spring, Transition, spring, transition};
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Icon, Selectable as _, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::store::ide::Ide;
use crate::ui::workbench::WorkbenchHandle;
use crate::ui::{tr, trf};

pub mod swarm;

actions!(rainy, [AgentMode, IdeMode, ToggleMode, ToggleSidebar]);

/// The window's layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// The conversation in the middle, the chats on the left, tools on the right on demand.
    #[default]
    Agent,
    /// The 4.2 workbench: files on the left, the editor in the middle, the AI pane on the right.
    Ide,
}

impl Mode {
    /// How `layout.json` names it.
    pub fn id(self) -> &'static str {
        match self {
            Mode::Agent => "agent",
            Mode::Ide => "ide",
        }
    }

    pub fn from_id(id: &str) -> Option<Mode> {
        match id {
            "agent" => Some(Mode::Agent),
            "ide" => Some(Mode::Ide),
            _ => None,
        }
    }
}

/// What the panel of Agent mode shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// The file tree and the editor.
    Files,
    /// The terminals, problems and the debugger.
    Terminal,
    /// The files the chat changed.
    Changes,
    /// The board and members of the chat's swarm.
    Swarm,
}

/// The widest the conversation's text gets, in logical pixels; a wider window centres it.
pub const COLUMN: f32 = 760.;
/// The sidebar's width until it is dragged, and the range it can be dragged in.
pub const SIDEBAR: f32 = 240.;
pub const SIDEBAR_MIN: f32 = 200.;
pub const SIDEBAR_MAX: f32 = 360.;
/// The narrowest the conversation gets while the window has room: the sidebar and the panel give way first.
pub const CONVERSATION_MIN: f32 = 420.;
/// The narrowest panel on the right, and the width it keeps before the sidebar may stay beside it.
pub const PANEL_MIN: f32 = 320.;
const PANEL_ROOMY: f32 = 480.;

/// What the Agent layout shows in a window: the docked sidebar's width and the panel's, when they are shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub sidebar: Option<f32>,
    pub panel: Option<f32>,
}

/// Lay out a window `width` wide: `sidebar` is the width of the sidebar the user keeps open, `panel` the width of the
/// open panel (0 follows the window: 42% of it, 360 to 760 px). The conversation keeps `CONVERSATION_MIN` while the
/// window has room for it. The panel is what the user is looking at, so the sidebar steps aside unless the panel
/// keeps its width (up to `PANEL_ROOMY`) beside it; then the panel narrows, to its least. An open panel stays open
/// even in a window too narrow for both, since the user asked for it.
pub fn fit(width: f32, sidebar: Option<f32>, panel: Option<f32>) -> Fit {
    let sidebar = sidebar.map(|sidebar| sidebar.clamp(SIDEBAR_MIN, SIDEBAR_MAX).round());
    let Some(panel) = panel else {
        return Fit { sidebar: sidebar.filter(|sidebar| width - sidebar >= CONVERSATION_MIN), panel: None };
    };
    let preferred = if panel > 0. { panel } else { (width * 0.42).clamp(360., 760.) };
    let sidebar = sidebar.filter(|sidebar| width - sidebar - CONVERSATION_MIN >= preferred.min(PANEL_ROOMY));
    let room = width - sidebar.unwrap_or(0.) - CONVERSATION_MIN;
    Fit { sidebar, panel: Some(preferred.min(room).max(PANEL_MIN).round()) }
}

#[derive(Default, Clone, Copy)]
struct ModeSwitchBounds {
    container: Option<Bounds<Pixels>>,
    buttons: [Option<Bounds<Pixels>>; 2],
}

#[derive(IntoElement)]
struct ModeSwitch {
    mode: Mode,
}

impl RenderOnce for ModeSwitch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (radius, muted, pill_bg, pill_fg) = {
            let theme = cx.theme();
            let dark = theme.mode.is_dark();
            let fg = if dark { theme.link } else { theme.primary_active };
            let bg = theme.primary.opacity(if dark { 0.12 * 1.8 } else { 0.12 });
            (theme.radius, theme.muted, bg, fg)
        };
        let bounds_state = window.use_keyed_state("agent-mode-switch-bounds", cx, |_, _| ModeSwitchBounds::default());
        let snapshot = *bounds_state.read(cx);
        let active_ix = usize::from(self.mode == Mode::Ide);
        let pill = snapshot.container.zip(snapshot.buttons[active_ix]).map(|(container, active)| {
            let target_x = f32::from(active.origin.x - container.origin.x);
            let target_w = f32::from(active.size.width);
            let target_y = f32::from(active.origin.y - container.origin.y);
            let target_h = f32::from(active.size.height);
            let policy = Spring::new(Duration::from_millis(180)).with_damping(0.92).with_epsilon(0.25);
            let left = spring(("agent-mode-pill", "x"), target_x, policy, window, cx);
            let width = spring(("agent-mode-pill", "w"), target_w, policy, window, cx);
            div()
                .absolute()
                .left(px(left))
                .top(px(target_y))
                .w(px(width))
                .h(px(target_h))
                .rounded(radius)
                .bg(pill_bg)
        });
        let has_pill = pill.is_some();
        let active_variant = if has_pill {
            ButtonCustomVariant::new(cx)
                .color(Hsla::transparent_black())
                .hover(Hsla::transparent_black())
                .active(Hsla::transparent_black())
                .foreground(pill_fg)
        } else {
            crate::theme::selected_button(cx)
        };
        let toggle = |ix: usize, id: &'static str, icon: Icon, label: &str, on: bool, action: Box<dyn Action>| {
            let state = bounds_state.clone();
            div()
                .on_prepaint(move |bounds, window, cx| {
                    let kick = state.update(cx, |s, _| {
                        let changed = s.buttons[ix].replace(bounds) != Some(bounds);
                        changed && s.container.is_some()
                    });
                    if kick {
                        window.request_animation_frame();
                    }
                })
                .child(
                    Button::new(id)
                        .ghost()
                        .small()
                        .icon(icon)
                        .toggled(on)
                        .selected(on)
                        .when(on, |this| this.custom(active_variant))
                        .tooltip_with_action(tr(cx, label), &ToggleMode, None)
                        .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx)),
                )
        };
        let container_state = bounds_state.clone();
        h_flex()
            .relative()
            .flex_shrink_0()
            .gap_0p5()
            .p_0p5()
            .rounded(radius)
            .bg(muted)
            .on_prepaint(move |bounds, window, cx| {
                let kick = container_state.update(cx, |s, _| {
                    let changed = s.container.replace(bounds) != Some(bounds);
                    changed && s.buttons.iter().any(Option::is_some)
                });
                if kick {
                    window.request_animation_frame();
                }
            })
            .children(pill)
            .child(toggle(0, "mode-agent", Icon::new(gpui_kit::assets::IconName::Sparkles), "agent.modeAgent", self.mode == Mode::Agent, Box::new(AgentMode)))
            .child(toggle(1, "mode-ide", Icon::new(gpui_kit::assets::IconName::CodeXml), "agent.modeIde", self.mode == Mode::Ide, Box::new(IdeMode)))
    }
}

/// The two toggles that switch the layout, at the left of the title bar.
pub fn mode_switch(mode: Mode, _cx: &App) -> impl IntoElement + use<> {
    ModeSwitch { mode }
}

/// `button`, naming the shown project, with the project menu: the projects to switch to, open a folder (on Windows or
/// on the execution host), add a folder to the project and remove it from the list.
pub fn with_project_menu(button: Button, cx: &App) -> impl IntoElement + use<> {
    let handle = Ide::global(cx);
    let ide = handle.read(cx);
    let current = ide.workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
    let workspaces = ide.workspaces.clone();
    button.dropdown_menu(move |mut menu, _, cx| {
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
        menu = menu
            .item(PopupMenuItem::new(tr(cx, "ide.ideOpenFolder")).on_click(|_, window, cx| crate::ui::ide::choose_folder(window, cx)))
            .item(PopupMenuItem::new(tr(cx, "ide.ideWslFolder")).on_click(|_, window, cx| crate::ui::ide::browse_folder(window, cx)));
        if let Some(current) = current.clone() {
            menu = menu.item(PopupMenuItem::new(tr(cx, "ide.ideAddFolder")).on_click(|_, window, cx| crate::ui::ide::choose_folder_to_attach(window, cx)));
            let handle = handle.clone();
            let title = workspaces.iter().find(|w| w.workspace_id == current).map(|w| w.title.clone()).unwrap_or_default();
            menu = menu.item(PopupMenuItem::new(tr(cx, "ide.ideRemoveWorkspace")).on_click(move |_, window, cx| {
                confirm_remove(handle.clone(), current.clone(), title.clone(), window, cx);
            }));
        }
        menu
    })
}

/// Confirm, then remove a project from the list; its files stay on disk.
pub(crate) fn confirm_remove(ide: Entity<Ide>, workspace_id: String, title: String, window: &mut Window, cx: &mut App) {
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

/// Bring the editor into view after a file was opened for the user (from a tool card, a link or a change to review):
/// Agent mode opens the files panel; IDE mode always shows the editor.
pub fn reveal_editor(cx: &mut App) {
    let Some(workbench) = cx.try_global::<WorkbenchHandle>().and_then(|handle| handle.0.upgrade()) else { return };
    // Deferred: the caller may be inside an update of an entity the workbench reads.
    cx.defer(move |cx| workbench.update(cx, |workbench, cx| workbench.show_tool(Tool::Files, cx)));
}

/// How much of the model's context a chat uses, as a ring that fills clockwise from the top (`fraction` 0 to 1).
pub fn usage_ring(fraction: f32, color: Hsla, track: Hsla) -> impl IntoElement {
    let target = fraction.clamp(0., 1.);
    canvas(
        move |_, window, cx| {
            let policy = Spring::new(Duration::from_millis(220)).with_damping(1.0).with_epsilon(0.002);
            let shown = spring("agent-usage-ring-fraction", target, policy, window, cx).clamp(0., 1.);
            let tinted = transition("agent-usage-ring-color", color, Transition::new(Duration::from_millis(220)), window, cx);
            (shown, tinted)
        },
        move |bounds, (shown, tinted), window, _| {
            let stroke = px(2.);
            let radius = (bounds.size.width.min(bounds.size.height) - stroke) / 2.;
            let center = bounds.center();
            let at = |turn: f32| {
                let angle = turn * std::f32::consts::TAU;
                point(center.x + radius * angle.sin(), center.y - radius * angle.cos())
            };
            let radii = point(radius, radius);
            // A whole circle is two half arcs: an arc from a point to itself draws nothing.
            let arc = |to: f32, color: Hsla, window: &mut Window| {
                let mut path = PathBuilder::stroke(stroke);
                path.move_to(at(0.));
                if to >= 0.999 {
                    path.arc_to(radii, px(0.), false, true, at(0.5));
                    path.arc_to(radii, px(0.), false, true, at(0.));
                } else {
                    path.arc_to(radii, px(0.), to > 0.5, true, at(to));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            arc(1., track, window);
            if shown > 0.005 {
                arc(shown, tinted, window);
            }
        },
    )
    .size(px(14.))
    .flex_shrink_0()
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn the_conversation_keeps_its_width_before_the_sidebar_and_the_panel() {
        // The smallest window at 225% scaling: the sidebar fits beside the conversation.
        assert_eq!(fit(909., Some(240.), None), Fit { sidebar: Some(240.), panel: None });
        // An open panel takes the sidebar's place, then narrows to leave the conversation its least.
        let narrow = fit(909., Some(240.), Some(0.));
        assert_eq!(narrow.sidebar, None);
        assert_eq!(narrow.panel, Some(382.));
        assert!(909. - narrow.panel.unwrap() >= CONVERSATION_MIN);
        assert_eq!(fit(1000., Some(240.), Some(0.)), Fit { sidebar: None, panel: Some(420.) });
        // A window with room for the panel's width shows all three; a dragged panel keeps its width while it fits.
        assert_eq!(fit(1280., Some(240.), Some(0.)), Fit { sidebar: Some(240.), panel: Some(538.) });
        assert_eq!(fit(1920., Some(240.), Some(0.)), Fit { sidebar: Some(240.), panel: Some(760.) });
        assert_eq!(fit(1920., Some(240.), Some(900.)), Fit { sidebar: Some(240.), panel: Some(900.) });
        assert_eq!(fit(1400., Some(260.), Some(900.)), Fit { sidebar: Some(260.), panel: Some(720.) });
        assert_eq!(fit(1100., Some(260.), Some(900.)), Fit { sidebar: None, panel: Some(680.) });
        // The panel never gets narrower than its least, even in a window too small for both.
        assert_eq!(fit(600., None, Some(0.)).panel, Some(PANEL_MIN));
        // A window too narrow for the sidebar and the conversation leaves the sidebar out; dragged widths are clamped.
        assert_eq!(fit(640., Some(240.), None).sidebar, None);
        assert_eq!(fit(2560., Some(999.), None).sidebar, Some(SIDEBAR_MAX));
        assert_eq!(fit(2560., None, None), Fit { sidebar: None, panel: None });
    }

    #[test]
    fn names_the_modes_as_layout_json_does() {
        for mode in [Mode::Agent, Mode::Ide] {
            assert_eq!(Mode::from_id(mode.id()), Some(mode));
        }
        assert_eq!(Mode::from_id("editor"), None);
        assert_eq!(Mode::default(), Mode::Agent);
    }
}
