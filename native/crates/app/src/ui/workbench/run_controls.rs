//! Title-bar run/debug controls and execution lifecycle (run, debug, stop, step, toggle breakpoint).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, IconName, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::store::execution::Execution;
use crate::store::ide::{RUN_LANGUAGES, RunMode, run_language};
use crate::ui::agent::Tool;
use crate::ui::ide::terminal_panel::BottomTab;
use crate::ui::ide::{MarkdownTypewriter, run_dialog};
use crate::ui::{tr, trf};

use super::Workbench;
use super::title_bar::title_control;

impl Workbench {
    /// Run, the run menu, Debug and Stop (3.x title-bar run group); in a narrow window Run shows only its icon.
    pub(super) fn render_run_controls(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ide = self.ide.read(cx);
        let active = ide.active.clone().filter(|key| ide.files.get(key).is_some_and(|file| file.editor.is_some()));
        let target = ide.run_target(active.as_deref());
        // Borrow execution JSON without cloning the entire object on every render.
        let profiles_ref = ide.execution_ref().and_then(|exec| exec["profiles"].as_array());
        let place = active.as_deref().map(|key| {
            let (root, path) = crate::store::ide::split_key(key);
            (root.map(str::to_owned), path.to_owned())
        });
        let runs_open = |profile: &Value| {
            place.as_ref().is_some_and(|(root, path)| profile["program"] == path.as_str() && profile["rootId"].as_str() == root.as_deref())
        };
        let others: Vec<String> = profiles_ref
            .into_iter()
            .flatten()
            .filter(|profile| !runs_open(profile))
            .take(8)
            .filter_map(|profile| profile["name"].as_str().map(str::to_owned))
            .collect();
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
    pub(super) fn launch(&mut self, debugging: bool, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn stop_run(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn step(&mut self, action: &'static str, cx: &mut Context<Self>) {
        let execution = Execution::global(cx);
        if execution.read(cx).debug().is_some_and(|debug| debug.running() && debug.phase == "paused") {
            execution.update(cx, |execution, cx| execution.control(action, cx));
        }
    }

    /// Set or clear the breakpoint on the cursor's line, in the running session too.
    pub(super) fn toggle_breakpoint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
}
