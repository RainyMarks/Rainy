//! Lightweight motion primitives shared across Rainy 5.0: a single reduced-motion source of truth, calm timing and
//! spring tokens, capped-FPS repeating indicators, measured sliding indicators, and finite one-shot transitions that
//! stop requesting frames the moment they settle (0 extra frames / 0 extra CPU at rest).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::base::{
    Easing, ElementExt as _, Interpolate, MotionReveal, Presence, PresencePhase, Spring, Transition,
    TransitionId, apply_system_reduce_motion, spring as base_spring, transition as base_transition,
};
use gpui_kit::component::{Icon, IconName, Sizable as _, Size};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// Calm, desktop-grade durations (ms).
#[allow(dead_code)]
pub const FAST: Duration = Duration::from_millis(110);
#[allow(dead_code)]
pub const NORMAL: Duration = Duration::from_millis(170);
pub const SETTLE: Duration = Duration::from_millis(220);

/// Frame-rate caps for active continuous indicators (0 fps once settled or idle).
#[allow(dead_code)]
pub const PULSE_FPS: f32 = 24.0;
#[allow(dead_code)]
pub const SPINNER_FPS: f32 = 30.0;
#[allow(dead_code)]
pub const OPTICS_IDLE_FPS: f32 = 10.0;
#[allow(dead_code)]
pub const OPTICS_BURST_FPS: f32 = 20.0;

/// Critically / near-critically damped springs: crisp settle without bouncy overshoot.
pub const SPRING_INDICATOR: Spring = Spring::new(Duration::from_millis(180)).with_damping(0.92).with_epsilon(0.25);
pub const SPRING_SLIDER: Spring = Spring::new(Duration::from_millis(140)).with_damping(0.90).with_epsilon(0.2);
#[allow(dead_code)]
pub const SPRING_DRAWER: Spring = Spring::new(Duration::from_millis(210)).with_damping(1.0).with_epsilon(0.5);
#[allow(dead_code)]
pub const SPRING_VALUE: Spring = Spring::new(Duration::from_millis(220)).with_damping(1.0).with_epsilon(0.002);

/// Whether `RAINY_REDUCED_MOTION=1` forced reduced motion at startup.
static ENV_REDUCED: AtomicBool = AtomicBool::new(false);
/// Mirrors `!cx.reduce_motion()` after [`init`]/[`sync_system`] so callers without `&App` stay in step.
static SYSTEM_ALLOWED: AtomicBool = AtomicBool::new(true);
/// Whether [`init`] has wired GPUI's `App::reduce_motion`.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Whether the environment explicitly asks for reduced motion (`RAINY_REDUCED_MOTION=1`).
pub fn env_reduced() -> bool {
    std::env::var("RAINY_REDUCED_MOTION").is_ok_and(|value| value == "1")
}

/// Reads the operating system's client-area animation switch.
#[cfg(windows)]
pub fn system_animations_enabled() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW};
    let mut enabled: i32 = 1;
    // SAFETY: `SPI_GETCLIENTAREAANIMATION` writes one BOOL into `enabled`, which outlives the call.
    let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&mut enabled as *mut i32).cast(), 0) };
    ok == 0 || enabled != 0
}

#[cfg(not(windows))]
pub fn system_animations_enabled() -> bool {
    true
}

/// Initializes Rainy's single reduced-motion truth right after `gpui_kit::init(cx)`.
///
/// `RAINY_REDUCED_MOTION=1` overrides the OS setting and locks `cx.reduce_motion()` to `true`; otherwise GPUI's flag
/// follows the operating system setting.
#[allow(dead_code)]
pub fn init(cx: &mut App) {
    let forced = env_reduced();
    ENV_REDUCED.store(forced, Ordering::Relaxed);
    INITIALIZED.store(true, Ordering::Relaxed);
    if forced {
        cx.set_reduce_motion(true);
        SYSTEM_ALLOWED.store(false, Ordering::Relaxed);
    } else {
        apply_system_reduce_motion(cx);
        SYSTEM_ALLOWED.store(!cx.reduce_motion(), Ordering::Relaxed);
    }
}

/// Re-reads the OS animation switch (for example on window activation) when not locked by `RAINY_REDUCED_MOTION=1`.
/// Returns whether `cx.reduce_motion()` changed.
#[allow(dead_code)]
pub fn sync_system(cx: &mut App) -> bool {
    let before = cx.reduce_motion();
    if ENV_REDUCED.load(Ordering::Relaxed) || env_reduced() {
        ENV_REDUCED.store(true, Ordering::Relaxed);
        if !before {
            cx.set_reduce_motion(true);
        }
        SYSTEM_ALLOWED.store(false, Ordering::Relaxed);
        return !before;
    }
    apply_system_reduce_motion(cx);
    let after = cx.reduce_motion();
    SYSTEM_ALLOWED.store(!after, Ordering::Relaxed);
    before != after
}

/// Whether animations may play in this `App` (honors both OS reduced-motion and `RAINY_REDUCED_MOTION=1`).
pub fn allowed(cx: &App) -> bool {
    !cx.reduce_motion() && !ENV_REDUCED.load(Ordering::Relaxed) && !env_reduced()
}

/// Shared check for call sites without `&App` (such as [`crate::ui::optics::backdrop::motion_allowed`]).
///
/// Once [`init`] has run, this reads the synchronized atomic without issuing OS FFI calls; before [`init`] (e.g. unit
/// tests or callers before wiring), it falls back to a 1-second thread-local cache of the OS switch.
pub fn allowed_without_app() -> bool {
    if ENV_REDUCED.load(Ordering::Relaxed) || env_reduced() {
        return false;
    }
    if INITIALIZED.load(Ordering::Relaxed) {
        return SYSTEM_ALLOWED.load(Ordering::Relaxed);
    }
    thread_local! {
        static FALLBACK: Cell<Option<(Instant, bool)>> = const { Cell::new(None) };
    }
    FALLBACK.with(|cache| {
        if let Some((at, ok)) = cache.get()
            && at.elapsed() < Duration::from_secs(1)
        {
            return ok;
        }
        let ok = system_animations_enabled();
        cache.set(Some((Instant::now(), ok)));
        ok
    })
}

/// Cubic ease-out curve (`1 - (1 - t)^3`), clamped to `[0, 1]`.
#[allow(dead_code)]
pub fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t.clamp(0.0, 1.0);
    1.0 - inv * inv * inv
}

/// Calm ease-out transition helper for the standard durations.
pub fn transition_policy(duration: Duration) -> Transition {
    Transition::new(duration).easing(Easing::EaseOut)
}

/// Samples a keyed spring value, snapping immediately when reduced motion is active (0 frames at rest).
pub fn spring_value<T>(id: impl Into<TransitionId>, target: T, policy: Spring, window: &mut Window, cx: &mut App) -> T::Output
where
    T: SpringTarget,
{
    let policy = if allowed(cx) { policy } else { policy.with_travel(false) };
    base_spring(id, target, policy, window, cx)
}

/// Samples a keyed value transition, snapping immediately when reduced motion is active (0 frames at rest).
pub fn transition_value<T>(id: impl Into<TransitionId>, target: T, duration: Duration, window: &mut Window, cx: &mut App) -> T
where
    T: Interpolate + PartialEq + 'static,
{
    let effective = if allowed(cx) { duration } else { Duration::ZERO };
    base_transition(id, target, transition_policy(effective), window, cx)
}

/// Convenience `f32` value transition (used for smooth metric count-ups and progress bars).
pub fn transition_f32(id: impl Into<TransitionId>, target: f32, duration: Duration, window: &mut Window, cx: &mut App) -> f32 {
    transition_value(id, target, duration, window, cx)
}

/// Phase-locked, 30 fps capped loading spinner that stops cleanly under reduced motion.
///
/// Replaces unconstrained native-refresh `Spinner` instances that otherwise redraw the whole window at 144–240 fps.
#[allow(dead_code)]
pub fn spinner(size: Size) -> impl IntoElement {
    spinner_styled(size, None)
}

/// Phase-locked, 30 fps capped loading spinner with an explicit color.
#[allow(dead_code)]
pub fn spinner_colored(size: Size, color: Hsla) -> impl IntoElement {
    spinner_styled(size, Some(color))
}

#[allow(dead_code)]
fn spinner_styled(size: Size, color: Option<Hsla>) -> impl IntoElement {
    let icon = Icon::new(IconName::Loader).with_size(size).when_some(color, |this, color| this.text_color(color));
    if !allowed_without_app() {
        return div().flex_shrink_0().child(icon).into_any_element();
    }
    div()
        .flex_shrink_0()
        .child(icon.with_animation(
            "rainy-spinner",
            Animation::new(Duration::from_millis(900)).repeat_synced().with_max_fps(SPINNER_FPS).with_easing(ease_in_out),
            |this, delta| this.transform(Transformation::rotate(percentage(delta))),
        ))
        .into_any_element()
}

/// One-shot entrance animation: gentle vertical settle (`dy_px -> 0`) + fade (`0 -> 1`).
///
/// Runs once per element `id`, then stops permanently (0 frames at rest). Honors reduced motion.
#[allow(dead_code)]
pub fn enter<E: IntoElement + Styled + 'static>(id: impl Into<ElementId>, dy_px: f32, duration: Duration, element: E) -> AnyElement {
    if !allowed_without_app() || duration.is_zero() {
        return element.into_any_element();
    }
    element
        .with_animation(id, Animation::new(duration).with_easing(ease_out_cubic), move |el, delta| {
            el.top(px(dy_px * (1.0 - delta))).opacity(delta)
        })
        .into_any_element()
}

/// Measured vertical reveal/collapse backed by `Presence` + `MotionReveal`.
///
/// Suitable for fixed-layout collapsible sections (e.g. `ChangesBar`, sidebar groups); avoid inside virtualized
/// `ListState` rows where dynamic height changes would trigger per-frame remeasure jitter.
/// Returns `None` once exited (0 layout/paint), unwraps directly when fully present (0 frames at rest).
#[allow(dead_code)]
pub fn reveal(
    id: impl Into<ElementId>,
    open: bool,
    duration: Duration,
    build: impl FnOnce() -> AnyElement,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    let id: ElementId = id.into();
    let effective = if allowed(cx) { duration } else { Duration::ZERO };
    let sample = Presence::new(id.clone(), open).transition(transition_policy(effective)).sample(window, cx);
    if !sample.should_render() {
        return None;
    }
    if sample.phase == PresencePhase::Present {
        return Some(build());
    }
    let reveal_id = ElementId::NamedChild(id.into(), "reveal".into());
    Some(MotionReveal::new(reveal_id, sample.progress, div().w_full().opacity(sample.progress).child(build()).into_any_element()).into_any_element())
}

/// Orientation of a sliding selection indicator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// Edge placement of [`sliding_bar`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum BarEdge {
    Top,
    Bottom,
    Left,
    Right,
}

/// Visual options for [`sliding_pill`].
#[derive(Clone, Copy, Debug)]
pub struct PillStyle {
    pub bg: Hsla,
    pub radius: Pixels,
    pub gap: Pixels,
}

/// Visual options for [`sliding_bar`].
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct BarStyle {
    pub axis: Axis,
    pub edge: BarEdge,
    pub color: Hsla,
    pub thickness: Pixels,
    pub inset: Pixels,
    pub gap: Pixels,
}

/// Prepaint-measured layout bounds for a group of selectable items.
#[derive(Default)]
struct IndicatorTrack {
    container: Option<Bounds<Pixels>>,
    items: Vec<Option<Bounds<Pixels>>>,
    placed: Option<(f32, f32)>,
}

impl IndicatorTrack {
    fn resize(&mut self, count: usize) {
        if self.items.len() != count {
            self.items.resize(count, None);
        }
    }

    /// Relative `(offset, extent)` along `axis` for `selected`, if both container and item were measured.
    fn target(&self, selected: usize, axis: Axis) -> Option<(f32, f32)> {
        let container = self.container?;
        let item = (*self.items.get(selected)?)?;
        let (offset, extent) = match axis {
            Axis::Horizontal => (f32::from(item.origin.x - container.origin.x), f32::from(item.size.width)),
            Axis::Vertical => (f32::from(item.origin.y - container.origin.y), f32::from(item.size.height)),
        };
        (extent > 0.0).then_some((offset, extent))
    }
}

/// Horizontal row of items with a single-quad spring-animated pill background behind the `selected` item.
///
/// The indicator snaps immediately on first mount and under reduced motion, and requests 0 frames at rest.
pub fn sliding_pill(
    id: impl Into<ElementId>,
    selected: usize,
    items: Vec<AnyElement>,
    style: PillStyle,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let id: ElementId = id.into();
    let count = items.len();
    let track = track_for(&id, count, window, cx);
    let view = window.current_view();
    let indicator = {
        let mut guard = track.borrow_mut();
        let target = guard.target(selected, Axis::Horizontal);
        guard.placed = target;
        target
    }
    .map(|(target_x, target_w)| {
        let cur_x = spring_value((id.clone(), "pill-x"), px(target_x), SPRING_INDICATOR, window, cx);
        let cur_w = spring_value((id.clone(), "pill-w"), px(target_w), SPRING_INDICATOR, window, cx);
        div().absolute().top_0().bottom_0().left(cur_x).w(cur_w).rounded(style.radius).bg(style.bg)
    });

    let container_track = track.clone();
    div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(style.gap)
        .on_prepaint(move |bounds, _, _| {
            container_track.borrow_mut().container = Some(bounds);
        })
        .children(indicator)
        .children(wrap_measured_items(items, selected, Axis::Horizontal, track, view))
}

/// Row or column of items with a crisp single-quad spring-animated accent bar along `edge` of the `selected` item.
///
/// `style.thickness` is the bar's minor dimension (e.g. `px(2.)` for tabs, `px(2.5)` for sidebar nav); `style.inset`
/// shortens the bar symmetrically along its major axis so vertical nav bars sit centered beside their row.
#[allow(dead_code)]
pub fn sliding_bar(
    id: impl Into<ElementId>,
    selected: usize,
    items: Vec<AnyElement>,
    style: BarStyle,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let id: ElementId = id.into();
    let count = items.len();
    let track = track_for(&id, count, window, cx);
    let view = window.current_view();
    let inset_f = f32::from(style.inset);
    let indicator = {
        let mut guard = track.borrow_mut();
        let target = guard.target(selected, style.axis);
        guard.placed = target;
        target
    }
    .map(|(target_pos, target_ext)| {
        let cur_pos = spring_value((id.clone(), "bar-p"), px(target_pos), SPRING_INDICATOR, window, cx);
        let cur_ext = spring_value((id.clone(), "bar-e"), px(target_ext), SPRING_INDICATOR, window, cx);
        let bar_pos = cur_pos + px(inset_f);
        let bar_ext = (cur_ext - px(inset_f * 2.0)).max(px(2.0));
        let base = div().absolute().rounded_full().bg(style.color);
        match (style.axis, style.edge) {
            (Axis::Horizontal, BarEdge::Top) => base.top_0().left(bar_pos).w(bar_ext).h(style.thickness),
            (Axis::Horizontal, _) => base.bottom_0().left(bar_pos).w(bar_ext).h(style.thickness),
            (Axis::Vertical, BarEdge::Right) => base.right_0().top(bar_pos).h(bar_ext).w(style.thickness),
            (Axis::Vertical, _) => base.left_0().top(bar_pos).h(bar_ext).w(style.thickness),
        }
    });

    let container_track = track.clone();
    let container = match style.axis {
        Axis::Horizontal => div().relative().flex().flex_row().items_center().gap(style.gap),
        Axis::Vertical => div().relative().flex().flex_col().gap(style.gap),
    };
    container
        .on_prepaint(move |bounds, _, _| {
            container_track.borrow_mut().container = Some(bounds);
        })
        .children(wrap_measured_items(items, selected, style.axis, track, view))
        .children(indicator)
}

fn track_for(id: &ElementId, count: usize, window: &mut Window, cx: &mut App) -> Rc<RefCell<IndicatorTrack>> {
    let key = ElementId::NamedChild(id.clone().into(), "indicator-track".into());
    let track = window.use_keyed_state(key, cx, |_, _| Rc::new(RefCell::new(IndicatorTrack::default()))).read(cx).clone();
    track.borrow_mut().resize(count);
    track
}

fn wrap_measured_items(
    items: Vec<AnyElement>,
    selected: usize,
    axis: Axis,
    track: Rc<RefCell<IndicatorTrack>>,
    view: EntityId,
) -> impl Iterator<Item = Div> {
    items.into_iter().enumerate().map(move |(index, item)| {
        let track = track.clone();
        let is_selected = index == selected;
        div()
            .relative()
            .on_prepaint(move |bounds, window, _| {
                let mut guard = track.borrow_mut();
                if let Some(slot) = guard.items.get_mut(index) {
                    *slot = Some(bounds);
                }
                if is_selected
                    && let Some(measured) = guard.target(index, axis)
                {
                    let moved = guard.placed.is_none_or(|(p, e)| (p - measured.0).abs() > 0.5 || (e - measured.1).abs() > 0.5);
                    if moved {
                        window.on_next_frame(move |_, cx| cx.notify(view));
                    }
                }
            })
            .child(item)
    })
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn ease_out_cubic_bounds_and_monotonic() {
        assert_eq!(ease_out_cubic(-0.5), 0.0);
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert_eq!(ease_out_cubic(2.0), 1.0);
        assert!(ease_out_cubic(0.25) > 0.5);
        assert!(ease_out_cubic(0.5) > ease_out_cubic(0.25));
        assert!(ease_out_cubic(0.9) < 1.0);
    }

    #[test]
    fn tokens_stay_calm_and_bounded() {
        const {
            assert!(PULSE_FPS <= 30.0 && SPINNER_FPS <= 30.0);
            assert!(OPTICS_IDLE_FPS <= OPTICS_BURST_FPS && OPTICS_BURST_FPS <= 24.0);
        }
        assert!(FAST < NORMAL && NORMAL < SETTLE);
        assert!(SPRING_INDICATOR.epsilon() >= 0.1);
        assert!(SPRING_SLIDER.epsilon() >= 0.1);
        assert!(SPRING_VALUE.epsilon() <= 0.01);
    }

    #[test]
    fn indicator_track_computes_relative_bounds_along_both_axes() {
        let mut track = IndicatorTrack::default();
        track.resize(3);
        track.container = Some(Bounds::new(point(px(100.), px(40.)), size(px(300.), px(120.))));
        track.items[1] = Some(Bounds::new(point(px(180.), px(72.)), size(px(90.), px(32.))));
        assert_eq!(track.target(1, Axis::Horizontal), Some((80.0, 90.0)));
        assert_eq!(track.target(1, Axis::Vertical), Some((32.0, 32.0)));
        assert_eq!(track.target(0, Axis::Horizontal), None);
        assert_eq!(track.target(9, Axis::Horizontal), None);
    }
}
