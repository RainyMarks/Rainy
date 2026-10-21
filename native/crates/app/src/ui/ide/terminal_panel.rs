//! The bottom panel's terminals (`IdeBottom.tsx`): one tab per terminal of the project, a button for a new one.

use std::collections::HashSet;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::store::execution::{Execution, ExecutionEvent};
use crate::ui::ide::terminal::TerminalView;
use crate::ui::tr;

pub struct TerminalPanel {
    execution: Entity<Execution>,
    views: Vec<Entity<TerminalView>>,
    /// Terminals closed here; the Host may still list them until they exit.
    closed: HashSet<String>,
    selected: Option<String>,
    starting: bool,
    _subscriptions: Vec<Subscription>,
}

impl TerminalPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let execution = Execution::global(cx);
        let subscriptions = vec![cx.subscribe(&execution, |this, _, event: &ExecutionEvent, cx| {
            if let ExecutionEvent::TerminalsChanged = event {
                this.sync(cx);
            }
        })];
        let mut panel = Self { execution, views: Vec::new(), closed: HashSet::new(), selected: None, starting: false, _subscriptions: subscriptions };
        panel.sync(cx);
        panel
    }

    /// One view per terminal the Host lists, in the order they appeared.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let terminals: Vec<String> = self.execution.read(cx).terminals.iter().map(|terminal| terminal.id.clone()).collect();
        self.views.retain(|view| terminals.contains(&view.read(cx).id));
        for id in terminals {
            if !self.closed.contains(&id) && !self.views.iter().any(|view| view.read(cx).id == id) {
                self.views.push(cx.new(|cx| TerminalView::new(id, cx)));
            }
        }
        if self.selected.as_ref().is_none_or(|selected| !self.views.iter().any(|view| view.read(cx).id == *selected)) {
            self.selected = self.views.last().map(|view| view.read(cx).id.clone());
        }
        cx.notify();
    }

    /// Start a terminal in the project and show it.
    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.starting {
            return;
        }
        self.starting = true;
        cx.notify();
        let task = self.execution.update(cx, |execution, cx| execution.start_terminal(100, 24, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.starting = false;
                match result {
                    Ok(id) => {
                        this.sync(cx);
                        this.selected = Some(id.clone());
                        if let Some(view) = this.views.iter().find(|view| view.read(cx).id == id) {
                            let handle = view.focus_handle(cx);
                            handle.focus(window, cx);
                        }
                    }
                    Err(error) => window.push_notification(Notification::error(error.message), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Stop a terminal and drop its tab.
    fn close(&mut self, id: String, cx: &mut Context<Self>) {
        let execution = self.execution.read(cx);
        if execution.terminals.iter().any(|terminal| terminal.id == id && terminal.running()) {
            execution.stop(&id, cx);
        }
        self.closed.insert(id);
        self.selected = None;
        self.sync(cx);
    }

    /// Put the cursor in the shown terminal.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        if let Some(view) = self.views.iter().find(|view| Some(&view.read(cx).id) == self.selected.as_ref()) {
            let handle = view.focus_handle(cx);
            handle.focus(window, cx);
        }
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let execution = self.execution.read(cx);
        let tabs: Vec<(String, bool)> = self
            .views
            .iter()
            .map(|view| {
                let id = view.read(cx).id.clone();
                let running = execution.terminals.iter().any(|terminal| terminal.id == id && terminal.running());
                (id, running)
            })
            .collect();
        let selected = self.selected.clone();
        let shown = self.views.iter().find(|view| Some(&view.read(cx).id) == selected.as_ref()).cloned();
        let label = tr(cx, "ide.ideTerminal");
        v_flex()
            .size_full()
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .id("terminal-tabs")
                    .h(px(32.))
                    .flex_shrink_0()
                    .px_2()
                    .gap_1()
                    .items_center()
                    .overflow_x_scroll()
                    .children(tabs.into_iter().enumerate().map(|(index, (id, running))| {
                        let active = selected.as_deref() == Some(id.as_str());
                        let (select, close) = (id.clone(), id);
                        h_flex()
                            .id(("terminal-tab", index))
                            .h(px(24.))
                            .pl_2()
                            .gap_1()
                            .items_center()
                            .flex_shrink_0()
                            .rounded(theme.radius)
                            .text_sm()
                            .cursor_pointer()
                            .when(active, |this| this.bg(theme.list_active))
                            .when(!active, |this| this.text_color(theme.muted_foreground).hover(|style| style.bg(theme.list_hover)))
                            .child(status_dot(running, &theme))
                            .child(format!("{label} {}", index + 1))
                            .child(Button::new(("terminal-close", index)).ghost().xsmall().icon(IconName::Close).on_click(cx.listener(
                                move |this, _, _, cx| this.close(close.clone(), cx),
                            )))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.selected = Some(select.clone());
                                this.focus(window, cx);
                                cx.notify();
                            }))
                    }))
                    .child(
                        Button::new("terminal-new")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip(tr(cx, "ide.ideNewTerminal"))
                            .disabled(self.starting)
                            .on_click(cx.listener(|this, _, window, cx| this.new_terminal(window, cx))),
                    ),
            )
            .child(div().flex_1().min_h_0().px_2().pb_1().map(|this| match shown {
                Some(view) => this.child(view),
                None => this.child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(tr(cx, "ide.ideTerminalEmpty"))
                        .child(
                            Button::new("terminal-start")
                                .outline()
                                .small()
                                .icon(IconName::SquareTerminal)
                                .label(tr(cx, "ide.ideNewTerminal"))
                                .disabled(self.starting)
                                .on_click(cx.listener(|this, _, window, cx| this.new_terminal(window, cx))),
                        ),
                ),
            }))
    }
}

/// A terminal tab's mark: filled while its shell runs.
fn status_dot(running: bool, theme: &gpui_kit::component::Theme) -> impl IntoElement {
    div().size(px(6.)).rounded_full().map(|this| if running { this.bg(theme.success) } else { this.border_1().border_color(theme.muted_foreground) })
}
