//! The status bar's part about the active file: cursor line and column, encoding and line endings, and the word count of
//! a Markdown file. The position is left out while a page stands in for the source editor (the rich Markdown view, a
//! preview, an SVG picture), where the editor's cursor is hidden.

use std::time::Duration;

use gpui_kit::component::input::{EditorState, InputEvent};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::store::ide::Ide;
use crate::boot::grouped;
use crate::ui::ide::viewers;
use crate::ui::ide::word_count::{WordCount, count_words};
use crate::ui::{tr, trf};

pub struct CursorStatus {
    ide: Entity<Ide>,
    editor: Option<Entity<EditorState>>,
    /// Last rendered cursor (line, character) so non-cursor editor notifications do not repaint.
    last_cursor: Option<(u32, u32)>,
    /// Whether the active file's source editor is on screen.
    source_shown: bool,
    /// Whether the active file is Markdown, whose words are counted.
    markdown: bool,
    words: Option<WordCount>,
    /// The count a moment after the text last changed.
    counting: Option<Task<()>>,
    watch: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl CursorStatus {
    pub fn new(ide: Entity<Ide>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            // Tracks active tab, editor load completion, and Markdown view mode switches.
            cx.observe(&ide, |this, _, cx| this.follow(cx)),
        ];
        let mut status = Self {
            ide,
            editor: None,
            last_cursor: None,
            source_shown: true,
            markdown: false,
            words: None,
            counting: None,
            watch: Vec::new(),
            _subscriptions: subscriptions,
        };
        status.follow(cx);
        status
    }

    /// Watch the active file's editor.
    fn follow(&mut self, cx: &mut Context<Self>) {
        let ide = self.ide.read(cx);
        let editor = ide.active.as_ref().filter(|key| !ide.diff_tabs.contains(*key)).and_then(|key| ide.files.get(key)).and_then(|file| file.editor.clone());
        let changed = editor.as_ref().map(Entity::entity_id) != self.editor.as_ref().map(Entity::entity_id);
        let mut dirty = false;
        if changed {
            self.last_cursor = editor.as_ref().map(|ed| {
                let pos = ed.read(cx).cursor_position();
                (pos.line, pos.character)
            });
            self.watch = editor
                .as_ref()
                .map(|editor| {
                    vec![
                        // Only text edits trigger the debounced Markdown word count.
                        cx.subscribe(editor, |this, _, event: &InputEvent, cx| {
                            if matches!(event, InputEvent::Change) {
                                this.count_soon(cx);
                            }
                        }),
                        // Repaint only when the cursor coordinates actually move.
                        cx.observe(editor, |this, editor, cx| {
                            let pos = editor.read(cx).cursor_position();
                            let pair = (pos.line, pos.character);
                            if this.last_cursor != Some(pair) {
                                this.last_cursor = Some(pair);
                                cx.notify();
                            }
                        }),
                    ]
                })
                .unwrap_or_default();
            self.editor = editor;
            dirty = true;
        }
        let ide = self.ide.read(cx);
        let active = ide.active.as_ref().and_then(|key| ide.files.get(key).map(|file| (key, file)));
        let markdown = active.is_some_and(|(_, file)| viewers::preview_kind(&file.document.path) == Some("markdown"));
        // A page in place of the editor (rich Markdown, a preview, an SVG picture) hides its cursor; a split shows both.
        let source_shown = active.is_none_or(|(key, file)| {
            viewers::page_view(&file.document.path, file.document.read_only_reason.as_deref(), ide.views.get(key).map(String::as_str)).is_none_or(|(_, split)| split)
        });
        if source_shown != self.source_shown {
            self.source_shown = source_shown;
            dirty = true;
        }
        if changed || markdown != self.markdown {
            self.markdown = markdown;
            self.words = None;
            self.count_soon(cx);
            dirty = true;
        }
        if dirty {
            cx.notify();
        }
    }

    /// Count the words of the active Markdown file once its text has rested for a moment, away from the interface thread.
    fn count_soon(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone().filter(|_| self.markdown) else {
            self.counting = None;
            return;
        };
        self.counting = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let text = cx.update(|cx| editor.read(cx).value());
            let words = cx.background_spawn(async move { count_words(&text) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.words != Some(words) {
                    this.words = Some(words);
                    cx.notify();
                }
            });
        }));
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
        let eol: SharedString = match eol.as_str() {
            "crlf" => "CRLF".into(),
            "mixed" => tr(cx, "ide.ideStatusEolMixed"),
            _ => "LF".into(),
        };
        let words = self.words.filter(|_| self.markdown).map(|words| {
            let tooltip = trf(
                cx,
                "viewers.wordCountTitle",
                &[("words", &grouped(words.words)), ("characters", &grouped(words.characters)), ("minutes", &words.minutes)],
            );
            div()
                .id("word-count")
                .child(trf(cx, "viewers.wordCount", &[("count", &grouped(words.words))]))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
        });
        h_flex()
            .gap_3()
            .text_color(theme.muted_foreground)
            .children(words)
            .when(self.source_shown, |this| {
                this.child(trf(cx, "ide.ideStatusPosition", &[("line", &(position.line + 1)), ("column", &(position.character + 1))]))
            })
            .child(encoding)
            .child(eol)
    }
}
