//! Breakpoint and paused-line marks in the code editors: a red tint on each line with a breakpoint, a yellow one on
//! the line the debugger stopped at.

use std::collections::HashMap;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{EditorState, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle, RopeExt as _};
use gpui_kit::*;

use crate::store::execution::{Execution, ExecutionEvent};
use crate::store::ide::{Ide, IdeEvent};

pub struct DebugMarks {
    /// The marks of each open file, with the editor they are drawn in.
    marks: HashMap<String, (WeakEntity<EditorState>, RangeDecorationCollection)>,
    _subscriptions: Vec<Subscription>,
}

impl DebugMarks {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&Ide::global(cx), |this, _, event: &IdeEvent, cx| {
                if matches!(event, IdeEvent::FilesChanged | IdeEvent::ExecutionChanged | IdeEvent::WorkspaceChanged | IdeEvent::ActiveChanged) {
                    this.refresh(cx);
                }
            }),
            cx.subscribe(&Execution::global(cx), |this, _, event: &ExecutionEvent, cx| {
                if matches!(event, ExecutionEvent::DebugChanged) {
                    this.refresh(cx);
                }
            }),
        ];
        Self { marks: HashMap::new(), _subscriptions: subscriptions }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let ide = Ide::global(cx);
        let ide = ide.read(cx);
        let paused: Option<(String, u32)> = Execution::global(cx).read(cx).pause.as_ref().and_then(|pause| {
            let frame = pause.frames.iter().find(|frame| Some(frame.id) == pause.frame_id).or(pause.frames.first())?;
            Some((ide.key_from_absolute(frame.path.as_deref()?)?, frame.line.max(1) as u32))
        });
        let wanted: Vec<(String, Entity<EditorState>, Vec<u32>, Option<u32>)> = ide
            .files
            .iter()
            .filter_map(|(key, file)| {
                let editor = file.editor.clone()?;
                let here = paused.as_ref().filter(|(paused, _)| paused == key).map(|(_, line)| *line);
                Some((key.clone(), editor, ide.breakpoints(key), here))
            })
            .collect();
        let (breakpoint, current) = (cx.theme().danger.opacity(0.16), cx.theme().warning.opacity(0.32));
        let mut kept = HashMap::new();
        for (key, editor, lines, here) in wanted {
            let marks: Vec<RangeDecoration> = {
                let text = editor.read(cx).text().clone();
                let rows = text.lines_len();
                let line = |row: u32, color: Hsla| {
                    let row = (row as usize).saturating_sub(1);
                    (row < rows).then(|| {
                        let start = text.line_start_offset(row);
                        // An empty line still shows its mark: it covers the line break.
                        let end = text.line_end_offset(row).max((start + 1).min(text.len()));
                        RangeDecoration::new(start..end).with_style(RangeDecorationStyle::Fill).with_color(color)
                    })
                };
                lines.iter().filter_map(|row| line(*row, breakpoint)).chain(here.and_then(|row| line(row, current))).collect()
            };
            let known = self.marks.remove(&key).filter(|(shown, _)| shown.upgrade().as_ref() == Some(&editor));
            match known {
                Some((shown, collection)) => {
                    collection.set(marks, cx);
                    kept.insert(key, (shown, collection));
                }
                None if marks.is_empty() => {}
                None => {
                    let collection = editor.update(cx, |state, cx| state.create_range_decorations_collection(marks, cx));
                    kept.insert(key, (editor.downgrade(), collection));
                }
            }
        }
        for (_, (_, collection)) in self.marks.drain() {
            collection.dispose(cx);
        }
        self.marks = kept;
    }
}
