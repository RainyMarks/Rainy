//! What the code editor does beyond typing (`IdeEditor.tsx` actions): go to a line, format the document, compare it
//! with the saved text, send the selection to the AI, find the references of a symbol, rename it, and the right-click
//! menu that offers them.

use std::collections::BTreeMap;

use gpui_kit::component::input::{Copy, Cut, EditorState, GoToDefinition, Paste, RopeExt as _, SelectAll};
use gpui_kit::component::native_menu::NativeMenu;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use lsp_types::{Location, OneOf, TextEdit, WorkspaceEdit};
use rainy_carrier::lsp::uri_path;
use serde_json::json;

use crate::store::ide::{Ide, language_of, split_key};
use crate::store::language::{LanguageServers, utf16_position};
use crate::ui::ide::{AiEditSelection, FindReferences, FormatDocument, RenameSymbol, SendSelection, ToggleBreakpoint};
use crate::ui::prompt::prompt_text;
use crate::ui::{tr, trf};

/// The shown file and its code editor.
pub fn active_editor(cx: &App) -> Option<(String, Entity<EditorState>)> {
    let ide = Ide::global(cx);
    let ide = ide.read(cx);
    let key = ide.active.clone()?;
    let editor = ide.files.get(&key)?.editor.clone()?;
    Some((key, editor))
}

fn fail(message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    window.push_notification(Notification::error(message), cx);
}

/// Ask for a line (and optionally `line:column`) and put the cursor there (3.x Ctrl+G).
pub fn go_to_line(window: &mut Window, cx: &mut App) {
    let Some((key, editor)) = active_editor(cx) else { return };
    let line = editor.read(cx).cursor_position().line + 1;
    prompt_text(
        tr(cx, "ide.ideGoLine"),
        line.to_string(),
        tr(cx, "native.ok"),
        move |text, window, cx| {
            let mut parts = text.split([':', ',', ' ']).filter(|part| !part.is_empty()).map(|part| part.trim().parse::<u32>().ok());
            let Some(Some(line)) = parts.next() else { return };
            let column = parts.next().flatten().unwrap_or(1);
            let key = key.clone();
            Ide::global(cx).update(cx, |ide, cx| ide.reveal(key, line.saturating_sub(1), column.saturating_sub(1), window, cx));
        },
        window,
        cx,
    );
}

/// Format the document with its language server (Shift+Alt+F).
pub fn format_document(window: &mut Window, cx: &mut App) {
    let Some((key, editor)) = active_editor(cx) else { return };
    let text = editor.read(cx).text().clone();
    let request = LanguageServers::global(cx).update(cx, |servers, _| {
        servers.document_request(&key, &text, "textDocument/formatting", json!({ "options": { "tabSize": 4, "insertSpaces": true } }))
    });
    let Some(request) = request else {
        fail(tr(cx, "native.noFormatter"), window, cx);
        return;
    };
    window
        .spawn(cx, async move |cx| {
            let result = request.await;
            let _ = cx.update(|window, cx| match result {
                Ok(value) => {
                    let edits: Vec<TextEdit> = serde_json::from_value(value).unwrap_or_default();
                    // Edits for an older text would land in the wrong places.
                    if edits.is_empty() || editor.read(cx).text().to_string() != text.to_string() {
                        return;
                    }
                    editor.update(cx, |state, cx| state.apply_lsp_edits(&edits, window, cx));
                }
                Err(error) => fail(error.to_string(), window, cx),
            });
        })
        .detach();
}

/// Show the unsaved edits as a comparison with the saved text.
pub fn compare_with_saved(window: &mut Window, cx: &mut App) {
    let Some((key, editor)) = active_editor(cx) else { return };
    let current = editor.read(cx).value().to_string();
    let ide = Ide::global(cx);
    let Some(saved) = ide.read(cx).files.get(&key).map(|file| file.saved.clone()) else { return };
    ide.update(cx, |ide, cx| ide.show_review(key, saved, current, window, cx));
}

/// Ask the AI about the selected code (3.x `sendSelection`).
pub fn send_selection(window: &mut Window, cx: &mut App) {
    let Some((key, editor)) = active_editor(cx) else { return };
    let state = editor.read(cx);
    let range = state.selected_range();
    if range.is_empty() {
        return;
    }
    let text = state.text();
    let selected = text.slice(range.clone()).to_string();
    let (start, end) = (text.offset_to_point(range.start), text.offset_to_point(range.end));
    let column = |point: gpui_kit::component::input::Point, offset: usize| text.slice(text.line_start_offset(point.row)..offset).to_string().chars().count() + 1;
    let span = format!("{}:{}–{}:{}", start.row + 1, column(start, range.start), end.row + 1, column(end, range.end));
    let ide = Ide::global(cx).read(cx);
    let path = ide.language_target(&key).map(|target| target.path).unwrap_or_else(|| split_key(&key).1.to_owned());
    let language = language_of(&path);
    let message = trf(cx, "ide.ideSelectionMessage", &[("path", &path), ("range", &span), ("language", &language), ("text", &selected)]);
    crate::ui::ask_ai(message.to_string(), window, cx);
}

/// List where the symbol under the cursor is used; a click opens the place.
pub fn find_references(window: &mut Window, cx: &mut App) {
    let Some((key, editor)) = active_editor(cx) else { return };
    let (text, offset) = {
        let state = editor.read(cx);
        (state.text().clone(), state.cursor())
    };
    let position = utf16_position(&text, offset);
    let request = LanguageServers::global(cx).update(cx, |servers, _| {
        servers.document_request(&key, &text, "textDocument/references", json!({ "position": position, "context": { "includeDeclaration": true } }))
    });
    let Some(request) = request else { return };
    window
        .spawn(cx, async move |cx| {
            let result = request.await;
            let _ = cx.update(|window, cx| match result {
                Ok(value) => {
                    let locations: Vec<Location> = serde_json::from_value(value).unwrap_or_default();
                    show_references(locations, window, cx);
                }
                Err(error) => fail(error.to_string(), window, cx),
            });
        })
        .detach();
}

/// One place a symbol is used: the file, the zero-based line and column, and the line's text when it is open.
struct Reference {
    key: String,
    line: u32,
    column: u32,
    preview: String,
}

fn show_references(locations: Vec<Location>, window: &mut Window, cx: &mut App) {
    let ide = Ide::global(cx);
    let references: Vec<Reference> = {
        let ide = ide.read(cx);
        locations
            .into_iter()
            .filter_map(|location| {
                let key = ide.key_from_absolute(&uri_path(location.uri.as_str())?)?;
                let start = location.range.start;
                let preview = ide
                    .files
                    .get(&key)
                    .and_then(|file| file.editor.as_ref())
                    .map(|editor| {
                        let text = editor.read(cx).text();
                        let row = start.line as usize;
                        if row < text.lines_len() { text.slice(text.line_start_offset(row)..text.line_end_offset(row)).to_string().trim().to_owned() } else { String::new() }
                    })
                    .unwrap_or_default();
                Some(Reference { key, line: start.line, column: start.character, preview })
            })
            .collect()
    };
    if references.is_empty() {
        window.push_notification(Notification::info(tr(cx, "native.noReferences")), cx);
        return;
    }
    let references = std::rc::Rc::new(references);
    let title = format!("{} ({})", tr(cx, "ide.ideFindReferences"), references.len());
    window.open_dialog(cx, move |dialog, _, cx| {
        let theme = cx.theme().clone();
        let rows = references.iter().enumerate().map(|(index, reference)| {
            let (key, line, column) = (reference.key.clone(), reference.line, reference.column);
            h_flex()
                .id(("reference", index))
                .px_2()
                .py_1()
                .gap_3()
                .rounded(theme.radius)
                .text_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme.list_hover))
                .child(div().flex_shrink_0().text_color(theme.muted_foreground).child(format!("{}:{}", split_key(&reference.key).1, reference.line + 1)))
                .child(div().flex_1().min_w_0().truncate().font_family(theme.mono_font_family.clone()).child(reference.preview.clone()))
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    let key = key.clone();
                    Ide::global(cx).update(cx, |ide, cx| ide.reveal(key, line, column, window, cx));
                })
        });
        dialog.title(title.clone()).w(px(640.)).child(v_flex().id("references").max_h(px(420.)).overflow_y_scrollbar().children(rows))
    });
}

/// The identifier around a byte offset.
fn word_at(text: &str, offset: usize) -> String {
    let offset = offset.min(text.len());
    let word = |c: char| c == '_' || c.is_alphanumeric();
    let start = text[..offset].char_indices().rev().take_while(|(_, c)| word(*c)).last().map_or(offset, |(index, _)| index);
    let end = text[offset..].char_indices().find(|(_, c)| !word(*c)).map_or(text.len(), |(index, _)| offset + index);
    text[start..end].to_owned()
}

/// Ask for a new name and rename the symbol under the cursor everywhere the server knows it.
pub fn rename_symbol(window: &mut Window, cx: &mut App) {
    let Some((key, editor)) = active_editor(cx) else { return };
    let (text, offset) = {
        let state = editor.read(cx);
        (state.text().clone(), state.cursor())
    };
    let current = word_at(&text.to_string(), offset);
    if current.is_empty() || !LanguageServers::global(cx).read(cx).has_server(&key) {
        return;
    }
    let position = utf16_position(&text, offset);
    prompt_text(
        tr(cx, "ide.ideRenameSymbol"),
        current.clone(),
        tr(cx, "ide.ideRename"),
        move |name, window, cx| {
            if name == current {
                return;
            }
            let request = LanguageServers::global(cx).update(cx, |servers, _| {
                servers.document_request(&key, &text, "textDocument/rename", json!({ "position": position, "newName": name }))
            });
            let Some(request) = request else { return };
            window
                .spawn(cx, async move |cx| {
                    let result = request.await;
                    let _ = cx.update(|window, cx| match result {
                        Ok(value) => match serde_json::from_value::<Option<WorkspaceEdit>>(value) {
                            Ok(Some(edit)) => apply_workspace_edit(edit, window, cx),
                            Ok(None) => {}
                            Err(error) => fail(error.to_string(), window, cx),
                        },
                        Err(error) => fail(error.to_string(), window, cx),
                    });
                })
                .detach();
        },
        window,
        cx,
    );
}

/// Apply a server's edits to the files they name: open files at once, others once they are opened. The edits stay
/// unsaved for the user to review.
fn apply_workspace_edit(edit: WorkspaceEdit, window: &mut Window, cx: &mut App) {
    let mut by_uri: BTreeMap<String, Vec<TextEdit>> = BTreeMap::new();
    for (uri, edits) in edit.changes.unwrap_or_default() {
        by_uri.entry(uri.to_string()).or_default().extend(edits);
    }
    if let Some(lsp_types::DocumentChanges::Edits(documents)) = edit.document_changes {
        for document in documents {
            let edits = document.edits.into_iter().map(|edit| match edit {
                OneOf::Left(edit) => edit,
                OneOf::Right(annotated) => annotated.text_edit,
            });
            by_uri.entry(document.text_document.uri.to_string()).or_default().extend(edits);
        }
    }
    let ide = Ide::global(cx);
    for (uri, edits) in by_uri {
        let Some(key) = uri_path(&uri).and_then(|path| ide.read(cx).key_from_absolute(&path)) else { continue };
        let opened = ide.update(cx, |ide, cx| ide.open_task(key.clone(), false, window, cx));
        let ide = ide.clone();
        window
            .spawn(cx, async move |cx| {
                opened.await;
                let _ = cx.update(|window, cx| {
                    let editor = ide.read(cx).files.get(&key).and_then(|file| file.editor.clone());
                    if let Some(editor) = editor {
                        editor.update(cx, |state, cx| state.apply_lsp_edits(&edits, window, cx));
                    }
                });
            })
            .detach();
    }
}

/// The right-click menu of a code editor (3.x Monaco context menu).
pub fn context_menu(menu: NativeMenu, _: &mut Window, cx: &mut App) -> NativeMenu {
    let Some((key, editor)) = active_editor(cx) else { return menu };
    let has_server = LanguageServers::global(cx).read(cx).has_server(&key);
    let selection = !editor.read(cx).selected_range().is_empty();
    menu.menu_with_disabled(tr(cx, "ide.ideGotoDefinition"), !has_server, Box::new(GoToDefinition))
        .menu_with_disabled(tr(cx, "ide.ideFindReferences"), !has_server, Box::new(FindReferences))
        .menu_with_disabled(tr(cx, "ide.ideRenameSymbol"), !has_server, Box::new(RenameSymbol))
        .menu_with_disabled(tr(cx, "ide.ideFormat"), !has_server, Box::new(FormatDocument))
        .separator()
        .menu(tr(cx, "ide.ideToggleBreakpoint"), Box::new(ToggleBreakpoint))
        .menu(tr(cx, "app.cmdAiEdit"), Box::new(AiEditSelection))
        .menu_with_disabled(tr(cx, "ide.ideSendSelection"), !selection, Box::new(SendSelection))
        .separator()
        .menu_with_disabled(tr(cx, "native.cut"), !selection, Box::new(Cut))
        .menu_with_disabled(tr(cx, "native.copy"), !selection, Box::new(Copy))
        .menu(tr(cx, "native.paste"), Box::new(Paste))
        .menu(tr(cx, "native.selectAll"), Box::new(SelectAll))
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn finds_the_word_around_the_cursor() {
        assert_eq!(word_at("let total_sum = 1", 6), "total_sum");
        assert_eq!(word_at("let total_sum = 1", 13), "total_sum");
        assert_eq!(word_at("变量 = 1", 3), "变量");
        assert_eq!(word_at("a + b", 2), "");
    }
}
