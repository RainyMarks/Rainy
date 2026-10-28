//! The AI pane: the selected chat's transcript, its approvals and queue, and the composer
//! (`ChatPane.tsx`, `Composer.tsx`, `ApprovalBar.tsx`, `ModelPicker.tsx`, `PermissionPicker.tsx`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::StyledExt as _;
use rainy_protocol::{ImageContent, ModelSelection, PendingApproval, QueuedMessage, RpcError};
use serde_json::{Value, json};

use crate::i18n::{self, Locale};
use crate::store::{ChatModel, Store, StoreEvent};
use crate::ui::composer::{ImageError, MAX_IMAGES};
use crate::ui::transcript::{Expanded, Fork, Toggle, TurnRow, TurnStyle, rebuild_rows, render_turn};
use crate::ui::{tr, trf};

/// One entry of the composer's suggestion list.
#[derive(Clone)]
enum Suggestion {
    Command { name: &'static str, description: &'static str },
    Completion { kind: String, insert: String, label: String, detail: Option<String> },
}

/// An image the next message carries: what is sent, and its thumbnail, decoded once when it was attached.
#[derive(Clone)]
struct Attachment {
    content: ImageContent,
    preview: Arc<Image>,
}

/// What the composer held for a chat while another one was shown.
#[derive(Default)]
struct Draft {
    text: String,
    attachments: Vec<Attachment>,
}

/// An image being attached: pasted bytes, or a file read off the interface thread.
enum NewImage {
    Bytes(Vec<u8>),
    File(PathBuf),
}

/// Where the reader was: a turn by key and how far into it. `previous` stands for the turn before it, which the
/// loaded window had cut short and only becomes whole once older entries arrive.
struct Anchor {
    key: SharedString,
    offset: Pixels,
    previous: bool,
}

/// The row above the turns of a chat whose older entries are not loaded.
#[derive(Clone, PartialEq)]
struct OlderRow {
    loading: bool,
    error: Option<SharedString>,
}

pub struct ChatPane {
    /// Images the next message carries.
    attachments: Vec<Attachment>,
    /// The composer's text and images of the chats not shown, by chat; `None` is the next new chat.
    drafts: HashMap<Option<String>, Draft>,
    /// The chat the composer's text and images belong to: always the chat shown, so a draft is never sent to
    /// another one.
    draft_key: Option<String>,
    /// A new chat is being created for a message; the new-chat composer sends nothing more until it exists.
    creating: bool,
    /// The mode new chats start in, from Settings (`security.status`).
    default_permission: String,
    trigger: Option<crate::ui::composer::Trigger>,
    suggestions: Vec<Suggestion>,
    active_suggestion: usize,
    suggestions_scroll: ScrollHandle,
    completion_seq: u64,
    /// The `completion_seq` whose files the list shows; an older one while a newer query is under way.
    shown_seq: u64,
    completion_task: Option<Task<()>>,
    /// Tab or Enter came while the list showed an older query's files: the first of the new ones goes in.
    accept_shown: bool,
    /// When Esc was pressed last while the chat ran; a second Esc soon after stops it.
    last_escape: Option<std::time::Instant>,
    /// Redraws a thinking timer.
    ticker: Option<Task<()>>,
    store: Entity<Store>,
    changes: Entity<crate::ui::changes::ChangesBar>,
    chat: Option<Entity<ChatModel>>,
    rows: Rc<Vec<TurnRow>>,
    /// Items of the list: the turns, after the load-older row when there is one (`header`).
    row_count: usize,
    header: usize,
    older: Option<OlderRow>,
    /// The chat's `loaded` count the rows were built from; another one means a new snapshot replaced it.
    loaded: u64,
    /// Where the reader was when older messages were asked for, to stay there once they arrive.
    anchor: Option<Anchor>,
    list: ListState,
    composer: Entity<TextareaState>,
    expanded: Expanded,
    /// The approval bar takes Enter (allow) and Esc (deny) while it has the focus; Allow itself never takes it.
    approval_focus: FocusHandle,
    /// The approval the bar shows, and whether its answer is on its way.
    approval_shown: Option<String>,
    answering: bool,
    /// The run error shows all of its text.
    error_open: bool,
    /// The reasoning stop under the pointer while the slider is held; it is sent once, on release.
    thinking_drag: Option<usize>,
    background_scroll: ScrollHandle,
    queue_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
    chat_subscription: Option<Subscription>,
}

/// The thinking levels and their labels.
const THINKING: [(&str, &str); 4] = [("off", "chat.thinkingOff"), ("low", "chat.thinkingLow"), ("high", "chat.thinkingHigh"), ("max", "chat.thinkingMax")];
const PERMISSIONS: [(&str, &str); 3] = [("read-only", "chat.permissionReadOnly"), ("ask", "chat.permissionAsk"), ("auto", "chat.permissionAuto")];
/// Heights of the lists above the composer, which scroll beyond them so the composer always keeps its place.
const MAX_QUEUE_HEIGHT: f32 = 100.;
const MAX_BACKGROUND_HEIGHT: f32 = 96.;
const MAX_ERROR_HEIGHT: f32 = 160.;
/// A run error longer than this may not fit three lines; it gets a details toggle.
const LONG_ERROR_CHARS: usize = 48;

impl ChatPane {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(600.));
        list.set_follow_mode(FollowMode::Tail);
        // The jump-to-latest button shows while the reader is scrolled up.
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |_, _, cx| {
            let weak = weak.clone();
            cx.defer(move |cx| {
                let _ = weak.update(cx, |_, cx| cx.notify());
            });
        });
        let placeholder = tr(cx, "chat.placeholder");
        let composer = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 12).submit_on_enter(true).placeholder(placeholder));
        let pane = cx.weak_entity();
        let subscriptions = vec![
            cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { secondary, .. } => {
                    // Enter takes the highlighted suggestion while the list is open.
                    if !this.suggestions.is_empty() && this.trigger.is_some() {
                        this.take_suggestion(true, window, cx);
                    } else {
                        this.send(*secondary, window, cx);
                    }
                }
                InputEvent::Change => this.update_trigger(window, cx),
                _ => {}
            }),
            // Keys the text area would otherwise take: the suggestion list's arrows, Esc, pasted images.
            cx.intercept_keystrokes(move |event, window, cx| {
                let _ = pane.update(cx, |this, cx| this.intercept(&event.keystroke, window, cx));
            }),
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| match event {
                StoreEvent::SelectionChanged => this.follow_selected(window, cx),
                StoreEvent::PrefsChanged => {
                    let placeholder = tr(cx, "chat.placeholder");
                    this.composer.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
                    // Preferences arrive after every (re)connect, when the default mode can be read too.
                    this.read_default_permission(cx);
                    this.rebuild(cx);
                }
                StoreEvent::Host { name, data } if name == "security.changed" => {
                    if let Some(mode) = data["defaultMode"].as_str() {
                        this.default_permission = mode.to_owned();
                        cx.notify();
                    }
                }
                StoreEvent::SummariesChanged | StoreEvent::Host { .. } | StoreEvent::HostExited(_) | StoreEvent::ResourcesDamaged(_) | StoreEvent::Update(_) => {}
            }),
            cx.observe(&store, |_, _, cx| cx.notify()),
        ];
        let changes = cx.new(|cx| crate::ui::changes::ChangesBar::new(store.clone(), cx));
        let mut pane = Self {
            attachments: Vec::new(),
            drafts: HashMap::new(),
            draft_key: None,
            creating: false,
            default_permission: "ask".into(),
            trigger: None,
            suggestions: Vec::new(),
            active_suggestion: 0,
            suggestions_scroll: ScrollHandle::new(),
            completion_seq: 0,
            shown_seq: 0,
            completion_task: None,
            accept_shown: false,
            last_escape: None,
            ticker: None,
            changes,
            store,
            chat: None,
            rows: Rc::default(),
            row_count: 0,
            header: 0,
            older: None,
            loaded: 0,
            anchor: None,
            list,
            composer,
            expanded: Rc::default(),
            approval_focus: cx.focus_handle(),
            approval_shown: None,
            answering: false,
            error_open: false,
            thinking_drag: None,
            background_scroll: ScrollHandle::new(),
            queue_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
            chat_subscription: None,
        };
        pane.follow_selected(window, cx);
        pane.read_default_permission(cx);
        pane
    }

    /// Show the chat the store has selected, with the composer's text and images for it.
    fn follow_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.store.read(cx).selected.clone();
        if self.chat.as_ref().map(|chat| chat.read(cx).id.clone()) == selected {
            return;
        }
        self.swap_draft(selected.clone(), window, cx);
        let chat = selected.map(|id| self.store.update(cx, |store, cx| store.chat(&id, cx)));
        self.chat_subscription = chat.as_ref().map(|chat| cx.observe(chat, |this, _, cx| this.rebuild(cx)));
        self.chat = chat;
        self.expanded = Rc::default();
        // The rows `rebuild_rows` may reuse are only ever this chat's.
        self.rows = Rc::default();
        self.row_count = 0;
        self.anchor = None;
        self.error_open = false;
        self.list.reset(0);
        self.list.set_follow_mode(FollowMode::Tail);
        self.rebuild(cx);
    }

    /// Keep the composer's text and images with the chat they were typed for, and bring back those of `next`.
    fn swap_draft(&mut self, next: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if next == self.draft_key {
            return;
        }
        let text = self.composer.read(cx).value().to_string();
        let attachments = std::mem::take(&mut self.attachments);
        let previous = std::mem::replace(&mut self.draft_key, next.clone());
        if text.trim().is_empty() && attachments.is_empty() {
            self.drafts.remove(&previous);
        } else {
            self.drafts.insert(previous, Draft { text, attachments });
        }
        let draft = self.drafts.remove(&next).unwrap_or_default();
        self.attachments = draft.attachments;
        self.composer.update(cx, |state, cx| state.set_value(draft.text, window, cx));
        self.trigger = None;
        self.suggestions.clear();
        self.completion_task = None;
    }

    /// The mode new chats start in, for the permission picker of the new-chat composer.
    fn read_default_permission(&mut self, cx: &mut Context<Self>) {
        let call = self.store.read(cx).call("security.status", json!({}));
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Ok(status) = call.await else { return };
            if let Some(mode) = status["defaultMode"].as_str() {
                let mode = mode.to_owned();
                let _ = this.update(cx, |this, cx| {
                    this.default_permission = mode;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Build the turns again after the chat changed, and tell the list which rows changed.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        // A reply still thinking is drawn again every second, so its timer runs between deltas.
        let thinking = self.chat.as_ref().is_some_and(|chat| chat.read(cx).view.as_ref().is_some_and(|view| view.thinking_since.is_some()));
        if thinking && self.ticker.is_none() {
            self.ticker = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let _ = this.update(cx, |this, cx| {
                    this.ticker = None;
                    this.rebuild(cx);
                });
            }));
        }
        let locale = self.store.read(cx).locale();
        let (loaded, older) = self.chat.as_ref().map_or((0, None), |chat| {
            let chat = chat.read(cx);
            let truncated = chat.view.as_ref().is_some_and(|view| view.truncated);
            (chat.loaded, truncated.then(|| OlderRow { loading: chat.loading_whole, error: chat.error.clone() }))
        });
        let old = self.row_count;
        // A new snapshot (older messages, a reload after a reconnect) replaces every row; the reader stays at the
        // turn they were at unless they follow the latest output.
        let replaced = old > 0 && loaded != self.loaded;
        let anchor = if replaced { self.anchor.take().or_else(|| if self.list.is_following_tail() { None } else { self.reading_position() }) } else { None };
        if loaded != self.loaded {
            // Rows of an older snapshot are not reused: the turn its window cut short is whole in the new one.
            self.rows = Rc::default();
        }
        let rows = self.chat.as_ref().and_then(|chat| chat.read(cx).view.as_ref().map(|view| rebuild_rows(view, locale, &self.rows))).unwrap_or_default();
        let older = older.filter(|_| !rows.is_empty());
        let header = usize::from(older.is_some());
        let count = rows.len() + header;
        let older_changed = older != self.older;
        self.loaded = loaded;
        self.older = older;
        self.rows = Rc::new(rows);
        self.header = header;
        self.row_count = count;
        if replaced {
            self.list.reset(count);
            match anchor.and_then(|anchor| self.offset_of(&anchor)) {
                Some(offset) => self.list.scroll_to(offset),
                None => self.list.set_follow_mode(FollowMode::Tail),
            }
        } else if old > 0 && count > old {
            self.list.remeasure_items(old - 1..old);
            self.list.splice(old..old, count - old);
        } else if count != old {
            self.list.reset(count);
            self.list.set_follow_mode(FollowMode::Tail);
        } else if count > 0 {
            // Only the last turn changes while a reply streams.
            self.list.remeasure_items(count - 1..count);
        }
        if older_changed && !replaced {
            // The load-older row shows its spinner or why the load failed; a failed load leaves no place to return to.
            if header > 0 && count == old {
                self.list.remeasure_items(0..1);
            }
            if self.older.as_ref().is_none_or(|older| !older.loading) {
                self.anchor = None;
            }
        }
        cx.notify();
    }

    /// The turn at the top of the transcript and how far into it. The load-older row, or a turn the loaded window
    /// cut short, gives the first whole turn after it, marked to show the one before it once that one is complete.
    fn reading_position(&self) -> Option<Anchor> {
        let top = self.list.logical_scroll_top();
        let index = top.item_ix.checked_sub(self.header);
        if let Some(row) = index.and_then(|index| self.rows.get(index))
            && row.key != "lead"
        {
            return Some(Anchor { key: row.key.clone(), offset: top.offset_in_item, previous: false });
        }
        let (at, row) = self.rows.iter().enumerate().skip(index.unwrap_or(0)).find(|(_, row)| row.key != "lead")?;
        Some(Anchor { key: row.key.clone(), offset: px(0.), previous: at > 0 && self.rows[at - 1].key == "lead" })
    }

    /// Where `anchor` is in the current rows.
    fn offset_of(&self, anchor: &Anchor) -> Option<ListOffset> {
        let index = self.rows.iter().position(|row| row.key == anchor.key)?;
        let index = if anchor.previous { index.saturating_sub(1) } else { index };
        Some(ListOffset { item_ix: index + self.header, offset_in_item: anchor.offset })
    }

    /// Load the whole chat; the reader stays at the turn they were at.
    fn load_older(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.session_id(cx) else { return };
        if self.older.as_ref().is_some_and(|older| older.loading) {
            return;
        }
        self.anchor = self.reading_position();
        self.store.update(cx, |store, cx| store.load_chat(&id, true, cx));
    }

    fn toggle(&mut self, key: SharedString, row: usize, cx: &mut Context<Self>) {
        let mut expanded: HashSet<SharedString> = (*self.expanded).clone();
        if !expanded.remove(&key) {
            expanded.insert(key);
        }
        self.expanded = Rc::new(expanded);
        // The clicked row grows or shrinks below the reader's position, which stays where it is.
        let top = self.list.logical_scroll_top();
        self.list.remeasure_items(row..row + 1);
        if top.item_ix < self.list.item_count() {
            self.list.scroll_to(top);
        }
        cx.notify();
    }

    fn session_id(&self, cx: &App) -> Option<String> {
        self.chat.as_ref().map(|chat| chat.read(cx).id.clone())
    }

    fn busy(&self, cx: &App) -> bool {
        self.chat.as_ref().is_some_and(|chat| chat.read(cx).busy())
    }

    /// The new-chat composer is waiting for the chat its message creates.
    fn creating_here(&self) -> bool {
        self.creating && self.draft_key.is_none()
    }

    /// Send the composer's text; while the chat works it is queued or steered (`busyEnter`, Ctrl+Enter for the other).
    /// The composer empties at once; a message that does not go out comes back to the draft it was typed in.
    fn send(&mut self, alternate: bool, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.composer.read(cx).value().to_string();
        let text = typed.trim().to_owned();
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }
        if self.attachments.is_empty()
            && let Some(name) = text.strip_prefix('/').filter(|name| crate::ui::composer::COMMANDS.iter().any(|(command, _)| command == name))
        {
            let name = name.to_owned();
            self.run_command(&name, window, cx);
            return;
        }
        // One new chat at a time: what is typed meanwhile waits until the first message's chat exists.
        if self.creating_here() {
            return;
        }
        // The draft belongs to the chat shown (`swap_draft`), which is where it goes.
        let session = self.draft_key.clone();
        let images = std::mem::take(&mut self.attachments);
        self.trigger = None;
        self.suggestions.clear();
        let mode = self.busy(cx).then(|| {
            let steer_first = self.store.read(cx).prefs.busy_enter == "steer";
            if steer_first != alternate { "steer" } else { "queue" }
        });
        self.composer.update(cx, |state, cx| state.set_value("", window, cx));
        self.list.set_follow_mode(FollowMode::Tail);
        self.list.scroll_to_end();
        let store = self.store.clone();
        let workspace = crate::store::ide::Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        if session.is_none() {
            self.creating = true;
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let session = match session {
                Some(id) => id,
                None => {
                    let created = store.update(cx, |store, cx| store.create_chat(workspace, cx)).await;
                    let _ = this.update(cx, |this, cx| {
                        this.creating = false;
                        cx.notify();
                        if let Ok(id) = &created {
                            // A composer still on the new chat now belongs to the chat made for its message.
                            if this.draft_key.is_none() {
                                this.draft_key = Some(id.clone());
                            }
                            this.store.update(cx, |store, cx| store.select(Some(id.clone()), cx));
                        }
                    });
                    match created {
                        Ok(id) => id,
                        Err(error) => {
                            let _ = this.update_in(cx, |this, window, cx| this.send_failed(None, typed, images, &error, window, cx));
                            return;
                        }
                    }
                }
            };
            let mut params = json!({ "sessionId": session, "text": text });
            if !images.is_empty() {
                params["images"] = json!(images.iter().map(|image| &image.content).collect::<Vec<_>>());
            }
            if let Some(mode) = mode {
                params["mode"] = json!(mode);
            }
            let call = store.read_with(cx, |store, _| store.call("chat.send", params));
            if let Err(error) = call.await {
                let _ = this.update_in(cx, |this, window, cx| this.send_failed(Some(session), typed, images, &error, window, cx));
            }
        })
        .detach();
    }

    /// Say why a message did not go out, and put it back in front of what its draft holds now.
    fn send_failed(&mut self, key: Option<String>, text: String, images: Vec<Attachment>, error: &RpcError, window: &mut Window, cx: &mut Context<Self>) {
        let message = trf(cx, "chat.sendFailed", &[("message", &crate::ui::error_text(cx, error))]);
        window.push_notification(Notification::error(message), cx);
        if key == self.draft_key {
            let typed = self.composer.read(cx).value().to_string();
            let (text, attachments) = (merge_text(text, typed), merge_images(images, std::mem::take(&mut self.attachments)));
            self.attachments = attachments;
            self.composer.update(cx, |state, cx| state.set_value(text, window, cx));
        } else {
            let draft = self.drafts.entry(key).or_default();
            draft.text = merge_text(text, std::mem::take(&mut draft.text));
            draft.attachments = merge_images(images, std::mem::take(&mut draft.attachments));
        }
        cx.notify();
    }

    fn call(&self, method: &'static str, params: Value, window: &mut Window, cx: &mut Context<Self>) {
        report_failure(self.store.read(cx).call(method, params), window, cx);
    }

    /// Work out what the text before the caret asks to complete, and fetch suggestions for it.
    fn update_trigger(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.composer.read(cx);
        let text = state.value().to_string();
        let caret = state.cursor().min(text.len());
        let trigger = text.is_char_boundary(caret).then(|| crate::ui::composer::find_trigger(&text[..caret])).flatten();
        if trigger == self.trigger {
            return;
        }
        self.trigger = trigger.clone();
        self.accept_shown = false;
        self.active_suggestion = 0;
        self.suggestions_scroll.scroll_to_item(0);
        match trigger {
            None => {
                self.suggestions.clear();
                self.completion_task = None;
            }
            Some(crate::ui::composer::Trigger::Command { query }) => {
                let has_chat = self.session_id(cx).is_some();
                self.suggestions = crate::ui::composer::COMMANDS
                    .iter()
                    .filter(|(name, _)| name.starts_with(&query.to_lowercase()) && (*name != "compact" || has_chat))
                    .map(|(name, description)| Suggestion::Command { name, description })
                    .collect();
            }
            Some(crate::ui::composer::Trigger::Reference { query, .. }) => {
                self.completion_seq += 1;
                let seq = self.completion_seq;
                let mut params = json!({ "query": query });
                if let Some(session) = self.session_id(cx) {
                    params["sessionId"] = json!(session);
                }
                params["workspaceId"] = json!(crate::store::ide::Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone()));
                let call = self.store.read(cx).call("chat.complete", params);
                self.completion_task = Some(cx.spawn_in(window, async move |this: WeakEntity<Self>, cx: &mut AsyncWindowContext| {
                    cx.background_executor().timer(std::time::Duration::from_millis(80)).await;
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
    fn take_suggestion(&mut self, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        let reference = matches!(self.trigger, Some(crate::ui::composer::Trigger::Reference { .. }));
        if reference && self.shown_seq != self.completion_seq {
            self.accept_shown = true;
        } else {
            self.accept_suggestion(self.active_suggestion, run, window, cx);
        }
    }

    /// Put a suggestion into the text: a command is written out by Tab and runs on Enter (`run`), a reference
    /// replaces what was typed after `@`.
    fn accept_suggestion(&mut self, index: usize, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(suggestion) = self.suggestions.get(index).cloned() else { return };
        match suggestion {
            Suggestion::Command { name, .. } if run => self.run_command(name, window, cx),
            Suggestion::Command { name, .. } => {
                let text = format!("/{name}");
                let character = text.chars().count() as u32;
                self.composer.update(cx, |state, cx| {
                    state.set_value(text, window, cx);
                    state.set_cursor_position(gpui_kit::component::input::Position { line: 0, character }, window, cx);
                });
                self.update_trigger(window, cx);
            }
            Suggestion::Completion { kind, insert, .. } => {
                let Some(crate::ui::composer::Trigger::Reference { start, .. }) = self.trigger.clone() else { return };
                let state = self.composer.read(cx);
                let text = state.value().to_string();
                let caret = state.cursor().min(text.len());
                // A folder keeps the list open for its contents.
                let continues = kind == "directory";
                let inserted = format!("@{insert}{}", if continues { "" } else { " " });
                let next = format!("{}{inserted}{}", &text[..start], &text[caret..]);
                let before = format!("{}{inserted}", &text[..start]);
                let line = before.matches('\n').count() as u32;
                let character = before.rsplit('\n').next().unwrap_or_default().chars().count() as u32;
                self.composer.update(cx, |state, cx| {
                    state.set_value(next, window, cx);
                    state.set_cursor_position(gpui_kit::component::input::Position { line, character }, window, cx);
                });
                if !continues {
                    self.trigger = None;
                    self.suggestions.clear();
                }
                self.update_trigger(window, cx);
            }
        }
    }

    /// `/compact` and `/new`.
    fn run_command(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |state, cx| state.set_value("", window, cx));
        self.trigger = None;
        self.suggestions.clear();
        match name {
            "compact" => {
                if let Some(session) = self.session_id(cx) {
                    self.call("chat.compact", json!({ "sessionId": session }), window, cx);
                }
            }
            _ => self.store.update(cx, |store, cx| store.select(None, cx)),
        }
        cx.notify();
    }

    /// Keys taken before the text area sees them, while it has the cursor.
    fn intercept(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
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
                self.trigger = None;
                self.suggestions.clear();
                cx.stop_propagation();
                cx.notify();
            }
            "escape" if self.busy(cx) => {
                // Esc twice in a row stops the run.
                let now = std::time::Instant::now();
                if self.last_escape.is_some_and(|last| now.duration_since(last) < std::time::Duration::from_millis(800)) {
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

    /// Encode an image a viewer page sent, then attach it; `added` is said once it is attached.
    pub fn attach_bytes(&mut self, name: String, bytes: Vec<u8>, added: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.attach_image(name, NewImage::Bytes(bytes), added, window, cx);
    }

    /// Read and encode an image off the interface thread, then attach it to the draft it was added to; `added` is said
    /// once it is attached.
    fn attach_image(&mut self, name: String, image: NewImage, added: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.draft_key.clone();
        let prepared = cx.background_spawn(async move {
            let bytes = match image {
                NewImage::Bytes(bytes) => bytes,
                NewImage::File(path) => crate::ui::composer::read_image(&path)?,
            };
            let (content, shown) = crate::ui::composer::prepare_image(&bytes)?;
            let preview = Arc::new(Image::from_bytes(image_format(&content.mime_type), shown));
            Ok::<_, ImageError>(Attachment { content, preview })
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = prepared.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(attachment) => {
                        let attachments = if key == this.draft_key { &mut this.attachments } else { &mut this.drafts.entry(key).or_default().attachments };
                        if attachments.len() < MAX_IMAGES {
                            attachments.push(attachment);
                            if let Some(added) = added {
                                window.push_notification(Notification::success(added), cx);
                            }
                        } else {
                            window.push_notification(Notification::error(trf(cx, "chat.imageLimit", &[("count", &MAX_IMAGES)])), cx);
                        }
                    }
                    Err(error) => {
                        let text = match &error {
                            ImageError::TooLarge => trf(cx, "chat.imageTooLarge", &[("name", &name)]),
                            ImageError::Unreadable => trf(cx, "chat.imageUnsupported", &[("name", &name)]),
                            ImageError::Read(error) => trf(cx, "chat.imageReadFailed", &[("name", &name), ("message", error)]),
                        };
                        window.push_notification(Notification::error(text), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Files from Windows: images are attached, other files are referenced with `@` and their path as the Host sees it.
    fn attach_paths(&mut self, paths: Vec<std::path::PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.store.read(cx).target.clone();
        let mut references = Vec::new();
        for path in paths {
            let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            if crate::ui::composer::is_image_file(&path) {
                self.attach_image(name, NewImage::File(path), None, window, cx);
                continue;
            }
            match crate::ui::composer::host_path(&path, target.as_ref()) {
                Some(host) => references.push(format!("@{}", crate::ui::composer::quote_path(&host))),
                None => window.push_notification(Notification::error(trf(cx, "chat.attachPathFailed", &[("name", &name)]).to_string()), cx),
            }
        }
        if !references.is_empty() {
            let text = format!("{} ", references.join(" "));
            self.composer.update(cx, |state, cx| {
                state.focus(window, cx);
                state.insert(text, window, cx);
            });
        }
    }

    /// Choose image files to attach.
    fn choose_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(tr(cx, "chat.attachImage")) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let _ = this.update_in(cx, |this, window, cx| this.attach_paths(paths, window, cx));
        })
        .detach();
    }

    fn render_suggestions(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let active = self.active_suggestion;
        div()
            .relative()
            .flex_shrink_0()
            .mx_3()
            .mb_1()
            .child(
                v_flex()
                    .id("suggestions")
                    .p_1()
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .track_scroll(&self.suggestions_scroll)
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_md()
                    .children(self.suggestions.iter().enumerate().map(|(index, suggestion)| {
                        let (label, detail, icon) = match suggestion {
                            Suggestion::Command { name, description } => (format!("/{name}"), Some(tr(cx, description).to_string()), IconName::SquareTerminal),
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
                            .rounded(theme.radius)
                            .text_sm()
                            .cursor_pointer()
                            .when(index == active, |this| this.bg(theme.list_active))
                            .hover(|style| style.bg(theme.list_hover))
                            .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
                            .when_some(detail, |this, detail| {
                                this.child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(theme.muted_foreground).child(detail))
                            })
                            .on_click(cx.listener(move |this, _, window, cx| this.accept_suggestion(index, true, window, cx)))
                    })),
            )
            .vertical_scrollbar(&self.suggestions_scroll)
    }

    fn render_attachments(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        h_flex().gap_2().flex_wrap().children(self.attachments.iter().enumerate().map(|(index, image)| {
            div()
                .relative()
                .size(px(56.))
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(img(image.preview.clone()).size_full().object_fit(ObjectFit::Cover))
                .child(div().absolute().top_0().right_0().child(Button::new(("remove-image", index)).ghost().xsmall().icon(IconName::Close).tooltip(tr(cx, "chat.removeImage")).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if index < this.attachments.len() {
                            this.attachments.remove(index);
                        }
                        cx.notify();
                    },
                ))))
        }))
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let title = self
            .chat
            .as_ref()
            .and_then(|chat| chat.read(cx).view.as_ref().map(|view| view.snapshot.summary.title.clone()))
            .filter(|title| !title.is_empty())
            .map(SharedString::from)
            .unwrap_or_else(|| tr(cx, "chat.newChat"));
        let theme = cx.theme();
        h_flex()
            .h(px(40.))
            .flex_shrink_0()
            .px_3()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_semibold().text_sm().child(title))
            .child(
                Button::new("new-chat")
                    .ghost()
                    .small()
                    .icon(Icon::new(gpui_kit::assets::IconName::CirclePlus))
                    .tooltip(tr(cx, "chat.newChat"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.store.update(cx, |store, cx| store.select(None, cx));
                    })),
            )
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_2()
            .child(img("brand/rainy-mark.svg").size(px(48.)))
            .child(div().text_xl().font_semibold().child(tr(cx, "chat.heroTitle")))
            .child(div().text_sm().text_color(theme.muted_foreground).child(tr(cx, "chat.heroCredit")))
    }

    /// Answer the approval the bar shows; one answer at a time.
    fn answer(&mut self, tool_call_id: String, decision: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session_id(cx) else { return };
        if self.answering {
            return;
        }
        self.answering = true;
        cx.notify();
        let request = self.store.read(cx).call("chat.approve", json!({ "sessionId": session, "toolCallId": tool_call_id, "decision": decision }));
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = request.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.answering = false;
                    let text = trf(cx, "chat.approvalFailed", &[("message", &crate::ui::error_text(cx, &error))]);
                    window.push_notification(Notification::error(text), cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// The approval bar's keys (`ApprovalBar.tsx`): Enter allows, Esc denies.
    fn approval_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tool_call_id) = self.approval_shown.clone() else { return };
        if event.keystroke.modifiers.modified() {
            return;
        }
        let decision = match event.keystroke.key.as_str() {
            "enter" => "allow",
            "escape" => "deny",
            _ => return,
        };
        cx.stop_propagation();
        self.answer(tool_call_id, decision, window, cx);
    }

    fn render_approval(&self, approval: &PendingApproval, focused: bool, locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let path = approval.path.clone().unwrap_or_default();
        let text = match approval.reason.as_str() {
            "outside-write" => i18n::format(locale, "chat.approvalOutsideWrite", &[("path", &path)]),
            "mcp" => i18n::text(locale, "chat.approvalMcp").to_owned(),
            "web" => i18n::format(locale, "chat.approvalWeb", &[("url", &path)]),
            _ => i18n::text(locale, "chat.approvalCommand").to_owned(),
        };
        let decide = |decision: &'static str| {
            let tool_call_id = approval.tool_call_id.clone();
            cx.listener(move |this, _, window, cx| this.answer(tool_call_id.clone(), decision, window, cx))
        };
        let answering = self.answering;
        v_flex()
            .id("approval")
            .flex_shrink_0()
            .track_focus(&self.approval_focus)
            .on_key_down(cx.listener(Self::approval_key))
            .mx_3()
            .mb_2()
            .p_3()
            .gap_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(if focused { theme.ring } else { theme.warning })
            .bg(theme.secondary)
            .child(div().text_sm().child(text))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(Button::new("approve").primary().small().label(i18n::text(locale, "chat.approvalAllow")).disabled(answering).on_click(decide("allow")))
                    .when_some(approval.rule.clone(), |this, rule| {
                        this.child(
                            Button::new("approve-always")
                                .outline()
                                .small()
                                .label(i18n::format(locale, "chat.approvalAlways", &[("rule", &rule)]))
                                .disabled(answering)
                                .on_click(decide("always")),
                        )
                    })
                    .child(Button::new("deny").ghost().small().label(i18n::text(locale, "chat.approvalDeny")).disabled(answering).on_click(decide("deny"))),
            )
            .when(focused, |this| this.child(div().text_xs().text_color(theme.muted_foreground).child(i18n::text(locale, "chat.approvalKeys"))))
    }

    fn render_queue(&self, session: &str, queue: &[QueuedMessage], locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let items: Vec<AnyElement> = queue
            .iter()
            .map(|item| {
                let label = if item.mode == "steer" { "chat.steering" } else { "chat.queued" };
                let params = json!({ "sessionId": session, "id": item.id });
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .text_xs()
                    .items_center()
                    .child(div().flex_shrink_0().text_color(theme.muted_foreground).child(i18n::text(locale, label)))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(item.text.clone()))
                    .child(
                        Button::new(ElementId::Name(format!("unqueue-{}", item.id).into()))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip(i18n::text(locale, "chat.removeQueued"))
                            .on_click(cx.listener(move |this, _, window, cx| this.call("chat.unqueue", params.clone(), window, cx))),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .relative()
            .flex_shrink_0()
            .mx_3()
            .mb_2()
            .child(v_flex().id("queue").max_h(px(MAX_QUEUE_HEIGHT)).overflow_y_scroll().track_scroll(&self.queue_scroll).gap_1().children(items))
            .vertical_scrollbar(&self.queue_scroll)
    }

    /// The model the composer sends with: the chat's, or the one new chats start with.
    fn current_model(&self, cx: &App) -> Option<ModelSelection> {
        let chat_model = self.chat.as_ref().and_then(|chat| chat.read(cx).view.as_ref().and_then(|view| view.snapshot.model.clone()));
        chat_model.or_else(|| self.store.read(cx).models.as_ref().and_then(|models| models.selected.clone()))
    }

    fn model_picker(&self, locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let current = self.current_model(cx);
        let label = match &current {
            // A long model name is cut so the composer's buttons keep their place.
            Some(model) if model.model.chars().count() > 22 => format!("{}…", model.model.chars().take(21).collect::<String>()),
            Some(model) => model.model.clone(),
            None => i18n::text(locale, "chat.noModel").to_owned(),
        };
        let store = self.store.clone();
        let session = self.session_id(cx);
        Button::new("model-picker")
            .ghost()
            .small()
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, cx| {
                let state = store.read(cx);
                let Some(models) = state.models.clone() else { return menu };
                for model in &models.models {
                    let selected = current.as_ref().is_some_and(|c| c.provider == model.provider && c.model == model.model);
                    let choice = ModelSelection { provider: model.provider.clone(), model: model.model.clone(), thinking: current.as_ref().and_then(|c| c.thinking.clone()) };
                    let (store, session) = (store.clone(), session.clone());
                    menu = menu.item(PopupMenuItem::new(format!("{} · {}", model.model, model.provider)).checked(selected).on_click(move |_, window, cx| {
                        set_model(&store, session.clone(), choice.clone(), window, cx);
                    }));
                }
                menu
            })
    }

    /// The levels the composer's model offers, the stop the slider shows (the one held while dragging) and the one the
    /// Host uses. None when the model has a single level.
    fn thinking_state(&self, cx: &App) -> Option<ThinkingState> {
        let current = self.current_model(cx)?;
        let levels = self.store.read(cx).models.as_ref()?.thinking_levels.get(&current.provider).cloned().unwrap_or_default();
        if levels.len() < 2 {
            return None;
        }
        let sent = shown_level(&levels, current.thinking.as_deref().unwrap_or("off"));
        let index = self.thinking_drag.unwrap_or(sent).min(levels.len() - 1);
        Some(ThinkingState { levels, index, sent, current })
    }

    /// Ends a press on the slider: the stop it was released on is sent, unless it is the level already in use.
    fn commit_thinking(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.thinking_drag.take() else { return };
        cx.notify();
        let Some(state) = self.thinking_state(cx) else { return };
        if index == state.sent {
            return;
        }
        let choice = ModelSelection { thinking: state.levels.get(index).cloned(), ..state.current };
        set_model(&self.store, self.session_id(cx), choice, window, cx);
    }

    /// A compact button with a strength meter and the level's name; it opens the reasoning slider above the composer.
    fn thinking_picker(&self, locale: Locale, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.thinking_state(cx)?;
        let theme = cx.theme();
        let (accent, rail) = (theme.primary, theme.muted_foreground.opacity(0.35));
        let meter = h_flex().h(px(12.)).gap(px(2.)).items_end().children((0..state.levels.len()).map(|bar| {
            let height = 12. * (bar + 1) as f32 / state.levels.len() as f32;
            div().w(px(3.)).h(px(height.max(3.))).rounded(px(1.)).bg(if bar <= state.index { accent } else { rail })
        }));
        let pane = cx.entity().downgrade();
        Some(
            Popover::new("thinking-popover")
                .anchor(gpui_kit::Anchor::BottomLeft)
                .trigger(
                    Button::new("thinking-picker")
                        .ghost()
                        .small()
                        .tooltip(i18n::text(locale, "chat.thinkingTitle"))
                        .child(h_flex().gap_1p5().items_center().child(meter).child(thinking_label(locale, &state.levels[state.index]))),
                )
                .content(move |_, _, cx| thinking_panel(&pane, locale, cx))
                .into_any_element(),
        )
    }

    /// The chat's mode; before a new chat's first message, the mode it will start in (3.x `nextMode`, else the
    /// default from Settings).
    fn permission_picker(&self, locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let session = self.session_id(cx);
        let current = self
            .chat
            .as_ref()
            .and_then(|chat| chat.read(cx).view.as_ref().map(|view| view.snapshot.permission.clone()))
            .or_else(|| session.is_none().then(|| self.store.read(cx).pending_permission.clone()).flatten())
            .unwrap_or_else(|| self.default_permission.clone());
        let label = PERMISSIONS.iter().find(|(id, _)| *id == current).map_or("chat.permissionAsk", |(_, key)| key);
        let store = self.store.clone();
        Button::new("permission-picker")
            .ghost()
            .small()
            .label(i18n::text(locale, label))
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for (mode, key) in PERMISSIONS {
                    let (store, session) = (store.clone(), session.clone());
                    menu = menu.item(PopupMenuItem::new(i18n::text(locale, key)).checked(current == mode).on_click(move |_, window, cx| match session.clone() {
                        Some(session) => report_failure(store.read(cx).call("chat.setPermission", json!({ "sessionId": session, "mode": mode })), window, cx),
                        None => store.update(cx, |store, cx| {
                            store.pending_permission = Some(mode.to_owned());
                            cx.notify();
                        }),
                    }));
                }
                menu
            })
    }

    fn render_composer(&self, locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let busy = self.busy(cx);
        let creating = self.creating_here();
        let context = self.chat.as_ref().and_then(|chat| chat.read(cx).view.as_ref().and_then(|view| view.snapshot.context.clone()));
        let model_picker = self.model_picker(locale, cx);
        let thinking_picker = self.thinking_picker(locale, cx);
        let permission_picker = self.permission_picker(locale, cx);
        let session = self.session_id(cx);
        let attachments = (!self.attachments.is_empty()).then(|| self.render_attachments(cx));
        let theme = cx.theme();
        v_flex()
            .id("composer")
            .flex_shrink_0()
            .mx_3()
            .mb_3()
            .p_2()
            .gap_1()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.input)
            .bg(theme.background)
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.border_color(cx.theme().ring))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.attach_paths(paths.paths().to_vec(), window, cx)))
            .children(attachments)
            .child(Textarea::new(&self.composer).appearance(false))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("attach-image")
                            .ghost()
                            .small()
                            .icon(Icon::new(gpui_kit::assets::IconName::Paperclip))
                            .tooltip(tr(cx, "chat.attachImage"))
                            .on_click(cx.listener(|this, _, window, cx| this.choose_images(window, cx))),
                    )
                    // The pickers give way in a narrow panel; the context share and the send button never leave it.
                    .child(h_flex().flex_1().min_w_0().overflow_hidden().gap_1().child(model_picker).children(thinking_picker).child(permission_picker))
                    .when_some(context, |this, context| {
                        let percent = if context.input_limit > 0.0 { (context.tokens / context.input_limit * 100.0).round() } else { 0.0 };
                        this.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(i18n::format(locale, "chat.contextUsage", &[("percent", &percent)])),
                        )
                    })
                    .child(div().flex_none().child(if busy {
                        Button::new("stop")
                            .small()
                            .icon(Icon::new(gpui_kit::assets::IconName::CircleStop))
                            .tooltip(i18n::text(locale, "chat.stop"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(session) = session.clone() {
                                    this.call("chat.abort", json!({ "sessionId": session }), window, cx);
                                }
                            }))
                    } else {
                        Button::new("send")
                            .primary()
                            .small()
                            .icon(Icon::new(gpui_kit::assets::IconName::Send))
                            .loading(creating)
                            .tooltip(i18n::text(locale, if creating { "chat.creating" } else { "chat.send" }))
                            .on_click(cx.listener(|this, _, window, cx| this.send(false, window, cx)))
                    })),
            )
    }
}

/// The text of a message that did not go out, in front of what was typed since.
fn merge_text(sent: String, typed: String) -> String {
    if typed.trim().is_empty() { sent } else { format!("{sent}\n{typed}") }
}

/// The images of a message that did not go out, in front of those added since, up to the most a message carries.
fn merge_images(mut sent: Vec<Attachment>, added: Vec<Attachment>) -> Vec<Attachment> {
    sent.extend(added);
    sent.truncate(MAX_IMAGES);
    sent
}

fn image_format(mime: &str) -> ImageFormat {
    match mime {
        "image/png" => ImageFormat::Png,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        _ => ImageFormat::Jpeg,
    }
}

/// Wait for a Host call and show its failure, in the interface language.
fn report_failure(request: impl Future<Output = Result<Value, RpcError>> + 'static, window: &mut Window, cx: &mut App) {
    window
        .spawn(cx, async move |cx| {
            if let Err(error) = request.await {
                let _ = cx.update(|window, cx| window.push_notification(crate::ui::error_notification(cx, &error), cx));
            }
        })
        .detach();
}

/// What the reasoning control draws; see [`ChatPane::thinking_state`].
struct ThinkingState {
    levels: Vec<String>,
    index: usize,
    sent: usize,
    current: ModelSelection,
}

fn thinking_label(locale: Locale, level: &str) -> &'static str {
    i18n::text(locale, THINKING.iter().find(|(id, _)| *id == level).map_or("chat.thinkingOff", |(_, key)| key))
}

/// The reasoning slider: one stop per level, weakest on the left, each named below. Pressing a stop or dragging
/// across them moves the thumb; the level is sent once, on release, wherever the button comes up. It reads the pane on
/// every draw, so it follows a chat or model switched while it is open.
fn thinking_panel(pane: &WeakEntity<ChatPane>, locale: Locale, cx: &mut App) -> AnyElement {
    const STOP: f32 = 52.;
    let Some(pane) = pane.upgrade() else { return div().into_any_element() };
    let Some(state) = pane.read(cx).thinking_state(cx) else { return div().into_any_element() };
    let theme = cx.theme();
    let (accent, rail, ring, strong, muted) = (theme.primary, theme.muted_foreground.opacity(0.35), theme.popover, theme.foreground, theme.muted_foreground);
    let count = state.levels.len();
    let stops = (0..count).map(|stop| {
        let (press, glide) = (pane.clone(), pane.clone());
        div()
            .id(("thinking-stop", stop))
            .w(px(STOP))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                press.update(cx, |this, cx| {
                    this.thinking_drag = Some(stop);
                    cx.notify();
                });
                cx.stop_propagation();
            })
            .on_mouse_move(move |event: &MouseMoveEvent, _, cx| {
                if event.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                glide.update(cx, |this, cx| {
                    if this.thinking_drag.is_some_and(|held| held != stop) {
                        this.thinking_drag = Some(stop);
                        cx.notify();
                    }
                });
            })
            .child(if stop == state.index {
                div().size(px(14.)).rounded_full().bg(accent).border_2().border_color(ring).shadow_sm()
            } else {
                div().size(px(6.)).rounded_full().bg(if stop < state.index { accent } else { rail })
            })
    }).collect::<Vec<_>>();
    let (release, release_out) = (pane.clone(), pane);
    let track = div()
        .id("thinking-slider")
        .relative()
        .h(px(24.))
        .w(px(count as f32 * STOP))
        .child(div().absolute().top(px(11.)).left(px(STOP / 2.)).w(px((count - 1) as f32 * STOP)).h(px(2.)).rounded_full().bg(rail))
        .child(div().absolute().top(px(11.)).left(px(STOP / 2.)).w(px(state.index as f32 * STOP)).h(px(2.)).rounded_full().bg(accent))
        .child(h_flex().relative().size_full().children(stops))
        .on_mouse_up(MouseButton::Left, move |_, window, cx| release.update(cx, |this, cx| this.commit_thinking(window, cx)))
        .on_mouse_up_out(MouseButton::Left, move |_, window, cx| release_out.update(cx, |this, cx| this.commit_thinking(window, cx)));
    let names = h_flex().w(px(count as f32 * STOP)).children(state.levels.iter().enumerate().map(|(stop, level)| {
        div().w(px(STOP)).text_center().text_color(if stop == state.index { strong } else { muted }).child(thinking_label(locale, level))
    }));
    v_flex()
        .gap_1()
        .text_xs()
        .child(
            h_flex()
                .justify_between()
                .pb_1()
                .child(div().text_color(muted).child(i18n::text(locale, "chat.thinkingTitle")))
                .child(div().text_color(strong).font_medium().child(thinking_label(locale, &state.levels[state.index]))),
        )
        .child(track)
        .child(names)
        .into_any_element()
}

/// The stop the slider shows for a requested level, as the Host resolves it (`effective_thinking`): a level the model
/// lacks becomes low when off was asked for and low exists, else the strongest.
fn shown_level(levels: &[String], requested: &str) -> usize {
    if let Some(index) = levels.iter().position(|level| level == requested) {
        return index;
    }
    let low = (requested == "off").then(|| levels.iter().position(|level| level == "low")).flatten();
    low.unwrap_or(levels.len().saturating_sub(1))
}

fn set_model(store: &Entity<Store>, session: Option<String>, choice: ModelSelection, window: &mut Window, cx: &mut App) {
    let request = match session {
        Some(session) => {
            let mut params = serde_json::to_value(&choice).unwrap_or_default();
            params["sessionId"] = json!(session);
            store.read(cx).call("chat.setModel", params)
        }
        None => store.read(cx).call("models.select", serde_json::to_value(&choice).unwrap_or_default()),
    };
    report_failure(request, window, cx);
}

impl Render for ChatPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locale = self.store.read(cx).locale();
        let show_usage = self.store.read(cx).prefs.show_usage;
        let failed = self.chat.as_ref().and_then(|chat| chat.read(cx).error.clone());
        // Only the parts drawn here are copied out of the chat, never its entries.
        let (approval, queue, retry, run_error, background) = self
            .chat
            .as_ref()
            .and_then(|chat| {
                let snapshot = &chat.read(cx).view.as_ref()?.snapshot;
                let run_error = (snapshot.summary.status == "error" && snapshot.retry.is_none()).then(|| snapshot.error.clone().unwrap_or_default());
                Some((snapshot.approvals.first().cloned(), snapshot.queue.clone(), snapshot.retry.clone(), run_error, snapshot.background.clone()))
            })
            .unwrap_or_default();
        // A new approval takes the keys only when the user is not typing anywhere (3.x): nothing has the focus, or only
        // the workbench itself. Allow is never focused.
        let shown = approval.as_ref().map(|approval| approval.tool_call_id.clone());
        if shown != self.approval_shown {
            self.approval_shown = shown;
            self.answering = false;
            let idle = window.focused(cx).is_none_or(|focused| cx.try_global::<crate::ui::workbench::WorkbenchFocus>().is_some_and(|workbench| workbench.0 == focused));
            if self.approval_shown.is_some() && idle {
                let focus = self.approval_focus.clone();
                window.defer(cx, move |window, cx| focus.focus(window, cx));
            }
        }
        let approval_focused = self.approval_focus.is_focused(window);
        let weak = cx.weak_entity();
        let toggle: Toggle = Rc::new(move |key, row, _, cx| {
            let _ = weak.update(cx, |this, cx| this.toggle(key, row, cx));
        });
        let rows = self.rows.clone();
        let expanded = self.expanded.clone();
        let style = TurnStyle {
            locale,
            show_usage,
            step_detail: self.store.read(cx).prefs.step_detail.clone().into(),
            session: self.session_id(cx).map(SharedString::from),
        };
        let fork_session = self.session_id(cx);
        let fork_store = self.store.clone();
        let fork: Fork = Rc::new(move |entry, window, cx| {
            let Some(session) = fork_session.clone() else { return };
            let call = fork_store.read(cx).call("sessions.fork", json!({ "sessionId": session, "entryId": entry.to_string() }));
            let store = fork_store.clone();
            window
                .spawn(cx, async move |cx| {
                    let result = call.await;
                    let _ = cx.update(|window, cx| match result {
                        Ok(summary) => {
                            if let Some(id) = summary["id"].as_str() {
                                store.update(cx, |store, cx| store.select(Some(id.to_owned()), cx));
                            }
                            window.push_notification(tr(cx, "chat.forked").to_string(), cx);
                        }
                        Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
                    });
                })
                .detach();
        });
        let body = if rows.is_empty() {
            match failed {
                Some(error) => div().flex_1().p_4().text_sm().text_color(cx.theme().danger).child(error).into_any_element(),
                None => self.render_empty(cx).into_any_element(),
            }
        } else {
            let scrolled_up = self.list.max_offset_for_scrollbar().y > px(0.)
                && !self.list.is_following_tail()
                && !self.list.is_scrolled_to_end().unwrap_or(false);
            let header = self.header;
            let older = self.older.clone().filter(|_| header > 0).map(|older| (older, tr(cx, "chat.loadOlder")));
            let pane = cx.weak_entity();
            let transcript = list(self.list.clone(), move |index, _, cx| {
                if index < header
                    && let Some((older, label)) = &older
                {
                    return render_older(older, label.clone(), pane.clone(), cx);
                }
                render_turn(&rows[index - header], index, &expanded, &toggle, &fork, &style, cx)
            })
            .size_full()
            .py_2();
            div()
                .id("transcript")
                .relative()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(transcript)
                .vertical_scrollbar(&self.list)
                .when(scrolled_up, |this| {
                    this.child(
                        div().absolute().bottom_2().left_0().right_0().flex().justify_center().child(
                            Button::new("to-latest")
                                .small()
                                .icon(IconName::ArrowDown)
                                .label(i18n::text(locale, "chat.backToLatest"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.list.set_follow_mode(FollowMode::Tail);
                                    this.list.scroll_to_end();
                                    cx.notify();
                                })),
                        ),
                    )
                })
                .into_any_element()
        };
        let session = self.session_id(cx).unwrap_or_default();
        let theme = cx.theme();
        // Everything below the transcript keeps its height (each list in it scrolls past a cap); the transcript
        // gives way, so nothing overlaps or pushes the composer out of the pane.
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(self.render_header(cx))
            .child(body)
            .when_some(approval, |this, approval| this.child(self.render_approval(&approval, approval_focused, locale, cx)))
            .when(!queue.is_empty(), |this| this.child(self.render_queue(&session, &queue, locale, cx)))
            .when_some(retry, |this, retry| {
                let seconds = ((retry.at - now_ms()) / 1000.0).ceil().max(0.0);
                let retried = session.clone();
                this.child(
                    h_flex()
                        .flex_shrink_0()
                        .mx_3()
                        .mb_2()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().min_w_0().text_xs().text_color(crate::theme::warning_text(cx)).child(trf(cx, "chat.retryWaiting", &[
                            ("seconds", &seconds),
                            ("attempt", &retry.attempt),
                            ("max", &retry.max),
                        ])))
                        .child(Button::new("retry-now").outline().xsmall().label(tr(cx, "chat.retryNow")).on_click(cx.listener(move |this, _, window, cx| {
                            this.call("chat.retry", json!({ "sessionId": retried }), window, cx);
                        }))),
                )
            })
            // A run that ended in an error can ask the model again.
            .when_some(run_error, |this, error| this.child(self.render_run_error(&session, error, cx)))
            .when(!background.is_empty(), |this| this.child(self.render_background(&session, &background, cx)))
            .child(self.changes.clone())
            .when(!self.suggestions.is_empty() && self.trigger.is_some(), |this| this.child(self.render_suggestions(cx)))
            .child(self.render_composer(locale, cx))
    }
}

/// The row above the turns that loads the chat's older messages.
fn render_older(older: &OlderRow, label: SharedString, pane: WeakEntity<ChatPane>, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .py_2()
        .gap_1()
        .items_center()
        .child(Button::new("load-older").ghost().small().icon(IconName::ArrowUp).label(label).loading(older.loading).on_click(move |_, _, cx| {
            let _ = pane.update(cx, |this, cx| this.load_older(cx));
        }))
        .when_some(older.error.clone(), |this, error| this.child(div().px_4().text_xs().text_color(cx.theme().danger).child(error)))
        .into_any_element()
}

impl ChatPane {
    /// The error a run ended in, three lines at most until its details are opened, and a retry.
    fn render_run_error(&self, session: &str, error: String, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let long = error.chars().count() > LONG_ERROR_CHARS || error.lines().count() > 3;
        let open = self.error_open && long;
        let text: SharedString = if error.is_empty() { tr(cx, "chat.errorTitle") } else { error.into() };
        let retried = session.to_owned();
        let theme = cx.theme();
        h_flex()
            .flex_shrink_0()
            .mx_3()
            .mb_2()
            .px_3()
            .py_2()
            .gap_2()
            .items_start()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.danger.opacity(0.4))
            .bg(theme.danger.opacity(0.08))
            .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.danger))
            .child(
                div()
                    .id("run-error")
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .when(open, |this| this.max_h(px(MAX_ERROR_HEIGHT)).overflow_y_scroll())
                    .when(!open, |this| this.line_clamp(3).text_ellipsis())
                    .child(text),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .items_end()
                    .child(Button::new("retry").outline().xsmall().icon(IconName::RefreshCw).label(tr(cx, "chat.retry")).on_click(cx.listener(move |this, _, window, cx| {
                        this.call("chat.retry", json!({ "sessionId": retried }), window, cx);
                    })))
                    .when(long, |this| {
                        this.child(Button::new("error-details").ghost().xsmall().label(tr(cx, if open { "chat.collapse" } else { "chat.showDetails" })).on_click(cx.listener(|this, _, _, cx| {
                            this.error_open = !this.error_open;
                            cx.notify();
                        })))
                    }),
            )
    }

    /// The chat's background commands (`BackgroundBar.tsx`): stop a running one, clear a finished one.
    fn render_background(&self, session: &str, commands: &[rainy_protocol::BackgroundCommand], cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let rows: Vec<AnyElement> = commands
            .iter()
            .enumerate()
            .map(|(index, command)| {
                let running = command.state == "running";
                let dot = if running { theme.warning } else if command.exit_code == Some(0) { theme.success } else { theme.danger };
                let state = if running {
                    if command.pending > 0.0 {
                        format!("{} · {}", tr(cx, "chat.backgroundRunning"), trf(cx, "chat.backgroundUnread", &[("bytes", &command.pending)]))
                    } else {
                        tr(cx, "chat.backgroundRunning").to_string()
                    }
                } else if command.signal.is_some() {
                    tr(cx, "chat.backgroundStopped").to_string()
                } else {
                    let code = command.exit_code.map_or("?".to_owned(), |code| code.to_string());
                    trf(cx, "chat.backgroundExited", &[("code", &code)]).to_string()
                };
                let what = if command.description.is_empty() { command.command.clone() } else { command.description.clone() };
                let (method, label) = if running { ("chat.stopCommand", "chat.backgroundStop") } else { ("chat.dismissCommand", "chat.backgroundDismiss") };
                let params = json!({ "sessionId": session, "id": command.id });
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(dot))
                    .child(div().flex_shrink_0().font_family(theme.mono_font_family.clone()).child(command.id.clone()))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(what))
                    .child(div().flex_shrink_0().text_color(theme.muted_foreground).child(state))
                    .child(Button::new(("background", index)).outline().xsmall().label(trf(cx, label, &[("id", &command.id)])).on_click(cx.listener(
                        move |this, _, window, cx| this.call(method, params.clone(), window, cx),
                    )))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .flex_shrink_0()
            .mx_3()
            .mb_2()
            .px_3()
            .py_2()
            .gap_1()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child(trf(cx, "chat.backgroundTitle", &[("count", &commands.len())])))
            .child(
                div()
                    .relative()
                    .child(v_flex().id("background-list").max_h(px(MAX_BACKGROUND_HEIGHT)).overflow_y_scroll().track_scroll(&self.background_scroll).gap_1().children(rows))
                    .vertical_scrollbar(&self.background_scroll),
            )
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn the_slider_shows_the_level_the_host_uses() {
        let all: Vec<String> = ["off", "low", "high", "max"].map(String::from).to_vec();
        let claude: Vec<String> = ["low", "high", "max"].map(String::from).to_vec();
        assert_eq!(shown_level(&all, "high"), 2);
        assert_eq!(shown_level(&all, "off"), 0);
        // A model that cannot turn reasoning off thinks a little; an unknown level is the strongest.
        assert_eq!(shown_level(&claude, "off"), 0);
        assert_eq!(shown_level(&claude, "medium"), 2);
    }

    #[test]
    fn an_unsent_message_goes_in_front_of_what_was_typed_since() {
        assert_eq!(merge_text("你好".into(), "".into()), "你好");
        assert_eq!(merge_text("你好".into(), "  \n".into()), "你好");
        assert_eq!(merge_text("你好".into(), "再见".into()), "你好\n再见");
        assert!(merge_images(Vec::new(), Vec::new()).is_empty());
    }
}
