//! The usage overview above a new chat: chats, messages, tokens, active days, the cache hit rate and the most used
//! model for all projects or one, over all time or the last 30 or 7 days; a map of the days with messages; the
//! models' shares; and the usage limits of external agents. Everything is counted by the Host from Rainy's own chat
//! files (`usage.stats`), refreshed on mount, filter changes, and chat summary updates (throttled to 30 s) — never as
//! an unbounded side effect inside `Render::render`.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike as _, Local, NaiveDate};
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{RpcError, UsageStats};
use serde_json::json;

use super::look::Look;
use crate::i18n::{self, Locale};
use crate::store::{Store, StoreEvent};
use crate::ui::motion::{self, SETTLE};

/// The overview is asked again when it is older than this.
const STALE: Duration = Duration::from_secs(30);
const CELL: f32 = 12.0;
const GAP: f32 = 3.0;
/// Subtle hover expansion per side on heatmap cells (`1.5px` total without layout shift).
const HOVER_INFLATE: f32 = 0.75;
pub const MAP_HEIGHT: f32 = 7.0 * CELL + 6.0 * GAP;
/// Width bucket granularity (px) for reusing measured overview height across minor window resizes.
const WIDTH_BUCKET: f32 = 24.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Models,
    Limits,
}

impl Tab {
    fn index(self) -> usize {
        match self {
            Tab::Overview => 0,
            Tab::Models => 1,
            Tab::Limits => 2,
        }
    }
}

/// What is counted: a project (`None` for all) and the last days (0 for all time).
type Choice = (Option<String>, u32);

/// Measured overview height cached alongside a coarse viewport width bucket.
type HeightMemo = Rc<Cell<Option<(Pixels, i32)>>>;

/// Cached stats snapshot with shared activity slice (no per-frame Vec copy) and local fetch date (prevents midnight
/// date drift).
struct StatsSnapshot {
    stats: UsageStats,
    activity: Arc<[u32]>,
    fetched_date: NaiveDate,
}

pub struct UsageCard {
    store: Entity<Store>,
    tab: Tab,
    choice: Choice,
    stats: Option<(Choice, Result<StatsSnapshot, SharedString>)>,
    asked: Option<(Choice, Instant)>,
    /// Whether initial mount subscription & fetch were scheduled.
    initialized: bool,
    /// The day of the map under the pointer, by its index in `UsageStats::activity`.
    hover: Option<usize>,
    /// How tall the overview was last drawn, in a width bucket: the other tabs keep at least that height so switching
    /// tabs never shifts the tab header vertically.
    overview_height: HeightMemo,
    _subscription: Option<Subscription>,
    _task: Option<Task<()>>,
}

impl UsageCard {
    pub fn new(store: Entity<Store>) -> Self {
        Self {
            store,
            tab: Tab::Overview,
            choice: (None, 0),
            stats: None,
            asked: None,
            initialized: false,
            hover: None,
            overview_height: Rc::default(),
            _subscription: None,
            _task: None,
        }
    }

    fn ensure_initialized(&mut self, cx: &mut Context<Self>) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        // Refresh only on meaningful store events (throttled by `STALE`), never on unrelated repaints.
        self._subscription = Some(cx.subscribe(&self.store, |this, _, event: &StoreEvent, cx| match event {
            StoreEvent::SummariesChanged | StoreEvent::HostReconnected(_) => this.refresh_if_needed(cx),
            _ => {}
        }));
        let card = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = card.update(cx, |this, cx| this.refresh_if_needed(cx));
        });
    }

    fn refresh_if_needed(&mut self, cx: &mut Context<Self>) {
        let fresh = self.asked.as_ref().is_some_and(|(choice, at)| *choice == self.choice && at.elapsed() < STALE);
        if fresh {
            return;
        }
        let choice = self.choice.clone();
        self.asked = Some((choice.clone(), Instant::now()));
        let now = Local::now();
        let offset = now.offset().local_minus_utc() / 60;
        let fetched_date = now.date_naive();
        let store = self.store.read(cx);
        let locale = store.locale();
        let call = store.call("usage.stats", json!({ "workspaceId": choice.0, "days": choice.1, "tzOffsetMinutes": offset }));
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = call.await.and_then(|value| serde_json::from_value::<UsageStats>(value).map_err(|error| RpcError { code: "invalid".into(), message: error.to_string(), data: None }));
            let result = match result {
                Ok(stats) => {
                    let activity = Arc::from(stats.activity.as_slice());
                    Ok(StatsSnapshot { stats, activity, fetched_date })
                }
                Err(error) => Err(SharedString::from(i18n::format(locale, "optics.failed", &[("error", &i18n::error_text(locale, &error))]))),
            };
            let _ = this.update(cx, |this, cx| {
                this.stats = Some((choice, result));
                cx.notify();
            });
        }));
    }

    fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        if self.choice != choice {
            self.choice = choice;
            self.hover = None;
            self.refresh_if_needed(cx);
            cx.notify();
        }
    }
}

fn width_bucket(width: Pixels) -> i32 {
    (f32::from(width) / WIDTH_BUCKET).round() as i32
}

impl Render for UsageCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_initialized(cx);
        let locale = self.store.read(cx).locale();
        let look = Look::of(cx.theme());
        let viewport = window.viewport_size();
        let with_map = viewport.height >= px(MIN_HEIGHT_FOR_MAP);
        let header = self.header(locale, &look, window, cx);
        let bucket = width_bucket(viewport.width);
        let body = match self.tab {
            Tab::Overview => {
                let measured = self.overview_height.clone();
                div()
                    .relative()
                    .on_prepaint(move |bounds, _, _| {
                        measured.set(Some((bounds.size.height, bucket)));
                    })
                    .child(self.overview(locale, &look, with_map, window, cx))
                    .into_any_element()
            }
            tab => {
                let body = if tab == Tab::Models { self.models(locale, &look, window, cx) } else { self.limits(locale, &look, cx) };
                let keep = self.overview_height.get().filter(|(_, saved)| *saved == bucket).map(|(height, _)| height);
                div().when_some(keep, |this, height| this.min_h(height)).child(body).into_any_element()
            }
        };
        super::glass(&look, cx.theme().radius_lg).id("optics-usage").w_full().p_3().gap_3().child(header).child(body)
    }
}

/// A window shorter than this leaves the map out, so the composer stays in view: the page with the map is about 530
/// pixels tall, the window adds its title bar and the grid of numbers may wrap to three rows.
pub const MIN_HEIGHT_FOR_MAP: f32 = 640.0;

/// Transparent-fill selected button variant so [`motion::sliding_pill`] carries the tinted background smoothly.
fn pill_selected_button(cx: &App) -> ButtonCustomVariant {
    let theme = cx.theme();
    let fg = if theme.mode.is_dark() { theme.link } else { theme.primary_active };
    let subtle_hover = theme.primary.opacity(if theme.mode.is_dark() { 0.08 } else { 0.05 });
    ButtonCustomVariant::new(cx).color(theme.transparent).foreground(fg).hover(subtle_hover).active(subtle_hover)
}

impl UsageCard {
    fn header(&self, locale: Locale, look: &Look, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = |key: &str| SharedString::from(i18n::text(locale, &format!("optics.{key}")));
        let theme = cx.theme();
        let pill_style = motion::PillStyle {
            bg: theme.primary.opacity(if look.dark { 0.22 } else { 0.12 }),
            radius: theme.radius,
            gap: px(2.),
        };
        let selected_variant = pill_selected_button(cx);

        let tab_btn = |id: &'static str, key: &str, value: Tab, cx: &mut Context<Self>| {
            let on = self.tab == value;
            Button::new(id)
                .ghost()
                .xsmall()
                .label(t(key))
                .when(on, |this| this.custom(selected_variant))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.tab != value {
                        this.tab = value;
                        if value == Tab::Limits {
                            this.read_limits(cx);
                        } else {
                            this.refresh_if_needed(cx);
                        }
                        cx.notify();
                    }
                }))
                .into_any_element()
        };
        let tabs = motion::sliding_pill(
            "usage-tabs",
            self.tab.index(),
            vec![
                tab_btn("usage-tab-overview", "tabOverview", Tab::Overview, cx),
                tab_btn("usage-tab-models", "tabModels", Tab::Models, cx),
                tab_btn("usage-tab-limits", "tabLimits", Tab::Limits, cx),
            ],
            pill_style,
            window,
            cx,
        );

        let row = h_flex().w_full().min_h(px(24.)).flex_wrap().gap_2().items_center().justify_between().child(tabs);
        if self.tab == Tab::Limits {
            return row;
        }

        let workspaces = crate::store::ide::Ide::global(cx).read(cx).workspaces.clone();
        let (project, days) = self.choice.clone();
        let project_name: SharedString = project
            .as_ref()
            .and_then(|id| workspaces.iter().find(|workspace| &workspace.workspace_id == id))
            .map_or_else(|| t("allProjects"), |workspace| workspace.title.clone().into());
        let card = cx.entity().downgrade();
        let all_label = t("allProjects");
        let projects = Button::new("usage-project").ghost().xsmall().label(project_name).dropdown_caret(true).dropdown_menu(move |mut menu, _, _| {
            let pick = |choice: Option<String>| {
                let card = card.clone();
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    let _ = card.update(cx, |card, cx| card.choose((choice.clone(), days), cx));
                }
            };
            menu = menu.item(PopupMenuItem::new(all_label.clone()).checked(project.is_none()).on_click(pick(None)));
            for workspace in workspaces.iter().rev() {
                let id = workspace.workspace_id.clone();
                menu = menu.item(PopupMenuItem::new(workspace.title.clone()).checked(project.as_deref() == Some(id.as_str())).on_click(pick(Some(id))));
            }
            menu
        });

        let ranges = [(0u32, "rangeAll"), (30, "range30"), (7, "range7")];
        let range_index = ranges.iter().position(|(value, _)| *value == self.choice.1).unwrap_or(0);
        let range_items: Vec<AnyElement> = ranges
            .into_iter()
            .map(|(value, key)| {
                let on = self.choice.1 == value;
                Button::new(ElementId::Name(format!("usage-range-{value}").into()))
                    .ghost()
                    .xsmall()
                    .label(t(key))
                    .when(on, |this| this.custom(selected_variant))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let project = this.choice.0.clone();
                        this.choose((project, value), cx);
                    }))
                    .into_any_element()
            })
            .collect();
        let range = motion::sliding_pill("usage-ranges", range_index, range_items, pill_style, window, cx);
        let divider = div().w(px(1.)).h(px(14.)).bg(look.cell_border);
        row.child(h_flex().gap_1().items_center().child(projects).child(divider).child(range))
    }

    /// The stats shown: the current choice's, or while it is being counted the last ones, dimmed.
    fn shown(&self) -> (Option<&Result<StatsSnapshot, SharedString>>, bool) {
        match &self.stats {
            Some((choice, result)) => (Some(result), *choice != self.choice),
            None => (None, true),
        }
    }

    fn overview(&self, locale: Locale, look: &Look, with_map: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = |key: &str| SharedString::from(i18n::text(locale, &format!("optics.{key}")));
        let (shown, stale) = self.shown();
        let snapshot = match shown {
            Some(Err(error)) => return div().text_sm().text_color(look.muted).child(error.clone()).into_any_element(),
            Some(Ok(snap)) => Some(snap),
            None => None,
        };
        let stats = snapshot.map(|s| &s.stats);
        let value = |text: Option<String>| text.unwrap_or_else(|| "…".into());
        let top_model = stats.and_then(|stats| stats.models.iter().max_by_key(|model| model.replies)).map(|model| self.model_name(&model.provider, &model.model, cx));
        let tokens_tip: Option<SharedString> = stats.map(|stats| {
            i18n::format(locale, "optics.tokensDetail", &[
                ("input", &crate::ui::format_tokens(stats.input)),
                ("output", &crate::ui::format_tokens(stats.output)),
                ("read", &crate::ui::format_tokens(stats.cache_read)),
                ("write", &crate::ui::format_tokens(stats.cache_write)),
            ])
            .into()
        });
        let none = t("none").to_string();

        // Smooth 220ms ease-out count roll on filter/range switches (0 frames once settled, instant under reduced motion).
        let sessions_text = stats.map(|s| {
            let v = motion::transition_f32("usage-val-sessions", s.sessions as f32, SETTLE, window, cx);
            (v.round().max(0.0) as u64).to_string()
        });
        let messages_text = stats.map(|s| {
            let v = motion::transition_f32("usage-val-messages", s.messages as f32, SETTLE, window, cx);
            (v.round().max(0.0) as u64).to_string()
        });
        let tokens_text = stats.map(|s| {
            let v = motion::transition_f32("usage-val-tokens", s.tokens() as f32, SETTLE, window, cx);
            crate::ui::format_tokens(f64::from(v.max(0.0)).round())
        });
        let active_days_text = stats.map(|s| {
            let v = motion::transition_f32("usage-val-days", s.active_days as f32, SETTLE, window, cx);
            (v.round().max(0.0) as u64).to_string()
        });
        let cache_hit_text = stats.map(|s| match s.cache_hit_rate() {
            Some(rate) => {
                let v = motion::transition_f32("usage-val-cache", rate as f32, SETTLE, window, cx);
                percent(f64::from(v.clamp(0.0, 1.0)))
            }
            None => none.clone(),
        });

        let cells: [(&str, String, Option<SharedString>); 6] = [
            ("sessions", value(sessions_text), None),
            ("messages", value(messages_text), None),
            ("tokens", value(tokens_text), tokens_tip),
            ("activeDays", value(active_days_text), None),
            ("cacheHit", value(cache_hit_text), None),
            ("topModel", value(stats.map(|_| top_model.clone().unwrap_or_else(|| none.clone()))), None),
        ];
        let theme = cx.theme();
        let grid = h_flex().w_full().flex_wrap().gap_2().children(cells.into_iter().map(|(key, text, tip)| {
            v_flex()
                .id(ElementId::Name(format!("usage-{key}").into()))
                .flex_grow_1()
                .flex_basis(relative(0.3))
                .min_w(px(132.))
                .px_3()
                .py_2()
                .gap_0p5()
                .rounded(theme.radius)
                .bg(look.cell)
                .border_1()
                .border_color(look.cell_border)
                .child(div().text_xs().text_color(look.muted).child(t(key)))
                .child(div().text_lg().font_semibold().overflow_hidden().text_ellipsis().whitespace_nowrap().when(stale, |this| this.opacity(0.55)).child(text))
                .when_some(tip, |this, tip| this.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)))
        }));
        let footnote = self.footnote(locale, snapshot, look);
        v_flex()
            .gap_3()
            .child(grid)
            .when_some(snapshot.filter(|_| with_map), |this, snap| this.child(self.map(snap, look, cx)))
            .child(footnote)
            .into_any_element()
    }

    fn footnote(&self, locale: Locale, snapshot: Option<&StatsSnapshot>, look: &Look) -> impl IntoElement + use<> {
        let text: Option<String> = match (snapshot, self.hover) {
            (Some(snap), Some(index)) => snap.activity.get(index).map(|count| {
                let date = day_label(locale, snap.fetched_date - chrono::Days::new((snap.activity.len() - 1 - index) as u64));
                if *count == 0 { i18n::format(locale, "optics.dayNone", &[("date", &date)]) } else { i18n::format(locale, "optics.day", &[("date", &date), ("count", count)]) }
            }),
            (Some(snap), None) if snap.stats.messages == 0 => Some(i18n::text(locale, "optics.empty").to_owned()),
            (Some(snap), None) => fun_line(locale, snap.stats.tokens()),
            (None, _) => None,
        };
        div().h(px(18.)).text_xs().text_color(look.muted).overflow_hidden().text_ellipsis().whitespace_nowrap().children(text)
    }

    /// The days with messages, a column per week ending this week, as many weeks as fit (up to a year).
    fn map(&self, snapshot: &StatsSnapshot, look: &Look, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let activity = snapshot.activity.clone();
        let today_row = snapshot.fetched_date.weekday().num_days_from_monday() as usize;
        let heat = look.heat;
        let theme = cx.theme();
        let today_ring = theme.foreground.opacity(0.55);
        let hover_ring = theme.foreground.opacity(0.85);
        let hovered = self.hover;
        let card = cx.entity().downgrade();
        let last_hover = Rc::new(Cell::new(hovered));
        div()
            .id("usage-activity-map")
            .w_full()
            .h(px(MAP_HEIGHT))
            .on_hover(cx.listener(|this, inside: &bool, _, cx| {
                if !*inside && this.hover.is_some() {
                    this.hover = None;
                    cx.notify();
                }
            }))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        let layout = MapLayout::new(f32::from(bounds.size.width), activity.len(), today_row);
                        let peak = activity.iter().copied().max().unwrap_or(0);
                        // Batch all <= 371 cells into a single paint_layer so they share one scene sort order.
                        window.paint_layer(bounds.dilate(px(2.0)), |window| {
                            let mut hovered_cell: Option<(Bounds<Pixels>, Hsla)> = None;
                            for (index, count) in activity.iter().enumerate() {
                                let Some((x, y)) = layout.place(index) else { continue };
                                let origin = point(bounds.origin.x + px(x), bounds.origin.y + px(y));
                                let cell = Bounds::new(origin, size(px(CELL), px(CELL)));
                                let color = heat[heat_level(*count, peak)];
                                if hovered == Some(index) {
                                    // Paint the hovered cell last so its 1.5px micro-expansion sits cleanly above neighbors.
                                    hovered_cell = Some((cell.dilate(px(HOVER_INFLATE)), color));
                                    continue;
                                }
                                let today = index + 1 == activity.len();
                                let ring = if today { Edges::all(px(1.)) } else { Edges::default() };
                                window.paint_quad(quad(cell, Corners::all(px(2.)), color, ring, today_ring, BorderStyle::default()));
                            }
                            if let Some((cell, color)) = hovered_cell {
                                window.paint_quad(quad(cell, Corners::all(px(2.5)), color, Edges::all(px(1.25)), hover_ring, BorderStyle::default()));
                            }
                        });
                        let (card, last_hover) = (card.clone(), last_hover.clone());
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if !phase.bubble() {
                                return;
                            }
                            let inside = bounds.contains(&event.position);
                            let at = inside.then(|| layout.index_at(f32::from(event.position.x - bounds.origin.x), f32::from(event.position.y - bounds.origin.y))).flatten();
                            if at != last_hover.get() {
                                last_hover.set(at);
                                let _ = card.update(cx, |card, cx| {
                                    card.hover = at;
                                    cx.notify();
                                });
                            }
                        });
                    },
                )
                .size_full(),
            )
    }

    fn models(&self, locale: Locale, look: &Look, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (shown, stale) = self.shown();
        let stats = match shown {
            Some(Err(error)) => return div().text_sm().text_color(look.muted).child(error.clone()).into_any_element(),
            Some(Ok(snap)) => &snap.stats,
            None => return div().h(px(MAP_HEIGHT)).into_any_element(),
        };
        if stats.models.is_empty() {
            return div().text_sm().text_color(look.muted).child(i18n::text(locale, "optics.noModels")).into_any_element();
        }
        let peak = stats.models.first().map_or(1.0, |model| model.tokens.max(1.0));
        let fill = cx.theme().primary;
        let rows: Vec<AnyElement> = stats
            .models
            .iter()
            .take(6)
            .enumerate()
            .map(|(row_ix, model)| {
                let target_share = ((model.tokens / peak).clamp(0.0, 1.0) as f32).max(0.02);
                let share = motion::transition_f32(("usage-model-bar", row_ix.to_string()), target_share, SETTLE, window, cx).clamp(0.02, 1.0);
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().w(px(150.)).flex_shrink_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(self.model_name(&model.provider, &model.model, cx)))
                    .child(div().flex_1().h(px(8.)).rounded(px(4.)).bg(look.cell).child(div().h_full().w(relative(share)).rounded(px(4.)).bg(fill.opacity(0.85))))
                    .child(div().w(px(64.)).flex_shrink_0().text_right().text_sm().font_medium().child(crate::ui::format_tokens(model.tokens)))
                    .child(div().w(px(88.)).flex_shrink_0().text_right().text_xs().text_color(look.muted).child(i18n::format(locale, "optics.replies", &[("count", &model.replies)])))
                    .into_any_element()
            })
            .collect();
        v_flex().gap_2().min_h(px(MAP_HEIGHT)).when(stale, |this| this.opacity(0.55)).children(rows).into_any_element()
    }

    fn limits(&self, locale: Locale, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let sections: Vec<AnyElement> = store
            .external_agents
            .iter()
            .filter(|agent| agent.enabled)
            .filter_map(|agent| {
                let section = crate::ui::quota::section(agent, None, locale, cx)?;
                Some(v_flex().gap_1().child(div().text_sm().font_medium().child(agent.label.clone())).child(section).into_any_element())
            })
            .collect();
        if sections.is_empty() {
            return div().text_sm().text_color(look.muted).child(i18n::text(locale, "optics.noLimits")).into_any_element();
        }
        v_flex().gap_3().min_h(px(MAP_HEIGHT)).children(sections).into_any_element()
    }

    /// A model's name as the composer shows it.
    fn model_name(&self, provider: &str, model: &str, cx: &App) -> String {
        let preset = self.store.read(cx).models.as_ref().and_then(|status| {
            status.presets.iter().find(|preset| preset.model.provider == provider && preset.model.model == model).map(|preset| preset.name.clone())
        });
        crate::ui::chat::short_model_name(model, preset.as_deref())
    }

    /// Ask the Host for the external agents' limits again, as the model card does when it opens.
    pub fn read_limits(&self, cx: &mut App) {
        let kinds: Vec<String> = self.store.read(cx).external_agents.iter().filter(|agent| agent.enabled && agent.installed).map(|agent| agent.kind.clone()).collect();
        for kind in kinds {
            crate::ui::quota::refresh(&self.store, &kind, false, cx);
        }
    }
}

/// Where the map's cells go in a given width: whole weeks, Monday on top, this week on the right.
#[derive(Clone, Copy, Debug, PartialEq)]
struct MapLayout {
    /// Days shown (from the end of the activity), and how many there are in all.
    shown: usize,
    total: usize,
    /// The row of the first day shown (0 is Monday).
    first_row: usize,
}

impl MapLayout {
    fn new(width: f32, total: usize, today_row: usize) -> Self {
        let weeks = (((width + GAP) / (CELL + GAP)).floor() as usize).clamp(1, 53);
        // Full weeks before this one, and this week up to today.
        let shown = ((weeks - 1) * 7 + today_row + 1).min(total);
        let first_row = (today_row + 7 * 53 + 1 - shown % 7) % 7;
        Self { shown, total, first_row }
    }

    /// The top left of day `index` of the activity, if it is shown.
    fn place(&self, index: usize) -> Option<(f32, f32)> {
        let from = self.total.checked_sub(self.shown)?;
        let offset = index.checked_sub(from)? + self.first_row;
        let (column, row) = (offset / 7, offset % 7);
        Some((column as f32 * (CELL + GAP), row as f32 * (CELL + GAP)))
    }

    fn index_at(&self, x: f32, y: f32) -> Option<usize> {
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let (column, row) = ((x / (CELL + GAP)) as usize, (y / (CELL + GAP)) as usize);
        if row >= 7 || x - column as f32 * (CELL + GAP) > CELL || y - row as f32 * (CELL + GAP) > CELL {
            return None;
        }
        let offset = (column * 7 + row).checked_sub(self.first_row)?;
        (offset < self.shown).then(|| self.total - self.shown + offset)
    }
}

/// The map's step for a day: 0 without messages, then 1 to 4 by its share of the busiest day.
fn heat_level(count: u32, peak: u32) -> usize {
    if count == 0 || peak == 0 {
        return 0;
    }
    let share = count as f32 / peak as f32;
    ((share * 4.0).ceil() as usize).clamp(1, 4)
}

fn percent(rate: f64) -> String {
    let value = rate * 100.0;
    if value >= 99.95 || value == 0.0 { format!("{value:.0}%") } else { format!("{value:.1}%") }
}

fn day_label(locale: Locale, date: NaiveDate) -> String {
    let weekday = date.weekday().num_days_from_monday() as usize;
    match locale {
        Locale::Zh => format!("{}月{}日 周{}", date.month(), date.day(), ["一", "二", "三", "四", "五", "六", "日"][weekday]),
        Locale::En => format!("{}, {} {}", ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][weekday], ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][date.month0() as usize], date.day()),
    }
}

/// Rough lengths in tokens, for the comparison under the numbers.
const PAGE_TOKENS: f64 = 600.0;
const BOOKS: [(&str, f64); 3] = [("optics.bookLittlePrince", 30_000.0), ("optics.bookRedChamber", 1_000_000.0), ("optics.bookBritannica", 60_000_000.0)];

/// The tokens as something to picture: copies of the longest book that fits at least three times, else pages.
fn fun_line(locale: Locale, tokens: f64) -> Option<String> {
    if tokens < PAGE_TOKENS {
        return None;
    }
    let count = |value: f64| if value < 10.0 { format!("{value:.1}") } else { format!("{value:.0}") };
    match BOOKS.iter().rev().find(|(_, size)| tokens / size >= 3.0) {
        Some((book, size)) => Some(i18n::format(locale, "optics.funBook", &[("count", &count(tokens / size)), ("book", &i18n::text(locale, book))])),
        None => Some(i18n::format(locale, "optics.funPages", &[("count", &count(tokens / PAGE_TOKENS))])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn the_map_ends_today_and_round_trips_the_pointer() {
        // 30 weeks fit; today is a Wednesday (row 2).
        let layout = MapLayout::new(30.0 * (CELL + GAP), 371, 2);
        assert_eq!(layout.shown, 29 * 7 + 3);
        let today = layout.place(370).unwrap();
        assert_eq!(today, (29.0 * (CELL + GAP), 2.0 * (CELL + GAP)));
        // The first day shown is on a Monday in the first column.
        assert_eq!(layout.place(371 - layout.shown).unwrap(), (0.0, 0.0));
        assert_eq!(layout.place(371 - layout.shown - 1), None);
        for index in [371 - layout.shown, 200, 300, 370] {
            let (x, y) = layout.place(index).unwrap();
            assert_eq!(layout.index_at(x + 1.0, y + 1.0), Some(index));
        }
        // Thursday of this week is not there yet; the gap between cells is nobody's.
        assert_eq!(layout.index_at(29.0 * (CELL + GAP) + 1.0, 3.0 * (CELL + GAP) + 1.0), None);
        assert_eq!(layout.index_at(CELL + 1.0, 1.0), None);
        // A Sunday: the last column is full. A narrow width still shows this week.
        assert_eq!(MapLayout::new(10.0 * (CELL + GAP), 371, 6).shown, 70);
        assert_eq!(MapLayout::new(5.0, 371, 0).shown, 1);
    }

    #[test]
    fn heat_steps_follow_the_busiest_day() {
        assert_eq!(heat_level(0, 10), 0);
        assert_eq!(heat_level(1, 10), 1);
        assert_eq!(heat_level(5, 10), 2);
        assert_eq!(heat_level(7, 10), 3);
        assert_eq!(heat_level(10, 10), 4);
        assert_eq!(heat_level(3, 0), 0);
    }

    #[test]
    fn numbers_read_naturally() {
        assert_eq!(percent(0.952), "95.2%");
        assert_eq!(percent(1.0), "100%");
        assert_eq!(percent(0.0), "0%");
        assert_eq!(day_label(Locale::Zh, NaiveDate::from_ymd_opt(2026, 11, 26).unwrap()), "11月26日 周四");
        assert_eq!(day_label(Locale::En, NaiveDate::from_ymd_opt(2026, 11, 26).unwrap()), "Thu, Nov 26");
        assert_eq!(fun_line(Locale::Zh, 100.0), None);
        assert_eq!(fun_line(Locale::Zh, 6_000.0).unwrap(), "这些 Token 约合 10 页 A4 文稿。");
        assert_eq!(fun_line(Locale::Zh, 29_000_000.0).unwrap(), "这些 Token 约等于 29 本《红楼梦》的篇幅。");
        assert_eq!(fun_line(Locale::En, 2_000_000.0).unwrap(), "That's about 67 copies of The Little Prince.");
    }

    #[test]
    fn width_buckets_absorb_minor_window_resizes() {
        assert_eq!(width_bucket(px(900.0)), width_bucket(px(905.0)));
        assert_ne!(width_bucket(px(900.0)), width_bucket(px(940.0)));
    }
}
