//! The project side of the window: file tree, editor area and folder choice.

pub mod cursor_status;
pub mod editor_area;
pub mod file_tree;
pub mod folder_dialog;
pub mod git;
pub mod quick_open;
pub mod search;
pub mod terminal;
pub mod terminal_panel;

use gpui_kit::component::IconName;
use gpui_kit::*;
use rainy_carrier::ExecutionTarget;

use crate::store::Store;
use crate::store::ide::Ide;
use crate::ui::tr;

actions!(rainy, [SaveFile, CloseFile, OpenFolder, FindInFiles, ReplaceInFiles, ShowExplorer, ShowSourceControl, ToggleTerminal, QuickOpen, OpenSettings, NewChat]);

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
    ]);
    terminal::bind_keys(cx);
}

/// Choose a folder and open it as a project: Windows' own picker for the Windows host, the in-app browser for a
/// Linux host, whose folders the Windows picker cannot reach as the Host sees them.
pub fn choose_folder(window: &mut Window, cx: &mut App) {
    let on_windows = Store::global(cx).read(cx).target.as_ref().is_none_or(|target| matches!(target, ExecutionTarget::Windows { .. }));
    if !on_windows {
        folder_dialog::open(window, cx);
        return;
    }
    let chosen = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(tr(cx, "ide.ideOpenFolder")),
    });
    let ide = Ide::global(cx);
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = cx.update(|window, cx| ide.update(cx, |ide, cx| ide.open_folder(path.to_string_lossy().into_owned(), window, cx)));
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
