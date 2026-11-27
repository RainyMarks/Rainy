//! Ctrl+K in the editor (`src/editor/ai.ts`): rewrite the selection, or insert at the cursor, for an instruction. The new
//! text goes into the editor at once, tinted, until the user accepts it or puts the original back.

use std::ops::Range;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{EditorState, Input, InputEvent, InputState, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle, RopeExt as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lsp_types::TextEdit;
use serde_json::{Value, json};

use crate::store::{Store, StoreEvent};
use crate::ui::tr;

/// Text sent around the selection (3.x `REWRITE_BEFORE`, `REWRITE_AFTER`), in characters.
const REWRITE_BEFORE: usize = 8000;
const REWRITE_AFTER: usize = 4000;

/// What the request is for.
pub struct Target {
    /// The open file's key.
    pub key: String,
    /// Workspace-relative path and editor language, shown to the model.
    pub path: String,
    pub language: String,
}

enum Phase {
    Prompt,
    Generating { request: String, text: String },
    /// The new text is in the editor; the original is kept to put back.
    Review { original: String },
    Failed(SharedString),
}

pub enum AiEditEvent {
    Closed,
}

pub struct AiEdit {
    store: Entity<Store>,
    pub key: String,
    path: String,
    language: String,
    editor: Entity<EditorState>,
    input: Entity<InputState>,
    /// The selection the edit replaces, as byte offsets when the edit started.
    range: Range<usize>,
    /// Incremented whenever the editor changes outside `AiEdit`'s own apply/restore so stale async results cannot corrupt edits.
    doc_version: u64,
    applying: bool,
    phase: Phase,
    /// The tint over the new text while it is reviewed.
    tint: Option<RangeDecorationCollection>,
    next_request: u64,
    task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AiEditEvent> for AiEdit {}

impl AiEdit {
    pub fn new(store: Entity<Store>, target: Target, editor: Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "ide.ideAiEditPlaceholder")));
        input.update(cx, |state, cx| state.focus(window, cx));
        let range = editor.read(cx).selected_range();
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { secondary, .. } = event {
                    if *secondary {
                        this.accept(window, cx);
                    } else {
                        this.generate(window, cx);
                    }
                }
            }),
            cx.subscribe(&editor, |this, _, event: &InputEvent, _| {
                if matches!(event, InputEvent::Change) && !this.applying {
                    this.doc_version = this.doc_version.wrapping_add(1);
                }
            }),
            cx.subscribe(&store, |this, _, event: &StoreEvent, cx| {
                if let StoreEvent::Host { name, data } = event
                    && name == "inline.progress"
                    && let Phase::Generating { request, text } = &mut this.phase
                    && data.get("requestId").and_then(Value::as_str) == Some(request.as_str())
                {
                    *text = data.get("text").and_then(Value::as_str).unwrap_or_default().to_owned();
                    cx.notify();
                }
            }),
        ];
        Self {
            store,
            key: target.key,
            path: target.path,
            language: target.language,
            editor,
            input,
            range,
            doc_version: 0,
            applying: false,
            phase: Phase::Prompt,
            tint: None,
            next_request: 0,
            task: None,
            _subscriptions: subscriptions,
        }
    }

    /// Ask for the new text; while one is reviewed, the original comes back first.
    fn generate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let instruction = self.input.read(cx).value().trim().to_owned();
        if instruction.is_empty() || matches!(self.phase, Phase::Generating { .. }) {
            return;
        }
        self.restore(window, cx);
        let text = self.editor.read(cx).text().clone();
        let range = self.range.start.min(text.len())..self.range.end.min(text.len());
        let (before, _) = crate::store::language::window_around_cursor(&text, range.start, REWRITE_BEFORE, 0);
        let (_, after) = crate::store::language::window_around_cursor(&text, range.end, 0, REWRITE_AFTER);
        let selection = text.slice(range.clone()).to_string();
        let expected_version = self.doc_version;
        self.next_request += 1;
        let request = format!("edit-native-{}", self.next_request);
        let call = self.store.read(cx).call(
            "inline.edit",
            json!({ "requestId": request, "path": self.path, "language": self.language, "before": before, "selection": selection, "after": after, "instruction": instruction }),
        );
        self.phase = Phase::Generating { request, text: String::new() };
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.task = None;
                match result {
                    Ok(value) => {
                        let replacement = value.get("text").and_then(Value::as_str).unwrap_or_default().to_owned();
                        if replacement == selection {
                            this.phase = Phase::Failed(tr(cx, "ide.ideAiEditUnchanged"));
                        } else if this.doc_version != expected_version {
                            this.phase = Phase::Failed(tr(cx, "ide.ideConflict"));
                        } else {
                            this.apply(selection, replacement, window, cx);
                        }
                    }
                    // A stopped request ends quietly.
                    Err(_) if matches!(this.phase, Phase::Prompt) => {}
                    Err(error) => this.phase = Phase::Failed(crate::ui::error_text(cx, &error)),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Put the new text in place of the selection and tint it.
    fn apply(&mut self, original: String, replacement: String, window: &mut Window, cx: &mut Context<Self>) {
        let range = self.range.clone();
        let current_matches = {
            let text = self.editor.read(cx).text();
            range.end <= text.len() && text.slice(range.clone()) == original
        };
        if !current_matches {
            self.phase = Phase::Failed(tr(cx, "ide.ideConflict"));
            return;
        }
        let tint = cx.theme().success.opacity(0.18);
        self.applying = true;
        let collection = self.editor.update(cx, |state, cx| {
            let text = state.text().clone();
            let edit = TextEdit::new(lsp_types::Range::new(text.offset_to_position(range.start), text.offset_to_position(range.end)), replacement.clone());
            state.apply_lsp_edits(&vec![edit], window, cx);
            let inserted = range.start..range.start + replacement.len();
            state.create_range_decorations_collection(vec![RangeDecoration::new(inserted).with_style(RangeDecorationStyle::Fill).with_color(tint)], cx)
        });
        self.applying = false;
        self.tint = Some(collection);
        self.phase = Phase::Review { original };
    }

    /// The text the edit put in, where it is now.
    fn inserted(&self, cx: &App) -> Option<Range<usize>> {
        self.tint.as_ref()?.get_ranges(cx).into_iter().next()
    }

    /// Take the new text back out, putting the original in its place.
    fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Phase::Review { original } = std::mem::replace(&mut self.phase, Phase::Prompt) else { return };
        let inserted = self.inserted(cx);
        if let Some(tint) = self.tint.take() {
            tint.dispose(cx);
        }
        let Some(inserted) = inserted else { return };
        self.applying = true;
        self.editor.update(cx, |state, cx| {
            let text = state.text().clone();
            let edit = TextEdit::new(lsp_types::Range::new(text.offset_to_position(inserted.start), text.offset_to_position(inserted.end)), original.clone());
            state.apply_lsp_edits(&vec![edit], window, cx);
        });
        self.applying = false;
        self.range = inserted.start..inserted.start + original.len();
    }

    /// Keep the new text.
    pub fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Review { .. }) {
            return;
        }
        if let Some(tint) = self.tint.take() {
            tint.dispose(cx);
        }
        self.phase = Phase::Prompt;
        self.close(window, cx);
    }

    /// Stop a request; while one is reviewed, put the original back; then close.
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop(cx);
        self.restore(window, cx);
        self.close(window, cx);
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Phase::Generating { request, .. } = &self.phase {
            let cancel = self.store.read(cx).call("inline.cancel", json!({ "requestId": request }));
            cx.background_spawn(async move {
                let _ = cancel.await;
            })
            .detach();
            self.phase = Phase::Prompt;
        }
        self.task = None;
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| state.focus(window, cx));
        cx.emit(AiEditEvent::Closed);
    }
}

impl Render for AiEdit {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let generating = matches!(self.phase, Phase::Generating { .. });
        let reviewing = matches!(self.phase, Phase::Review { .. });
        let note: Option<AnyElement> = match &self.phase {
            Phase::Generating { text, .. } => Some(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .child(Spinner::new().xsmall())
                    .child(
                        div()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(text.lines().last().filter(|line| !line.trim().is_empty()).map(str::to_owned).unwrap_or_else(|| tr(cx, "ide.ideAiEditGenerating").to_string())),
                    )
                    .into_any_element(),
            ),
            Phase::Review { .. } => Some(div().text_color(theme.muted_foreground).child(tr(cx, "native.aiEditReview")).into_any_element()),
            Phase::Failed(message) => Some(div().text_color(theme.danger).child(message.clone()).into_any_element()),
            Phase::Prompt => None,
        };
        div()
            .id("ai-edit")
            .key_context("AiEdit")
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_1()
            .flex()
            .flex_col()
            .text_sm()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.dismiss(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_shrink_0().font_semibold().child(tr(cx, "ide.ideAiEdit")))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.input).small()))
                    .when(generating, |this| {
                        this.child(Button::new("ai-edit-stop").outline().xsmall().label(tr(cx, "ide.ideAiEditStop")).on_click(cx.listener(|this, _, _, cx| this.stop(cx))))
                    })
                    .when(!generating, |this| {
                        this.child(Button::new("ai-edit-generate").primary().xsmall().label(tr(cx, "ide.ideAiEditGenerate")).on_click(cx.listener(|this, _, window, cx| this.generate(window, cx))))
                    })
                    .when(reviewing, |this| {
                        this.child(Button::new("ai-edit-accept").outline().xsmall().label(tr(cx, "ide.ideAiEditAccept")).on_click(cx.listener(|this, _, window, cx| this.accept(window, cx))))
                    })
                    .child(
                        Button::new("ai-edit-reject")
                            .ghost()
                            .xsmall()
                            .label(if reviewing { tr(cx, "ide.ideAiEditReject") } else { tr(cx, "chat.cancel") })
                            .on_click(cx.listener(|this, _, window, cx| this.dismiss(window, cx))),
                    ),
            )
            .children(note)
    }
}
