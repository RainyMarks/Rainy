//! The reasoning slider of the model card: the levels a model offers as stops on a track that fills from the coolest
//! level's color to the chosen one's, with stars twinkling in the filled part. A click or a drag picks a level; the
//! level is sent when the button is released ([`Gesture`]).
//!
//! The thumb moves with a damped spring on clicks/releases and tracks the pointer without lag while dragged; the stars
//! twinkle locally at 12 fps only for [`SPARKLE_HOLD`] after opening or interacting, then settle to 0 frames / 0 CPU
//! (never calling `window.refresh()`, never running under reduced motion). All quads share one `paint_layer`.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::ui::motion::{self, SPRING_SLIDER};

pub const TRACK_HEIGHT: f32 = 22.0;
const THUMB: f32 = 18.0;
const RADIUS: f32 = 6.0;
const STARS: u32 = 24;
/// 12 fps local sparkle cadence while active.
const SPARKLE_TICK: Duration = Duration::from_millis(83);
/// Stars settle automatically after this long without interaction (0 fps at rest).
const SPARKLE_HOLD: Duration = Duration::from_secs(3);

/// What the pointer did, with the stop under it: pressed on the track, moved (with the left button held or not) or
/// let the left button go. Moves and releases come wherever the pointer is; whoever keeps the drag decides what they
/// mean, from the drag as it is when they come: a press and its release can both arrive before the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    Press(usize),
    Move { stop: usize, held: bool },
    Release(usize),
}

/// Called with each [`Gesture`].
pub type OnGesture = Rc<dyn Fn(Gesture, &mut Window, &mut App)>;

/// What `gesture` does to `drag` (the stop dragged to, if a drag is under way): the drag after it and the stop picked
/// when it ends one, or `None` when it leaves things as they are (a move or a release with no drag). A move with the
/// button up ends a drag whose release was missed (let go outside the window) without picking.
pub fn follow(drag: Option<usize>, gesture: Gesture) -> Option<(Option<usize>, Option<usize>)> {
    match (drag, gesture) {
        (_, Gesture::Press(stop)) => Some((Some(stop), None)),
        (Some(_), Gesture::Move { stop, held: true }) => Some((Some(stop), None)),
        (Some(_), Gesture::Move { held: false, .. }) => Some((None, None)),
        (Some(_), Gesture::Release(stop)) => Some((None, Some(stop))),
        _ => None,
    }
}

/// Where stop `index` of `count` is along a track `width` wide, from its left edge.
pub fn stop_x(index: usize, count: usize, width: f32) -> f32 {
    let inset = THUMB / 2.0 + 2.0;
    if count < 2 {
        return inset;
    }
    inset + (width - 2.0 * inset).max(0.0) * index.min(count - 1) as f32 / (count - 1) as f32
}

/// Normalized `[0, 1]` position of stop `index` among `count` stops.
fn stop_ratio(index: usize, count: usize) -> f32 {
    if count < 2 {
        0.0
    } else {
        index.min(count - 1) as f32 / (count - 1) as f32
    }
}

/// The stop nearest to `x`, measured from the track's left edge.
pub fn stop_at(x: f32, count: usize, width: f32) -> usize {
    (0..count).min_by(|a, b| (stop_x(*a, count, width) - x).abs().total_cmp(&(stop_x(*b, count, width) - x).abs())).unwrap_or(0)
}

/// Per-card local slider state: finite-duration sparkle clock, drag tracking and hovered stop.
struct SliderState {
    dragging: bool,
    drag_ratio: Option<f32>,
    hovered_stop: Option<usize>,
    last_shown: Option<usize>,
    sparkle_clock: f32,
    sparkle_until: Option<Instant>,
    last_tick: Option<Instant>,
    ticker: Option<Task<()>>,
}

impl SliderState {
    fn new() -> Self {
        Self {
            dragging: false,
            drag_ratio: None,
            hovered_stop: None,
            last_shown: None,
            sparkle_clock: 0.0,
            sparkle_until: Some(Instant::now() + SPARKLE_HOLD),
            last_tick: None,
            ticker: None,
        }
    }

    fn wake_sparkles(&mut self, view: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if !motion::allowed(cx) {
            self.sparkle_until = None;
            self.ticker = None;
            return;
        }
        self.sparkle_until = Some(Instant::now() + SPARKLE_HOLD);
        self.ensure_ticking(view, window, cx);
    }

    fn should_tick(&self, window: &Window, cx: &App) -> bool {
        motion::allowed(cx) && window.is_window_active() && self.sparkle_until.is_some_and(|until| Instant::now() < until)
    }

    fn ensure_ticking(&mut self, view: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if self.ticker.is_some() || !self.should_tick(window, cx) {
            return;
        }
        self.last_tick = Some(Instant::now());
        self.ticker = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(SPARKLE_TICK).await;
                let going = this
                    .update_in(cx, |this, window, cx| {
                        if !this.should_tick(window, cx) {
                            this.ticker = None;
                            this.last_tick = None;
                            return false;
                        }
                        let now = Instant::now();
                        let elapsed = this.last_tick.map_or(0.0, |last| now.duration_since(last).as_secs_f32());
                        this.last_tick = Some(now);
                        this.sparkle_clock = (this.sparkle_clock + elapsed.min(0.2)) % 3600.0;
                        App::notify(cx, view);
                        true
                    })
                    .unwrap_or(false);
                if !going {
                    break;
                }
            }
        }));
    }
}

/// The track for `fills` (one color per level), showing level `shown`.
pub fn render(fills: Vec<Hsla>, shown: usize, dark: bool, on_gesture: OnGesture) -> impl IntoElement {
    ReasoningSlider { fills, shown, dark, on_gesture }
}

#[derive(IntoElement)]
struct ReasoningSlider {
    fills: Vec<Hsla>,
    shown: usize,
    dark: bool,
    on_gesture: OnGesture,
}

impl RenderOnce for ReasoningSlider {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let count = self.fills.len();
        let shown = if count == 0 { 0 } else { self.shown.min(count - 1) };
        let view = window.current_view();
        let state = window.use_keyed_state("optics-reasoning-slider", cx, |_, _| SliderState::new());

        let (dragging, drag_ratio, hovered_stop, sparkle_time) = {
            let need_wake = state.read(cx).last_shown != Some(shown);
            if need_wake {
                state.update(cx, |s, cx| {
                    s.last_shown = Some(shown);
                    s.wake_sparkles(view, window, cx);
                });
            } else {
                state.update(cx, |s, cx| s.ensure_ticking(view, window, cx));
            }
            let s = state.read(cx);
            let time = if motion::allowed(cx) { s.sparkle_clock } else { 0.0 };
            (s.dragging, s.drag_ratio, s.hovered_stop, time)
        };

        let target_ratio = drag_ratio.filter(|_| dragging).unwrap_or_else(|| stop_ratio(shown, count));
        // Spring uses normalized `[0, 1]` coordinates: scale epsilon appropriately so pixel settle stays crisp (~0.2px).
        let policy = if dragging {
            SPRING_SLIDER.with_travel(false).with_epsilon(0.001)
        } else {
            SPRING_SLIDER.with_epsilon(0.001)
        };
        let animated_ratio = motion::spring_value("optics-slider-pos", target_ratio, policy, window, cx).clamp(0.0, 1.0);

        let fills = self.fills;
        let dark = self.dark;
        let on_gesture = self.on_gesture;
        let weak_state = state.downgrade();

        div().w_full().h(px(TRACK_HEIGHT)).cursor_pointer().child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    paint(bounds, &fills, animated_ratio, hovered_stop, sparkle_time, dark, window);
                    listen(bounds, fills.len(), on_gesture, weak_state, view, window);
                },
            )
            .size_full(),
        )
    }
}

fn hash(index: u32, salt: u32) -> f32 {
    let mut h = index.wrapping_mul(0x9e37_79b9) ^ salt.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Linearly interpolates between adjacent stop colors along the track's normalized `ratio`.
fn sample_fill(fills: &[Hsla], ratio: f32) -> Hsla {
    if fills.len() <= 1 {
        return fills[0];
    }
    let scaled = ratio.clamp(0.0, 1.0) * (fills.len() - 1) as f32;
    let lo = (scaled.floor() as usize).min(fills.len() - 1);
    let hi = (lo + 1).min(fills.len() - 1);
    let t = scaled - lo as f32;
    let (a, b) = (fills[lo], fills[hi]);
    let hue_delta = (b.h - a.h + 0.5).rem_euclid(1.0) - 0.5;
    Hsla {
        h: (a.h + hue_delta * t).rem_euclid(1.0),
        s: a.s + (b.s - a.s) * t,
        l: a.l + (b.l - a.l) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

/// A rounded rectangle without a border.
fn rounded(bounds: Bounds<Pixels>, radius: f32, background: impl Into<Background>) -> PaintQuad {
    quad(bounds, Corners::all(px(radius)), background, Edges::default(), transparent_black(), BorderStyle::default())
}

/// Computes the clamped filled width and star right limit for a track of `width` and normalized `ratio`.
fn track_geometry(width: f32, count: usize, ratio: f32) -> (f32, f32) {
    let inset = (THUMB / 2.0 + 2.0).min(width * 0.5);
    let travel = (width - 2.0 * inset).max(0.0);
    let thumb_offset = if count < 2 { inset } else { inset + travel * ratio.clamp(0.0, 1.0) };
    let filled_width = (thumb_offset + THUMB / 2.0).clamp(0.0, width.max(0.0));
    (thumb_offset, filled_width)
}

fn paint(bounds: Bounds<Pixels>, fills: &[Hsla], ratio: f32, hovered_stop: Option<usize>, time: f32, dark: bool, window: &mut Window) {
    let count = fills.len();
    if count == 0 {
        return;
    }
    let (left, top) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    if width <= 1.0 || height <= 1.0 {
        return;
    }
    let (thumb_offset, filled_width) = track_geometry(width, count, ratio);
    let thumb_x = left + thumb_offset;
    let active_color = sample_fill(fills, ratio);
    let white = hsla(0.0, 0.0, 1.0, 1.0);
    let track = if dark { white.opacity(0.08) } else { hsla(0.0, 0.0, 0.0, 0.06) };

    // Batch all ~34 primitives (track, fill, stops, stars, thumb shadow + knob) into one layer.
    window.paint_layer(bounds.dilate(px(4.0)), |window| {
        window.paint_quad(rounded(bounds, RADIUS, track));

        if filled_width > 0.5 {
            let filled = Bounds::new(bounds.origin, size(px(filled_width), bounds.size.height));
            let gradient = linear_gradient(90.0, linear_color_stop(fills[0], 0.0), linear_color_stop(active_color, 1.0));
            window.paint_quad(rounded(filled, RADIUS, gradient));
        }

        // Unreached stops, with a subtle highlight dot on the hovered stop.
        for index in 0..count {
            let x = left + stop_x(index, count, width);
            if x <= thumb_x + THUMB * 0.35 {
                continue;
            }
            let hovered = hovered_stop == Some(index);
            let d = if hovered { 5.5 } else { 4.0 };
            let dot = Bounds::new(point(px(x - d * 0.5), px(top + (height - d) * 0.5)), size(px(d), px(d)));
            let color = match (dark, hovered) {
                (true, true) => white.opacity(0.58),
                (true, false) => white.opacity(0.28),
                (false, true) => hsla(0.0, 0.0, 0.0, 0.45),
                (false, false) => hsla(0.0, 0.0, 0.0, 0.22),
            };
            window.paint_quad(rounded(dot, d * 0.5, color));
        }

        // Stars inside the filled segment (guarded against narrow widths).
        let star_span_x = (width - 8.0).max(0.0);
        let star_span_y = (height - 8.0).max(0.0);
        let star_limit_x = (thumb_x - THUMB / 2.0 - 2.0).min(left + width - 4.0);
        if star_span_x > 2.0 && star_span_y > 1.0 && star_limit_x > left + 4.0 {
            for star in 0..STARS {
                let x = left + 4.0 + hash(star, 1) * star_span_x;
                if x > star_limit_x {
                    continue;
                }
                let y = top + 4.0 + hash(star, 2) * star_span_y;
                let side = 1.4 + hash(star, 3) * 1.4;
                let pace = 1.2 + hash(star, 4) * 2.6;
                let glow = 0.25 + 0.75 * (0.5 + 0.5 * (time * pace + hash(star, 5) * std::f32::consts::TAU).sin());
                let dot = Bounds::new(point(px(x - side / 2.0), px(y - side / 2.0)), size(px(side), px(side)));
                window.paint_quad(rounded(dot, side / 2.0, white.opacity(glow * 0.95)));
            }
        }

        let thumb = Bounds::new(point(px(thumb_x - THUMB / 2.0), px(top + (height - THUMB) / 2.0)), size(px(THUMB), px(THUMB)));
        window.paint_drop_shadows(thumb, Corners::all(px(THUMB / 2.0)), &[BoxShadow::new(px(0.), px(1.), hsla(0.0, 0.0, 0.0, 0.30)).blur_radius(px(3.))]);
        window.paint_quad(quad(thumb, Corners::all(px(THUMB / 2.0)), white, Edges::all(px(2.)), active_color, BorderStyle::default()));
    });
}

fn ratio_at(x: f32, width: f32) -> f32 {
    let inset = (THUMB / 2.0 + 2.0).min(width * 0.5);
    let travel = (width - 2.0 * inset).max(1.0);
    ((x - inset) / travel).clamp(0.0, 1.0)
}

fn listen(bounds: Bounds<Pixels>, count: usize, on_gesture: OnGesture, state: WeakEntity<SliderState>, view: EntityId, window: &mut Window) {
    let width = f32::from(bounds.size.width);
    let at = move |position: Point<Pixels>| stop_at(f32::from(position.x - bounds.origin.x), count, width);
    let ratio = move |position: Point<Pixels>| ratio_at(f32::from(position.x - bounds.origin.x), width);

    let pressed = on_gesture.clone();
    let state_down = state.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase.bubble() && event.button == MouseButton::Left && bounds.contains(&event.position) {
            cx.stop_propagation();
            let r = ratio(event.position);
            let _ = state_down.update(cx, |s, cx| {
                s.dragging = true;
                s.drag_ratio = Some(r);
                s.wake_sparkles(view, window, cx);
            });
            pressed(Gesture::Press(at(event.position)), window, cx);
        }
    });

    let moved = on_gesture.clone();
    let state_move = state.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if !phase.bubble() {
            return;
        }
        let inside = bounds.contains(&event.position);
        let hovered = inside.then(|| at(event.position));
        let mut notify_move = false;
        let _ = state_move.update(cx, |s, cx| {
            let was_dragging = s.dragging;
            if s.dragging && event.dragging() {
                s.drag_ratio = Some(ratio(event.position));
                s.wake_sparkles(view, window, cx);
                App::notify(cx, view);
                notify_move = true;
            } else if s.dragging && !event.dragging() {
                s.dragging = false;
                s.drag_ratio = None;
                App::notify(cx, view);
                notify_move = true;
            }
            if hovered != s.hovered_stop {
                s.hovered_stop = hovered;
                if hovered.is_some() {
                    s.wake_sparkles(view, window, cx);
                }
                App::notify(cx, view);
            }
            notify_move |= was_dragging;
        });
        if notify_move {
            moved(Gesture::Move { stop: at(event.position), held: event.dragging() }, window, cx);
        }
    });

    let state_up = state;
    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
        if phase.bubble() && event.button == MouseButton::Left {
            let mut was_dragging = false;
            let _ = state_up.update(cx, |s, cx| {
                if s.dragging {
                    was_dragging = true;
                    s.dragging = false;
                    s.drag_ratio = None;
                    s.wake_sparkles(view, window, cx);
                    App::notify(cx, view);
                }
            });
            if was_dragging {
                on_gesture(Gesture::Release(at(event.position)), window, cx);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn stops_spread_over_the_track_and_the_nearest_is_picked() {
        let width = 240.0;
        assert_eq!(stop_x(0, 5, width), THUMB / 2.0 + 2.0);
        assert_eq!(stop_x(4, 5, width), width - THUMB / 2.0 - 2.0);
        for index in 0..5 {
            assert_eq!(stop_at(stop_x(index, 5, width) + 3.0, 5, width), index);
        }
        assert_eq!(stop_at(-50.0, 5, width), 0);
        assert_eq!(stop_at(width + 50.0, 5, width), 4);
        assert_eq!(stop_at(10.0, 1, width), 0);
    }

    #[test]
    fn a_click_picks_even_when_press_and_release_come_before_a_redraw() {
        // Both arrive against the drag as the events left it, not as the last frame drew it.
        let (drag, picked) = follow(None, Gesture::Press(4)).unwrap();
        assert_eq!((drag, picked), (Some(4), None));
        assert_eq!(follow(drag, Gesture::Release(4)), Some((None, Some(4))));
        // A drag follows the pointer and picks where it is let go.
        assert_eq!(follow(Some(4), Gesture::Move { stop: 2, held: true }), Some((Some(2), None)));
        assert_eq!(follow(Some(2), Gesture::Release(2)), Some((None, Some(2))));
    }

    #[test]
    fn moves_and_releases_without_a_drag_change_nothing_and_a_missed_release_picks_nothing() {
        assert_eq!(follow(None, Gesture::Move { stop: 1, held: true }), None);
        assert_eq!(follow(None, Gesture::Release(1)), None);
        assert_eq!(follow(Some(3), Gesture::Move { stop: 1, held: false }), Some((None, None)));
    }

    #[test]
    fn narrow_tracks_never_overflow_filled_bounds() {
        for width in [0.0, 4.0, 10.0, 18.0, 24.0, 240.0] {
            for count in [1, 2, 6] {
                for ratio in [0.0, 0.5, 1.0] {
                    let (_, filled) = track_geometry(width, count, ratio);
                    assert!(filled >= 0.0 && filled <= width + 1e-4, "width={width} count={count} ratio={ratio} filled={filled}");
                }
            }
        }
    }

    #[test]
    fn sample_fill_interpolates_smoothly_across_stops() {
        let stops = [hsla(0.5, 0.8, 0.5, 1.0), hsla(0.7, 0.8, 0.6, 1.0)];
        assert_eq!(sample_fill(&stops, 0.0), stops[0]);
        assert_eq!(sample_fill(&stops, 1.0), stops[1]);
        let mid = sample_fill(&stops, 0.5);
        assert!((mid.h - 0.6).abs() < 1e-5 && (mid.l - 0.55).abs() < 1e-5);
    }
}
