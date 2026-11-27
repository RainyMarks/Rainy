//! Model & reasoning level selector popover, external agent picker, and orchestration setup.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::reasoning::{Level, Reasoning};
use rainy_protocol::{ExternalAgentStatus, ExternalBackendRef, ModelSelection, ModelSetup};
use serde_json::json;

use super::drafts::report_failure;
use crate::i18n::{self, Locale};
use crate::store::{ChatModel, Store};

/// The labels of the reasoning levels, by the names the providers share, and what each trades.
pub(super) const THINKING: [(Level, &str, &str); 7] = [
    (Level::Off, "chat.thinkingOff", "chat.thinkingOffInfo"),
    (Level::Minimal, "chat.thinkingMinimal", "chat.thinkingMinimalInfo"),
    (Level::Low, "chat.thinkingLow", "chat.thinkingLowInfo"),
    (Level::Medium, "chat.thinkingMedium", "chat.thinkingMediumInfo"),
    (Level::High, "chat.thinkingHigh", "chat.thinkingHighInfo"),
    (Level::XHigh, "chat.thinkingXHigh", "chat.thinkingXHighInfo"),
    (Level::Max, "chat.thinkingMax", "chat.thinkingMaxInfo"),
];

/// The card the model control opens.
const CARD_WIDTH: f32 = 280.;
const CARD_PADDING: f32 = 8.;
/// How long a level sent from the card is shown, at most, before the chat reports it.
const PENDING: Duration = Duration::from_secs(3);

/// What the model control shows: the selection the composer sends with, what its model offers for reasoning and the
/// level requests use now. Works for both built-in models and external CLI agents (`external:<kind>`).
pub(super) struct ModelState {
    pub(super) current: ModelSelection,
    pub(super) levels: Vec<Level>,
    pub(super) default_level: Option<Level>,
    pub(super) toggle_reasoning: bool,
    pub(super) always_reasons: bool,
    pub(super) level: Option<Level>,
    pub(super) name: SharedString,
    /// Present when this chat (or pending new chat) targets an external CLI agent.
    pub(super) external: Option<ExternalBackendRef>,
    /// Compact badge label for external agents (`Codex`, `AGY`, `ACP`, or custom label).
    pub(super) external_badge: Option<SharedString>,
    /// The executor of an orchestrated chat (the model above plans), and its short name.
    pub(super) executor: Option<(ModelSelection, SharedString)>,
}

impl ModelState {
    fn adjustable(&self) -> bool {
        self.levels.len() > 1
    }

    fn reasons(&self) -> bool {
        self.always_reasons || self.levels.iter().any(|level| *level != Level::Off)
    }

    fn toggle(&self) -> bool {
        self.toggle_reasoning
    }
}

/// The card's own state, kept while it is open, isolated per active session.
#[derive(Default)]
pub(super) struct CardState {
    /// Session id this card state belongs to; switching sessions resets transient navigation/drag state.
    pub(super) session: Option<String>,
    /// A level sent and not yet reported by the chat: the selection it replaced, its index and when it was sent.
    pub(super) pending: Option<(ModelSelection, usize, Instant)>,
    /// The model list shows instead of the levels.
    pub(super) list: bool,
    /// The orchestration page shows.
    pub(super) orchestrate: bool,
    /// The external agent whose usage limits this opening already asked the Host to read again.
    pub(super) quota_asked: Option<String>,
    /// The level under the pointer while the reasoning slider is dragged.
    pub(super) drag: Option<usize>,
}

impl CardState {
    /// Reset transient popover state whenever the active session changes so another chat's selection never bleeds across.
    pub(super) fn sync_session(&mut self, session: &Option<String>) -> bool {
        if self.session == *session {
            return false;
        }
        self.session = session.clone();
        self.pending = None;
        self.list = false;
        self.orchestrate = false;
        self.quota_asked = None;
        self.drag = None;
        true
    }
}

struct CardContext<'a> {
    store: &'a Entity<Store>,
    session: Option<String>,
    state: &'a ModelState,
    card: &'a Entity<CardState>,
    popover: Entity<PopoverState>,
    locale: Locale,
}

struct LevelChoice {
    store: Entity<Store>,
    session: Option<String>,
    card: Entity<CardState>,
    current: ModelSelection,
    level: Level,
    index: usize,
}

fn external_badge_label(backend: &ExternalBackendRef, locale: Locale) -> SharedString {
    match backend.kind.as_str() {
        "codex" => i18n::text(locale, "external.badgeCodex").into(),
        "antigravity" => i18n::text(locale, "external.badgeAgy").into(),
        _ => backend
            .label
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .map(|label| label.to_owned().into())
            .unwrap_or_else(|| i18n::text(locale, "external.badgeAcp").into()),
    }
}

pub(super) fn external_ref_from_status(agent: &ExternalAgentStatus) -> ExternalBackendRef {
    ExternalBackendRef {
        kind: agent.kind.clone(),
        binary: agent.binary.clone(),
        args: agent.args.clone(),
        label: Some(agent.label.clone()),
    }
}

pub(super) fn external_default_selection(agent: &ExternalAgentStatus, model_id: Option<&str>) -> ModelSelection {
    let model = model_id
        .map(str::to_owned)
        .or_else(|| agent.default_model.clone())
        .or_else(|| agent.models.iter().find(|m| m.is_default == Some(true)).map(|m| m.id.clone()))
        .or_else(|| agent.models.first().map(|m| m.id.clone()))
        .unwrap_or_else(|| "default".to_owned());
    let thinking = agent
        .models
        .iter()
        .find(|m| m.id == model)
        .and_then(|m| m.default_effort.clone())
        .or_else(|| agent.default_thinking.clone());
    ModelSelection { provider: format!("external:{}", agent.kind), model, thinking }
}

pub(super) fn external_model_display(model_id: &str, agent: Option<&ExternalAgentStatus>, backend: &ExternalBackendRef, locale: Locale) -> String {
    if let Some(info) = agent.and_then(|a| a.models.iter().find(|m| m.id == model_id))
        && let Some(name) = info.display_name.as_deref().filter(|name| !name.trim().is_empty())
    {
        return name.to_owned();
    }
    if model_id == "default" || model_id.trim().is_empty() {
        return agent
            .map(|a| a.label.clone())
            .or_else(|| backend.label.clone().filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| i18n::text(locale, "external.defaultModel").to_owned());
    }
    short_model_name(model_id, None)
}

fn parse_external_levels(efforts: &[String]) -> Vec<Level> {
    let mut levels: Vec<Level> = efforts
        .iter()
        .filter_map(|raw| match raw.trim().to_ascii_lowercase().as_str() {
            "none" | "off" => Some(Level::Off),
            other => Level::parse(other),
        })
        .collect();
    levels.sort();
    levels.dedup();
    levels
}

fn resolve_external_level(levels: &[Level], requested: Option<&str>, default_level: Option<Level>) -> Option<Level> {
    if levels.is_empty() {
        return None;
    }
    let parsed = requested.and_then(|raw| match raw.trim().to_ascii_lowercase().as_str() {
        "none" | "off" => Some(Level::Off),
        other => Level::parse(other),
    });
    if let Some(level) = parsed.or(default_level) {
        if levels.contains(&level) {
            return Some(level);
        }
        return levels.iter().copied().min_by_key(|candidate| (*candidate as i32 - level as i32).abs());
    }
    levels.first().copied()
}

/// The model the composer sends with: the chat's, or the one new chats start with.
pub(super) fn model_state(store: &Entity<Store>, chat: Option<&Entity<ChatModel>>, cx: &App) -> Option<ModelState> {
    let data = store.read(cx);
    let locale = data.locale();
    let external = match chat {
        Some(chat) => {
            let model = chat.read(cx);
            model
                .view
                .as_ref()
                .and_then(|view| view.snapshot.summary.external_backend.clone())
                .or_else(|| data.summaries.iter().find(|s| s.id == model.id).and_then(|s| s.external_backend.clone()))
        }
        None => data.pending_external.clone(),
    };
    if let Some(backend) = external {
        let agent = data.external_agents.iter().find(|a| a.kind == backend.kind);
        let chat_model = chat.and_then(|chat| chat.read(cx).view.as_ref().and_then(|view| view.snapshot.model.clone()));
        let current = match chat {
            Some(_) => chat_model.unwrap_or_else(|| match agent {
                Some(agent) => external_default_selection(agent, None),
                None => ModelSelection {
                    provider: format!("external:{}", backend.kind),
                    model: "default".to_owned(),
                    thinking: None,
                },
            }),
            None => data.pending_external_model.clone().unwrap_or_else(|| match agent {
                Some(agent) => external_default_selection(agent, None),
                None => ModelSelection {
                    provider: format!("external:{}", backend.kind),
                    model: "default".to_owned(),
                    thinking: None,
                },
            }),
        };
        let info = agent.and_then(|a| a.models.iter().find(|m| m.id == current.model));
        let levels = info.map(|m| parse_external_levels(&m.reasoning_efforts)).unwrap_or_default();
        let default_level = info
            .and_then(|m| m.default_effort.as_deref())
            .or_else(|| agent.and_then(|a| a.default_thinking.as_deref()))
            .and_then(Level::parse)
            .filter(|lvl| levels.contains(lvl))
            .or_else(|| levels.first().copied());
        let level = resolve_external_level(&levels, current.thinking.as_deref(), default_level);
        let display = external_model_display(&current.model, agent, &backend, locale);
        let badge = external_badge_label(&backend, locale);
        return Some(ModelState {
            current,
            levels,
            default_level,
            toggle_reasoning: false,
            always_reasons: false,
            level,
            name: display.into(),
            external: Some(backend),
            external_badge: Some(badge),
            executor: None,
        });
    }

    let models = data.models.as_ref()?;
    let chat_model = chat.and_then(|chat| chat.read(cx).view.as_ref().and_then(|view| view.snapshot.model.clone()));
    let current = chat_model.or_else(|| models.selected.clone())?;
    let profile = models.model(&current.provider).filter(|profile| profile.model == current.model);
    let reasoning = profile.map_or(Reasoning::NONE, ModelSetup::reasoning);
    // The level the Host resolves for this selection (`effective_thinking`).
    let level = reasoning.choose(current.thinking.as_deref().or_else(|| profile.and_then(|profile| profile.thinking.as_deref())));
    let preset = models.presets.iter().find(|preset| preset.model.provider == current.provider && preset.model.model == current.model);
    let name = short_model_name(&current.model, preset.map(|preset| preset.name.as_str())).into();
    let executor = match chat {
        Some(chat) => chat.read(cx).view.as_ref().and_then(|view| view.snapshot.summary.orchestrate.clone()),
        None => data.pending_orchestrate.clone(),
    }
    .map(|executor| {
        let preset = models.presets.iter().find(|preset| preset.model.provider == executor.provider && preset.model.model == executor.model);
        let name = short_model_name(&executor.model, preset.map(|preset| preset.name.as_str())).into();
        (executor, name)
    });
    Some(ModelState {
        current,
        levels: reasoning.levels().collect(),
        default_level: reasoning.default,
        toggle_reasoning: reasoning.toggle(),
        always_reasons: reasoning.always,
        level,
        name,
        external: None,
        external_badge: None,
        executor,
    })
}

/// A level's label; a model that only switches reasoning calls its level On.
pub(crate) fn thinking_label(locale: Locale, level: Level, toggle: bool) -> &'static str {
    if toggle && level != Level::Off {
        return i18n::text(locale, "chat.thinkingOn");
    }
    i18n::text(locale, THINKING.iter().find(|(known, ..)| *known == level).map_or("chat.thinkingOff", |(_, key, _)| key))
}

/// What a level trades, in a few words, for the card's list.
pub(super) fn thinking_info(locale: Locale, level: Level, toggle: bool) -> &'static str {
    if toggle && level != Level::Off {
        return i18n::text(locale, "chat.thinkingOnInfo");
    }
    i18n::text(locale, THINKING.iter().find(|(known, ..)| *known == level).map_or("chat.thinkingOffInfo", |(.., info)| info))
}

/// A model's name as the composer shows it: a preset's without the vendor ("Opus 5.5"), a known id in its vendor's
/// spelling ("GPT-6.1 Sol", "Gemini 3.1 Pro"), else the id.
pub(crate) fn short_model_name(id: &str, preset: Option<&str>) -> String {
    if let Some(name) = preset {
        return name.strip_prefix("Claude ").unwrap_or(name).to_owned();
    }
    let base = id.rsplit('/').next().unwrap_or(id);
    let lower = base.to_ascii_lowercase();
    let words: Vec<&str> = lower.split('-').filter(|word| !word.is_empty()).collect();
    let title = |word: &str| {
        let mut chars = word.chars();
        chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
    };
    let joined = |first: String, from: usize| std::iter::once(first).chain(words[from..].iter().map(|word| title(word))).collect::<Vec<_>>().join(" ");
    match words.as_slice() {
        ["claude", family, version @ ..] if !version.is_empty() => {
            // `claude-haiku-4-5-20251001`: the version is the short numbers before a date.
            let numbers: Vec<&str> = version.iter().copied().take_while(|part| part.len() <= 2 && part.chars().all(|c| c.is_ascii_digit())).collect();
            format!("{} {}", title(family), numbers.join("."))
        }
        ["gpt" | "glm", version, ..] => joined(format!("{}-{}", words[0].to_ascii_uppercase(), if version.chars().all(char::is_alphabetic) { version.to_ascii_uppercase() } else { (*version).to_owned() }), 2),
        ["deepseek", ..] => joined("DeepSeek".into(), 1),
        [first, ..] if ["gemini", "grok", "kimi", "mistral", "magistral"].contains(first) || first.starts_with("qwen") => joined(title(first), 1),
        _ => base.to_owned(),
    }
}

/// Who a live turn of `chat` waits for: the external agent's name ("Codex"), else the model's.
pub(super) fn waiting_for(store: &Entity<Store>, chat: Option<&Entity<ChatModel>>, cx: &App) -> SharedString {
    let Some(state) = model_state(store, chat, cx) else { return SharedString::default() };
    match &state.external {
        Some(backend) => store
            .read(cx)
            .external_agents
            .iter()
            .find(|agent| agent.kind == backend.kind)
            .map(|agent| agent.label.clone())
            .or_else(|| backend.label.clone())
            .unwrap_or_else(|| backend.kind.clone())
            .into(),
        None => state.name,
    }
}

/// The model and reasoning control both chat layouts place under their composer: the model's short name and its
/// reasoning level, opening a card with the level slider and the model list. `chat` is the chat shown, `None` before a
/// new chat's first message. The card reads both on every draw, so it follows a chat or model switched while it is
/// open.
pub fn model_control(store: &Entity<Store>, chat: Option<&Entity<ChatModel>>, cx: &App) -> AnyElement {
    let locale = store.read(cx).locale();
    let state = model_state(store, chat, cx);
    let theme = cx.theme();
    let badge = state.as_ref().and_then(|state| state.external_badge.clone());
    let name: SharedString = state.as_ref().map_or_else(
        || i18n::text(locale, "chat.noModel").into(),
        |state| match &state.executor {
            Some((_, executor)) => format!("{} → {executor}", state.name).into(),
            None => state.name.clone(),
        },
    );
    let optics = crate::ui::optics::enabled(cx).then(|| crate::ui::optics::look::Look::of(theme).worst_glass(theme.background));
    let level = state.as_ref().filter(|state| state.adjustable()).and_then(|state| {
        state.level.map(|level| {
            let color = match optics {
                Some(glass) => crate::ui::optics::look::level_color(level, theme, &[theme.popover, glass]),
                None => theme.muted_foreground,
            };
            (thinking_label(locale, level, state.toggle()), color)
        })
    });
    // An external agent close to its limit says what is left, so it shows before a turn fails.
    let low = state.as_ref().and_then(|state| {
        let backend = state.external.as_ref()?;
        let agent = store.read(cx).external_agents.iter().find(|agent| agent.kind == backend.kind)?;
        let model = Some(state.current.model.as_str()).filter(|model| !model.is_empty() && *model != "default");
        crate::ui::quota::lowest(agent, model).filter(|left| *left < crate::ui::quota::LOW)
    });
    let low_color = crate::theme::warning_text(cx);
    // The name gives way first, so a narrow composer keeps the level and the other buttons.
    let trigger = Button::new("model-control").ghost().small().dropdown_caret(true).tooltip(i18n::text(locale, "chat.modelTitle")).min_w_0().flex_shrink(1.).child(
        h_flex()
            .min_w_0()
            .gap_1p5()
            .items_center()
            .when_some(badge, |this, badge| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .px_1()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.secondary)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(badge),
                )
            })
            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(name))
            .when_some(level, |this, (level, color)| this.child(div().flex_shrink_0().text_color(color).child(level)))
            .when_some(low, |this, left| this.child(div().flex_shrink_0().text_color(low_color).child(i18n::format(locale, "external.quotaLeft", &[("percent", &left)])))),
    );
    let (store, chat) = (store.clone(), chat.cloned());
    Popover::new("model-control-card")
        .anchor(gpui_kit::Anchor::BottomLeft)
        .w(px(CARD_WIDTH))
        .p(px(CARD_PADDING))
        .rounded(theme.radius_lg)
        .trigger(trigger)
        .content(move |_, window, cx| {
            crate::web::mark_popover_open(cx);
            let popover = cx.entity();
            model_card(&store, chat.as_ref(), popover, window, cx)
        })
        .into_any_element()
}

fn enter_card(content: AnyElement, open_token: EntityId, cx: &App) -> AnyElement {
    if cx.reduce_motion() {
        return content;
    }
    div()
        .child(content)
        .with_animation(
            ElementId::Name(format!("model-card-enter-{open_token:?}").into()),
            Animation::new(Duration::from_millis(130)).with_easing(gpui_kit::base::animation::ease_out_cubic),
            |el, delta| el.opacity(delta).relative().top(px(5.0 * (1.0 - delta))),
        )
        .into_any_element()
}

/// The card the control opens: the levels for a model with reasoning levels, the list of saved models on request,
/// and for a model without levels the list with a note.
fn model_card(store: &Entity<Store>, chat: Option<&Entity<ChatModel>>, popover: Entity<PopoverState>, window: &mut Window, cx: &mut App) -> AnyElement {
    let open_token = window.use_state(cx, |_, _| ()).entity_id();
    let card = window.use_keyed_state("model-card", cx, |_, _| CardState::default());
    let locale = store.read(cx).locale();
    let state = model_state(store, chat, cx);
    let session = chat.map(|chat| chat.read(cx).id.clone());
    // Isolate card UI state across sessions so switching chats never leaves a stale list/drag/pending state behind.
    card.update(cx, |c, _| {
        c.sync_session(&session);
    });
    let Some(state) = state else {
        // Even with no built-in model configured yet, still allow picking detected external CLI agents.
        let has_external = !store.read(cx).external_agents.is_empty();
        if has_external {
            let placeholder = ModelState {
                current: ModelSelection { provider: String::new(), model: String::new(), thinking: None },
                levels: Vec::new(),
                default_level: None,
                toggle_reasoning: false,
                always_reasons: false,
                level: None,
                name: i18n::text(locale, "chat.noModel").into(),
                external: None,
                external_badge: None,
                executor: None,
            };
            let ctx = CardContext { store, session, state: &placeholder, card: &card, popover, locale };
            return enter_card(model_list(&ctx, cx), open_token, cx);
        }
        return div().text_sm().text_color(cx.theme().muted_foreground).child(i18n::text(locale, "chat.noModel")).into_any_element();
    };
    // Opening the card reads the external agent's limits again (the Host keeps a reading under a minute old).
    if let Some(kind) = state.external.as_ref().map(|backend| backend.kind.clone())
        && card.read(cx).quota_asked.as_deref() != Some(kind.as_str())
    {
        card.update(cx, |card, _| card.quota_asked = Some(kind.clone()));
        crate::ui::quota::refresh(store, &kind, false, cx);
    }
    let ctx = CardContext { store, session, state: &state, card: &card, popover, locale };
    let content = if card.read(cx).orchestrate && state.external.is_none() {
        orchestrate_page(&ctx, cx)
    } else if card.read(cx).list || !state.adjustable() {
        model_list(&ctx, cx)
    } else {
        level_list(&ctx, cx)
    };
    enter_card(content, open_token, cx)
}

/// Send a level chosen in the card, and show it until the chat reports it.
fn send_level(choice: LevelChoice, window: &mut Window, cx: &mut App) {
    let LevelChoice { store, session, card, current, level, index } = choice;
    card.update(cx, |card, cx| {
        card.pending = Some((current.clone(), index, Instant::now()));
        cx.notify();
    });
    set_model(&store, session, ModelSelection { thinking: Some(level.as_str().to_owned()), ..current }, window, cx);
}

/// The usage limits of the external agent `state` uses, for the card.
fn quota_section(store: &Entity<Store>, state: &ModelState, locale: Locale, cx: &App) -> Option<AnyElement> {
    let backend = state.external.as_ref()?;
    let agent = store.read(cx).external_agents.iter().find(|agent| agent.kind == backend.kind)?;
    let model = Some(state.current.model.as_str()).filter(|model| !model.is_empty() && *model != "default");
    crate::ui::quota::section(agent, model, locale, cx)
}

/// The card for a model with reasoning levels, laid out like the model list: the model, which opens the list; an
/// external agent's usage limits; and the levels, each with what it trades and the provider's default marked. A
/// level picked is sent at once and checked until the chat reports it.
fn level_list(ctx: &CardContext<'_>, cx: &App) -> AnyElement {
    let CardContext { store, session, state, card, locale, .. } = ctx;
    let locale = *locale;
    let theme = cx.theme();
    let (hover, muted, border) = (theme.accent, theme.muted_foreground, theme.border);
    let accent = if theme.mode.is_dark() { theme.link } else { theme.primary };
    let sent = state.level.and_then(|level| state.levels.iter().position(|known| *known == level));
    let pending = card.read(cx).pending.as_ref().filter(|(before, _, at)| *before == state.current && at.elapsed() < PENDING).map(|(_, index, _)| *index);
    let shown = pending.or(sent);
    let toggle = state.toggle();

    let list = (*card).clone();
    let model = h_flex()
        .id("reasoning-model")
        .gap_2()
        .px_2()
        .py_1p5()
        .items_center()
        .rounded(theme.radius)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .when_some(state.external_badge.clone(), |this, badge| {
            this.child(
                div()
                    .flex_shrink_0()
                    .px_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(border)
                    .bg(theme.secondary)
                    .text_xs()
                    .text_color(muted)
                    .child(badge),
            )
        })
        .child(div().flex_1().min_w_0().text_sm().font_medium().overflow_hidden().text_ellipsis().whitespace_nowrap().child(state.name.clone()))
        .child(Icon::new(IconName::ChevronRight).xsmall().text_color(muted))
        .on_click(move |_, _, cx| {
            list.update(cx, |card, cx| {
                card.list = true;
                cx.notify();
            });
        });
    let quota = quota_section(store, state, locale, cx).map(|section| div().px_2().pt_0p5().pb_1().child(section));

    let heading = div()
        .mt_1()
        .px_2()
        .pt_2()
        .pb_0p5()
        .border_t_1()
        .border_color(border)
        .text_xs()
        .font_medium()
        .text_color(muted)
        .child(i18n::text(locale, "chat.thinkingTitle"));
    let rows = state.levels.iter().enumerate().map(|(index, level)| {
        let selected = shown == Some(index);
        let is_default = state.default_level == Some(*level);
        let (store, session, card, current, level) = ((*store).clone(), session.clone(), (*card).clone(), state.current.clone(), *level);
        h_flex()
            .id(("reasoning-level", index))
            .gap_2()
            .px_2()
            .py_1()
            .items_center()
            .rounded(theme.radius)
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_baseline()
                            .child(div().text_sm().when(selected, |this| this.font_medium()).child(thinking_label(locale, level, toggle)))
                            .when(is_default, |this| this.child(div().text_xs().text_color(muted).child(i18n::text(locale, "chat.thinkingDefault")))),
                    )
                    .child(div().text_xs().text_color(muted).overflow_hidden().text_ellipsis().whitespace_nowrap().child(thinking_info(locale, level, toggle))),
            )
            .when(selected, |this| this.child(Icon::new(IconName::Check).small().text_color(accent)))
            .on_click(move |_, window, cx| {
                if !selected {
                    send_level(
                        LevelChoice {
                            store: store.clone(),
                            session: session.clone(),
                            card: card.clone(),
                            current: current.clone(),
                            level,
                            index,
                        },
                        window,
                        cx,
                    );
                }
            })
    });
    let orchestrate = state.external.is_none().then(|| orchestrate_entry(state, card, locale, cx));
    let levels = if crate::ui::optics::enabled(cx) {
        level_slider(ctx, shown, cx).into_any_element()
    } else {
        v_flex().child(heading).child(v_flex().id("reasoning-levels").max_h(px(320.)).overflow_y_scroll().gap_0p5().children(rows)).into_any_element()
    };
    v_flex()
        .gap_0p5()
        .child(model)
        .children(quota)
        .child(levels)
        .children(orchestrate)
        .into_any_element()
}

/// The optical look's levels: the level shown in its color, the slider, and what the level trades.
fn level_slider(ctx: &CardContext<'_>, shown: Option<usize>, cx: &App) -> impl IntoElement + use<> {
    use crate::ui::optics::{look, slider};
    let CardContext { store, session, state, card, locale, .. } = ctx;
    let locale = *locale;
    let theme = cx.theme();
    let (muted, dark) = (theme.muted_foreground, theme.mode.is_dark());
    let toggle = state.toggle();
    let drag = card.read(cx).drag;
    let index = drag.or(shown).unwrap_or(0).min(state.levels.len().saturating_sub(1));
    let level = state.levels.get(index).copied();
    let fills: Vec<Hsla> = state.levels.iter().map(|level| look::level_fill(*level, dark)).collect();
    // The drag lives in the card, so what a move or a release means is read when it comes, not when the slider was
    // last drawn (`slider::follow`).
    let on_gesture: slider::OnGesture = Rc::new({
        let (store, session, card, current, levels) = ((*store).clone(), session.clone(), (*card).clone(), state.current.clone(), state.levels.clone());
        move |gesture, window, cx| {
            let dragging = card.read(cx).drag;
            let Some((drag, release)) = slider::follow(dragging, gesture) else { return };
            if drag != dragging {
                card.update(cx, |card, cx| {
                    card.drag = drag;
                    cx.notify();
                });
            }
            if let Some(stop) = release
                && let Some(level) = levels.get(stop)
            {
                send_level(
                    LevelChoice {
                        store: store.clone(),
                        session: session.clone(),
                        card: card.clone(),
                        current: current.clone(),
                        level: *level,
                        index: stop,
                    },
                    window,
                    cx,
                );
            }
        }
    });
    v_flex()
        .mt_1()
        .px_2()
        .pt_2()
        .pb_1()
        .gap_2()
        .border_t_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(div().text_xs().font_medium().text_color(muted).child(i18n::text(locale, "optics.sliderTitle")))
                .when_some(level, |this, level| {
                    this.child(div().text_sm().font_semibold().text_color(look::level_color(level, theme, &[theme.popover])).child(thinking_label(locale, level, toggle)))
                }),
        )
        .child(div().id("reasoning-slider").child(slider::render(fills, index, dark, on_gesture)))
        .when_some(level, |this, level| {
            this.child(
                h_flex()
                    .gap_1p5()
                    .min_w_0()
                    .text_xs()
                    .text_color(muted)
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(thinking_info(locale, level, toggle)))
                    .when(state.default_level == Some(level), |this| this.child(div().flex_shrink_0().child(i18n::text(locale, "chat.thinkingDefault")))),
            )
        })
}

/// The row under the levels and the model list that opens the orchestration page, with its current choice.
fn orchestrate_entry(state: &ModelState, card: &Entity<CardState>, locale: Locale, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (muted, strong) = (theme.muted_foreground, theme.foreground);
    let label: SharedString = match &state.executor {
        Some((_, executor)) => i18n::format(locale, "chat.orchestrateOn", &[("executor", executor)]).into(),
        None => i18n::text(locale, "chat.orchestrateOff").into(),
    };
    let open = card.clone();
    h_flex()
        .id("reasoning-orchestrate")
        .mt_1p5()
        .pt_1p5()
        .gap_1p5()
        .items_center()
        .border_t_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(muted)
        .cursor_pointer()
        .hover(move |style| style.text_color(strong))
        .child(Icon::new(gpui_kit::assets::IconName::Workflow).xsmall())
        .child(div().flex_shrink_0().child(i18n::text(locale, "chat.orchestrate")))
        .child(new_badge(locale, cx))
        .child(div().flex_1().min_w_0().text_right().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
        .child(Icon::new(IconName::ChevronRight).xsmall())
        .on_click(move |_, _, cx| {
            open.update(cx, |card, cx| {
                card.orchestrate = true;
                cx.notify();
            });
        })
}

/// The small "new" mark of a feature added in this version.
fn new_badge(locale: Locale, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let color = if theme.mode.is_dark() { theme.link } else { theme.primary };
    div().flex_shrink_0().px_1().rounded(theme.radius).bg(theme.primary.opacity(0.15)).text_xs().text_color(color).child(i18n::text(locale, "chat.new"))
}

/// Orchestration under the model list: off, or one of the other saved models as the executor of the model in use,
/// which then plans. The info mark explains the pair.
fn orchestrate_page(ctx: &CardContext<'_>, cx: &App) -> AnyElement {
    let CardContext { store, session, state, card, popover, locale } = ctx;
    let locale = *locale;
    let theme = cx.theme();
    let (hover, muted, accent) = (theme.accent, theme.muted_foreground, if theme.mode.is_dark() { theme.link } else { theme.primary });
    let (models, presets) = store.read(cx).models.as_ref().map(|status| (status.models.clone(), status.presets.clone())).unwrap_or_default();
    let back = (*card).clone();
    let header = h_flex()
        .h(px(26.))
        .gap_1()
        .items_center()
        .child(Button::new("orchestrate-back").ghost().xsmall().icon(Icon::new(IconName::ChevronLeft)).on_click(move |_, _, cx| {
            back.update(cx, |card, cx| {
                card.orchestrate = false;
                cx.notify();
            });
        }))
        .child(div().text_sm().font_semibold().child(i18n::text(locale, "chat.orchestrate")))
        .child(new_badge(locale, cx));
    let about = div().px_1().pb_1().text_xs().text_color(muted).child(i18n::text(locale, "chat.orchestrateInfo"));
    let others = models.iter().filter(|model| model.provider != state.current.provider || model.model != state.current.model).count();
    if others == 0 {
        // Orchestration needs a second model to execute; the user adds one in Settings.
        let (card, popover) = ((*card).clone(), popover.clone());
        let add = Button::new("orchestrate-add-model").small().primary().label(i18n::text(locale, "chat.orchestrateAddModel")).on_click(move |_, window, cx| {
            card.update(cx, |card, cx| {
                card.orchestrate = false;
                cx.notify();
            });
            popover.update(cx, |popover, cx| popover.dismiss(window, cx));
            if let Some(workbench) = cx.try_global::<crate::ui::workbench::WorkbenchHandle>().and_then(|handle| handle.0.upgrade()) {
                workbench.update(cx, |workbench, cx| workbench.open_settings(crate::ui::settings::Section::Models, window, cx));
            }
        });
        let need = div().px_1().text_sm().child(i18n::text(locale, "chat.orchestrateNeedModel"));
        return v_flex().gap_1().child(header).child(about).child(need).child(h_flex().px_1().pt_1().child(add)).into_any_element();
    }
    let current = state.executor.as_ref().map(|(executor, _)| executor.clone());
    let row = |id: usize, title: SharedString, detail: Option<String>, choice: Option<ModelSelection>| {
        let selected = current.as_ref().map(|executor| (&executor.provider, &executor.model)) == choice.as_ref().map(|choice| (&choice.provider, &choice.model));
        let (store, session) = ((*store).clone(), session.clone());
        h_flex()
            .id(("orchestrate-row", id))
            .gap_2()
            .px_2()
            .py_1()
            .items_center()
            .rounded(theme.radius)
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
                    .when_some(detail, |this, detail| this.child(div().text_xs().text_color(muted).overflow_hidden().text_ellipsis().whitespace_nowrap().child(detail))),
            )
            .when(selected, |this| this.child(Icon::new(IconName::Check).small().text_color(accent)))
            .on_click(move |_, window, cx| {
                if !selected {
                    set_orchestrate(&store, session.clone(), choice.clone(), window, cx);
                }
            })
    };
    let mut rows = vec![row(0, i18n::text(locale, "chat.orchestrateNone").into(), None, None).into_any_element()];
    for (index, model) in models.iter().enumerate() {
        if model.provider == state.current.provider && model.model == state.current.model {
            continue;
        }
        let preset = presets.iter().find(|preset| preset.model.provider == model.provider && preset.model.model == model.model).map(|preset| preset.name.as_str());
        let short = short_model_name(&model.model, preset);
        let title = i18n::format(locale, "chat.orchestrateWith", &[("planner", &state.name), ("executor", &short)]).into();
        let choice = ModelSelection { provider: model.provider.clone(), model: model.model.clone(), thinking: None };
        rows.push(row(index + 1, title, Some(format!("{} · {}", model.model, model.provider)), Some(choice)).into_any_element());
    }
    v_flex().gap_1().child(header).child(about).child(v_flex().id("orchestrate-rows").max_h(px(300.)).overflow_y_scroll().gap_0p5().children(rows)).into_any_element()
}

/// Turn orchestration on with `executor` as the executor, or off; before a new chat's first message it is kept for it.
fn set_orchestrate(store: &Entity<Store>, session: Option<String>, executor: Option<ModelSelection>, window: &mut Window, cx: &mut App) {
    match session {
        Some(session) => {
            let request = store.read(cx).call("chat.setOrchestrate", json!({ "sessionId": session, "executor": executor }));
            report_failure(request, window, cx);
        }
        None => store.update(cx, |store, cx| {
            store.pending_orchestrate = executor;
            store.notify_composer(cx);
            cx.notify();
        }),
    }
}

/// The saved models and external CLI agents, cleanly grouped into Built-in vs External Agents.
/// For a new chat, users switch freely between built-in models and external agents; inside an existing
/// chat, switching models within the active engine updates it in place while picking the other engine
/// starts a new chat cleanly.
fn model_list(ctx: &CardContext<'_>, cx: &App) -> AnyElement {
    let CardContext { store, session, state, card, popover, locale } = ctx;
    let locale = *locale;
    let theme = cx.theme();
    let adjustable = state.adjustable();
    let store_ref = store.read(cx);
    let (models, presets) = store_ref
        .models
        .as_ref()
        .map(|status| (status.models.clone(), status.presets.clone()))
        .unwrap_or_default();
    let external_agents = store_ref.external_agents.clone();
    let back = (*card).clone();
    let header = h_flex()
        .h(px(26.))
        .gap_1()
        .items_center()
        .when(adjustable, |this| {
            this.child(Button::new("reasoning-back").ghost().xsmall().icon(Icon::new(IconName::ChevronLeft)).on_click(move |_, _, cx| {
                back.update(cx, |card, cx| {
                    card.list = false;
                    cx.notify();
                });
            }))
        })
        .child(div().text_sm().font_semibold().child(i18n::text(locale, "chat.modelsTitle")));
    let note = (!adjustable && !state.current.provider.is_empty()).then(|| {
        let key = if state.reasons() { "chat.thinkingFixed" } else { "chat.thinkingNone" };
        div().px_1().text_xs().text_color(theme.muted_foreground).child(i18n::text(locale, key))
    });
    let (hover, muted, accent, border_color, foreground) = (
        theme.accent,
        theme.muted_foreground,
        if theme.mode.is_dark() { theme.link } else { theme.primary },
        theme.border,
        theme.foreground,
    );
    let active_external_kind = state.external.as_ref().map(|ext| ext.kind.as_str());
    let colored_levels = crate::ui::optics::enabled(cx);
    let mut rows: Vec<AnyElement> = Vec::new();
    let mut row_idx: usize = 0;

    // Helper for compact section heading inside the model popover.
    let section_header = |title: &'static str, first: bool| -> AnyElement {
        div()
            .when(!first, |this| this.mt_1p5().pt_1p5().border_t_1().border_color(border_color))
            .px_1()
            .pb_0p5()
            .text_xs()
            .font_medium()
            .text_color(muted)
            .child(title)
            .into_any_element()
    };

    // If the active chat is an external-agent session, surface its own models right at the top.
    if let Some(kind) = active_external_kind
        && session.is_some()
        && let Some(agent) = external_agents.iter().find(|a| a.kind == kind)
        && !agent.models.is_empty()
    {
        rows.push(section_header(i18n::text(locale, "external.groupAgents"), true));
        for ext_model in &agent.models {
            let idx = row_idx;
            row_idx += 1;
            let provider = format!("external:{}", agent.kind);
            let selected = state.current.provider == provider && state.current.model == ext_model.id;
            let choice = ModelSelection {
                provider,
                model: ext_model.id.clone(),
                thinking: ext_model.default_effort.clone().or_else(|| state.current.thinking.clone()),
            };
            let title = ext_model
                .display_name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| short_model_name(&ext_model.id, None));
            let detail = format!("{} · {}", agent.label, ext_model.id);
            let (store, session, card) = ((*store).clone(), session.clone(), (*card).clone());
            rows.push(
                h_flex()
                    .id(("ext-cur-model", idx))
                    .gap_2()
                    .px_2()
                    .py_1()
                    .items_center()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
                            .child(div().text_xs().text_color(muted).overflow_hidden().text_ellipsis().whitespace_nowrap().child(detail)),
                    )
                    .when(selected, |this| this.child(Icon::new(IconName::Check).small().text_color(accent)))
                    .on_click(move |_, window, cx| {
                        card.update(cx, |card, cx| {
                            card.list = false;
                            card.pending = None;
                            cx.notify();
                        });
                        if !selected {
                            set_model(&store, session.clone(), choice.clone(), window, cx);
                        }
                    })
                    .into_any_element(),
            );
        }
    }

    // Group 1: Built-in models.
    if !models.is_empty() {
        let is_first = rows.is_empty();
        rows.push(section_header(i18n::text(locale, "external.groupBuiltIn"), is_first));
        let in_external_session = session.is_some() && active_external_kind.is_some();
        for model in &models {
            let idx = row_idx;
            row_idx += 1;
            let selected = active_external_kind.is_none() && model.provider == state.current.provider && model.model == state.current.model;
            let choice = ModelSelection { provider: model.provider.clone(), model: model.model.clone(), thinking: state.current.thinking.clone() };
            let preset = presets
                .iter()
                .find(|preset| preset.model.provider == model.provider && preset.model.model == model.model)
                .map(|preset| preset.name.as_str());
            let (store, session, card, popover) = ((*store).clone(), session.clone(), (*card).clone(), popover.clone());
            let subtitle = if in_external_session {
                i18n::format(locale, "external.newChatWith", &[("name", &short_model_name(&model.model, preset))])
            } else {
                format!("{} · {}", model.model, model.provider)
            };
            // The level this model answers with: the chat's for the chosen one, its own default for the others.
            let tag = colored_levels
                .then(|| {
                    let reasoning = model.reasoning();
                    let level = if selected { state.level } else { reasoning.choose(model.thinking.as_deref()) };
                    let shows = reasoning.levels().count() > 1 || reasoning.always;
                    level.filter(|_| shows).map(|level| (thinking_label(locale, level, reasoning.toggle()), crate::ui::optics::look::level_color(level, theme, &[theme.popover])))
                })
                .flatten();
            rows.push(
                h_flex()
                    .id(("model-row", idx))
                    .gap_2()
                    .px_2()
                    .py_1()
                    .items_center()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_1p5()
                                    .items_baseline()
                                    .child(div().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(short_model_name(&model.model, preset)))
                                    .when_some(tag, |this, (label, color)| this.child(div().flex_shrink_0().text_sm().font_medium().text_color(color).child(label))),
                            )
                            .child(div().text_xs().text_color(muted).overflow_hidden().text_ellipsis().whitespace_nowrap().child(subtitle)),
                    )
                    .when(selected, |this| this.child(Icon::new(IconName::Check).small().text_color(accent)))
                    .on_click(move |_, window, cx| {
                        card.update(cx, |card, cx| {
                            card.list = false;
                            card.pending = None;
                            cx.notify();
                        });
                        if in_external_session {
                            store.update(cx, |store, cx| {
                                store.clear_pending_external(cx);
                                store.select(None, cx);
                            });
                            set_model(&store, None, choice.clone(), window, cx);
                            popover.update(cx, |popover, cx| popover.dismiss(window, cx));
                        } else if !selected {
                            set_model(&store, session.clone(), choice.clone(), window, cx);
                        }
                    })
                    .into_any_element(),
            );
        }
    }

    // Group 2: External Agents (Codex / Antigravity / custom ACP).
    let visible_agents: Vec<&ExternalAgentStatus> = external_agents.iter().filter(|agent| agent.enabled).collect();
    if !visible_agents.is_empty() {
        let is_first = rows.is_empty();
        rows.push(section_header(i18n::text(locale, "external.groupAgents"), is_first));
        if session.is_some() && active_external_kind.is_none() {
            rows.push(
                div()
                    .px_1()
                    .pb_0p5()
                    .text_xs()
                    .text_color(muted)
                    .child(i18n::text(locale, "external.switchNeedsNewChat"))
                    .into_any_element(),
            );
        }
        for agent in visible_agents {
            // Skip repeating the active external session's own models (already listed at top).
            if session.is_some() && active_external_kind == Some(agent.kind.as_str()) {
                continue;
            }
            if !agent.installed {
                let idx = row_idx;
                row_idx += 1;
                let popover = popover.clone();
                rows.push(
                    h_flex()
                        .id(("ext-missing", idx))
                        .gap_2()
                        .px_2()
                        .py_1()
                        .items_center()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().text_color(muted).overflow_hidden().text_ellipsis().whitespace_nowrap().child(agent.label.clone()))
                                .child(div().text_xs().text_color(muted).child(i18n::text(locale, "external.notInstalled"))),
                        )
                        .child(Icon::new(IconName::ChevronRight).xsmall().text_color(muted))
                        .on_click(move |_, window, cx| {
                            popover.update(cx, |popover, cx| popover.dismiss(window, cx));
                            if let Some(workbench) = cx.try_global::<crate::ui::workbench::WorkbenchHandle>().and_then(|handle| handle.0.upgrade()) {
                                workbench.update(cx, |workbench, cx| workbench.open_settings(crate::ui::settings::Section::Models, window, cx));
                            }
                        })
                        .into_any_element(),
                );
                continue;
            }

            let backend_ref = external_ref_from_status(agent);
            let badge = external_badge_label(&backend_ref, locale);
            let starts_new = session.is_some();

            if !starts_new && !agent.models.is_empty() {
                // In a new chat, show each model offered by this installed external agent.
                for ext_model in &agent.models {
                    let idx = row_idx;
                    row_idx += 1;
                    let selected = active_external_kind == Some(agent.kind.as_str()) && state.current.model == ext_model.id;
                    let selection = external_default_selection(agent, Some(&ext_model.id));
                    let title = ext_model
                        .display_name
                        .as_deref()
                        .filter(|name| !name.trim().is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| short_model_name(&ext_model.id, None));
                    let detail = format!("{} · {}", agent.label, ext_model.id);
                    let (store, card, backend_ref) = ((*store).clone(), (*card).clone(), backend_ref.clone());
                    rows.push(
                        h_flex()
                            .id(("ext-model-row", idx))
                            .gap_2()
                            .px_2()
                            .py_1()
                            .items_center()
                            .rounded(theme.radius)
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .px_1()
                                    .rounded(theme.radius)
                                    .border_1()
                                    .border_color(border_color)
                                    .bg(theme.secondary)
                                    .text_xs()
                                    .text_color(muted)
                                    .child(badge.clone()),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
                                    .child(div().text_xs().text_color(muted).overflow_hidden().text_ellipsis().whitespace_nowrap().child(detail)),
                            )
                            .when(selected, |this| this.child(Icon::new(IconName::Check).small().text_color(accent)))
                            .on_click(move |_, _, cx| {
                                card.update(cx, |card, cx| {
                                    card.list = false;
                                    card.pending = None;
                                    cx.notify();
                                });
                                store.update(cx, |store, cx| {
                                    store.pending_external = Some(backend_ref.clone());
                                    store.pending_external_model = Some(selection.clone());
                                    store.pending_orchestrate = None;
                                    store.notify_composer(cx);
                                    cx.notify();
                                });
                            })
                            .into_any_element(),
                    );
                }
            } else {
                // Single row per external agent (either no model catalog reported, or switching from an existing chat -> new chat).
                let idx = row_idx;
                row_idx += 1;
                let selected = !starts_new && active_external_kind == Some(agent.kind.as_str());
                let selection = external_default_selection(agent, None);
                let title: SharedString = if starts_new {
                    i18n::format(locale, "external.newChatWith", &[("name", &agent.label)]).into()
                } else {
                    agent.label.clone().into()
                };
                let model_label = if selection.model == "default" {
                    i18n::text(locale, "external.defaultModel").to_owned()
                } else {
                    selection.model.clone()
                };
                let subtitle = agent
                    .version
                    .as_deref()
                    .map(|ver| format!("{ver} · {model_label}"))
                    .unwrap_or(model_label);
                let (store, card, popover, backend_ref) = ((*store).clone(), (*card).clone(), popover.clone(), backend_ref.clone());
                rows.push(
                    h_flex()
                        .id(("ext-agent-row", idx))
                        .gap_2()
                        .px_2()
                        .py_1()
                        .items_center()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .child(
                            div()
                                .flex_shrink_0()
                                .px_1()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(border_color)
                                .bg(theme.secondary)
                                .text_xs()
                                .text_color(muted)
                                .child(badge),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
                                .child(div().text_xs().text_color(muted).overflow_hidden().text_ellipsis().whitespace_nowrap().child(subtitle)),
                        )
                        .when(selected, |this| this.child(Icon::new(IconName::Check).small().text_color(accent)))
                        .on_click(move |_, window, cx| {
                            card.update(cx, |card, cx| {
                                card.list = false;
                                card.pending = None;
                                cx.notify();
                            });
                            store.update(cx, |store, cx| {
                                store.pending_external = Some(backend_ref.clone());
                                store.pending_external_model = Some(selection.clone());
                                store.pending_orchestrate = None;
                                if starts_new {
                                    store.select(None, cx);
                                }
                                store.notify_composer(cx);
                                cx.notify();
                            });
                            if starts_new {
                                popover.update(cx, |popover, cx| popover.dismiss(window, cx));
                            }
                        })
                        .into_any_element(),
                );
            }
        }
    }

    // Settings shortcut for external agents & models.
    let popover_settings = popover.clone();
    let configure_row = h_flex()
        .id("external-configure-link")
        .mt_1()
        .pt_1()
        .border_t_1()
        .border_color(border_color)
        .px_1()
        .gap_1p5()
        .items_center()
        .text_xs()
        .text_color(muted)
        .cursor_pointer()
        .hover(move |style| style.text_color(foreground))
        .child(Icon::new(IconName::Settings).xsmall())
        .child(div().flex_1().child(i18n::text(locale, "external.configureInSettings")))
        .child(Icon::new(IconName::ChevronRight).xsmall())
        .on_click(move |_, window, cx| {
            popover_settings.update(cx, |popover, cx| popover.dismiss(window, cx));
            if let Some(workbench) = cx.try_global::<crate::ui::workbench::WorkbenchHandle>().and_then(|handle| handle.0.upgrade()) {
                workbench.update(cx, |workbench, cx| workbench.open_settings(crate::ui::settings::Section::Models, window, cx));
            }
        });

    let orchestrate = state.external.is_none().then(|| orchestrate_entry(state, card, locale, cx));
    let quota = quota_section(store, state, locale, cx).map(|section| div().px_1().pb_1().child(section));
    v_flex()
        .gap_1()
        .child(header)
        .children(note)
        .children(quota)
        .child(v_flex().id("model-rows").max_h(px(320.)).overflow_y_scroll().gap_0p5().children(rows))
        .children(orchestrate)
        .child(configure_row)
        .into_any_element()
}

fn set_model(store: &Entity<Store>, session: Option<String>, choice: ModelSelection, window: &mut Window, cx: &mut App) {
    if session.is_none() && choice.provider.starts_with("external:") {
        store.update(cx, |store, cx| {
            store.pending_external_model = Some(choice);
            store.notify_composer(cx);
            cx.notify();
        });
        return;
    }
    if session.is_none() {
        store.update(cx, |store, cx| store.clear_pending_external(cx));
    }
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
