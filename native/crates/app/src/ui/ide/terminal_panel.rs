//! The bottom panel (`IdeBottom.tsx`): terminals, runs and debug consoles as tabs, the problems the language servers
//! report, and the debugger: controls, stack, breakpoints, variables, watches and an expression console.

use std::collections::HashSet;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::store::execution::{Execution, ExecutionEvent, OperationKind};
use crate::store::ide::{Ide, IdeEvent, file_name};
use crate::store::language::LanguageServers;
use crate::ui::ide::terminal::TerminalView;
use crate::ui::{tr, trf};

/// Lines of terminal output sent to the AI at most (3.x `SENT_LINES`).
const SENT_LINES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottomTab {
    Terminal,
    Problems,
    Debug,
}

pub struct TerminalPanel {
    execution: Entity<Execution>,
    views: Vec<Entity<TerminalView>>,
    /// Operations closed here; the Host may still list them until they exit.
    closed: HashSet<String>,
    selected: Option<String>,
    starting: bool,
    pub tab: BottomTab,
    watch: Entity<InputState>,
    console: Entity<InputState>,
    /// The console's last answer, or why it failed.
    evaluated: Option<Result<String, String>>,
    tabs_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl TerminalPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let execution = Execution::global(cx);
        let watch = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "ide.ideAddWatch")));
        let console = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "ide.ideConsole")));
        let subscriptions = vec![
            cx.subscribe(&execution, |this, _, event: &ExecutionEvent, cx| match event {
                ExecutionEvent::TerminalsChanged => this.sync(cx),
                ExecutionEvent::Started(id) => {
                    this.closed.remove(id);
                    this.drop_finished_like(id, cx);
                    this.sync(cx);
                    this.selected = Some(id.clone());
                    this.reveal_selected(cx);
                    cx.notify();
                }
                ExecutionEvent::DebugChanged => cx.notify(),
                ExecutionEvent::Output { .. } => {}
            }),
            cx.observe(&LanguageServers::global(cx), |this, _, cx| {
                if this.tab == BottomTab::Problems {
                    cx.notify();
                }
            }),
            cx.subscribe(&Ide::global(cx), |_, _, event: &IdeEvent, cx| {
                if *event == IdeEvent::ExecutionChanged {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&watch, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.add_watch(window, cx);
                }
            }),
            cx.subscribe_in(&console, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.evaluate(window, cx);
                }
            }),
        ];
        let mut panel = Self {
            execution,
            views: Vec::new(),
            closed: HashSet::new(),
            selected: None,
            starting: false,
            tab: BottomTab::Terminal,
            watch,
            console,
            evaluated: None,
            tabs_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        panel.sync(cx);
        panel
    }

    /// One view per operation the Host lists, in the order they appeared.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let operations = self.execution.read(cx).operations();
        self.views.retain(|view| operations.contains(&view.read(cx).id) && !self.closed.contains(&view.read(cx).id));
        for id in operations {
            if !self.closed.contains(&id) && !self.views.iter().any(|view| view.read(cx).id == id) {
                self.views.push(cx.new(|cx| TerminalView::new(id, cx)));
            }
        }
        if self.selected.as_ref().is_none_or(|selected| !self.views.iter().any(|view| view.read(cx).id == *selected)) {
            self.selected = self.views.last().map(|view| view.read(cx).id.clone());
        }
        cx.notify();
    }

    /// A new run or debug session takes the place of the finished ones of the same configuration.
    fn drop_finished_like(&mut self, id: &str, cx: &mut Context<Self>) {
        let execution = self.execution.read(cx);
        let kind = execution.kind_of(id);
        let name_of = |id: &str| match execution.kind_of(id) {
            OperationKind::Run => execution.runs.iter().find(|run| run.id == id).map(|run| run.name.clone()),
            OperationKind::Debug => execution.debugs.iter().find(|debug| debug.id == id).map(|debug| debug.name.clone()),
            OperationKind::Terminal => None,
        };
        let Some(name) = name_of(id) else { return };
        let finished: Vec<String> = self
            .views
            .iter()
            .map(|view| view.read(cx).id.clone())
            .filter(|other| other != id && execution.kind_of(other) == kind && !execution.running(other) && name_of(other).as_deref() == Some(name.as_str()))
            .collect();
        self.closed.extend(finished);
    }

    /// Scroll the tab strip to the shown tab.
    fn reveal_selected(&self, cx: &App) {
        if let Some(index) = self.views.iter().position(|view| Some(&view.read(cx).id) == self.selected.as_ref()) {
            self.tabs_scroll.scroll_to_item(index);
        }
    }

    /// Start a terminal in the project and show it.
    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.starting {
            return;
        }
        self.starting = true;
        self.tab = BottomTab::Terminal;
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
                        this.reveal_selected(cx);
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

    /// Stop an operation and drop its tab.
    fn close(&mut self, id: String, cx: &mut Context<Self>) {
        let execution = self.execution.read(cx);
        if execution.running(&id) {
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

    /// The shown tab's operation while it runs, which Stop ends.
    pub fn running_selection(&self, cx: &App) -> Option<String> {
        self.selected.clone().filter(|id| self.execution.read(cx).running(id))
    }

    pub fn show(&mut self, tab: BottomTab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    fn label(&self, id: &str, index: usize, cx: &App) -> String {
        let execution = self.execution.read(cx);
        match execution.kind_of(id) {
            OperationKind::Run => {
                let name = execution.runs.iter().find(|run| run.id == id).map(|run| run.name.clone()).unwrap_or_default();
                format!("{}: {name}", tr(cx, "ide.ideRun"))
            }
            OperationKind::Debug => {
                let name = execution.debugs.iter().find(|debug| debug.id == id).map(|debug| debug.name.clone()).unwrap_or_default();
                format!("{}: {name}", tr(cx, "ide.ideDebug"))
            }
            OperationKind::Terminal => format!("{} {}", tr(cx, "ide.ideTerminal"), index + 1),
        }
    }

    /// Ask the AI about the shown terminal's last lines.
    fn send_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.views.iter().find(|view| Some(&view.read(cx).id) == self.selected.as_ref()).cloned() else { return };
        let text = view.read(cx).recent_text(SENT_LINES);
        if text.trim().is_empty() {
            return;
        }
        crate::ui::ask_ai(trf(cx, "ide.ideTerminalAsk", &[("output", &text)]).to_string(), window, cx);
    }

    fn add_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.watch.read(cx).value().trim().to_owned();
        if value.is_empty() {
            return;
        }
        let ide = Ide::global(cx);
        ide.update(cx, |ide, cx| {
            let mut execution = ide.execution();
            let mut watches: Vec<Value> = execution["watches"].as_array().cloned().unwrap_or_default();
            if !watches.iter().any(|known| known.as_str() == Some(value.as_str())) {
                watches.push(Value::String(value.clone()));
            }
            execution["watches"] = Value::Array(watches);
            ide.set_execution(execution, cx);
        });
        self.execution.update(cx, |execution, cx| execution.evaluate(value, "watch", cx)).detach();
        self.watch.update(cx, |state, cx| state.set_value("", window, cx));
    }

    fn remove_watch(&mut self, watch: String, cx: &mut Context<Self>) {
        Ide::global(cx).update(cx, |ide, cx| {
            let mut execution = ide.execution();
            let watches: Vec<Value> = execution["watches"].as_array().cloned().unwrap_or_default().into_iter().filter(|known| known.as_str() != Some(watch.as_str())).collect();
            execution["watches"] = Value::Array(watches);
            ide.set_execution(execution, cx);
        });
    }

    fn evaluate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let expression = self.console.read(cx).value().trim().to_owned();
        if expression.is_empty() {
            return;
        }
        let task = self.execution.update(cx, |execution, cx| execution.evaluate(expression, "repl", cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.evaluated = Some(result);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let problems = LanguageServers::global(cx).read(cx).problems().len();
        let tab = |id: &'static str, label: SharedString, which: BottomTab, cx: &mut Context<Self>| {
            let active = self.tab == which;
            div()
                .id(id)
                .px_2()
                .py_0p5()
                .rounded(theme.radius)
                .text_sm()
                .cursor_pointer()
                .when(active, |this| this.bg(theme.list_active).font_semibold())
                .when(!active, |this| this.text_color(theme.muted_foreground).hover(|style| style.bg(theme.list_hover)))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.show(which, cx)))
        };
        let problems_label: SharedString = if problems > 0 { format!("{} {problems}", tr(cx, "ide.ideProblems")).into() } else { tr(cx, "ide.ideProblems") };
        h_flex()
            .h(px(32.))
            .flex_shrink_0()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(tab("bottom-terminal", tr(cx, "ide.ideTerminal"), BottomTab::Terminal, cx))
            .child(tab("bottom-problems", problems_label, BottomTab::Problems, cx))
            .child(tab("bottom-debug", tr(cx, "ide.ideDebug"), BottomTab::Debug, cx))
    }

    fn render_terminals(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let tabs: Vec<(String, String, bool)> = self
            .views
            .iter()
            .enumerate()
            .map(|(index, view)| {
                let id = view.read(cx).id.clone();
                (id.clone(), self.label(&id, index, cx), self.execution.read(cx).running(&id))
            })
            .collect();
        let selected = self.selected.clone();
        let shown = self.views.iter().find(|view| Some(&view.read(cx).id) == selected.as_ref()).cloned();
        let has_shown = shown.is_some();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .id("terminal-tabs")
                    .h(px(30.))
                    .flex_shrink_0()
                    .px_2()
                    .gap_1()
                    .items_center()
                    .overflow_x_scroll()
                    .track_scroll(&self.tabs_scroll)
                    .children(tabs.into_iter().enumerate().map(|(index, (id, label, running))| {
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
                            .child(label)
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
                    )
                    .child(div().flex_1())
                    .when(has_shown, |this| {
                        this.child(
                            Button::new("terminal-send")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Bot)
                                .tooltip(tr(cx, "ide.ideTerminalSend"))
                                .on_click(cx.listener(|this, _, window, cx| this.send_output(window, cx))),
                        )
                    }),
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
            .into_any_element()
    }

    fn render_problems(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let problems = LanguageServers::global(cx).read(cx).problems();
        if problems.is_empty() {
            return v_flex().size_full().items_center().justify_center().text_sm().text_color(theme.muted_foreground).child(tr(cx, "ide.ideNoProblems")).into_any_element();
        }
        v_flex()
            .id("problems")
            .size_full()
            .p_1()
            .overflow_y_scrollbar()
            .children(problems.into_iter().enumerate().map(|(index, problem)| {
                let (key, line, column) = (problem.key.clone(), problem.line, problem.column);
                let ask = problem.clone();
                h_flex()
                    .id(("problem", index))
                    .px_2()
                    .py_0p5()
                    .gap_2()
                    .items_center()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.list_hover))
                    .child(div().flex_shrink_0().text_color(if problem.error { theme.danger } else { theme.warning }).child(if problem.error { "×" } else { "△" }))
                    .child(div().flex_1().min_w_0().truncate().child(problem.message.clone()))
                    .child(div().flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(format!("{}:{line}:{column}", crate::store::ide::split_key(&problem.key).1)))
                    .child(Button::new(("problem-fix", index)).ghost().xsmall().label(tr(cx, "ide.ideProblemFix")).on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        fix_problem(&ask, window, cx);
                    }))
                    .on_click(move |_, window, cx| {
                        let key = key.clone();
                        Ide::global(cx).update(cx, |ide, cx| ide.reveal(key, line.saturating_sub(1), column.saturating_sub(1), window, cx));
                    })
            }))
            .into_any_element()
    }

    fn render_debug(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let execution = self.execution.read(cx);
        let Some(debug) = execution.debug().cloned() else {
            return v_flex().size_full().items_center().justify_center().text_sm().text_color(theme.muted_foreground).child(tr(cx, "ide.ideNoDebug")).into_any_element();
        };
        let pause = execution.pause.clone().unwrap_or_default();
        let paused = debug.phase == "paused";
        let active = debug.running();
        let capabilities = debug.capabilities.clone().unwrap_or_default();
        let control = |id: &'static str, label: &str, action: &'static str, enabled: bool, cx: &mut Context<Self>| {
            Button::new(id).outline().xsmall().label(tr(cx, label)).disabled(!enabled).on_click(cx.listener(move |this, _, _, cx| {
                this.execution.update(cx, |execution, cx| execution.control(action, cx));
            }))
        };
        let reason = debug.error.clone().or(debug.reason.clone()).unwrap_or_else(|| debug.phase.clone());
        let toolbar = h_flex()
            .flex_shrink_0()
            .gap_1()
            .items_center()
            .child(if paused {
                control("debug-continue", "ide.ideContinue", "continue", active, cx)
            } else {
                control("debug-pause", "ide.idePause", "pause", active && capabilities.pause, cx)
            })
            .child(control("debug-next", "ide.ideStepOver", "next", paused && capabilities.next, cx))
            .child(control("debug-in", "ide.ideStepInto", "stepIn", paused && capabilities.step_in, cx))
            .child(control("debug-out", "ide.ideStepOut", "stepOut", paused && capabilities.step_out, cx))
            .child({
                let id = debug.id.clone();
                Button::new("debug-stop").outline().xsmall().label(tr(cx, "ide.ideStop")).disabled(!active).on_click(cx.listener(move |this, _, _, cx| {
                    this.execution.read(cx).stop(&id, cx);
                }))
            })
            .child(div().flex_1())
            .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(format!("{} · {reason}", debug.name)));
        let heading = |label: SharedString| div().text_xs().font_semibold().text_color(theme.muted_foreground).py_0p5().child(label);
        // Stack and breakpoints.
        let frames = pause.frames.iter().map(|frame| {
            let selected = pause.frame_id == Some(frame.id);
            let place = frame.path.as_ref().map(|path| format!("{}:{}", path.rsplit(['/', '\\']).next().unwrap_or(path), frame.line)).unwrap_or_default();
            let chosen = frame.clone();
            div()
                .id(("frame", frame.id as u64))
                .px_1()
                .rounded(theme.radius)
                .text_xs()
                .cursor_pointer()
                .truncate()
                .when(selected, |this| this.bg(theme.list_active))
                .hover(|style| style.bg(theme.list_hover))
                .child(format!("{}  {place}", frame.name))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let frame = chosen.clone();
                    this.execution.update(cx, |execution, cx| {
                        execution.reveal(&frame, cx);
                        execution.select_frame(frame.id, cx);
                    });
                }))
                .into_any_element()
        });
        let ide = Ide::global(cx);
        let breakpoints: Vec<(String, u64)> = ide.read(cx).execution()["breakpoints"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|source| {
                let path = source["path"].as_str().unwrap_or_default().to_owned();
                let root = source["rootId"].as_str().map(str::to_owned);
                source["lines"].as_array().cloned().unwrap_or_default().into_iter().filter_map(move |line| {
                    let key = match &root {
                        Some(root) => format!("\0{root}/{path}"),
                        None => path.clone(),
                    };
                    Some((key, line.as_u64()?))
                })
            })
            .collect();
        let breakpoint_rows = breakpoints.into_iter().enumerate().map(|(index, (key, line))| {
            let label = format!("● {}:{line}", file_name(&key));
            div()
                .id(("breakpoint", index))
                .px_1()
                .rounded(theme.radius)
                .text_xs()
                .cursor_pointer()
                .truncate()
                .hover(|style| style.bg(theme.list_hover))
                .child(label)
                .on_click(move |_, window, cx| {
                    let key = key.clone();
                    Ide::global(cx).update(cx, |ide, cx| ide.reveal(key, line.saturating_sub(1) as u32, 0, window, cx));
                })
                .into_any_element()
        });
        let left = v_flex()
            .id("debug-stack")
            .flex_1()
            .min_w_0()
            .overflow_y_scrollbar()
            .when(pause.threads.len() > 1, |this| {
                let threads = pause.threads.clone();
                let current = pause.thread_id;
                let name = threads.iter().find(|thread| Some(thread.id) == current).map(|thread| thread.name.clone()).unwrap_or_default();
                let execution = self.execution.clone();
                this.child(heading(tr(cx, "ide.ideThreads"))).child(Button::new("debug-threads").outline().xsmall().label(name).dropdown_menu(move |mut menu, _, _| {
                    for thread in &threads {
                        let (execution, id) = (execution.clone(), thread.id);
                        menu = menu.item(PopupMenuItem::new(thread.name.clone()).checked(current == Some(id)).on_click(move |_, _, cx| {
                            execution.update(cx, |execution, cx| execution.select_thread(id, cx));
                        }));
                    }
                    menu
                }))
            })
            .child(heading(tr(cx, "ide.ideStack")))
            .children(frames.collect::<Vec<_>>())
            .child(heading(tr(cx, "ide.ideBreakpoints")))
            .children(breakpoint_rows.collect::<Vec<_>>());
        // Variables.
        let mut rows: Vec<AnyElement> = Vec::new();
        for scope in &pause.scopes {
            rows.push(heading(scope.name.clone().into()).into_any_element());
            self.variable_rows(&pause, scope.variables_reference, 0, &mut rows, cx);
        }
        let middle = v_flex()
            .id("debug-variables")
            .flex_1()
            // Variables get more of the width than the stack and the watches.
            .map(|mut this| {
                this.style().flex_grow = Some(1.6);
                this
            })
            .min_w_0().overflow_y_scrollbar().child(heading(tr(cx, "ide.ideVariables"))).children(rows);
        // Watches.
        let watches: Vec<String> = ide.read(cx).execution()["watches"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
        let watch_rows = watches.into_iter().enumerate().map(|(index, watch)| {
            let value = match pause.watches.get(&watch) {
                Some(Ok(value)) => value.clone(),
                Some(Err(error)) => error.clone(),
                None => String::new(),
            };
            let removed = watch.clone();
            h_flex()
                .gap_1()
                .text_xs()
                .child(div().flex_1().min_w_0().truncate().child(format!("{watch}: {value}")))
                .child(Button::new(("watch-remove", index)).ghost().xsmall().icon(IconName::Close).on_click(cx.listener(move |this, _, _, cx| {
                    this.remove_watch(removed.clone(), cx);
                })))
                .into_any_element()
        });
        let right = v_flex()
            .id("debug-watches")
            .flex_1()
            .min_w_0()
            .gap_1()
            .overflow_y_scrollbar()
            .child(heading(tr(cx, "ide.ideWatches")))
            .child(Input::new(&self.watch).xsmall())
            .children(watch_rows.collect::<Vec<_>>());
        let evaluated = self.evaluated.clone();
        v_flex()
            .size_full()
            .p_2()
            .gap_2()
            .child(toolbar)
            .child(h_flex().flex_1().min_h_0().gap_3().items_start().child(left).child(middle).child(right))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.console).xsmall().disabled(!paused)))
                    .child(Button::new("debug-evaluate").outline().xsmall().label(tr(cx, "ide.ideEvaluate")).disabled(!paused).on_click(cx.listener(
                        |this, _, window, cx| this.evaluate(window, cx),
                    ))),
            )
            .when_some(evaluated, |this, result| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .max_h(px(80.))
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(if result.is_ok() { theme.foreground } else { theme.danger })
                        .child(match result {
                            Ok(value) | Err(value) => value,
                        }),
                )
            })
            .into_any_element()
    }

    /// The rows of a scope's or variable's children, with the opened ones' children below them.
    fn variable_rows(&self, pause: &crate::store::execution::Pause, reference: i64, depth: usize, rows: &mut Vec<AnyElement>, cx: &mut Context<Self>) {
        let Some(variables) = pause.variables.get(&reference) else { return };
        let theme = cx.theme().clone();
        for variable in variables {
            let child = variable.variables_reference;
            let open = child > 0 && pause.variables.contains_key(&child);
            let marker = if child > 0 { if open { "▾ " } else { "▸ " } } else { "  " };
            rows.push(
                div()
                    .id(SharedString::from(format!("variable-{reference}-{}-{}", variable.name, rows.len())))
                    .pl(px(depth as f32 * 12.))
                    .text_xs()
                    .truncate()
                    .font_family(theme.mono_font_family.clone())
                    .when(child > 0, |this| this.cursor_pointer().hover(|style| style.bg(theme.list_hover)))
                    .child(format!("{marker}{} = {}", variable.name, variable.value))
                    .when(child > 0, |this| {
                        this.on_click(cx.listener(move |this, _, _, cx| {
                            this.execution.update(cx, |execution, cx| if open { execution.collapse(child, cx) } else { execution.expand(child, cx) });
                        }))
                    })
                    .into_any_element(),
            );
            if open && depth < 8 {
                self.variable_rows(pause, child, depth + 1, rows, cx);
            }
        }
    }
}

/// Ask the AI to fix a problem, with the lines around it (3.x `fixProblem`).
fn fix_problem(problem: &crate::store::language::Problem, window: &mut Window, cx: &mut App) {
    let ide = Ide::global(cx);
    let ide = ide.read(cx);
    let Some(file) = ide.files.get(&problem.key) else { return };
    let text = file.editor.as_ref().map(|editor| editor.read(cx).value().to_string()).unwrap_or_default();
    let first = problem.line.saturating_sub(3).max(1) as usize;
    let code: Vec<String> = text.lines().enumerate().skip(first - 1).take(7).map(|(index, line)| format!("{}: {line}", index + 1)).collect();
    let path = crate::store::ide::split_key(&problem.key).1.to_owned();
    let language = crate::store::ide::language_of(&path);
    let prompt = trf(
        cx,
        "ide.ideProblemAsk",
        &[("path", &path), ("line", &problem.line), ("column", &problem.column), ("message", &problem.message), ("language", &language), ("code", &code.join("\n"))],
    );
    crate::ui::ask_ai(prompt.to_string(), window, cx);
}

impl Render for TerminalPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let header = self.render_header(cx);
        let body = match self.tab {
            BottomTab::Terminal => self.render_terminals(cx),
            BottomTab::Problems => self.render_problems(cx),
            BottomTab::Debug => self.render_debug(cx),
        };
        v_flex().size_full().bg(theme.background).border_t_1().border_color(theme.border).child(header).child(div().flex_1().min_h_0().child(body))
    }
}

/// A terminal tab's mark: filled while its process runs.
fn status_dot(running: bool, theme: &gpui_kit::component::Theme) -> impl IntoElement {
    div().size(px(6.)).rounded_full().map(|this| if running { this.bg(theme.success) } else { this.border_1().border_color(theme.muted_foreground) })
}
