//! The AI pane: the selected chat's transcript, its approvals and queue, and the composer
//! (`ChatPane.tsx`, `Composer.tsx`, `ApprovalBar.tsx`, `ModelPicker.tsx`, `PermissionPicker.tsx`). In Agent mode the
//! same pane is the middle of the window (`wide`): the transcript in a centred column, the composer below it with the
//! permission, model and context under it, and a new chat's composer in the middle under a greeting.

mod banners;
mod composer_view;
mod drafts;
mod empty_state;
mod model_card;
mod suggestions;

use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::base::GlobalState;
use gpui_kit::base::motion::{Presence, Transition};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{AssistantBlock, ContextBreakdown, TranscriptEntry};
use serde_json::{Value, json};

use drafts::{Attachment, Draft, report_failure};
#[allow(unused_imports)]
pub use model_card::model_control;
use model_card::waiting_for;
pub(crate) use model_card::{short_model_name, thinking_label};
use suggestions::Suggestion;

use crate::i18n::{self, Locale};
use crate::store::chat::ChatView;
use crate::store::ide::Ide;
use crate::store::{ChatModel, Store, StoreEvent};
use crate::ui::optics::backdrop::PetActivity;
use crate::ui::optics::pet::{self, MiniPet};
use crate::ui::tr;
use crate::ui::transcript::{Expanded, Fork, Toggle, TurnRender, TurnRow, TurnStyle, rebuild_rows, render_turn};

/// Minimum viewport dimensions for showing the compact active-chat companion without crowding narrow windows.
const MINI_PET_MIN_WIDTH: f32 = 640.;
const MINI_PET_MIN_HEIGHT: f32 = 420.;

/// Summary of the active chat's execution status used to drive [`MiniPet`] reactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PetTurnSignal {
    pub activity: PetActivity,
    /// Whether the turn ended cleanly (idle without error or user abort), eligible for a brief celebration.
    pub clean_idle: bool,
}

/// Maps a loaded [`ChatView`] (or pending new-chat creation) to its [`PetTurnSignal`].
pub(super) fn pet_turn_signal(view: Option<&ChatView>, creating: bool) -> PetTurnSignal {
    let Some(view) = view else {
        return PetTurnSignal {
            activity: if creating { PetActivity::Thinking } else { PetActivity::Idle },
            clean_idle: false,
        };
    };
    let snapshot = &view.snapshot;
    if !snapshot.approvals.is_empty() || snapshot.summary.awaiting_approval == Some(true) {
        return PetTurnSignal { activity: PetActivity::WaitingApproval, clean_idle: false };
    }
    match snapshot.summary.status.as_str() {
        "compacting" => PetTurnSignal { activity: PetActivity::Thinking, clean_idle: false },
        "running" => {
            let streaming_thinking = view.thinking_since.is_some()
                || snapshot
                    .streaming
                    .as_ref()
                    .and_then(|msg| msg.content.last())
                    .is_some_and(|block| matches!(block, AssistantBlock::Thinking { .. }));
            let activity = if streaming_thinking {
                PetActivity::Thinking
            } else if !view.tools.is_empty()
                || snapshot.streaming.as_ref().is_some_and(|msg| !msg.content.is_empty())
            {
                PetActivity::Responding
            } else {
                PetActivity::Thinking
            };
            PetTurnSignal { activity, clean_idle: false }
        }
        "idle" => {
            let mut clean = snapshot.error.is_none();
            if clean && let Some(last_turn) = view.turn_ranges().last() {
                let items = &snapshot.entries[last_turn.items.clone()];
                let aborted_notice = items
                    .iter()
                    .any(|entry| matches!(entry, TranscriptEntry::Notice { code: Some(code), .. } if code == "stopped"));
                let bad_stop = items.iter().rev().find_map(|entry| match entry {
                    TranscriptEntry::Assistant { message, .. } => Some(matches!(message.stop_reason.as_str(), "error" | "aborted")),
                    _ => None,
                });
                if aborted_notice || bad_stop == Some(true) {
                    clean = false;
                }
            }
            PetTurnSignal { activity: PetActivity::Idle, clean_idle: clean }
        }
        _ => PetTurnSignal { activity: PetActivity::Idle, clean_idle: false },
    }
}

/// Returns `true` only when the same chat transitions from an active turn into a clean idle finish.
pub(super) fn should_celebrate_pet(
    prev_session: Option<&str>,
    prev_activity: PetActivity,
    next_session: Option<&str>,
    next: PetTurnSignal,
) -> bool {
    prev_session.is_some()
        && prev_session == next_session
        && matches!(prev_activity, PetActivity::Thinking | PetActivity::Responding | PetActivity::WaitingApproval)
        && next.activity == PetActivity::Idle
        && next.clean_idle
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
    /// LRU recency order for bounded draft eviction (releasing image attachment bytes of stale drafts).
    draft_order: VecDeque<Option<String>>,
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
    /// Monotonic counter bumped each time the suggestion popup opens so its entrance transition replays reliably.
    suggestions_gen: u64,
    /// Tracks composer action button busy state to replay the send/stop transition on every toggle.
    action_busy: bool,
    action_gen: u64,
    /// Cached non-empty flag for the composer text so empty <-> non-empty transitions notify once for the send button fade.
    composer_has_text: bool,
    completion_seq: u64,
    /// The `completion_seq` whose files the list shows; an older one while a newer query is under way.
    shown_seq: u64,
    completion_task: Option<Task<()>>,
    /// Tab or Enter came while the list showed an older query's files: the first of the new ones goes in.
    accept_shown: bool,
    /// When Esc was pressed last while the chat ran; a second Esc soon after stops it.
    last_escape: Option<Instant>,
    /// Redraws live thinking, waiting and retry countdown timers once per second without rebuilding rows.
    ticker: Option<Task<()>>,
    /// The chat and external agent whose usage limits were last asked for, so showing the chat asks once.
    quota_asked: std::cell::RefCell<Option<String>>,
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
    /// Cached scrolled-up state so the scroll handler only notifies when crossing the threshold.
    scrolled_up: bool,
    composer: Entity<TextareaState>,
    expanded: Expanded,
    /// The approval bar takes Enter (allow) and Esc (deny) while it has the focus; Allow itself never takes it.
    approval_focus: FocusHandle,
    /// The approval the bar shows, and the `tool_call_id` whose answer RPC is currently in flight.
    approval_shown: Option<String>,
    answering_for: Option<String>,
    /// The run error shows all of its text.
    error_open: bool,
    background_scroll: ScrollHandle,
    queue_scroll: ScrollHandle,
    /// Cached `session.contextBreakdown` result keyed by `(session_id, chat_revision)`.
    breakdown: Option<(String, u64, Result<ContextBreakdown, SharedString>)>,
    /// In-flight `session.contextBreakdown` request key to deduplicate triggers.
    breakdown_loading: Option<(String, u64)>,
    /// Drawn as the middle of Agent mode rather than as the IDE's side pane.
    wide: bool,
    /// The optical look's page clock and usage overview, made the first time a new chat shows them.
    backdrop: Option<Entity<crate::ui::optics::backdrop::Backdrop>>,
    usage: Option<Entity<crate::ui::optics::usage::UsageCard>>,
    /// Low-distraction stationary mini companion for active Agent-mode chats when optics are enabled.
    mini_pet: Entity<MiniPet>,
    /// Whether the last render drew the optical page: the field wakes when the page comes back, not when the page
    /// that was showing is repainted for an unrelated update.
    optics_shown: bool,
    /// Whether the last render drew `mini_pet`, so composer typing only wakes it while visible.
    mini_pet_shown: bool,
    /// Last `(session_id, activity)` synced to `mini_pet` to prevent false celebrations across chat switches.
    pet_state: Option<(Option<String>, PetActivity)>,
    /// Suppresses typing notifications during programmatic `set_value` (draft swaps and post-send clears).
    pub(super) suppress_typing_notify: bool,
    /// Cached active workspace `(id, title)` so greeting & worktree picker update on rename/switch without noisy editor redraws.
    workspace_sig: Option<(String, String)>,
    _subscriptions: Vec<Subscription>,
    chat_subscription: Option<Subscription>,
}

impl ChatPane {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(600.));
        list.set_follow_mode(FollowMode::Tail);
        // The jump-to-latest button shows while the reader is scrolled up; only notify when that flag flips.
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |_, _, cx| {
            let weak = weak.clone();
            cx.defer(move |cx| {
                let _ = weak.update(cx, |this, cx| {
                    let next = this.compute_scrolled_up();
                    if this.scrolled_up != next {
                        this.scrolled_up = next;
                        cx.notify();
                    }
                });
            });
        });
        let placeholder = tr(cx, "chat.placeholder");
        let composer = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 12).submit_on_enter(true).placeholder(placeholder));
        let pane = cx.weak_entity();
        let ide = Ide::global(cx);
        let initial_workspace_sig = ide.read(cx).workspace.as_ref().map(|w| (w.workspace_id.clone(), w.title.clone()));
        let subscriptions = vec![
            cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, window, cx| match event {
                // Shift+Enter: the text area has put in a line break, and still reports the key; it sends nothing.
                InputEvent::PressEnter { shift: true, .. } => {}
                InputEvent::PressEnter { secondary, .. } => {
                    // Enter takes the highlighted suggestion while the list is open.
                    if !this.suggestions.is_empty() && this.trigger.is_some() {
                        this.take_suggestion(true, window, cx);
                    } else {
                        this.send(*secondary, window, cx);
                    }
                }
                InputEvent::Change => {
                    this.update_trigger(window, cx);
                    this.notify_pet_typed(window, cx);
                }
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
                StoreEvent::SummariesChanged => {
                    if this.chat.is_some() || this.creating {
                        cx.notify();
                    }
                }
                StoreEvent::Host { name, data } if name == "security.changed" => {
                    if let Some(mode) = data["defaultMode"].as_str() {
                        this.default_permission = mode.to_owned();
                        cx.notify();
                    }
                }
                StoreEvent::Host { name, .. } if matches!(name.as_str(), "models.changed" | "external.changed" | "external.commands" | "composer.pending") => {
                    cx.notify();
                }
                StoreEvent::HostReconnected(_) => {
                    this.breakdown = None;
                    this.breakdown_loading = None;
                    this.follow_selected(window, cx);
                    this.read_default_permission(cx);
                    cx.notify();
                }
                StoreEvent::Host { .. }
                | StoreEvent::HostExited(_)
                | StoreEvent::ResourcesDamaged(_)
                | StoreEvent::Update(_)
                | StoreEvent::TeamChanged(_)
                | StoreEvent::CredentialAlert(_) => {}
            }),
            cx.observe(&ide, |this, ide, cx| {
                let next = ide.read(cx).workspace.as_ref().map(|w| (w.workspace_id.clone(), w.title.clone()));
                if this.workspace_sig != next {
                    this.workspace_sig = next;
                    cx.notify();
                }
            }),
        ];
        let changes = cx.new(|cx| crate::ui::changes::ChangesBar::new(store.clone(), cx));
        let mini_pet = cx.new(|cx| MiniPet::new(window, cx));
        let mut pane = Self {
            attachments: Vec::new(),
            drafts: HashMap::new(),
            draft_order: VecDeque::new(),
            draft_key: None,
            creating: false,
            default_permission: "ask".into(),
            trigger: None,
            suggestions: Vec::new(),
            active_suggestion: 0,
            suggestions_scroll: ScrollHandle::new(),
            suggestions_gen: 0,
            action_busy: false,
            action_gen: 0,
            composer_has_text: false,
            completion_seq: 0,
            shown_seq: 0,
            completion_task: None,
            accept_shown: false,
            last_escape: None,
            ticker: None,
            quota_asked: std::cell::RefCell::default(),
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
            scrolled_up: false,
            composer,
            expanded: Rc::default(),
            approval_focus: cx.focus_handle(),
            approval_shown: None,
            answering_for: None,
            error_open: false,
            background_scroll: ScrollHandle::new(),
            queue_scroll: ScrollHandle::new(),
            breakdown: None,
            breakdown_loading: None,
            wide: false,
            backdrop: None,
            usage: None,
            mini_pet,
            optics_shown: false,
            mini_pet_shown: false,
            pet_state: None,
            suppress_typing_notify: false,
            workspace_sig: initial_workspace_sig,
            _subscriptions: subscriptions,
            chat_subscription: None,
        };
        pane.follow_selected(window, cx);
        pane.read_default_permission(cx);
        pane
    }

    /// Draw the pane as the middle of Agent mode, or as the IDE's AI pane.
    pub fn set_wide(&mut self, wide: bool, cx: &mut Context<Self>) {
        if self.wide != wide {
            self.wide = wide;
            // Rows wrap at another width.
            self.list.remeasure();
            cx.notify();
        }
    }

    pub fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        self.composer.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Inject a quoted block (e.g. terminal output or diagnostic error) into the active chat composer without sending yet.
    pub fn inject_into_composer(&mut self, snippet: String, window: &mut Window, cx: &mut Context<Self>) {
        let trimmed = snippet.trim();
        if trimmed.is_empty() {
            return;
        }
        let current = self.composer.read(cx).value().to_string();
        let next = if current.trim().is_empty() {
            format!("{trimmed}\n\n")
        } else {
            format!("{}\n\n{trimmed}\n\n", current.trim_end())
        };
        self.composer.update(cx, |state, cx| {
            state.set_value(next, window, cx);
            state.focus(window, cx);
        });
        self.composer_has_text = true;
        cx.notify();
    }

    /// The files the chat changed, which Agent mode shows in its changes panel.
    pub fn changes(&self) -> Entity<crate::ui::changes::ChangesBar> {
        self.changes.clone()
    }

    fn compute_scrolled_up(&self) -> bool {
        self.list.max_offset_for_scrollbar().y > px(0.)
            && !self.list.is_following_tail()
            && !self.list.is_scrolled_to_end().unwrap_or(false)
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
        self.approval_shown = None;
        self.answering_for = None;
        self.breakdown = None;
        self.breakdown_loading = None;
        self.scrolled_up = false;
        self.list.reset(0);
        self.list.set_follow_mode(FollowMode::Tail);
        self.rebuild(cx);
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

    /// Build the turns again after the chat changed, and keep the 1 s timer alive when a live timer is visible.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.rebuild_rows(cx);
        self.ensure_ticker(cx);
    }

    /// Schedule a lightweight 1 s notify tick while thinking, waiting, or retry countdown timers are active.
    /// Does not rebuild rows or remeasure list items; elapsed counters are computed live during render.
    fn ensure_ticker(&mut self, cx: &mut Context<Self>) {
        let (thinking, retry_active) = self.chat.as_ref().map_or((false, false), |chat| {
            chat.read(cx).view.as_ref().map_or((false, false), |view| {
                (view.thinking_since.is_some(), view.snapshot.retry.is_some())
            })
        });
        let waiting = self.rows.last().is_some_and(|row| row.waiting.is_some());
        if (thinking || waiting || retry_active) && self.ticker.is_none() {
            self.ticker = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let _ = this.update(cx, |this, cx| {
                    this.ticker = None;
                    cx.notify();
                    this.ensure_ticker(cx);
                });
            }));
        }
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
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
        self.scrolled_up = self.compute_scrolled_up();
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

    pub(super) fn session_id(&self, cx: &App) -> Option<String> {
        self.chat.as_ref().map(|chat| chat.read(cx).id.clone())
    }

    pub(super) fn busy(&self, cx: &App) -> bool {
        self.chat.as_ref().is_some_and(|chat| chat.read(cx).busy())
    }

    /// The new-chat composer is waiting for the chat its message creates.
    pub(super) fn creating_here(&self) -> bool {
        self.creating && self.draft_key.is_none()
    }

    pub(super) fn call(&self, method: &'static str, params: Value, window: &mut Window, cx: &mut Context<Self>) {
        report_failure(self.store.read(cx).call(method, params), window, cx);
    }

    /// The kind of external agent the active (or pending new) chat runs on.
    pub(super) fn external_kind(&self, cx: &App) -> Option<String> {
        if let Some(chat) = &self.chat {
            let model = chat.read(cx);
            if let Some(view) = &model.view {
                return view.snapshot.summary.external_backend.as_ref().map(|backend| backend.kind.clone());
            }
            return self.store.read(cx).summaries.iter().find(|s| s.id == model.id).and_then(|s| s.external_backend.as_ref().map(|backend| backend.kind.clone()));
        }
        self.store.read(cx).pending_external.as_ref().map(|backend| backend.kind.clone())
    }

    /// Whether the active (or pending new) chat runs on an external CLI agent rather than Rainy's built-in engine.
    pub(super) fn is_external_chat(&self, cx: &App) -> bool {
        if let Some(chat) = &self.chat {
            let model = chat.read(cx);
            if let Some(view) = &model.view {
                return view.snapshot.summary.external_backend.is_some();
            }
            return self
                .store
                .read(cx)
                .summaries
                .iter()
                .find(|s| s.id == model.id)
                .is_some_and(|s| s.external_backend.is_some());
        }
        self.store.read(cx).pending_external.is_some()
    }

    /// A new chat, or one with nothing in it yet; not a chat being created for the message just sent.
    fn is_blank(&self, cx: &App) -> bool {
        match &self.chat {
            None => !self.creating_here(),
            Some(chat) => chat.read(cx).view.as_ref().is_some_and(|view| view.snapshot.entries.is_empty() && view.snapshot.streaming.is_none() && !view.busy()),
        }
    }

    fn current_pet_signal(&self, cx: &App) -> PetTurnSignal {
        let creating = self.creating_here();
        match &self.chat {
            Some(chat) => pet_turn_signal(chat.read(cx).view.as_ref(), creating),
            None => pet_turn_signal(None, creating),
        }
    }

    fn notify_pet_typed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.suppress_typing_notify || self.composer.read(cx).value().is_empty() {
            return;
        }
        if self.optics_shown
            && let Some(backdrop) = self.backdrop.clone()
        {
            backdrop.update(cx, |backdrop, cx| backdrop.notify_typed(window, cx));
        } else if self.mini_pet_shown {
            self.mini_pet.update(cx, |pet, cx| pet.notify_typed(window, cx));
        }
    }

    /// Floating "back to latest" capsule with solid theme surface, crisp border, subtle elevation shadow and 140 ms
    /// `Presence` fade/rise so it never blends illegibly into the transcript text underneath.
    fn render_to_latest(&self, scrolled_up: bool, locale: Locale, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let presence = Presence::new("chat-to-latest", scrolled_up)
            .transition(Transition::new(Duration::from_millis(140)))
            .sample(window, cx);
        if !presence.should_render() {
            return None;
        }
        let p = presence.progress;
        let dy = if cx.reduce_motion() { 0.0 } else { 6.0 * (1.0 - p) };
        let (popover, popover_foreground, border, accent, dark) = {
            let theme = cx.theme();
            (theme.popover, theme.popover_foreground, theme.border, theme.accent, theme.mode.is_dark())
        };
        let shadow_alpha = (if dark { 0.36 } else { 0.14 }) * p.powi(3);
        let pill = h_flex()
            .id("to-latest")
            .h(px(28.))
            .px_3()
            .gap_1p5()
            .items_center()
            .rounded_full()
            .bg(popover)
            .text_color(popover_foreground)
            .border_1()
            .border_color(border)
            .shadow(vec![BoxShadow {
                color: hsla(0.0, 0.0, 0.0, shadow_alpha),
                offset: point(px(0.), px(2.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
                inset: false,
            }])
            .text_xs()
            .font_medium()
            .cursor_pointer()
            .hover(move |style| style.bg(accent))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                GlobalState::suppress_text_selection(cx);
                cx.stop_propagation();
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.list.set_follow_mode(FollowMode::Tail);
                this.list.scroll_to_end();
                this.scrolled_up = false;
                cx.notify();
            }))
            .child(Icon::new(IconName::ArrowDown).xsmall())
            .child(i18n::text(locale, "chat.backToLatest"));
        Some(
            div()
                .absolute()
                .bottom_2()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(div().relative().top(px(dy)).opacity(p).child(pill))
                .into_any_element(),
        )
    }
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
            self.answering_for = None;
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
        let chat_busy = self.busy(cx);
        if chat_busy != self.action_busy {
            self.action_busy = chat_busy;
            self.action_gen = self.action_gen.wrapping_add(1);
        }
        let style = TurnStyle {
            locale,
            show_usage,
            step_detail: self.store.read(cx).prefs.step_detail.clone().into(),
            session: self.session_id(cx).map(SharedString::from),
            waiting_for: waiting_for(&self.store, self.chat.as_ref(), cx),
            chat_busy,
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
        let wide = self.wide;
        let optics_on = crate::ui::optics::enabled(cx);
        let has_suggestions = !self.suggestions.is_empty() && self.trigger.is_some();
        // Agent mode greets a new chat with the composer in the middle of the window.
        let greeting = wide && failed.is_none() && rows.is_empty() && self.is_blank(cx);
        if !greeting
            && std::mem::replace(&mut self.optics_shown, false)
            && let Some(backdrop) = self.backdrop.clone()
        {
            backdrop.update(cx, |backdrop, cx| {
                backdrop.reset_ephemeral();
                cx.notify();
            });
        }
        let viewport = window.viewport_size();
        let show_mini_pet = wide
            && !greeting
            && failed.is_none()
            && optics_on
            && !has_suggestions
            && viewport.width >= px(MINI_PET_MIN_WIDTH)
            && viewport.height >= px(MINI_PET_MIN_HEIGHT);
        let current_session = self.session_id(cx);
        let signal = self.current_pet_signal(cx);
        if show_mini_pet {
            let was_shown = std::mem::replace(&mut self.mini_pet_shown, true);
            let next_key = (current_session.clone(), signal.activity);
            if !was_shown || self.pet_state.as_ref() != Some(&next_key) {
                let (session_changed, celebrate) = match &self.pet_state {
                    Some((prev_session, prev_activity)) => (
                        prev_session != &current_session,
                        was_shown && should_celebrate_pet(prev_session.as_deref(), *prev_activity, current_session.as_deref(), signal),
                    ),
                    None => (false, false),
                };
                self.pet_state = Some(next_key);
                self.mini_pet.update(cx, |pet, cx| pet.sync_activity(signal.activity, celebrate, session_changed, window, cx));
            }
        } else {
            if std::mem::replace(&mut self.mini_pet_shown, false) {
                self.mini_pet.update(cx, |pet, _| pet.reset_silent());
            }
            self.pet_state = Some((current_session, signal.activity));
        }
        let body = if greeting {
            self.render_greeting(locale, window, cx)
        } else if rows.is_empty() {
            match failed {
                Some(error) => div().flex_1().p_4().text_sm().text_color(cx.theme().danger).child(error).into_any_element(),
                None if wide => div().flex_1().into_any_element(),
                None => self.render_empty(cx).into_any_element(),
            }
        } else {
            let scrolled_up = self.compute_scrolled_up();
            self.scrolled_up = scrolled_up;
            let header = self.header;
            let older = self.older.clone().filter(|_| header > 0).map(|older| (older, tr(cx, "chat.loadOlder")));
            let pane = cx.weak_entity();
            let transcript = list(self.list.clone(), move |index, window, cx| {
                let row = if index < header
                    && let Some((older, label)) = &older
                {
                    render_older(older, label.clone(), pane.clone(), cx)
                } else {
                    let render_ctx = TurnRender { expanded: &expanded, toggle: &toggle, fork: &fork, style: &style };
                    render_turn(&rows[index - header], index, &render_ctx, window, cx)
                };
                if wide { centered(row) } else { row }
            })
            .size_full()
            .py_2();
            let to_latest = self.render_to_latest(scrolled_up, locale, window, cx);
            div()
                .id("transcript")
                .relative()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(transcript)
                .vertical_scrollbar(&self.list)
                .children(to_latest)
                .into_any_element()
        };
        let session = self.session_id(cx).unwrap_or_default();
        // The changes panel of Agent mode shows them instead.
        let changes = (!self.changes.read(cx).docked()).then(|| self.changes.clone());
        let mini_pet = show_mini_pet.then(|| {
            let (_, mini_h) = pet::mini_size();
            div().absolute().top(px(-mini_h)).right(px(28.)).child(self.mini_pet.clone())
        });
        // Everything below the transcript keeps its height (each list in it scrolls past a cap); the transcript
        // gives way, so nothing overlaps or pushes the composer out of the pane. In Agent mode it lines up with the
        // transcript's column.
        let below = (!greeting).then(|| {
            v_flex()
                .relative()
                .w_full()
                .flex_shrink_0()
                .when(wide, |this| this.max_w(px(crate::ui::agent::COLUMN)).px_1())
                .children(mini_pet)
                .when_some(approval, |this, approval| this.child(self.render_approval(&approval, approval_focused, locale, cx)))
                .when(!queue.is_empty(), |this| this.child(self.render_queue(&session, &queue, locale, cx)))
                .when_some(retry, |this, retry| this.child(self.render_retry(&session, &retry, cx)))
                // A run that ended in an error can ask the model again.
                .when_some(run_error, |this, error| this.child(self.render_run_error(&session, error, cx)))
                .when(!background.is_empty(), |this| this.child(self.render_background(&session, &background, cx)))
                .children(changes)
                .when(has_suggestions, |this| this.child(self.render_suggestions(cx)))
                .map(|this| {
                    if wide {
                        let this = this.child(self.render_composer_wide(locale, window, cx));
                        // The optical look has the pickers in the glass: the gap their row kept under it stays.
                        if optics_on {
                            this.child(div().flex_shrink_0().h_3())
                        } else {
                            this.child(self.render_composer_footer(locale, false, cx))
                        }
                    } else {
                        this.child(self.render_composer(locale, window, cx))
                    }
                })
        });
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .when(!wide, |this| this.child(self.render_header(cx)))
            .child(body)
            .when_some(below, |this, below| this.child(h_flex().w_full().flex_shrink_0().justify_center().child(below)))
    }
}

/// A row of the transcript in Agent mode's column, centred in a wider window.
fn centered(row: AnyElement) -> AnyElement {
    h_flex().w_full().justify_center().child(div().w_full().max_w(px(crate::ui::agent::COLUMN)).child(row)).into_any_element()
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

pub(super) fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}
