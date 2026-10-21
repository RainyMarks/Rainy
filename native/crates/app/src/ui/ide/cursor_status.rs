//! The status bar's part about the active file: cursor line and column, encoding and line endings.

use gpui_kit::component::input::EditorState;
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::*;

use crate::store::ide::{Ide, IdeEvent};
use crate::ui::trf;

pub struct CursorStatus {
    ide: Entity<Ide>,
    editor: Option<Entity<EditorState>>,
    watch: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl CursorStatus {
    pub fn new(ide: Entity<Ide>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&ide, |this, _, event: &IdeEvent, cx| {
                if *event == IdeEvent::ActiveChanged {
                    this.follow(cx);
                }
            }),
            // An editor appears when its file has loaded.
            cx.observe(&ide, |this, _, cx| this.follow(cx)),
        ];
        let mut status = Self { ide, editor: None, watch: None, _subscriptions: subscriptions };
        status.follow(cx);
        status
    }

    /// Watch the active file's editor.
    fn follow(&mut self, cx: &mut Context<Self>) {
        let ide = self.ide.read(cx);
        let editor = ide.active.as_ref().filter(|key| !ide.diff_tabs.contains(*key)).and_then(|key| ide.files.get(key)).and_then(|file| file.editor.clone());
        if editor.as_ref().map(Entity::entity_id) != self.editor.as_ref().map(Entity::entity_id) {
            self.watch = editor.as_ref().map(|editor| cx.observe(editor, |_, _, cx| cx.notify()));
            self.editor = editor;
            cx.notify();
        }
    }
}

impl Render for CursorStatus {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(editor) = self.editor.clone() else { return h_flex() };
        let position = editor.read(cx).cursor_position();
        let ide = self.ide.read(cx);
        let document = ide.active.as_ref().and_then(|key| ide.files.get(key)).map(|file| (file.document.bom, file.document.eol.clone()));
        let (bom, eol) = document.unwrap_or((false, "lf".into()));
        let encoding = if bom { "UTF-8 BOM" } else { "UTF-8" };
        let eol = match eol.as_str() {
            "crlf" => "CRLF",
            "mixed" => "Mixed",
            _ => "LF",
        };
        h_flex()
            .gap_3()
            .text_color(theme.muted_foreground)
            .child(trf(cx, "ide.ideStatusPosition", &[("line", &(position.line + 1)), ("column", &(position.character + 1))]))
            .child(encoding)
            .child(eol)
    }
}
