//! Project map WebView2 pane lifecycle and pure rendering (never creates panes during `Render::render`).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::*;

use crate::store::Store;
use crate::ui::ide::viewers::{self, PageRequest};
use crate::ui::tr;
use crate::web::{PaneEvent, WebPane};

use super::EditorArea;

impl EditorArea {
    /// Show or hide the project map; it is created explicitly here (never inside `Render::render`).
    pub fn toggle_map(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.ide.read(cx).workspace.clone() else { return };
        self.map_shown = !self.map_shown || self.map.as_ref().is_none_or(|(id, _)| *id != workspace.workspace_id);
        if self.map_shown && self.map.as_ref().is_none_or(|(id, _)| *id != workspace.workspace_id) {
            let Some(launch) = Store::global(cx).read(cx).launch_url() else {
                self.map_shown = false;
                cx.notify();
                return;
            };
            let pane = viewers::create_pane(viewers::map_url(&launch, &workspace.workspace_id), "map", window, cx);
            cx.subscribe_in(&pane, window, |this, _, event: &PaneEvent, window, cx| this.on_map(event, window, cx)).detach();
            self.map = Some((workspace.workspace_id.clone(), pane));
            self.map_session = None;
        }
        cx.notify();
    }

    /// Bring the open files back in place of the project map.
    pub fn hide_map(&mut self, cx: &mut Context<Self>) {
        if self.map_shown {
            self.map_shown = false;
            cx.notify();
        }
    }

    /// What the map page asked for.
    fn on_map(&mut self, event: &PaneEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PaneEvent::Message(text) => match viewers::parse_message(text) {
                Some(PageRequest::Ready) => self.send_session(true, cx),
                Some(PageRequest::OpenKey { key, line }) => {
                    self.map_shown = false;
                    self.ide.update(cx, |ide, cx| match line {
                        Some(line) => ide.reveal(key, (line as u32).saturating_sub(1), 0, window, cx),
                        None => ide.open(key, true, window, cx),
                    });
                    cx.notify();
                }
                Some(PageRequest::Ask(text)) => crate::ui::ask_ai(text, window, cx),
                Some(PageRequest::OpenUrl(url)) => viewers::open_page_link(&url, window, cx),
                _ => {}
            },
            PaneEvent::Shortcut(shortcut) => {
                if let Some(action) = viewers::shortcut_action(*shortcut, "map") {
                    crate::web::reclaim_focus(window);
                    window.dispatch_action(action, cx);
                }
            }
            PaneEvent::NewWindow(url) => viewers::open_page_link(url, window, cx),
            PaneEvent::Ready | PaneEvent::Failed(_) => cx.notify(),
        }
    }

    /// Tell the map which chat's footprint to list and changes to mark: when the page is ready (`always`), and then
    /// whenever another chat is selected.
    pub(super) fn send_session(&mut self, always: bool, cx: &App) {
        let Some((_, pane)) = &self.map else { return };
        let session = Store::global(cx).read(cx).selected.clone();
        if !always && self.map_session.as_ref() == Some(&session) {
            return;
        }
        if let Some(view) = pane.read(cx).view() {
            view.execute(&viewers::set_session_script(session.as_deref()), |_| {});
            self.map_session = Some(session);
        }
    }

    /// Pure render of the project map pane if currently shown and matching the active workspace.
    pub(super) fn render_map(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.map_shown {
            return None;
        }
        let current_ws = self.ide.read(cx).workspace.as_ref().map(|shown| shown.workspace_id.as_str())?;
        let (id, pane) = self.map.as_ref()?;
        if id != current_ws {
            return None;
        }
        let theme = cx.theme().clone();
        Some(
            v_flex()
                .size_full()
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .h(px(34.))
                        .px_3()
                        .gap_2()
                        .items_center()
                        .text_sm()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(Icon::new(gpui_kit::assets::IconName::Map).small())
                        .child(div().flex_1().font_semibold().child(tr(cx, "native.projectMap")))
                        .child(
                            Button::new("map-close")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip(tr(cx, "native.closeMap"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.map_shown = false;
                                    cx.notify();
                                })),
                        ),
                )
                .child(div().flex_1().min_h_0().relative().child(div().absolute().inset_0().child(WebPane::area(pane))))
                .into_any_element(),
        )
    }
}
