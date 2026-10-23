//! The project side of the window: file tree, editor area and folder choice.

pub mod ai_edit;
pub mod cursor_status;
pub mod debug_marks;
pub mod editor_commands;
pub mod editor_area;
pub mod viewers;
pub mod file_tree;
pub mod folder_dialog;
pub mod outline;
pub mod git;
pub mod quick_open;
pub mod run_dialog;
pub mod search;
pub mod terminal;
pub mod terminal_panel;

use gpui_kit::component::IconName;
use gpui_kit::*;
use rainy_carrier::ExecutionTarget;

use crate::store::Store;
use crate::store::ide::Ide;
use crate::ui::tr;

actions!(rainy, [SaveFile, CloseFile, OpenFolder, FindInFiles, ReplaceInFiles, ShowExplorer, ShowSourceControl, ToggleTerminal, QuickOpen, OpenSettings, NewChat, AiEditSelection, CommandPalette, OpenRuntimeSettings, ShowMap, ShowChat, RunFile, DebugFile, StopRun, StepOver, StepInto, StepOut, ToggleBreakpoint, RunProfiles, GoToLine, FormatDocument, CompareWithSaved, SendSelection, FindReferences, RenameSymbol, ExportPdf, ExportHtml, ShowOutline, MarkdownSource, MarkdownFocus, MarkdownTypewriter, AttachFolder, SaveAll, ToggleFiles, ToggleAgent, FocusEditor, ShowProblems, SearchHistory, GitRefresh, CheckUpdates]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-s", SaveFile, None),
        KeyBinding::new("ctrl-w", CloseFile, None),
        // The 3.x keys, plus the usual Ctrl+P and Ctrl+O.
        KeyBinding::new("ctrl-alt-o", OpenFolder, None),
        KeyBinding::new("ctrl-o", OpenFolder, None),
        KeyBinding::new("ctrl-shift-f", FindInFiles, None),
        KeyBinding::new("ctrl-shift-h", ReplaceInFiles, None),
        KeyBinding::new("ctrl-shift-e", ShowExplorer, None),
        KeyBinding::new("ctrl-shift-g", ShowSourceControl, None),
        KeyBinding::new("ctrl-`", ToggleTerminal, None),
        KeyBinding::new("ctrl-alt-p", QuickOpen, None),
        KeyBinding::new("ctrl-p", QuickOpen, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-alt-n", NewChat, None),
        KeyBinding::new("ctrl-shift-p", CommandPalette, None),
        KeyBinding::new("f1", CommandPalette, None),
        // Run and debug as in VS Code; F5 continues while the debugger is paused.
        KeyBinding::new("ctrl-f5", RunFile, None),
        KeyBinding::new("f5", DebugFile, None),
        KeyBinding::new("shift-f5", StopRun, None),
        KeyBinding::new("f10", StepOver, None),
        KeyBinding::new("f11", StepInto, None),
        KeyBinding::new("shift-f11", StepOut, None),
        KeyBinding::new("f9", ToggleBreakpoint, Some("Input && mode == editor")),
        KeyBinding::new("ctrl-/", MarkdownSource, None),
        KeyBinding::new("f8", MarkdownFocus, None),
        KeyBinding::new("ctrl-alt-k", SearchHistory, None),
        KeyBinding::new("ctrl-g", GoToLine, Some("Input && mode == editor")),
        KeyBinding::new("shift-alt-f", FormatDocument, Some("Input && mode == editor")),
        KeyBinding::new("shift-f12", FindReferences, Some("Input && mode == editor")),
        KeyBinding::new("f2", RenameSymbol, Some("Input && mode == editor")),
        KeyBinding::new("ctrl-k", AiEditSelection, Some("Input && mode == editor")),
        KeyBinding::new("f12", gpui_kit::component::input::GoToDefinition, Some("Input && mode == editor")),
        KeyBinding::new("ctrl-space", gpui_kit::component::input::ShowCompletions, Some("Input && mode == editor")),
    ]);
    terminal::bind_keys(cx);
}

/// Choose a folder and open it as a project: Windows' own picker for the Windows host, the in-app browser for a
/// Linux host, whose folders the Windows picker cannot reach as the Host sees them.
pub fn choose_folder(window: &mut Window, cx: &mut App) {
    pick_folder(false, window, cx);
}

/// Choose a folder and add it to the shown project (3.x "add folder to workspace").
pub fn choose_folder_to_attach(window: &mut Window, cx: &mut App) {
    pick_folder(true, window, cx);
}

fn pick_folder(attach: bool, window: &mut Window, cx: &mut App) {
    // Debug builds under test take the folder from the environment instead of a picker over the tester's windows.
    if cfg!(debug_assertions)
        && let Some(path) = std::env::var_os("RAINY_QA_PICK_FOLDER")
    {
        let path = path.to_string_lossy().into_owned();
        Ide::global(cx).update(cx, |ide, cx| if attach { ide.attach_root(path, window, cx) } else { ide.open_folder(path, window, cx) });
        return;
    }
    let on_windows = Store::global(cx).read(cx).target.as_ref().is_none_or(|target| matches!(target, ExecutionTarget::Windows { .. }));
    if !on_windows {
        folder_dialog::open(attach, window, cx);
        return;
    }
    let chosen = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(tr(cx, if attach { "ide.ideAddFolder" } else { "ide.ideOpenFolder" })),
    });
    let ide = Ide::global(cx);
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let path = path.to_string_lossy().into_owned();
            let _ = cx.update(|window, cx| {
                ide.update(cx, |ide, cx| if attach { ide.attach_root(path, window, cx) } else { ide.open_folder(path, window, cx) })
            });
        })
        .detach();
}

/// The icon of a file name.
pub fn file_icon(name: &str) -> IconName {
    match name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase().as_str() {
        "md" | "markdown" | "txt" | "rst" | "doc" | "docx" | "pdf" | "odt" | "rtf" => IconName::FileText,
        _ => IconName::File,
    }
}
