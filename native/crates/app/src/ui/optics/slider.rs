//! The reasoning slider of the model card: the levels a model offers as stops on a track that fills from the coolest
//! level's color to the chosen one's, with stars twinkling in the filled part. A click or a drag picks a level; the
//! level is sent when the button is released ([`Gesture`]). The stars move only while the card is open, the window is in front and
//! Windows' animation effects are on, at most ten times a second.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gpui_kit::*;

use super::backdrop::motion_allowed;

pub const TRACK_HEIGHT: f32 = 22.0;
const THUMB: f32 = 18.0;
const RADIUS: f32 = 6.0;
const STARS: u32 = 24;
const TWINKLE: Duration = Duration::from_millis(100);

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

/// The stop nearest to `x`, measured from the track's left edge.
pub fn stop_at(x: f32, count: usize, width: f32) -> usize {
    (0..count).min_by(|a, b| (stop_x(*a, count, width) - x).abs().total_cmp(&(stop_x(*b, count, width) - x).abs())).unwrap_or(0)
}

/// The track for `fills` (one color per level), showing level `shown`.
pub fn render(fills: Vec<Hsla>, shown: usize, dark: bool, on_gesture: OnGesture) -> impl IntoElement {
    div().w_full().h(px(TRACK_HEIGHT)).cursor_pointer().child(
        canvas(
            |_, _, _| (),
            move |bounds, (), window, cx| {
                paint(bounds, &fills, shown, dark, window);
                listen(bounds, fills.len(), on_gesture, window);
                if motion_allowed() && window.is_window_active() {
                    twinkle_later(window, cx);
                }
            },
        )
        .size_full(),
    )
}

fn seconds() -> f32 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f32()
}

/// Redraw once more after [`TWINKLE`], unless a redraw is already on its way.
fn twinkle_later(window: &mut Window, cx: &App) {
    thread_local! {
        static PENDING: Cell<bool> = const { Cell::new(false) };
    }
    if PENDING.with(Cell::get) {
        return;
    }
    PENDING.with(|pending| pending.set(true));
    window
        .spawn(cx, async move |cx| {
            cx.background_executor().timer(TWINKLE).await;
            PENDING.with(|pending| pending.set(false));
            let _ = cx.update(|window, _| window.refresh());
        })
        .detach();
}

fn hash(index: u32, salt: u32) -> f32 {
    let mut h = index.wrapping_mul(0x9e37_79b9) ^ salt.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// A rounded rectangle without a border.
fn rounded(bounds: Bounds<Pixels>, radius: f32, background: impl Into<Background>) -> PaintQuad {
    quad(bounds, Corners::all(px(radius)), background, Edges::default(), transparent_black(), BorderStyle::default())
}

fn paint(bounds: Bounds<Pixels>, fills: &[Hsla], shown: usize, dark: bool, window: &mut Window) {
    let count = fills.len();
    if count == 0 {
        return;
    }
    let shown = shown.min(count - 1);
    let (left, top) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let white = hsla(0.0, 0.0, 1.0, 1.0);
    let track = if dark { white.opacity(0.08) } else { hsla(0.0, 0.0, 0.0, 0.06) };
    window.paint_quad(rounded(bounds, RADIUS, track));

    let thumb_x = left + stop_x(shown, count, width);
    let filled = Bounds::new(bounds.origin, size(px(thumb_x - left + THUMB / 2.0), bounds.size.height));
    let gradient = linear_gradient(90.0, linear_color_stop(fills[0], 0.0), linear_color_stop(fills[shown], 1.0));
    window.paint_quad(rounded(filled, RADIUS, gradient));

    // The stops not reached yet.
    for index in shown + 1..count {
        let x = left + stop_x(index, count, width);
        let dot = Bounds::new(point(px(x - 2.0), px(top + height / 2.0 - 2.0)), size(px(4.0), px(4.0)));
        window.paint_quad(rounded(dot, 2.0, if dark { white.opacity(0.28) } else { hsla(0.0, 0.0, 0.0, 0.22) }));
    }

    // Stars in the filled part, each twinkling at its own pace.
    let time = if motion_allowed() { seconds() } else { 0.0 };
    for star in 0..STARS {
        let x = left + 4.0 + hash(star, 1) * (width - 8.0);
        if x > thumb_x - THUMB / 2.0 - 2.0 {
            continue;
        }
        let y = top + 4.0 + hash(star, 2) * (height - 8.0);
        let side = 1.4 + hash(star, 3) * 1.4;
        let pace = 1.2 + hash(star, 4) * 2.6;
        let glow = 0.25 + 0.75 * (0.5 + 0.5 * (time * pace + hash(star, 5) * 6.283).sin());
        let dot = Bounds::new(point(px(x - side / 2.0), px(y - side / 2.0)), size(px(side), px(side)));
        window.paint_quad(rounded(dot, side / 2.0, white.opacity(glow * 0.95)));
    }

    let thumb = Bounds::new(point(px(thumb_x - THUMB / 2.0), px(top + (height - THUMB) / 2.0)), size(px(THUMB), px(THUMB)));
    window.paint_drop_shadows(thumb, Corners::all(px(THUMB / 2.0)), &[BoxShadow::new(px(0.), px(1.), hsla(0.0, 0.0, 0.0, 0.30)).blur_radius(px(3.))]);
    window.paint_quad(quad(thumb, Corners::all(px(THUMB / 2.0)), white, Edges::all(px(2.)), fills[shown], BorderStyle::default()));
}

fn listen(bounds: Bounds<Pixels>, count: usize, on_gesture: OnGesture, window: &mut Window) {
    let width = f32::from(bounds.size.width);
    let at = move |position: Point<Pixels>| stop_at(f32::from(position.x - bounds.origin.x), count, width);
    let pressed = on_gesture.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase.bubble() && event.button == MouseButton::Left && bounds.contains(&event.position) {
            cx.stop_propagation();
            pressed(Gesture::Press(at(event.position)), window, cx);
        }
    });
    let moved = on_gesture.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase.bubble() {
            moved(Gesture::Move { stop: at(event.position), held: event.dragging() }, window, cx);
        }
    });
    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
        if phase.bubble() && event.button == MouseButton::Left {
            on_gesture(Gesture::Release(at(event.position)), window, cx);
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
}
