//! The AI pane: the selected chat's transcript, its approvals and queue, and the composer
//! (`ChatPane.tsx`, `Composer.tsx`, `ApprovalBar.tsx`, `ModelPicker.tsx`, `PermissionPicker.tsx`).

use std::collections::HashSet;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::StyledExt as _;
use rainy_protocol::{ModelSelection, PendingApproval, QueuedMessage};
use serde_json::{Value, json};

use crate::i18n::{self, Locale};
use crate::store::{ChatModel, Store, StoreEvent};
use crate::ui::transcript::{Expanded, Fork, Toggle, TurnRow, TurnStyle, build_rows, render_turn};
use crate::ui::{tr, trf};

/// One entry of the composer's suggestion list.
#[derive(Clone)]
enum Suggestion {
    Command { name: &'static str, description: &'static str },
    Completion { kind: String, insert: String, label: String, detail: Option<String> },
}

pub struct ChatPane {
    /// Images the next message carries.
    attachments: Vec<rainy_protocol::ImageContent>,
    trigger: Option<crate::ui::composer::Trigger>,
    suggestions: Vec<Suggestion>,
    active_suggestion: usize,
    completion_seq: u64,
    completion_task: Option<Task<()>>,
    /// When Esc was pressed last while the chat ran; a second Esc soon after stops it.
    last_escape: Option<std::time::Instant>,
    /// Redraws a thinking timer.
    ticker: Option<Task<()>>,
    store: Entity<Store>,
    changes: Entity<crate::ui::changes::ChangesBar>,
    chat: Option<Entity<ChatModel>>,
    rows: Rc<Vec<TurnRow>>,
    row_count: usize,
    list: ListState,
    composer: Entity<TextareaState>,
    expanded: Expanded,
    _subscriptions: Vec<Subscription>,
    chat_subscription: Option<Subscription>,
}

/// The thinking levels and their labels.
const THINKING: [(&str, &str); 4] = [("off", "chat.thinkingOff"), ("low", "chat.thinkingLow"), ("high", "chat.thinkingHigh"), ("max", "chat.thinkingMax")];
const PERMISSIONS: [(&str, &str); 3] = [("read-only", "chat.permissionReadOnly"), ("ask", "chat.permissionAsk"), ("auto", "chat.permissionAuto")];

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
                        this.accept_suggestion(this.active_suggestion, window, cx);
                    } else {
                        this.send(*secondary, window, cx);
                    }
                }
                InputEvent::Change => this.update_trigger(cx),
                _ => {}
            }),
            // Keys the text area would otherwise take: the suggestion list's arrows, Esc, pasted images.
            cx.intercept_keystrokes(move |event, window, cx| {
                let _ = pane.update(cx, |this, cx| this.intercept(&event.keystroke, window, cx));
            }),
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| match event {
                StoreEvent::SelectionChanged => this.follow_selected(cx),
                StoreEvent::PrefsChanged => {
                    let placeholder = tr(cx, "chat.placeholder");
                    this.composer.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
                    this.rebuild(cx);
                }
                StoreEvent::SummariesChanged | StoreEvent::Host { .. } | StoreEvent::HostExited(_) | StoreEvent::ResourcesDamaged(_) | StoreEvent::Update(_) => {}
            }),
            cx.observe(&store, |_, _, cx| cx.notify()),
        ];
        let changes = cx.new(|cx| crate::ui::changes::ChangesBar::new(store.clone(), cx));
        let mut pane = Self {
            attachments: Vec::new(),
            trigger: None,
            suggestions: Vec::new(),
            active_suggestion: 0,
            completion_seq: 0,
            completion_task: None,
            last_escape: None,
            ticker: None,
            changes,
            store,
            chat: None,
            rows: Rc::default(),
            row_count: 0,
            list,
            composer,
            expanded: Rc::default(),
            _subscriptions: subscriptions,
            chat_subscription: None,
        };
        pane.follow_selected(cx);
        pane
    }

    /// Show the chat the store has selected.
    fn follow_selected(&mut self, cx: &mut Context<Self>) {
        let selected = self.store.read(cx).selected.clone();
        if self.chat.as_ref().map(|chat| chat.read(cx).id.clone()) == selected {
            return;
        }
        let chat = selected.map(|id| self.store.update(cx, |store, cx| store.chat(&id, cx)));
        self.chat_subscription = chat.as_ref().map(|chat| cx.observe(chat, |this, _, cx| this.rebuild(cx)));
        self.chat = chat;
        self.expanded = Rc::default();
        self.row_count = 0;
        self.list.reset(0);
        self.list.set_follow_mode(FollowMode::Tail);
        self.rebuild(cx);
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
        let rows = self.chat.as_ref().and_then(|chat| chat.read(cx).view.as_ref().map(|view| build_rows(view, locale))).unwrap_or_default();
        let (old, count) = (self.row_count, rows.len());
        self.rows = Rc::new(rows);
        self.row_count = count;
        if old > 0 && count > old {
            self.list.remeasure_items(old - 1..old);
            self.list.splice(old..old, count - old);
        } else if count != old {
            self.list.reset(count);
            self.list.set_follow_mode(FollowMode::Tail);
        } else if count > 0 {
            // Only the last turn changes while a reply streams.
            self.list.remeasure_items(count - 1..count);
        }
        cx.notify();
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

    /// Send the composer's text; while the chat works it is queued or steered (`busyEnter`, Ctrl+Enter for the other).
    fn send(&mut self, alternate: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_owned();
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
        let session = self.session_id(cx);
        let workspace = crate::store::ide::Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        let window_handle = window.window_handle();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            let session = match session {
                Some(id) => Some(id),
                None => store.update(cx, |store, cx| store.create_chat(workspace, cx)).await,
            };
            let Some(session) = session else { return anyhow::Ok(()) };
            let mut params = json!({ "sessionId": session, "text": text });
            if !images.is_empty() {
                params["images"] = json!(images);
            }
            if let Some(mode) = mode {
                params["mode"] = json!(mode);
            }
            let call = store.read_with(cx, |store, _| store.call("chat.send", params));
            if let Err(error) = call.await {
                window_handle.update(cx, |_, window, cx| window.push_notification(Notification::error(error.message), cx))?;
            }
            Ok(())
        })
        .detach();
    }

    fn call(&self, method: &'static str, params: Value, window: &mut Window, cx: &mut Context<Self>) {
        let request = self.store.read(cx).call(method, params);
        let window_handle = window.window_handle();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            if let Err(error) = request.await {
                let _ = window_handle.update(cx, |_, window, cx| window.push_notification(Notification::error(error.message), cx));
            }
        })
        .detach();
    }

    /// Work out what the text before the caret asks to complete, and fetch suggestions for it.
    fn update_trigger(&mut self, cx: &mut Context<Self>) {
        let state = self.composer.read(cx);
        let text = state.value().to_string();
        let caret = state.cursor().min(text.len());
        let trigger = text.is_char_boundary(caret).then(|| crate::ui::composer::find_trigger(&text[..caret])).flatten();
        if trigger == self.trigger {
            return;
        }
        self.trigger = trigger.clone();
        self.active_suggestion = 0;
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
                self.completion_task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    cx.background_executor().timer(std::time::Duration::from_millis(80)).await;
                    let Ok(items) = call.await else { return };
                    let _ = this.update(cx, |this, cx| {
                        if this.completion_seq != seq {
                            return;
                        }
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
                        cx.notify();
                    });
                }));
            }
        }
        cx.notify();
    }

    /// Put a suggestion into the text: a command runs, a reference replaces what was typed after `@`.
    fn accept_suggestion(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(suggestion) = self.suggestions.get(index).cloned() else { return };
        match suggestion {
            Suggestion::Command { name, .. } => self.run_command(name, window, cx),
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
                self.update_trigger(cx);
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
                cx.stop_propagation();
                cx.notify();
            }
            "tab" if open && !modifiers.modified() => {
                self.accept_suggestion(self.active_suggestion, window, cx);
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
                    let mime = match image.format {
                        ImageFormat::Jpeg => "image/jpeg",
                        ImageFormat::Gif => "image/gif",
                        ImageFormat::Webp => "image/webp",
                        _ => "image/png",
                    };
                    self.attach_bytes("clipboard".into(), image.bytes.clone(), mime, None, window, cx);
                    cx.stop_propagation();
                }
            }
            _ => {}
        }
    }

    /// Encode an image off the interface thread, then attach it; `added` is said once it is attached.
    pub fn attach_bytes(&mut self, name: String, bytes: Vec<u8>, mime: &'static str, added: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let encoded = cx.background_spawn(async move { crate::ui::composer::encode_image(&bytes, mime) });
        cx.spawn_in(window, async move |this, cx| {
            let result = encoded.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(image) if this.attachments.len() < crate::ui::composer::MAX_IMAGES => {
                        this.attachments.push(image);
                        if let Some(added) = added {
                            window.push_notification(Notification::success(added), cx);
                        }
                    }
                    Ok(_) => window.push_notification(Notification::error(trf(cx, "chat.imageLimit", &[("count", &crate::ui::composer::MAX_IMAGES)]).to_string()), cx),
                    Err(error) => window.push_notification(Notification::error(format!("{name}: {error}")), cx),
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
            if let Some(mime) = crate::ui::composer::image_mime(&path) {
                match std::fs::read(&path) {
                    Ok(bytes) if bytes.len() <= crate::ui::composer::MAX_IMAGE_BYTES => self.attach_bytes(name, bytes, mime, None, window, cx),
                    Ok(_) => window.push_notification(Notification::error(trf(cx, "chat.imageTooLarge", &[("name", &name)]).to_string()), cx),
                    Err(error) => window.push_notification(Notification::error(format!("{name}: {error}")), cx),
                }
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
        v_flex()
            .id("suggestions")
            .mx_3()
            .mb_1()
            .p_1()
            .max_h(px(260.))
            .overflow_y_scroll()
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
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .when(index == active, |this| this.bg(theme.list_active))
                    .hover(|style| style.bg(theme.list_hover))
                    .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                    .child(div().flex_shrink_0().child(label))
                    .when_some(detail, |this, detail| {
                        this.child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(theme.muted_foreground).child(detail))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.accept_suggestion(index, window, cx)))
            }))
    }

    fn render_attachments(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        h_flex().gap_2().flex_wrap().children(self.attachments.iter().enumerate().map(|(index, image)| {
            let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &image.data).unwrap_or_default();
            let format = if image.mime_type == "image/png" { ImageFormat::Png } else if image.mime_type == "image/gif" { ImageFormat::Gif } else if image.mime_type == "image/webp" { ImageFormat::Webp } else { ImageFormat::Jpeg };
            div()
                .relative()
                .size(px(56.))
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(img(std::sync::Arc::new(Image::from_bytes(format, bytes))).size_full().object_fit(ObjectFit::Cover))
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

    fn render_approval(&self, session: &str, approval: &PendingApproval, locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let path = approval.path.clone().unwrap_or_default();
        let text = match approval.reason.as_str() {
            "outside-write" => i18n::format(locale, "chat.approvalOutsideWrite", &[("path", &path)]),
            "mcp" => i18n::text(locale, "chat.approvalMcp").to_owned(),
            "web" => i18n::format(locale, "chat.approvalWeb", &[("url", &path)]),
            _ => i18n::text(locale, "chat.approvalCommand").to_owned(),
        };
        let decide = |decision: &'static str| {
            let params = json!({ "sessionId": session, "toolCallId": approval.tool_call_id, "decision": decision });
            cx.listener(move |this, _, window, cx| this.call("chat.approve", params.clone(), window, cx))
        };
        v_flex()
            .mx_3()
            .mb_2()
            .p_3()
            .gap_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.warning)
            .bg(theme.secondary)
            .child(div().text_sm().child(text))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(Button::new("approve").primary().small().label(i18n::text(locale, "chat.approvalAllow")).on_click(decide("allow")))
                    .when_some(approval.rule.clone(), |this, rule| {
                        this.child(
                            Button::new("approve-always")
                                .outline()
                                .small()
                                .label(i18n::format(locale, "chat.approvalAlways", &[("rule", &rule)]))
                                .on_click(decide("always")),
                        )
                    })
                    .child(Button::new("deny").ghost().small().label(i18n::text(locale, "chat.approvalDeny")).on_click(decide("deny"))),
            )
    }

    fn render_queue(&self, session: &str, queue: &[QueuedMessage], locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        v_flex().mx_3().mb_2().gap_1().children(queue.iter().map(|item| {
            let label = if item.mode == "steer" { "chat.steering" } else { "chat.queued" };
            let params = json!({ "sessionId": session, "id": item.id });
            h_flex()
                .gap_2()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .bg(theme.muted)
                .text_xs()
                .items_center()
                .child(div().text_color(theme.muted_foreground).child(i18n::text(locale, label)))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(item.text.clone()))
                .child(
                    Button::new(ElementId::Name(format!("unqueue-{}", item.id).into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip(i18n::text(locale, "chat.removeQueued"))
                        .on_click(cx.listener(move |this, _, window, cx| this.call("chat.unqueue", params.clone(), window, cx))),
                )
        }))
    }

    /// The model the composer sends with: the chat's, or the one new chats start with.
    fn current_model(&self, cx: &App) -> Option<ModelSelection> {
        let chat_model = self.chat.as_ref().and_then(|chat| chat.read(cx).view.as_ref().and_then(|view| view.snapshot.model.clone()));
        chat_model.or_else(|| self.store.read(cx).models.as_ref().and_then(|models| models.selected.clone()))
    }

    fn model_picker(&self, locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let current = self.current_model(cx);
        let label = match &current {
            Some(model) => {
                let level = model.thinking.as_deref().unwrap_or("off");
                let thinking = THINKING.iter().find(|(id, _)| *id == level).map_or(level, |(_, key)| i18n::text(locale, key));
                // A long model name is cut so the composer's buttons keep their place.
                let name: String = if model.model.chars().count() > 22 { format!("{}…", model.model.chars().take(21).collect::<String>()) } else { model.model.clone() };
                format!("{name} · {thinking}")
            }
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
                let current = current.clone();
                for model in &models.models {
                    let selected = current.as_ref().is_some_and(|c| c.provider == model.provider && c.model == model.model);
                    let choice = ModelSelection { provider: model.provider.clone(), model: model.model.clone(), thinking: current.as_ref().and_then(|c| c.thinking.clone()) };
                    let (store, session) = (store.clone(), session.clone());
                    menu = menu.item(PopupMenuItem::new(format!("{} · {}", model.model, model.provider)).checked(selected).on_click(move |_, _, cx| {
                        set_model(&store, session.clone(), choice.clone(), cx);
                    }));
                }
                if let Some(current) = current.clone() {
                    let levels = models.thinking_levels.get(&current.provider).cloned().unwrap_or_default();
                    if !levels.is_empty() {
                        menu = menu.separator().label(i18n::text(locale, "chat.thinking"));
                        for level in levels {
                            let key = THINKING.iter().find(|(id, _)| *id == level).map_or("chat.thinkingOff", |(_, key)| key);
                            let checked = current.thinking.as_deref().unwrap_or("off") == level;
                            let choice = ModelSelection { thinking: Some(level.clone()), ..current.clone() };
                            let (store, session) = (store.clone(), session.clone());
                            menu = menu.item(PopupMenuItem::new(i18n::text(locale, key)).checked(checked).on_click(move |_, _, cx| {
                                set_model(&store, session.clone(), choice.clone(), cx);
                            }));
                        }
                    }
                }
                menu
            })
    }

    fn permission_picker(&self, locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let current = self
            .chat
            .as_ref()
            .and_then(|chat| chat.read(cx).view.as_ref().map(|view| view.snapshot.permission.clone()))
            .unwrap_or_else(|| "ask".into());
        let label = PERMISSIONS.iter().find(|(id, _)| *id == current).map_or("chat.permissionAsk", |(_, key)| key);
        let store = self.store.clone();
        let session = self.session_id(cx);
        Button::new("permission-picker")
            .ghost()
            .small()
            .label(i18n::text(locale, label))
            .dropdown_caret(true)
            .disabled(session.is_none())
            .dropdown_menu(move |mut menu, _, _| {
                for (mode, key) in PERMISSIONS {
                    let (store, session) = (store.clone(), session.clone());
                    menu = menu.item(PopupMenuItem::new(i18n::text(locale, key)).checked(current == mode).on_click(move |_, _, cx| {
                        let Some(session) = session.clone() else { return };
                        let request = store.read(cx).call("chat.setPermission", json!({ "sessionId": session, "mode": mode }));
                        cx.background_spawn(async move {
                            let _ = request.await;
                        })
                        .detach();
                    }));
                }
                menu
            })
    }

    fn render_composer(&self, locale: Locale, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let busy = self.busy(cx);
        let context = self.chat.as_ref().and_then(|chat| chat.read(cx).view.as_ref().and_then(|view| view.snapshot.context.clone()));
        let model_picker = self.model_picker(locale, cx);
        let permission_picker = self.permission_picker(locale, cx);
        let session = self.session_id(cx);
        let attachments = (!self.attachments.is_empty()).then(|| self.render_attachments(cx));
        let theme = cx.theme();
        v_flex()
            .id("composer")
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
                    .child(h_flex().flex_1().min_w_0().overflow_hidden().gap_1().child(model_picker).child(permission_picker))
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
                            .tooltip(i18n::text(locale, "chat.send"))
                            .on_click(cx.listener(|this, _, window, cx| this.send(false, window, cx)))
                    })),
            )
    }
}

fn set_model(store: &Entity<Store>, session: Option<String>, choice: ModelSelection, cx: &mut App) {
    let request = match session {
        Some(session) => {
            let mut params = serde_json::to_value(&choice).unwrap_or_default();
            params["sessionId"] = json!(session);
            store.read(cx).call("chat.setModel", params)
        }
        None => store.read(cx).call("models.select", serde_json::to_value(&choice).unwrap_or_default()),
    };
    cx.background_spawn(async move {
        let _ = request.await;
    })
    .detach();
}

impl Render for ChatPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locale = self.store.read(cx).locale();
        let show_usage = self.store.read(cx).prefs.show_usage;
        let view = self.chat.as_ref().and_then(|chat| chat.read(cx).view.clone());
        let failed = self.chat.as_ref().and_then(|chat| chat.read(cx).error.clone());
        let weak = cx.weak_entity();
        let toggle: Toggle = Rc::new(move |key, row, _, cx| {
            let _ = weak.update(cx, |this, cx| this.toggle(key, row, cx));
        });
        let rows = self.rows.clone();
        let expanded = self.expanded.clone();
        let style = TurnStyle { locale, show_usage, step_detail: self.store.read(cx).prefs.step_detail.clone().into() };
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
                        Err(error) => window.push_notification(Notification::error(error.message), cx),
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
            let transcript = list(self.list.clone(), move |index, _, cx| {
                render_turn(&rows[index], index, &expanded, &toggle, &fork, &style, cx)
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
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(self.render_header(cx))
            .child(body)
            .when_some(view.as_ref().and_then(|view| view.snapshot.approvals.first().cloned()), |this, approval| {
                this.child(self.render_approval(&session, &approval, locale, cx))
            })
            .when_some(view.as_ref().map(|view| view.snapshot.queue.clone()).filter(|queue| !queue.is_empty()), |this, queue| {
                this.child(self.render_queue(&session, &queue, locale, cx))
            })
            .when_some(view.as_ref().and_then(|view| view.snapshot.retry.clone()), |this, retry| {
                let seconds = ((retry.at - now_ms()) / 1000.0).ceil().max(0.0);
                let retried = session.clone();
                this.child(
                    h_flex()
                        .mx_3()
                        .mb_2()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().text_xs().text_color(cx.theme().warning).child(trf(cx, "chat.retryWaiting", &[
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
            .when(view.as_ref().is_some_and(|view| view.snapshot.summary.status == "error" && view.snapshot.retry.is_none()), |this| {
                let error = view.as_ref().and_then(|view| view.snapshot.error.clone()).unwrap_or_default();
                let retried = session.clone();
                let theme = cx.theme();
                this.child(
                    h_flex()
                        .mx_3()
                        .mb_2()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .items_center()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.danger.opacity(0.4))
                        .bg(theme.danger.opacity(0.08))
                        .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.danger))
                        .child(div().flex_1().min_w_0().text_xs().child(if error.is_empty() { tr(cx, "chat.errorTitle") } else { error.into() }))
                        .child(Button::new("retry").outline().xsmall().icon(IconName::RefreshCw).label(tr(cx, "chat.retry")).on_click(cx.listener(move |this, _, window, cx| {
                            this.call("chat.retry", json!({ "sessionId": retried }), window, cx);
                        }))),
                )
            })
            .when_some(view.as_ref().map(|view| view.snapshot.background.clone()).filter(|commands| !commands.is_empty()), |this, commands| {
                this.child(self.render_background(&session, &commands, cx))
            })
            .child(self.changes.clone())
            .when(!self.suggestions.is_empty() && self.trigger.is_some(), |this| this.child(self.render_suggestions(cx)))
            .child(self.render_composer(locale, cx))
    }
}

impl ChatPane {
    /// The chat's background commands (`BackgroundBar.tsx`): stop a running one, clear a finished one.
    fn render_background(&self, session: &str, commands: &[rainy_protocol::BackgroundCommand], cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        v_flex()
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
            .children(commands.iter().enumerate().map(|(index, command)| {
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
            }))
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}
