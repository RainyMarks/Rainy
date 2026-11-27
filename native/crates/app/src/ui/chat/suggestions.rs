//! Slash commands, `@` file/session completion popup (properly debounced) and keyboard interception.

use std::time::{Duration, Instant};

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::json;

use super::ChatPane;
use super::drafts::{NewImage, report_outcome};
use crate::i18n::{self, Locale};
use crate::store::Store;
use crate::ui::tr;

/// One entry of the composer's suggestion list.
#[derive(Clone)]
pub(super) enum Suggestion {
    Command { name: &'static str, description: &'static str },
    /// A command of the chat's external agent; the text goes to it as `/name …`. `input`: it takes text after it.
    Agent { name: String, description: Option<String>, input: bool },
    Completion { kind: String, insert: String, label: String, detail: Option<String> },
}

/// Safely replace `text[start..caret]` with `@<insert>` (plus optional trailing space) and return `(next, line, character)`.
/// Returns `None` if `start`/`caret` are out of bounds, inverted, or not on UTF-8 character boundaries.
pub(super) fn apply_reference_completion(text: &str, start: usize, caret: usize, insert: &str, continues: bool) -> Option<(String, u32, u32)> {
    if start > caret || caret > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(caret) {
        return None;
    }
    let inserted = format!("@{insert}{}", if continues { "" } else { " " });
    let prefix = &text[..start];
    let suffix = &text[caret..];
    let mut next = String::with_capacity(prefix.len() + inserted.len() + suffix.len());
    next.push_str(prefix);
    next.push_str(&inserted);
    let cursor_end = next.len();
    next.push_str(suffix);
    let before = &next[..cursor_end];
    let line = before.matches('\n').count() as u32;
    let character = before.rsplit('\n').next().unwrap_or_default().chars().count() as u32;
    Some((next, line, character))
}

impl ChatPane {
    /// Dismiss suggestions and invalidate any in-flight completion RPC so stale replies never pop back up or auto-insert.
    pub(super) fn cancel_suggestions(&mut self) {
        self.trigger = None;
        self.suggestions.clear();
        self.completion_task = None;
        self.completion_seq = self.completion_seq.wrapping_add(1);
        self.accept_shown = false;
    }

    /// Work out what the text before the caret asks to complete, and fetch suggestions for it.
    pub(super) fn update_trigger(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.composer.read(cx);
        let text = state.value().to_string();
        let has_text = !text.trim().is_empty();
        let readiness_changed = self.composer_has_text != has_text;
        self.composer_has_text = has_text;
        let caret = state.cursor().min(text.len());
        let trigger = text.is_char_boundary(caret).then(|| crate::ui::composer::find_trigger(&text[..caret])).flatten();
        if trigger == self.trigger {
            if readiness_changed {
                cx.notify();
            }
            return;
        }
        if self.trigger.is_none() && trigger.is_some() {
            self.suggestions_gen = self.suggestions_gen.wrapping_add(1);
        }
        self.trigger = trigger.clone();
        self.accept_shown = false;
        self.active_suggestion = 0;
        self.suggestions_scroll.scroll_to_item(0);
        match trigger {
            None => {
                self.cancel_suggestions();
            }
            Some(crate::ui::composer::Trigger::Command { query }) => {
                self.completion_task = None;
                self.completion_seq = self.completion_seq.wrapping_add(1);
                let has_chat = self.session_id(cx).is_some();
                let query = query.to_lowercase();
                // An external agent's chat: Rainy's /new, then the agent's own commands (Antigravity's /swarm, /teamwork…).
                let external = self.external_kind(cx);
                let mut suggestions: Vec<Suggestion> = crate::ui::composer::COMMANDS
                    .iter()
                    .filter(|(name, _)| name.starts_with(&query) && if external.is_some() { *name == "new" } else { *name != "compact" || has_chat })
                    .map(|(name, description)| Suggestion::Command { name, description })
                    .collect();
                if let Some(kind) = &external {
                    suggestions.extend(
                        external_commands(self.store.read(cx), kind)
                            .into_iter()
                            .filter(|command| command.name.to_lowercase().starts_with(&query))
                            .map(|command| Suggestion::Agent { input: command.hint.is_some(), name: command.name, description: command.description }),
                    );
                }
                self.suggestions = suggestions;
            }
            Some(crate::ui::composer::Trigger::Reference { query, .. }) => {
                self.completion_seq = self.completion_seq.wrapping_add(1);
                let seq = self.completion_seq;
                let mut params = json!({ "query": query });
                if let Some(session) = self.session_id(cx) {
                    params["sessionId"] = json!(session);
                }
                params["workspaceId"] = json!(crate::store::ide::Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone()));
                let store = self.store.clone();
                // True debounce: wait 80 ms before firing the Host RPC so rapid typing only issues one completion request.
                self.completion_task = Some(cx.spawn_in(window, async move |this: WeakEntity<Self>, cx: &mut AsyncWindowContext| {
                    cx.background_executor().timer(Duration::from_millis(80)).await;
                    let still_current = this.read_with(cx, |this, _| this.completion_seq == seq).unwrap_or(false);
                    if !still_current {
                        return;
                    }
                    let call = store.read_with(cx, |store, _| store.call("chat.complete", params));
                    let Ok(items) = call.await else { return };
                    let _ = this.update_in(cx, |this, window, cx| {
                        if this.completion_seq != seq {
                            return;
                        }
                        this.shown_seq = seq;
                        this.suggestions = items
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|item| {
                                Some(Suggestion::Completion {
                                    kind: item["kind"].as_str()?.to_owned(),
                                    insert: item["insert"].as_str()?.to_owned(),
                                    label: item["label"].as_str()?.to_owned(),
                                    detail: item["detail"].as_str().map(str::to_owned),
                                })
                            })
                            .collect();
                        this.active_suggestion = 0;
                        this.suggestions_scroll.scroll_to_item(0);
                        if std::mem::take(&mut this.accept_shown) && !this.suggestions.is_empty() {
                            this.accept_suggestion(0, false, window, cx);
                        }
                        cx.notify();
                    });
                }));
            }
        }
        cx.notify();
    }

    /// Tab (`run` false) or Enter on the open list. While it still shows what an earlier `@` query found, the first
    /// file of the query typed goes in once it arrives, never one of the earlier files.
    pub(super) fn take_suggestion(&mut self, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        let reference = matches!(self.trigger, Some(crate::ui::composer::Trigger::Reference { .. }));
        if reference && self.shown_seq != self.completion_seq {
            self.accept_shown = true;
        } else {
            self.accept_suggestion(self.active_suggestion, run, window, cx);
        }
    }

    /// Put a suggestion into the text: a command is written out by Tab and runs on Enter (`run`), a reference
    /// replaces what was typed after `@`.
    pub(super) fn accept_suggestion(&mut self, index: usize, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(suggestion) = self.suggestions.get(index).cloned() else { return };
        match suggestion {
            Suggestion::Command { name, .. } if run && !crate::ui::composer::takes_text(name) => self.run_command(name, window, cx),
            Suggestion::Command { name, .. } => {
                // A command that takes text (`/swarm <goal>`) is written out with a space for it.
                let text = if crate::ui::composer::takes_text(name) { format!("/{name} ") } else { format!("/{name}") };
                let character = text.chars().count() as u32;
                self.composer.update(cx, |state, cx| {
                    state.set_value(text, window, cx);
                    state.set_cursor_position(gpui_kit::component::input::Position { line: 0, character }, window, cx);
                });
                self.update_trigger(window, cx);
            }
            Suggestion::Agent { name, input, .. } => {
                // Sent at once by Enter when it takes nothing after it, else written out with a space for the text.
                let now = run && !input;
                let text = if now { format!("/{name}") } else { format!("/{name} ") };
                let character = text.chars().count() as u32;
                self.composer.update(cx, |state, cx| {
                    state.set_value(text, window, cx);
                    state.set_cursor_position(gpui_kit::component::input::Position { line: 0, character }, window, cx);
                });
                if now {
                    self.cancel_suggestions();
                    self.send(false, window, cx);
                } else {
                    self.update_trigger(window, cx);
                }
            }
            Suggestion::Completion { kind, insert, .. } => {
                let Some(crate::ui::composer::Trigger::Reference { start, .. }) = self.trigger.clone() else { return };
                let state = self.composer.read(cx);
                let text = state.value().to_string();
                let caret = state.cursor().min(text.len());
                // A folder keeps the list open for its contents.
                let continues = kind == "directory";
                let Some((next, line, character)) = apply_reference_completion(&text, start, caret, &insert, continues) else {
                    self.cancel_suggestions();
                    self.update_trigger(window, cx);
                    return;
                };
                self.composer.update(cx, |state, cx| {
                    state.set_value(next, window, cx);
                    state.set_cursor_position(gpui_kit::component::input::Position { line, character }, window, cx);
                });
                if !continues {
                    self.cancel_suggestions();
                }
                self.update_trigger(window, cx);
            }
        }
    }

    /// `/compact` and `/new`; `/swarm` alone asks for its goal.
    pub(super) fn run_command(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if crate::ui::composer::takes_text(name) {
            let text = format!("/{name} ");
            let character = text.chars().count() as u32;
            self.composer.update(cx, |state, cx| {
                state.set_value(text, window, cx);
                state.set_cursor_position(gpui_kit::component::input::Position { line: 0, character }, window, cx);
            });
            self.composer_has_text = true;
            self.cancel_suggestions();
            cx.notify();
            return;
        }
        self.composer.update(cx, |state, cx| state.set_value("", window, cx));
        self.composer_has_text = false;
        self.cancel_suggestions();
        match name {
            "compact" => {
                if let Some(session) = self.session_id(cx) {
                    report_outcome(self.store.read(cx).call("chat.compact", json!({ "sessionId": session })), window, cx);
                }
            }
            _ => self.store.update(cx, |store, cx| store.select(None, cx)),
        }
        cx.notify();
    }

    /// Keys taken before the text area sees them, while it has the cursor.
    pub(super) fn intercept(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        if !self.composer.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        let modifiers = keystroke.modifiers;
        let open = !self.suggestions.is_empty() && self.trigger.is_some();
        match keystroke.key.as_str() {
            "up" | "down" if open && !modifiers.modified() => {
                let count = self.suggestions.len();
                self.active_suggestion = if keystroke.key == "down" { (self.active_suggestion + 1) % count } else { (self.active_suggestion + count - 1) % count };
                // The highlighted row stays in view as the arrows move past the list's height.
                self.suggestions_scroll.scroll_to_item(self.active_suggestion);
                cx.stop_propagation();
                cx.notify();
            }
            "tab" if open && !modifiers.modified() => {
                self.take_suggestion(false, window, cx);
                cx.stop_propagation();
            }
            "escape" if open => {
                self.cancel_suggestions();
                cx.stop_propagation();
                cx.notify();
            }
            "escape" if self.busy(cx) => {
                // Esc twice in a row stops the run.
                let now = Instant::now();
                if self.last_escape.is_some_and(|last| now.duration_since(last) < Duration::from_millis(800)) {
                    self.last_escape = None;
                    if let Some(session) = self.session_id(cx) {
                        self.call("chat.abort", json!({ "sessionId": session }), window, cx);
                    }
                } else {
                    self.last_escape = Some(now);
                }
            }
            "v" if modifiers.control && !modifiers.alt => {
                // A pasted image is attached; text pastes as usual.
                let image = cx.read_from_clipboard().and_then(|item| {
                    item.entries().iter().find_map(|entry| match entry {
                        ClipboardEntry::Image(image) => Some(image.clone()),
                        _ => None,
                    })
                });
                if let Some(image) = image {
                    // Windows hands over a screenshot as a BMP; `encode_image` types and converts it.
                    self.attach_image(tr(cx, "chat.clipboardImage").to_string(), NewImage::Bytes(image.bytes.clone()), None, window, cx);
                    cx.stop_propagation();
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_suggestions(&self, cx: &mut Context<Self>) -> AnyElement {
        let (radius_lg, radius, border, popover, list_active, list_hover, muted_foreground) = {
            let theme = cx.theme();
            (theme.radius_lg, theme.radius, theme.border, theme.popover, theme.list_active, theme.list_hover, theme.muted_foreground)
        };
        let active = self.active_suggestion;
        let container = div()
            .relative()
            .flex_shrink_0()
            .mx_3()
            .mb_1()
            .child(
                canvas(|bounds, _, cx| crate::web::mark_overlay_bounds(bounds, cx), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .child(
                v_flex()
                    .id("suggestions")
                    .p_1()
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .track_scroll(&self.suggestions_scroll)
                    .rounded(radius_lg)
                    .border_1()
                    .border_color(border)
                    .bg(popover)
                    .shadow_md()
                    .children(self.suggestions.iter().enumerate().map(|(index, suggestion)| {
                        let (label, detail, icon) = match suggestion {
                            Suggestion::Command { name, description } => (format!("/{name}"), Some(tr(cx, description).to_string()), IconName::SquareTerminal),
                            Suggestion::Agent { name, description, .. } => (format!("/{name}"), description.clone(), IconName::SquareTerminal),
                            Suggestion::Completion { kind, label, detail, .. } => (
                                label.clone(),
                                detail.clone(),
                                match kind.as_str() {
                                    "directory" => IconName::Folder,
                                    "session" => IconName::Inbox,
                                    _ => crate::ui::ide::file_icon(label),
                                },
                            ),
                        };
                        h_flex()
                            .id(("suggestion", index))
                            .h(px(28.))
                            .flex_shrink_0()
                            .px_2()
                            .gap_2()
                            .items_center()
                            .rounded(radius)
                            .text_sm()
                            .cursor_pointer()
                            .when(index == active, |this| this.bg(list_active))
                            .hover(move |style| style.bg(list_hover))
                            .child(Icon::new(icon).small().text_color(muted_foreground))
                            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
                            .when_some(detail, |this, detail| {
                                this.child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(muted_foreground).child(detail))
                            })
                            .on_click(cx.listener(move |this, _, window, cx| this.accept_suggestion(index, true, window, cx)))
                    })),
            )
            .vertical_scrollbar(&self.suggestions_scroll);
        if cx.reduce_motion() {
            container.into_any_element()
        } else {
            let anim_id = ElementId::Name(format!("chat-suggestions-enter-{}", self.suggestions_gen).into());
            container
                .with_animation(
                    anim_id,
                    Animation::new(Duration::from_millis(130)).with_easing(gpui_kit::base::animation::ease_out_cubic),
                    |el, delta| el.opacity(delta).top(px(5.0 * (1.0 - delta))),
                )
                .into_any_element()
        }
    }
}

/// The commands an external agent offers, as it last listed them; Antigravity's swarm and teamwork before it has.
fn external_commands(store: &Store, kind: &str) -> Vec<rainy_protocol::ExternalCommand> {
    let listed = store.external_agents.iter().find(|agent| agent.kind == kind).map(|agent| agent.commands.clone()).unwrap_or_default();
    if listed.is_empty() && kind == "antigravity" { antigravity_fallback(store.locale()) } else { listed }
}

/// Antigravity's `/swarm` and `/teamwork` (antigravity.google/docs/swarm, /teamwork), offered before it lists its commands.
pub(super) fn antigravity_fallback(locale: Locale) -> Vec<rainy_protocol::ExternalCommand> {
    [("swarm", "chat.commandAgySwarm"), ("teamwork", "chat.commandAgyTeamwork")]
        .into_iter()
        .map(|(name, key)| rainy_protocol::ExternalCommand { name: name.into(), description: Some(i18n::text(locale, key).to_owned()), hint: Some(i18n::text(locale, "chat.commandAgyHint").to_owned()) })
        .collect()
}
