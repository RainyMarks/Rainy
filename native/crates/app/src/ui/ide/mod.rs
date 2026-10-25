//! The project side of the window: file tree, editor area and folder choice.

pub mod ai_edit;
pub mod cursor_status;
pub mod debug_marks;
pub mod editor_commands;
pub mod editor_area;
pub mod viewers;
pub mod word_count;
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

actions!(rainy, [SaveFile, CloseFile, OpenFolder, FindInFiles, ReplaceInFiles, ShowExplorer, ShowSourceControl, ToggleTerminal, QuickOpen, OpenSettings, NewChat, AiEditSelection, CommandPalette, OpenRuntimeSettings, ShowMap, ShowChat, RunFile, DebugFile, StopRun, StepOver, StepInto, StepOut, ToggleBreakpoint, RunProfiles, GoToLine, FormatDocument, CompareWithSaved, SendSelection, FindReferences, RenameSymbol, ExportPdf, ExportHtml, ShowOutline, MarkdownSource, MarkdownFocus, MarkdownTypewriter, MarkdownPreview, MarkdownSplit, AttachFolder, SaveAll, ToggleFiles, ToggleAgent, FocusEditor, ShowProblems, SearchHistory, GitRefresh, CheckUpdates]);

/// The key context of a Markdown tab's views and toolbar.
pub const MARKDOWN_TAB: &str = "MarkdownTab";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys(key_bindings());
    terminal::bind_keys(cx);
}

/// The workbench's keys; the terminal adds its own.
fn key_bindings() -> Vec<KeyBinding> {
    vec![
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
        // VS Code's Markdown keys, where the native window has the keyboard: the source and split views' editor. There
        // Ctrl+K only starts Ctrl+K V, as in 3.x; the AI edit is not offered for Markdown (the palette leaves it out
        // too). The `NoAction` comes after the AI edit's Ctrl+K at the same depth, so it wins and the chord waits for V.
        KeyBinding::new("ctrl-shift-v", MarkdownPreview, Some(MARKDOWN_TAB)),
        KeyBinding::new("ctrl-k v", MarkdownSplit, Some(MARKDOWN_TAB)),
        KeyBinding::new("ctrl-k", NoAction, Some("MarkdownTab > Input")),
    ]
}

/// Choose a folder and open it as a project (3.x "open Windows folder"): Windows' own picker, its path mapped into WSL
/// with `wslpath` on a WSL host; on an SSH server, whose folders the Windows picker cannot reach, the in-app browser.
pub fn choose_folder(window: &mut Window, cx: &mut App) {
    pick_folder(false, window, cx);
}

/// Choose a folder and add it to the shown project (3.x "add folder to workspace").
pub fn choose_folder_to_attach(window: &mut Window, cx: &mut App) {
    pick_folder(true, window, cx);
}

/// Browse the execution host's own folders in the app (3.x "browse folders on current host").
pub fn browse_folder(window: &mut Window, cx: &mut App) {
    folder_dialog::open(false, window, cx);
}

fn adopt(path: String, attach: bool, window: &mut Window, cx: &mut App) {
    Ide::global(cx).update(cx, |ide, cx| if attach { ide.attach_root(path, window, cx) } else { ide.open_folder(path, window, cx) });
}

fn pick_folder(attach: bool, window: &mut Window, cx: &mut App) {
    // Debug builds under test take the folder from the environment instead of a picker over the tester's windows.
    if cfg!(debug_assertions)
        && let Some(path) = std::env::var_os("RAINY_QA_PICK_FOLDER")
    {
        adopt(path.to_string_lossy().into_owned(), attach, window, cx);
        return;
    }
    let target = Store::global(cx).read(cx).target.clone();
    let distro = match &target {
        Some(ExecutionTarget::Ssh { .. }) => {
            folder_dialog::open(attach, window, cx);
            return;
        }
        Some(ExecutionTarget::Wsl { distro, .. }) => Some(distro.clone()),
        _ => None,
    };
    let chosen = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(tr(cx, if attach { "ide.ideAddFolder" } else { "ide.ideOpenFolder" })),
    });
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let path = match distro {
                Some(distro) => {
                    let mapped = cx.background_spawn(async move { rainy_carrier::wsl::linux_paths(&distro, &[path.as_path()]) }).await;
                    match mapped.map(|mut paths| paths.remove(0)) {
                        Ok(path) => path,
                        Err(error) => {
                            let _ = cx.update(|window, cx| {
                                use gpui_kit::component::WindowExt as _;
                                window.push_notification(gpui_kit::component::notification::Notification::error(error.to_string()).title(tr(cx, "ide.ideWslPathFailed")), cx);
                            });
                            return;
                        }
                    }
                }
                None => path.to_string_lossy().into_owned(),
            };
            let _ = cx.update(|window, cx| adopt(path, attach, window, cx));
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

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    /// The actions `keys` runs with the keyboard in `contexts` (outermost first), and whether more keys could follow.
    fn resolve(keys: &str, contexts: &[&str]) -> (Vec<&'static str>, bool) {
        let keymap = Keymap::new(key_bindings());
        let input: Vec<Keystroke> = keys.split(' ').map(|key| Keystroke::parse(key).expect("a keystroke")).collect();
        let contexts: Vec<KeyContext> = contexts.iter().map(|context| KeyContext::parse(context).expect("a context")).collect();
        let (bindings, pending) = keymap.bindings_for_input(&input, &contexts);
        (bindings.iter().map(|binding| binding.action().name()).collect(), pending)
    }

    #[test]
    fn markdown_tabs_take_the_vs_code_preview_keys() {
        let markdown = ["Root", "MarkdownTab", "Input mode=editor"];
        assert_eq!(resolve("ctrl-shift-v", &markdown), (vec!["rainy::MarkdownPreview"], false));
        // Ctrl+K waits for the V, with nothing to run in the meantime.
        assert_eq!(resolve("ctrl-k", &markdown), (vec![], true));
        assert_eq!(resolve("ctrl-k v", &markdown), (vec!["rainy::MarkdownSplit"], false));
        // The tab's toolbar has the keys too.
        assert_eq!(resolve("ctrl-k v", &["Root", "MarkdownTab"]), (vec!["rainy::MarkdownSplit"], false));
    }

    #[test]
    fn other_editors_keep_ctrl_k_for_the_ai_edit() {
        let code = ["Root", "Input mode=editor"];
        assert_eq!(resolve("ctrl-k", &code), (vec!["rainy::AiEditSelection"], false));
        assert_eq!(resolve("ctrl-shift-v", &code), (vec![], false));
        assert_eq!(resolve("ctrl-shift-v", &["Root", "Input mode=textarea"]), (vec![], false));
    }
}
