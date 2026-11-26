//! The field of characters behind a new chat: faint letters in drifting clouds that change one by one, brighten
//! around the pointer and ripple where it clicks.
//!
//! Nothing is stored per character: a cell's letter, brightness and sparkle come from hashing its place and the
//! clock, so a resized window needs no work and a frame is one pass over the cells that are bright enough to draw.
//! The clock only runs while it is worth watching: at most [`FPS`] frames a second, while the window is in front,
//! the page is shown and the window came to the front, the page appeared or the pointer moved over it in the last
//! [`IDLE`]; never when Windows' animation effects are off. A repaint for anything else wakes nothing, so paused, it
//! costs nothing.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use super::look::{GLYPH_GAIN, Look};

/// Frames a second while the field moves.
pub const FPS: u32 = 10;
const FRAME: Duration = Duration::from_millis(1000 / FPS as u64);
/// The field stops this long after the pointer last moved over the page.
pub const IDLE: Duration = Duration::from_secs(45);
/// A page not painted for this long is no longer shown, and the clock stops.
const UNSEEN: Duration = Duration::from_millis(600);
const FONT_SIZE: f32 = 12.0;
const LINE: f32 = 17.0;
const CHARSET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
/// The half-width of the column of content around the middle, where characters are fainter, and its soft edge.
const COLUMN_MARGIN: f32 = 28.0;
const COLUMN_EDGE: f32 = 72.0;
const POINTER_RADIUS: f32 = 130.0;
const RIPPLE_SPEED: f32 = 300.0;
const RIPPLE_LIFE: f32 = 1.7;
const RIPPLE_WIDTH: f32 = 30.0;
const MAX_RIPPLES: usize = 3;
/// Below this alpha a character is not drawn.
const INVISIBLE: f32 = 0.012;

/// Whether things may move: Windows' "Animation effects" (Settings → Accessibility → Visual effects), read at most
/// once a second. `RAINY_REDUCED_MOTION=1` acts as if they were off.
pub fn motion_allowed() -> bool {
    thread_local! {
        static READ: Cell<Option<(Instant, bool)>> = const { Cell::new(None) };
    }
    READ.with(|read| {
        if let Some((at, allowed)) = read.get()
            && at.elapsed() < Duration::from_secs(1)
        {
            return allowed;
        }
        let allowed = std::env::var("RAINY_REDUCED_MOTION").map_or(true, |value| value != "1") && system_animations();
        read.set(Some((Instant::now(), allowed)));
        allowed
    })
}

#[cfg(windows)]
fn system_animations() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW};
    let mut enabled: i32 = 1;
    // SAFETY: the call writes one BOOL into `enabled`, which lives across it.
    let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&mut enabled as *mut i32).cast(), 0) };
    ok == 0 || enabled != 0
}

#[cfg(not(windows))]
fn system_animations() -> bool {
    true
}

/// The glyphs of [`CHARSET`] in the code font, shaped once per font.
struct Glyphs {
    family: SharedString,
    ids: Vec<(FontId, GlyphId)>,
    advance: f32,
    ascent: f32,
}

impl Glyphs {
    fn shape(family: SharedString, window: &Window) -> Option<Self> {
        let run = TextRun { len: CHARSET.len(), font: font(family.clone()), color: gpui_kit::black(), background_color: None, underline: None, strikethrough: None };
        let layout = window.text_system().layout_line(CHARSET, px(FONT_SIZE), &[run], None);
        let mut ids = vec![None; CHARSET.len()];
        for run in &layout.runs {
            for glyph in &run.glyphs {
                if let Some(slot) = ids.get_mut(glyph.index) {
                    *slot = Some((run.font_id, glyph.id));
                }
            }
        }
        let ids: Vec<(FontId, GlyphId)> = ids.into_iter().flatten().collect();
        let advance = f32::from(layout.width) / CHARSET.len() as f32;
        (!ids.is_empty() && advance > 1.0).then(|| Self { family, ids, advance, ascent: f32::from(layout.ascent) })
    }
}

/// The page's clock and what the pointer did; one per chat pane, drawn only on a new chat in Agent mode.
pub struct Backdrop {
    /// Seconds the field has moved; it stands still while paused, so it never jumps.
    clock: f32,
    last_tick: Option<Instant>,
    awake_until: Option<Instant>,
    pointer: Option<Point<Pixels>>,
    ripples: Vec<(Point<Pixels>, f32)>,
    /// When the pet was last pointed at, on the clock, and whether the pointer is on her now.
    poked: f32,
    pet_inside: bool,
    painted: Rc<Cell<Option<Instant>>>,
    glyphs: Option<Rc<Glyphs>>,
    ticker: Option<Task<()>>,
    _activation: Subscription,
}

impl Backdrop {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.wake(window, cx);
            }
        });
        Self {
            clock: 0.0,
            last_tick: None,
            awake_until: None,
            pointer: None,
            ripples: Vec::new(),
            poked: f32::NEG_INFINITY,
            pet_inside: false,
            painted: Rc::default(),
            glyphs: None,
            ticker: None,
            _activation: activation,
        }
    }

    pub fn clock(&self) -> f32 {
        self.clock
    }

    pub fn poked(&self) -> f32 {
        self.poked
    }

    pub fn pointer(&self) -> Option<Point<Pixels>> {
        self.pointer
    }

    /// Whether the page's clock is paused for now (the window is in the background or nobody moved the pointer for a
    /// while): the pet sleeps then.
    pub fn dozing(&self, window: &Window) -> bool {
        !(window.is_window_active() && self.awake_until.is_some_and(|until| Instant::now() < until))
    }

    /// Keep the field moving for another [`IDLE`].
    pub fn wake(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.awake_until = Some(Instant::now() + IDLE);
        self.ensure_ticking(window, cx);
    }

    pub fn pointer_moved(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if !motion_allowed() {
            return;
        }
        self.pointer = Some(position);
        self.wake(window, cx);
    }

    pub fn pointer_left(&mut self) {
        self.pointer = None;
    }

    pub fn ripple(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if !motion_allowed() {
            return;
        }
        if self.ripples.len() >= MAX_RIPPLES {
            self.ripples.remove(0);
        }
        self.ripples.push((position, self.clock));
        self.wake(window, cx);
    }

    /// The pointer moved on or off the pet; coming onto her, she hops (once in a while, not on every move).
    pub fn pet_hovered(&mut self, inside: bool, window: &mut Window, cx: &mut Context<Self>) {
        if inside && !self.pet_inside && motion_allowed() && self.clock - self.poked >= 1.2 {
            self.poked = self.clock;
            self.wake(window, cx);
        }
        self.pet_inside = inside;
    }

    fn should_tick(&self, window: &Window) -> bool {
        let shown = self.painted.get().is_some_and(|at| at.elapsed() < UNSEEN);
        let awake = self.awake_until.is_some_and(|until| Instant::now() < until);
        tick_rule(motion_allowed(), window.is_window_active(), shown, awake)
    }

    fn ensure_ticking(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ticker.is_some() || !tick_rule(motion_allowed(), window.is_window_active(), true, self.awake_until.is_some_and(|until| Instant::now() < until)) {
            return;
        }
        self.last_tick = Some(Instant::now());
        self.ticker = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(FRAME).await;
                let going = this
                    .update_in(cx, |this, window, cx| {
                        if !this.should_tick(window) {
                            this.ticker = None;
                            this.last_tick = None;
                            // Once more, so the pet is drawn asleep now rather than at the next unrelated repaint.
                            cx.notify();
                            return false;
                        }
                        let now = Instant::now();
                        let elapsed = this.last_tick.map_or(0.0, |last| now.duration_since(last).as_secs_f32());
                        this.last_tick = Some(now);
                        // A late frame (a busy interface thread) moves the field one frame's worth, not a jump.
                        this.clock += elapsed.min(2.0 / FPS as f32);
                        let clock = this.clock;
                        this.ripples.retain(|(_, start)| clock - start < RIPPLE_LIFE);
                        cx.notify();
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

/// The clock runs only when everything says so.
fn tick_rule(motion: bool, window_active: bool, shown: bool, awake: bool) -> bool {
    motion && window_active && shown && awake
}

impl Render for Backdrop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (family, look) = {
            let theme = cx.theme();
            (theme.mono_font_family.clone(), Look::of(theme))
        };
        if self.glyphs.as_ref().is_none_or(|glyphs| glyphs.family != family) {
            self.glyphs = Glyphs::shape(family, window).map(Rc::new);
        }
        let field = Field {
            look,
            clock: if motion_allowed() { self.clock } else { 0.0 },
            pointer: self.pointer.filter(|_| motion_allowed()),
            ripples: self.ripples.clone(),
            column: crate::ui::agent::COLUMN,
        };
        let (glyphs, painted) = (self.glyphs.clone(), self.painted.clone());
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                painted.set(Some(Instant::now()));
                if let Some(glyphs) = glyphs {
                    // One layer for the whole field: its thousands of glyphs share one place in the frame's order
                    // instead of each being sorted in on its own, which took most of the field's time (a frame of
                    // the field went from about 2 ms to 0.3 ms).
                    window.with_content_mask(Some(ContentMask { bounds }), |window| window.paint_layer(bounds, |window| field.paint(bounds, &glyphs, window)));
                }
            },
        )
        .size_full()
    }
}

/// One frame of the field.
struct Field {
    look: Look,
    clock: f32,
    pointer: Option<Point<Pixels>>,
    ripples: Vec<(Point<Pixels>, f32)>,
    column: f32,
}

impl Field {
    fn paint(&self, bounds: Bounds<Pixels>, glyphs: &Glyphs, window: &mut Window) {
        let (left, top) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let cell = glyphs.advance;
        let (columns, rows) = ((width / cell).ceil() as i32, (height / LINE).ceil() as i32);
        let middle = left + width / 2.0;
        let half_column = self.column / 2.0 + COLUMN_MARGIN;
        let baseline = (LINE - FONT_SIZE) / 2.0 + glyphs.ascent;
        let pointer = self.pointer.map(|point| (f32::from(point.x), f32::from(point.y)));
        let ripples: Vec<(f32, f32, f32)> = self
            .ripples
            .iter()
            .filter_map(|(origin, start)| {
                let age = self.clock - start;
                (0.0..RIPPLE_LIFE).contains(&age).then(|| (f32::from(origin.x), f32::from(origin.y), age))
            })
            .collect();
        for row in 0..rows {
            let y = top + row as f32 * LINE;
            for column in 0..columns {
                let x = left + column as f32 * cell;
                let center = (x + cell / 2.0, y + LINE / 2.0);
                let inside = smoothstep(0.0, COLUMN_EDGE, (center.0 - middle).abs() - half_column);
                let zone = self.look.glyph_alpha_column + (self.look.glyph_alpha - self.look.glyph_alpha_column) * inside;
                let (alpha, spark, scramble) = cell_light(column, row, center, self.clock, pointer, &ripples);
                let alpha = (zone * alpha).min(zone * GLYPH_GAIN);
                if alpha < INVISIBLE {
                    continue;
                }
                let index = letter(column, row, self.clock, scramble) % glyphs.ids.len();
                let (font_id, glyph_id) = glyphs.ids[index];
                let color = if spark { self.look.glyph_spark } else { self.look.glyph };
                let _ = window.paint_glyph(point(px(x), px(y + baseline)), font_id, glyph_id, px(FONT_SIZE), color.opacity(alpha));
            }
        }
    }
}

/// How bright a cell is, before its place's alpha: the clouds, a sparkle, the pointer's glow and ripples. Returns the
/// brightness (up to [`GLYPH_GAIN`]), whether it shows in the spark color and whether a ripple shuffles its letter.
fn cell_light(column: i32, row: i32, (x, y): (f32, f32), clock: f32, pointer: Option<(f32, f32)>, ripples: &[(f32, f32, f32)]) -> (f32, bool, bool) {
    let mut light = smoothstep(0.40, 0.86, clouds(x, y, clock));
    let mut spark = sparkles(column, row, clock);
    if spark {
        light = (light.max(0.5)) * GLYPH_GAIN;
    }
    if let Some((px_, py)) = pointer {
        let distance = ((x - px_).powi(2) + (y - py).powi(2)).sqrt();
        if distance < POINTER_RADIUS {
            let near = (1.0 - distance / POINTER_RADIUS).powi(2);
            light = light.max(0.35) * (1.0 + near);
            spark |= near > 0.45;
        }
    }
    let mut scramble = false;
    for (ox, oy, age) in ripples {
        let distance = ((x - ox).powi(2) + (y - oy).powi(2)).sqrt();
        let ring = (-((distance - RIPPLE_SPEED * age) / RIPPLE_WIDTH).powi(2)).exp() * (1.0 - age / RIPPLE_LIFE);
        if ring > 0.05 {
            light += 1.2 * ring;
            spark |= ring > 0.5;
            scramble |= ring > 0.35;
        }
    }
    (light.min(GLYPH_GAIN), spark, scramble)
}

/// About one cell in a hundred shines in the spark color for a few seconds at a time.
fn sparkles(column: i32, row: i32, clock: f32) -> bool {
    let epoch = (clock * 0.15 + unit(hash(column, row, 9))) as u32;
    hash(column, row, epoch.wrapping_mul(31).wrapping_add(5)) % 101 == 0
}

/// The letter a cell shows: each cell changes every 4 to 20 seconds, at its own pace.
fn letter(column: i32, row: i32, clock: f32, scramble: bool) -> usize {
    let rate = 0.05 + unit(hash(column, row, 7)) * 0.2;
    let epoch = (clock * rate + unit(hash(column, row, 8))) as u32 + u32::from(scramble) * 977;
    hash(column, row, epoch.wrapping_add(11)) as usize
}

/// Two layers of value noise drifting slowly in opposite directions: the clouds, 0 to 1.
fn clouds(x: f32, y: f32, clock: f32) -> f32 {
    0.65 * noise(x / 260.0 + clock * 0.020, y / 200.0 + clock * 0.006, 1) + 0.35 * noise(x / 95.0 - clock * 0.045, y / 80.0 + clock * 0.012, 2)
}

fn noise(x: f32, y: f32, seed: u32) -> f32 {
    let (fx, fy) = (x.floor(), y.floor());
    let (tx, ty) = (x - fx, y - fy);
    let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
    let (x0, y0) = (fx as i32, fy as i32);
    let corner = |dx: i32, dy: i32| unit(hash(x0.wrapping_add(dx), y0.wrapping_add(dy), seed));
    let top = corner(0, 0) + (corner(1, 0) - corner(0, 0)) * sx;
    let bottom = corner(0, 1) + (corner(1, 1) - corner(0, 1)) * sx;
    top + (bottom - top) * sy
}

fn hash(x: i32, y: i32, z: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x27d4_eb2d) ^ (y as u32).wrapping_mul(0x1656_67b1) ^ z.wrapping_mul(0x9e37_79b9);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^ (h >> 16)
}

/// A hash as a number in [0, 1).
fn unit(hash: u32) -> f32 {
    (hash >> 8) as f32 / (1u32 << 24) as f32
}

fn smoothstep(from: f32, to: f32, value: f32) -> f32 {
    let t = ((value - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn the_clock_runs_only_when_everything_says_so() {
        assert!(tick_rule(true, true, true, true));
        for (motion, active, shown, awake) in [(false, true, true, true), (true, false, true, true), (true, true, false, true), (true, true, true, false)] {
            assert!(!tick_rule(motion, active, shown, awake));
        }
    }

    #[test]
    fn the_field_is_the_same_for_the_same_clock_and_changes_slowly() {
        let sample = |clock: f32| (0..400).map(|i| letter(i % 40, i / 40, clock, false)).collect::<Vec<_>>();
        assert_eq!(sample(3.0), sample(3.0));
        // A tenth of a second later at most a few letters in 400 changed; a minute later most did.
        let changed = |a: &[usize], b: &[usize]| a.iter().zip(b).filter(|(a, b)| a != b).count();
        assert!(changed(&sample(3.0), &sample(3.1)) <= 12, "{}", changed(&sample(3.0), &sample(3.1)));
        assert!(changed(&sample(3.0), &sample(63.0)) > 300);
    }

    #[test]
    fn clouds_cover_part_of_the_page_and_stay_in_range() {
        let mut lit = 0;
        for i in 0..10_000 {
            let (x, y) = ((i % 100) as f32 * 7.3, (i / 100) as f32 * 17.0);
            let value = clouds(x, y, 12.5);
            assert!((0.0..=1.0).contains(&value));
            let (light, ..) = cell_light(i % 100, i / 100, (x, y), 12.5, None, &[]);
            assert!(light <= GLYPH_GAIN);
            lit += usize::from(light > 0.06);
        }
        // Clouds, not a wall of text and not an empty page.
        assert!((2_000..8_000).contains(&lit), "{lit}");
    }

    #[test]
    fn a_ripple_brightens_a_ring_that_moves_out() {
        // A dim cell: the ring brightens it while it passes and leaves it as it was once it has.
        let (cell, x) = ((3, 4), RIPPLE_SPEED * 0.5);
        let dim = (0..200).map(|i| i as f32 * 0.37).find(|clock| cell_light(cell.0, cell.1, (x, 0.0), *clock, None, &[]).0 < 0.5).unwrap();
        let quiet = cell_light(cell.0, cell.1, (x, 0.0), dim, None, &[]).0;
        let passing = cell_light(cell.0, cell.1, (x, 0.0), dim, None, &[(0.0, 0.0, 0.5)]).0;
        let passed = cell_light(cell.0, cell.1, (x, 0.0), dim, None, &[(0.0, 0.0, 1.5)]).0;
        assert!(passing > quiet + 0.3, "{quiet} {passing}");
        assert!((passed - quiet).abs() < 0.05, "{quiet} {passed}");
    }
}
