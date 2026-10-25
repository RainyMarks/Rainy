//! The command palette (`palette` in `src/renderer/app/Workbench.tsx`, Ctrl+Shift+P or F1): every command by name.

use std::rc::Rc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::json;

use crate::store::Store;
use crate::store::ide::Ide;
use crate::ui::ide::{
    AttachFolder, CheckUpdates, CompareWithSaved, ExportHtml, ExportPdf, FindReferences, FocusEditor, FormatDocument, GitRefresh, GoToLine, MarkdownFocus,
    MarkdownSource, MarkdownTypewriter, RenameSymbol, SaveAll, SearchHistory, SendSelection, ShowOutline, ShowProblems, ToggleAgent, ToggleFiles,
};
use crate::ui::settings::Section;
use crate::ui::ide::{
    AiEditSelection, CloseFile, CommandPalette, DebugFile, FindInFiles, NewChat, OpenFolder, OpenRuntimeSettings, OpenSettings, QuickOpen,
    ReplaceInFiles, RunFile, RunProfiles, SaveFile, ShowExplorer, ShowMap, ShowSourceControl, StopRun, ToggleBreakpoint, ToggleTerminal,
};
use crate::ui::tr;

type Run = Rc<dyn Fn(&mut Window, &mut App)>;

struct Command {
    category: SharedString,
    label: SharedString,
    keys: &'static str,
    enabled: bool,
    run: Run,
}

/// A command that dispatches an action to the focused part of the window.
fn action(action: impl Action + Clone) -> Run {
    Rc::new(move |window, cx| window.dispatch_action(action.boxed_clone(), cx))
}

/// A command that opens the settings at a section.
fn settings(section: Section) -> Run {
    Rc::new(move |window, cx| {
        if let Some(workbench) = cx.try_global::<crate::ui::workbench::WorkbenchHandle>().and_then(|handle| handle.0.upgrade()) {
            workbench.update(cx, |workbench, cx| workbench.open_settings(section, window, cx));
        }
    })
}

fn prefs(patch: serde_json::Value) -> Run {
    Rc::new(move |window, cx| crate::ui::settings::set_prefs(patch.clone(), window, cx))
}

fn commands(cx: &App) -> Vec<Command> {
    let ide = Ide::global(cx);
    let ide = ide.read(cx);
    let workspace = ide.workspace.is_some();
    let editing = ide.active.as_ref().and_then(|key| ide.files.get(key)).is_some_and(|file| file.editor.is_some());
    let has_file = ide.active.is_some();
    let markdown = ide.active.as_ref().and_then(|key| ide.files.get(key)).is_some_and(|file| {
        file.editor.is_some() && crate::ui::ide::viewers::preview_kind(&file.document.path) == Some("markdown")
    });
    let code = editing && !markdown;
    let selection = crate::ui::ide::editor_commands::active_editor(cx).is_some_and(|(_, editor)| !editor.read(cx).selected_range().is_empty());
    let any_dirty = ide.files.values().any(|file| file.dirty);
    let php = ide.run_target(ide.active.as_deref()).is_some_and(|(configuration, _)| configuration["language"] == "php");
    let prefs_now = Store::global(cx).read(cx).prefs.clone();
    let locale = if crate::ui::locale(cx) == crate::i18n::Locale::Zh { "en" } else { "zh" };
    let entry = |category: &str, label: SharedString, keys: &'static str, enabled: bool, run: Run| Command {
        category: tr(cx, &format!("app.{category}")),
        label,
        keys,
        enabled,
        run,
    };
    vec![
        entry("catFile", tr(cx, "ide.ideOpenFolder"), "Ctrl+Alt+O", true, action(OpenFolder)),
        entry("catFile", tr(cx, "ide.ideAddFolder"), "", workspace, action(AttachFolder)),
        entry("catFile", tr(cx, "ide.ideQuickOpen"), "Ctrl+P", workspace, action(QuickOpen)),
        entry("catFile", tr(cx, "ide.ideSave"), "Ctrl+S", editing, action(SaveFile)),
        entry("catFile", tr(cx, "ide.ideSaveAll"), "", any_dirty, action(SaveAll)),
        entry("catFile", tr(cx, "app.cmdCloseEditor"), "Ctrl+W", has_file, action(CloseFile)),
        entry("catFile", tr(cx, "viewers.exportPdf"), "", markdown, action(ExportPdf)),
        entry("catFile", tr(cx, "viewers.exportHtml"), "", markdown, action(ExportHtml)),
        entry("catView", tr(cx, "ide.ideOutline"), "", true, action(ShowOutline)),
        entry("catView", tr(cx, "viewers.toggleSourceMode"), "Ctrl+/", markdown, action(MarkdownSource)),
        entry("catView", tr(cx, "viewers.focusMode"), "F8", true, action(MarkdownFocus)),
        entry("catView", tr(cx, "viewers.typewriterMode"), "F9", true, action(MarkdownTypewriter)),
        entry("catEdit", tr(cx, "ide.ideFind"), "Ctrl+F", code, action(gpui_kit::component::input::Search)),
        entry("catEdit", tr(cx, "ide.ideReplace"), "Ctrl+H", code, action(gpui_kit::component::input::Replace)),
        entry("catEdit", tr(cx, "ide.ideGoLine"), "Ctrl+G", editing, action(GoToLine)),
        entry("catEdit", tr(cx, "ide.ideFormat"), "Shift+Alt+F", code, action(FormatDocument)),
        entry("catEdit", tr(cx, "ide.ideCompare"), "", editing, action(CompareWithSaved)),
        entry("catEdit", tr(cx, "ide.ideFindReferences"), "Shift+F12", code, action(FindReferences)),
        entry("catEdit", tr(cx, "ide.ideRenameSymbol"), "F2", code, action(RenameSymbol)),
        entry("catEdit", tr(cx, "app.cmdSearch"), "Ctrl+Shift+F", workspace, action(FindInFiles)),
        entry("catEdit", tr(cx, "app.cmdReplace"), "Ctrl+Shift+H", workspace, action(ReplaceInFiles)),
        entry("catView", tr(cx, "app.cmdPalette"), "Ctrl+Shift+P", true, action(CommandPalette)),
        entry("catView", tr(cx, "app.cmdExplorer"), "Ctrl+Shift+E", true, action(ShowExplorer)),
        entry("catView", tr(cx, "app.cmdSourceControl"), "Ctrl+Shift+G", workspace, action(ShowSourceControl)),
        entry("catView", tr(cx, "ide.ideToggleFiles"), "", true, action(ToggleFiles)),
        entry("catView", tr(cx, "ide.ideToggleBottom"), "Ctrl+`", workspace, action(ToggleTerminal)),
        entry("catView", tr(cx, "ide.ideToggleAgent"), "", true, action(ToggleAgent)),
        entry("catView", tr(cx, "ide.ideFocusEditor"), "", editing, action(FocusEditor)),
        entry("catView", tr(cx, "app.cmdProblems"), "", workspace, action(ShowProblems)),
        entry("catView", tr(cx, "app.cmdMap"), "", workspace, action(ShowMap)),
        entry("catRun", tr(cx, "app.cmdRunFile"), "Ctrl+F5", editing, action(RunFile)),
        entry("catRun", tr(cx, "app.cmdDebugFile"), "F5", editing && !php, action(DebugFile)),
        entry("catRun", tr(cx, "ide.ideStop"), "Shift+F5", workspace, action(StopRun)),
        entry("catRun", tr(cx, "ide.ideToggleBreakpoint"), "F9", editing, action(ToggleBreakpoint)),
        entry("catRun", tr(cx, "ide.ideRunProfiles"), "", workspace, action(RunProfiles)),
        entry("catAi", tr(cx, "app.cmdNewChat"), "Ctrl+Alt+N", true, action(NewChat)),
        entry("catAi", tr(cx, "app.cmdHistory"), "Ctrl+Alt+K", true, action(SearchHistory)),
        entry("catAi", tr(cx, "app.cmdAiEdit"), "Ctrl+K", code, action(AiEditSelection)),
        entry("catAi", tr(cx, "ide.ideSendSelection"), "", selection, action(SendSelection)),
        entry("catAi", tr(cx, "app.cmdToggleSuggest"), "", true, prefs(json!({ "inlineCompletion": !prefs_now.inline_completion }))),
        entry("catGit", tr(cx, "app.cmdGitCommit"), "", workspace, action(ShowSourceControl)),
        entry("catGit", tr(cx, "app.cmdGitRefresh"), "", workspace, action(GitRefresh)),
        entry("catSettings", tr(cx, "app.settings"), "Ctrl+,", true, action(OpenSettings)),
        entry("catSettings", tr(cx, "app.cmdSsh"), "", true, action(OpenRuntimeSettings)),
        entry("catSettings", tr(cx, "settings.navAppearance"), "", true, settings(Section::Appearance)),
        entry("catSettings", tr(cx, "settings.navModels"), "", true, settings(Section::Models)),
        entry("catSettings", tr(cx, "settings.navExtensions"), "", true, settings(Section::Extensions)),
        entry("catSettings", tr(cx, "settings.navMemory"), "", true, settings(Section::Memory)),
        entry("catSettings", tr(cx, "settings.navSecurity"), "", true, settings(Section::Security)),
        entry("catSettings", tr(cx, "app.cmdThemeLight"), "", true, prefs(json!({ "theme": "light" }))),
        entry("catSettings", tr(cx, "app.cmdThemeDark"), "", true, prefs(json!({ "theme": "dark" }))),
        entry("catSettings", tr(cx, "app.cmdThemeSystem"), "", true, prefs(json!({ "theme": "system" }))),
        entry("catSettings", tr(cx, "app.cmdLanguage"), "", true, prefs(json!({ "locale": locale }))),
        entry("catHelp", tr(cx, "native.checkUpdates"), "", true, action(CheckUpdates)),
    ]
}

pub struct Palette {
    query: Entity<InputState>,
    commands: Vec<Command>,
    /// Indexes of the commands that match the query, in order.
    shown: Vec<usize>,
    selected: usize,
    /// Follows the highlighted row as the arrow keys move it.
    scroll: ScrollHandle,
    /// What had the focus when the palette opened.
    back: Option<FocusHandle>,
    _subscriptions: Vec<Subscription>,
}

impl Palette {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "app.palettePlaceholder")));
        let subscriptions = vec![cx.subscribe_in(&query, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.filter(cx);
            }
        })];
        let back = window.focused(cx);
        let mut palette = Self { query, commands: commands(cx), shown: Vec::new(), selected: 0, scroll: ScrollHandle::new(), back, _subscriptions: subscriptions };
        palette.filter(cx);
        palette
    }

    /// Commands whose category and name hold every word of the query.
    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        self.shown = self
            .commands
            .iter()
            .enumerate()
            .filter(|(_, command)| {
                let text = format!("{} {}", command.category, command.label).to_lowercase();
                words.iter().all(|word| text.contains(word))
            })
            .map(|(index, _)| index)
            .collect();
        // Commands that can run now come first.
        self.shown.sort_by_key(|index| !self.commands[*index].enabled);
        self.selected = 0;
        self.scroll.scroll_to_item(0);
        cx.notify();
    }

    fn chosen(&self) -> Option<(Run, Option<FocusHandle>)> {
        let command = &self.commands[*self.shown.get(self.selected)?];
        command.enabled.then(|| (command.run.clone(), self.back.clone()))
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.shown.len();
        if count == 0 {
            return;
        }
        match event.keystroke.key.as_str() {
            "down" => self.selected = (self.selected + 1) % count,
            "up" => self.selected = (self.selected + count - 1) % count,
            _ => return,
        }
        self.scroll.scroll_to_item(self.selected);
        cx.stop_propagation();
        cx.notify();
    }
}

/// Run a command once the dialog has closed, with the focus back where it was when the palette opened (or on the
/// workbench), so the command acts on that part of the window.
fn run_later(run: Run, back: Option<FocusHandle>, window: &mut Window, cx: &mut App) {
    window.defer(cx, move |window, cx| {
        let focus = back.or_else(|| cx.try_global::<crate::ui::workbench::WorkbenchFocus>().map(|global| global.0.clone()));
        if let Some(focus) = focus {
            focus.focus(window, cx);
        }
        window.defer(cx, move |window, cx| run(window, cx));
    });
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let selected = self.selected;
        let rows: Vec<AnyElement> = self
            .shown
            .iter()
            .enumerate()
            .map(|(row, index)| {
                let command = &self.commands[*index];
                let run = command.run.clone();
                let back = self.back.clone();
                let enabled = command.enabled;
                h_flex()
                    .id(("palette", row))
                    .h(px(30.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .rounded(theme.radius)
                    .text_sm()
                    .when(enabled, |this| this.cursor_pointer().hover(|style| style.bg(theme.list_hover)))
                    .when(!enabled, |this| this.opacity(0.45))
                    .when(row == selected, |this| this.bg(theme.list_active))
                    .child(div().flex_shrink_0().text_color(theme.muted_foreground).child(format!("{}:", command.category)))
                    .child(div().flex_1().min_w_0().truncate().child(command.label.clone()))
                    .child(div().flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(command.keys))
                    .when(enabled, |this| {
                        this.on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            run_later(run.clone(), back.clone(), window, cx);
                        })
                    })
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty();
        v_flex()
            .gap_2()
            .capture_key_down(cx.listener(Self::key_down))
            .child(Input::new(&self.query))
            .child(
                div()
                    .relative()
                    .child(
                        v_flex()
                            .id("palette-results")
                            .max_h(px(420.))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .children(rows)
                            .when(empty, |this| this.child(div().p_2().text_sm().text_color(theme.muted_foreground).child(tr(cx, "app.paletteEmpty")))),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
    }
}

/// Show the palette with the cursor in its field.
pub fn open(window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| Palette::new(window, cx));
    let query = view.read(cx).query.clone();
    // Enter is the dialog's Confirm: it runs the highlighted command.
    window.open_dialog(cx, move |dialog, _, cx| {
        let chosen = view.clone();
        dialog.title(tr(cx, "app.paletteTitle")).w(px(620.)).child(view.clone()).on_ok(move |_, window, cx| {
            if let Some((run, back)) = chosen.read(cx).chosen() {
                run_later(run, back, window, cx);
            }
            true
        })
    });
    window.defer(cx, move |window, cx| {
        window.defer(cx, move |window, cx| query.update(cx, |state, cx| state.focus(window, cx)));
    });
}
