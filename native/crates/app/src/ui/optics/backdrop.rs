//! The field of characters behind a new chat: faint letters in drifting clouds that change one by one, brighten
//! around the pointer with a soft comet trail and ripple where it clicks.
//!
//! Nothing is stored per character: a cell's letter, brightness and sparkle come from hashing its place and the
//! clock, so a resized window needs no work and a frame is one pass over the visible cells.
//! The clock only runs while it is worth watching: [`FPS`] frames a second for calm drifting clouds, temporarily
//! stepping up to [`BURST_FPS`] during pointer motion, ripples or pet interactions, then settling back and stopping
//! completely after [`IDLE`] (0 frames / 0 CPU at rest); never when reduced motion is active.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use super::look::{GLYPH_GAIN, Look};
use super::{GLOW_BREATH_MAX, GLOW_BREATH_MIN, GLOW_PARALLAX_MAX};

/// Frames a second while the field calmly drifts.
pub const FPS: u32 = 10;
/// Frames a second during short interactive bursts (pointer motion, ripples, pet hop).
pub const BURST_FPS: u32 = 20;
const FRAME: Duration = Duration::from_millis(1000 / FPS as u64);
const BURST_FRAME: Duration = Duration::from_millis(1000 / BURST_FPS as u64);
/// How long an interactive burst keeps [`BURST_FPS`] before easing back to [`FPS`].
const BURST_HOLD: Duration = Duration::from_millis(1500);
/// The field stops this long after the last interaction.
pub const IDLE: Duration = Duration::from_secs(30);
/// A page not painted for this long is no longer shown, and the clock stops.
const UNSEEN: Duration = Duration::from_millis(600);
const FONT_SIZE: f32 = 12.0;
const LINE: f32 = 17.0;
const CHARSET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
/// The half-width of the column of content around the middle, where characters are fainter, and its soft edge.
const COLUMN_MARGIN: f32 = 28.0;
const COLUMN_EDGE: f32 = 72.0;
const POINTER_RADIUS: f32 = 130.0;
const POINTER_RADIUS_SQ: f32 = POINTER_RADIUS * POINTER_RADIUS;
const TRAIL_CAP: usize = 6;
const TRAIL_LIFE: f32 = 0.45;
const TRAIL_RADIUS: f32 = 76.0;
const TRAIL_RADIUS_SQ: f32 = TRAIL_RADIUS * TRAIL_RADIUS;
const TRAIL_MIN_DIST_SQ: f32 = 16.0 * 16.0;
const RIPPLE_SPEED: f32 = 300.0;
const RIPPLE_LIFE: f32 = 1.7;
const RIPPLE_WIDTH: f32 = 30.0;
const RIPPLE_ECHO_LAG: f32 = 0.24;
const RIPPLE_ECHO_GAIN: f32 = 0.35;
const MAX_RIPPLES: usize = 3;
/// Below this alpha a character is not drawn.
const INVISIBLE: f32 = 0.012;
/// How long typing keeps the pet attentively looking down at the composer.
pub const TYPING_HOLD: f32 = 2.0;
/// How long a pat/click hop lasts on the page clock.
pub const PAT_HOLD: f32 = 1.2;
/// How long an excited triple-click bounce lasts, and its cooldown.
pub const EXCITED_HOLD: f32 = 1.5;
const EXCITED_COOLDOWN: f32 = 4.0;
/// How long an AI-completion celebration lasts on the clock.
pub const CELEBRATE_HOLD: f32 = 1.8;

/// High-level AI state mirrored to the pixel pet when optics are enabled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PetActivity {
    #[default]
    Idle,
    Thinking,
    Responding,
    WaitingApproval,
}

/// Whether things may move: shares the single reduced-motion source of truth in [`crate::ui::motion`].
pub fn motion_allowed() -> bool {
    crate::ui::motion::allowed_without_app()
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

#[derive(Clone, Copy, Debug)]
struct Ripple {
    origin: Point<Pixels>,
    start: f64,
}

#[derive(Clone, Copy, Debug)]
struct TrailPoint {
    origin: Point<Pixels>,
    at: f64,
}

/// The page's clock and what the pointer did; one per chat pane, drawn only on a new chat in Agent mode.
pub struct Backdrop {
    /// Seconds the field has moved (`f64` avoids long-session float drift); stands still while paused.
    clock: f64,
    last_tick: Option<Instant>,
    awake_until: Option<Instant>,
    /// Grace window after [`wake`] so the first tick never aborts before the wake-triggered paint lands.
    woken_at: Option<Instant>,
    /// Short high-rate window (`BURST_FPS`) for ripples, pointer trail and pet hops.
    burst_until: Option<Instant>,
    pointer: Option<Point<Pixels>>,
    trail: [Option<TrailPoint>; TRAIL_CAP],
    trail_head: usize,
    ripples: [Option<Ripple>; MAX_RIPPLES],
    /// Smoothed horizontal parallax offset (`[-GLOW_PARALLAX_MAX, GLOW_PARALLAX_MAX]`) for [`super::glow_for`].
    parallax_x: f32,
    parallax_target: f32,
    /// Horizontal span `(left, width)` of the last painted canvas, used to normalize pointer parallax.
    painted_span: Rc<Cell<(f32, f32)>>,
    /// When the pet was last pointed at, on the clock, and whether the pointer is on her now.
    poked: f64,
    pet_inside: bool,
    /// Pet interaction & AI state timestamps on the same clock.
    last_typed: f64,
    patted: f64,
    pat_streak: u8,
    excited_at: f64,
    excited_cooldown_until: f64,
    ai_activity: PetActivity,
    celebrated_at: f64,
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
            woken_at: None,
            burst_until: None,
            pointer: None,
            trail: [None; TRAIL_CAP],
            trail_head: 0,
            ripples: [None; MAX_RIPPLES],
            parallax_x: 0.0,
            parallax_target: 0.0,
            painted_span: Rc::new(Cell::new((0.0, 0.0))),
            poked: f64::NEG_INFINITY,
            pet_inside: false,
            last_typed: f64::NEG_INFINITY,
            patted: f64::NEG_INFINITY,
            pat_streak: 0,
            excited_at: f64::NEG_INFINITY,
            excited_cooldown_until: f64::NEG_INFINITY,
            ai_activity: PetActivity::Idle,
            celebrated_at: f64::NEG_INFINITY,
            painted: Rc::default(),
            glyphs: None,
            ticker: None,
            _activation: activation,
        }
    }

    pub fn clock(&self) -> f32 {
        self.clock as f32
    }

    #[allow(dead_code)]
    pub fn clock_f64(&self) -> f64 {
        self.clock
    }

    pub fn poked(&self) -> f32 {
        self.poked as f32
    }

    pub fn pointer(&self) -> Option<Point<Pixels>> {
        self.pointer
    }

    pub fn pet_inside(&self) -> bool {
        self.pet_inside
    }

    pub fn ai_activity(&self) -> PetActivity {
        self.ai_activity
    }

    /// Seconds since the user last typed in the composer, while within [`TYPING_HOLD`].
    pub fn typing_age(&self) -> Option<f32> {
        let age = (self.clock - self.last_typed) as f32;
        (0.0..TYPING_HOLD).contains(&age).then_some(age)
    }

    /// Seconds since the user clicked the pet, while within [`PAT_HOLD`].
    pub fn pat_age(&self) -> Option<f32> {
        let age = (self.clock - self.patted) as f32;
        (0.0..PAT_HOLD).contains(&age).then_some(age)
    }

    /// Seconds since the triple-click excited bounce began, while within [`EXCITED_HOLD`].
    pub fn excited_age(&self) -> Option<f32> {
        let age = (self.clock - self.excited_at) as f32;
        (0.0..EXCITED_HOLD).contains(&age).then_some(age)
    }

    /// Seconds since AI completion celebration began, while within [`CELEBRATE_HOLD`].
    pub fn celebrate_age(&self) -> Option<f32> {
        let age = (self.clock - self.celebrated_at) as f32;
        (0.0..CELEBRATE_HOLD).contains(&age).then_some(age)
    }

    /// Downward alpha breathing factor (`0.88..=1.0`) for the composer glow; static `1.0` when reduced-motion or dozing.
    pub fn glow_breath(&self, window: &Window) -> f32 {
        if !motion_allowed() || self.dozing(window) {
            return GLOW_BREATH_MAX;
        }
        glow_breath_at(self.clock)
    }

    /// Smoothed horizontal pointer parallax offset (`±18px`) for the composer glow; `0.0` when reduced-motion.
    pub fn glow_parallax_x(&self) -> f32 {
        if motion_allowed() { self.parallax_x } else { 0.0 }
    }

    /// Whether the page's clock is paused for now (the window is in the background or nobody moved the pointer for a
    /// while): the pet sleeps then.
    pub fn dozing(&self, window: &Window) -> bool {
        !(window.is_window_active() && self.awake_until.is_some_and(|until| Instant::now() < until))
    }

    /// Keep the field moving for another [`IDLE`]. Notifies immediately so `painted` refreshes before the first tick.
    pub fn wake(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.awake_until = Some(now + IDLE);
        self.woken_at = Some(now);
        cx.notify();
        self.ensure_ticking(window, cx);
    }

    fn note_burst(&mut self) {
        self.burst_until = Some(Instant::now() + BURST_HOLD);
    }

    pub fn pointer_moved(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if !motion_allowed() {
            return;
        }
        self.pointer = Some(position);
        self.push_trail(position);
        let (left, width) = self.painted_span.get();
        if width > 1.0 {
            let norm = (((f32::from(position.x) - left) / width) * 2.0 - 1.0).clamp(-1.0, 1.0);
            self.parallax_target = norm * GLOW_PARALLAX_MAX;
        }
        self.note_burst();
        self.wake(window, cx);
    }

    fn push_trail(&mut self, position: Point<Pixels>) {
        let prev = self.trail[self.trail_head];
        let should_push = match prev {
            None => true,
            Some(last) => {
                let dx = f32::from(position.x - last.origin.x);
                let dy = f32::from(position.y - last.origin.y);
                dx * dx + dy * dy >= TRAIL_MIN_DIST_SQ || (self.clock - last.at) >= 0.06
            }
        };
        if should_push {
            self.trail_head = (self.trail_head + 1) % TRAIL_CAP;
            self.trail[self.trail_head] = Some(TrailPoint { origin: position, at: self.clock });
        }
    }

    /// Clears the live pointer & trail when the pointer leaves the page (kept signature-compatible with `chat.rs`).
    pub fn pointer_left(&mut self) {
        self.pointer = None;
        self.trail = [None; TRAIL_CAP];
        self.parallax_target = 0.0;
    }

    /// Clears the pointer and immediately schedules a repaint so no highlight lingers after leaving the window.
    pub fn pointer_left_notify(&mut self, cx: &mut Context<Self>) {
        let had = self.pointer.is_some() || self.parallax_x.abs() > 0.1;
        self.pointer_left();
        if had {
            cx.notify();
        }
    }

    /// Clears transient interaction artifacts (pointer, trail, ripples, parallax) when leaving the blank greeting page.
    pub fn reset_ephemeral(&mut self) {
        self.pointer = None;
        self.trail = [None; TRAIL_CAP];
        self.ripples = [None; MAX_RIPPLES];
        self.pet_inside = false;
        self.parallax_x = 0.0;
        self.parallax_target = 0.0;
        self.burst_until = None;
        self.woken_at = None;
    }

    pub fn ripple(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if !motion_allowed() {
            return;
        }
        // Reuse an empty slot, else overwrite the oldest ripple (zero heap allocation).
        let slot = self
            .ripples
            .iter()
            .position(Option::is_none)
            .unwrap_or_else(|| {
                self.ripples
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| a.map_or(f64::INFINITY, |r| r.start).total_cmp(&b.map_or(f64::INFINITY, |r| r.start)))
                    .map_or(0, |(idx, _)| idx)
            });
        self.ripples[slot] = Some(Ripple { origin: position, start: self.clock });
        self.note_burst();
        self.wake(window, cx);
    }

    /// The pointer moved on or off the pet; coming onto her, she hops (once in a while, not on every move).
    pub fn pet_hovered(&mut self, inside: bool, window: &mut Window, cx: &mut Context<Self>) {
        if inside && !self.pet_inside && motion_allowed() && self.clock - self.poked >= 1.2 {
            self.poked = self.clock;
            self.note_burst();
            self.wake(window, cx);
        }
        self.pet_inside = inside;
    }

    /// Left-clicking the pet pats her: a lively hop with a heart; 3 rapid pats trigger an excited double-hop easter egg.
    pub fn pet_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !motion_allowed() {
            return;
        }
        let now = self.clock;
        if now - self.patted <= 3.0 {
            self.pat_streak = self.pat_streak.saturating_add(1);
        } else {
            self.pat_streak = 1;
        }
        self.patted = now;
        if self.pat_streak >= 3 && now >= self.excited_cooldown_until {
            self.excited_at = now;
            self.excited_cooldown_until = now + EXCITED_HOLD as f64 + EXCITED_COOLDOWN as f64;
            self.pat_streak = 0;
        }
        self.note_burst();
        self.wake(window, cx);
    }

    /// Notes composer input so the pet pauses her stroll and attentively looks toward what is being typed.
    pub fn notify_typed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !motion_allowed() {
            return;
        }
        self.last_typed = self.clock;
        self.note_burst();
        self.wake(window, cx);
    }

    /// Updates AI turn status; finishing a running turn automatically triggers a short celebration.
    #[allow(dead_code)]
    pub fn set_ai_activity(&mut self, activity: PetActivity, window: &mut Window, cx: &mut Context<Self>) {
        let was_active = matches!(self.ai_activity, PetActivity::Thinking | PetActivity::Responding);
        self.ai_activity = activity;
        if was_active && activity == PetActivity::Idle && motion_allowed() {
            self.celebrated_at = self.clock;
            self.note_burst();
            self.wake(window, cx);
        } else if matches!(activity, PetActivity::Thinking | PetActivity::Responding) && motion_allowed() {
            self.wake(window, cx);
        } else {
            cx.notify();
        }
    }

    /// Explicitly triggers a short celebration hop on the pet.
    #[allow(dead_code)]
    pub fn trigger_celebrate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !motion_allowed() {
            return;
        }
        self.celebrated_at = self.clock;
        self.note_burst();
        self.wake(window, cx);
    }

    fn is_bursting(&self, now: Instant) -> bool {
        let clock = self.clock;
        let has_ripple = self.ripples.iter().flatten().any(|r| clock - r.start < (RIPPLE_LIFE + RIPPLE_ECHO_LAG) as f64);
        let has_trail = self.trail.iter().flatten().any(|t| clock - t.at < TRAIL_LIFE as f64);
        let pet_hopping = (0.0..0.5).contains(&(clock - self.poked))
            || (0.0..PAT_HOLD as f64).contains(&(clock - self.patted))
            || (0.0..EXCITED_HOLD as f64).contains(&(clock - self.excited_at))
            || (0.0..CELEBRATE_HOLD as f64).contains(&(clock - self.celebrated_at));
        let parallax_moving = (self.parallax_target - self.parallax_x).abs() > 0.25;
        burst_rule(has_ripple, has_trail, pet_hopping || parallax_moving, self.burst_until.is_some_and(|until| now < until))
    }

    fn next_interval(&self, now: Instant) -> Duration {
        if self.is_bursting(now) { BURST_FRAME } else { FRAME }
    }

    fn should_tick(&self, window: &Window) -> bool {
        let recently_painted = self.painted.get().is_some_and(|at| at.elapsed() < UNSEEN);
        let recently_woken = self.woken_at.is_some_and(|at| at.elapsed() < UNSEEN);
        let shown = recently_painted || recently_woken;
        let awake = self.awake_until.is_some_and(|until| Instant::now() < until);
        tick_rule(motion_allowed(), window.is_window_active(), shown, awake)
    }

    fn ensure_ticking(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ticker.is_some() || !tick_rule(motion_allowed(), window.is_window_active(), true, self.awake_until.is_some_and(|until| Instant::now() < until)) {
            return;
        }
        let now = Instant::now();
        self.last_tick = Some(now);
        let mut interval = self.next_interval(now);
        self.ticker = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                let next = this
                    .update_in(cx, |this, window, cx| {
                        if !this.should_tick(window) {
                            this.ticker = None;
                            this.last_tick = None;
                            this.woken_at = None;
                            // Once more, so the pet is drawn asleep now rather than at the next unrelated repaint.
                            cx.notify();
                            return None;
                        }
                        let now = Instant::now();
                        let elapsed = this.last_tick.map_or(0.0, |last| now.duration_since(last).as_secs_f64());
                        this.last_tick = Some(now);
                        // A late frame (a busy interface thread) moves the field one frame's worth, not a jump.
                        let dt = elapsed.min(2.0 / FPS as f64);
                        this.clock += dt;
                        let clock = this.clock;
                        let ripple_max = (RIPPLE_LIFE + RIPPLE_ECHO_LAG) as f64;
                        for slot in &mut this.ripples {
                            if slot.is_some_and(|r| clock - r.start >= ripple_max) {
                                *slot = None;
                            }
                        }
                        for slot in &mut this.trail {
                            if slot.is_some_and(|t| clock - t.at >= TRAIL_LIFE as f64) {
                                *slot = None;
                            }
                        }
                        let blend = 1.0 - (-8.0 * dt as f32).exp();
                        this.parallax_x += (this.parallax_target - this.parallax_x) * blend;
                        if (this.parallax_target - this.parallax_x).abs() < 0.05 {
                            this.parallax_x = this.parallax_target;
                        }
                        cx.notify();
                        Some(this.next_interval(now))
                    })
                    .ok()
                    .flatten();
                match next {
                    Some(wait) => interval = wait,
                    None => break,
                }
            }
        }));
    }
}

/// The clock runs only when everything says so.
fn tick_rule(motion: bool, window_active: bool, shown: bool, awake: bool) -> bool {
    motion && window_active && shown && awake
}

/// Whether the clock temporarily steps up from [`FPS`] to [`BURST_FPS`].
fn burst_rule(has_ripple: bool, has_trail: bool, active_motion: bool, within_hold: bool) -> bool {
    has_ripple || has_trail || active_motion || within_hold
}

/// Calm sinusoidal glow breathing strictly bounded inside `[GLOW_BREATH_MIN, GLOW_BREATH_MAX]`.
pub(crate) fn glow_breath_at(clock: f64) -> f32 {
    let half = (GLOW_BREATH_MAX - GLOW_BREATH_MIN) * 0.5;
    let mid = GLOW_BREATH_MIN + half;
    let wave = ((clock * 0.98) % std::f64::consts::TAU).sin() as f32;
    (mid + half * wave).clamp(GLOW_BREATH_MIN, GLOW_BREATH_MAX)
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
        let allowed = motion_allowed();
        let field = Field {
            look,
            clock: if allowed { self.clock } else { 0.0 },
            pointer: self.pointer.filter(|_| allowed),
            trail: if allowed { self.trail } else { [None; TRAIL_CAP] },
            ripples: if allowed { self.ripples } else { [None; MAX_RIPPLES] },
            column: crate::ui::agent::COLUMN,
        };
        let (glyphs, painted, span) = (self.glyphs.clone(), self.painted.clone(), self.painted_span.clone());
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                painted.set(Some(Instant::now()));
                span.set((f32::from(bounds.origin.x), f32::from(bounds.size.width)));
                if let Some(glyphs) = glyphs {
                    // One layer for the whole field: its thousands of glyphs share one place in the frame's order
                    // instead of each being sorted in on its own.
                    window.with_content_mask(Some(ContentMask { bounds }), |window| window.paint_layer(bounds, |window| field.paint(bounds, &glyphs, window)));
                }
            },
        )
        .size_full()
    }
}

/// One frame of the field (all stack-allocated, zero per-frame heap copies for ripples/trail).
struct Field {
    look: Look,
    clock: f64,
    pointer: Option<Point<Pixels>>,
    trail: [Option<TrailPoint>; TRAIL_CAP],
    ripples: [Option<Ripple>; MAX_RIPPLES],
    column: f32,
}

impl Field {
    fn paint(&self, bounds: Bounds<Pixels>, glyphs: &Glyphs, window: &mut Window) {
        let visible = bounds.intersect(&window.content_mask().bounds);
        if visible.is_empty() {
            return;
        }
        let (left, top) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let cell = glyphs.advance;
        let (total_cols, total_rows) = ((width / cell).ceil() as i32, (height / LINE).ceil() as i32);
        let col_start = ((f32::from(visible.origin.x) - left) / cell).floor().max(0.0) as i32;
        let col_end = ((f32::from(visible.right()) - left) / cell).ceil().min(total_cols as f32).max(0.0) as i32;
        let row_start = ((f32::from(visible.origin.y) - top) / LINE).floor().max(0.0) as i32;
        let row_end = ((f32::from(visible.bottom()) - top) / LINE).ceil().min(total_rows as f32).max(0.0) as i32;
        if col_start >= col_end || row_start >= row_end {
            return;
        }

        let middle = left + width / 2.0;
        let half_column = self.column / 2.0 + COLUMN_MARGIN;
        let baseline = (LINE - FONT_SIZE) / 2.0 + glyphs.ascent;
        let pointer = self.pointer.map(|point| (f32::from(point.x), f32::from(point.y)));

        // Stack-allocated active trail points and ripples (zero heap allocations).
        let mut trail_buf = [(0.0f32, 0.0f32, 0.0f32); TRAIL_CAP];
        let mut trail_len = 0usize;
        for entry in self.trail.into_iter().flatten() {
            let age = (self.clock - entry.at) as f32;
            if (0.0..TRAIL_LIFE).contains(&age) {
                let fade = (1.0 - age / TRAIL_LIFE).powi(2);
                trail_buf[trail_len] = (f32::from(entry.origin.x), f32::from(entry.origin.y), fade);
                trail_len += 1;
            }
        }
        let trail = &trail_buf[..trail_len];

        let mut ripple_buf = [(0.0f32, 0.0f32, 0.0f32); MAX_RIPPLES];
        let mut ripple_len = 0usize;
        for entry in self.ripples.into_iter().flatten() {
            let age = (self.clock - entry.start) as f32;
            if (0.0..(RIPPLE_LIFE + RIPPLE_ECHO_LAG)).contains(&age) {
                ripple_buf[ripple_len] = (f32::from(entry.origin.x), f32::from(entry.origin.y), age);
                ripple_len += 1;
            }
        }
        let ripples = &ripple_buf[..ripple_len];

        // Precompute the horizontal column falloff once per column rather than `rows * cols` times.
        let col_count = (col_end - col_start) as usize;
        let mut zones = Vec::with_capacity(col_count);
        for column in col_start..col_end {
            let cx = left + column as f32 * cell + cell * 0.5;
            let inside = smoothstep(0.0, COLUMN_EDGE, (cx - middle).abs() - half_column);
            zones.push(self.look.glyph_alpha_column + (self.look.glyph_alpha - self.look.glyph_alpha_column) * inside);
        }

        let clock_f32 = (self.clock % 86_400.0) as f32;
        for row in row_start..row_end {
            let y = top + row as f32 * LINE;
            let cy = y + LINE * 0.5;
            for (offset, column) in (col_start..col_end).enumerate() {
                let x = left + column as f32 * cell;
                let cx = x + cell * 0.5;
                let zone = zones[offset];
                let (alpha, spark, scramble) = cell_light(column, row, (cx, cy), clock_f32, pointer, trail, ripples);
                let alpha = (zone * alpha).min(zone * GLYPH_GAIN);
                if alpha < INVISIBLE {
                    continue;
                }
                let index = letter(column, row, clock_f32, scramble) % glyphs.ids.len();
                let (font_id, glyph_id) = glyphs.ids[index];
                let color = if spark { self.look.glyph_spark } else { self.look.glyph };
                let _ = window.paint_glyph(point(px(x), px(y + baseline)), font_id, glyph_id, px(FONT_SIZE), color.opacity(alpha));
            }
        }
    }
}

/// How bright a cell is, before its place's alpha: the clouds, a sparkle, the pointer's glow + soft trail, and double
/// ripples. Returns the brightness (strictly clamped to [`GLYPH_GAIN`]), whether it shows in the spark color and
/// whether a ripple shuffles its letter.
fn cell_light(
    column: i32,
    row: i32,
    (x, y): (f32, f32),
    clock: f32,
    pointer: Option<(f32, f32)>,
    trail: &[(f32, f32, f32)],
    ripples: &[(f32, f32, f32)],
) -> (f32, bool, bool) {
    let mut light = smoothstep(0.40, 0.86, clouds(x, y, clock));
    let mut spark = sparkles(column, row, clock);
    if spark {
        light = (light.max(0.5)) * GLYPH_GAIN;
    }
    if let Some((px_, py)) = pointer {
        let dx = x - px_;
        let dy = y - py;
        if dx.abs() < POINTER_RADIUS && dy.abs() < POINTER_RADIUS {
            let d2 = dx * dx + dy * dy;
            if d2 < POINTER_RADIUS_SQ {
                let near = (1.0 - d2.sqrt() / POINTER_RADIUS).powi(2);
                light = light.max(0.35) * (1.0 + near);
                spark |= near > 0.45;
            }
        }
    }
    if !trail.is_empty() {
        let mut trail_boost: f32 = 0.0;
        for &(tx, ty, fade) in trail {
            let dx = x - tx;
            let dy = y - ty;
            if dx.abs() < TRAIL_RADIUS && dy.abs() < TRAIL_RADIUS {
                let d2 = dx * dx + dy * dy;
                if d2 < TRAIL_RADIUS_SQ {
                    let falloff = 1.0 - d2 / TRAIL_RADIUS_SQ;
                    trail_boost = trail_boost.max(0.55 * fade * falloff * falloff);
                }
            }
        }
        if trail_boost > 0.02 {
            light = (light.max(0.20) + trail_boost).min(GLYPH_GAIN);
        }
    }
    let mut scramble = false;
    for &(ox, oy, age) in ripples {
        let dx = x - ox;
        let dy = y - oy;
        let band = RIPPLE_WIDTH * 3.5;
        let primary_r = RIPPLE_SPEED * age;
        let max_reach = primary_r + band;
        if dx.abs() > max_reach || dy.abs() > max_reach {
            continue;
        }
        let d2 = dx * dx + dy * dy;
        let min_r = (RIPPLE_SPEED * (age - RIPPLE_ECHO_LAG).max(0.0) - band).max(0.0);
        if d2 > max_reach * max_reach || d2 < min_r * min_r {
            continue;
        }
        let distance = d2.sqrt();
        if age < RIPPLE_LIFE {
            let ring = (-((distance - primary_r) / RIPPLE_WIDTH).powi(2)).exp() * (1.0 - age / RIPPLE_LIFE);
            if ring > 0.05 {
                light += 1.2 * ring;
                spark |= ring > 0.5;
                scramble |= ring > 0.35;
            }
        }
        let echo_age = age - RIPPLE_ECHO_LAG;
        if (0.0..RIPPLE_LIFE).contains(&echo_age) {
            let echo_r = RIPPLE_SPEED * 0.88 * echo_age;
            let echo = (-((distance - echo_r) / (RIPPLE_WIDTH * 0.85)).powi(2)).exp() * (1.0 - echo_age / RIPPLE_LIFE);
            if echo > 0.06 {
                light += 1.2 * RIPPLE_ECHO_GAIN * echo;
            }
        }
    }
    (light.min(GLYPH_GAIN), spark, scramble)
}

/// About one cell in a hundred shines in the spark color for a few seconds at a time.
fn sparkles(column: i32, row: i32, clock: f32) -> bool {
    let epoch = (clock * 0.15 + unit(hash(column, row, 9))) as u32;
    hash(column, row, epoch.wrapping_mul(31).wrapping_add(5)).is_multiple_of(101)
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
        assert!(!burst_rule(false, false, false, false));
        assert!(burst_rule(true, false, false, false));
        assert!(burst_rule(false, true, false, false));
        assert!(burst_rule(false, false, true, false));
        assert!(burst_rule(false, false, false, true));
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
            let (light, ..) = cell_light(i % 100, i / 100, (x, y), 12.5, None, &[], &[]);
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
        let dim = (0..200).map(|i| i as f32 * 0.37).find(|clock| cell_light(cell.0, cell.1, (x, 0.0), *clock, None, &[], &[]).0 < 0.5).unwrap();
        let quiet = cell_light(cell.0, cell.1, (x, 0.0), dim, None, &[], &[]).0;
        let passing = cell_light(cell.0, cell.1, (x, 0.0), dim, None, &[], &[(0.0, 0.0, 0.5)]).0;
        let passed = cell_light(cell.0, cell.1, (x, 0.0), dim, None, &[], &[(0.0, 0.0, 1.85)]).0;
        assert!(passing > quiet + 0.3, "{quiet} {passing}");
        assert!((passed - quiet).abs() < 0.05, "{quiet} {passed}");
    }

    #[test]
    fn pointer_trail_and_echo_ripples_never_exceed_glyph_gain() {
        let trail = [(10.0, 10.0, 1.0), (14.0, 10.0, 0.8), (18.0, 10.0, 0.6)];
        let ripples = [(10.0, 10.0, 0.01), (10.0, 10.0, 0.25), (10.0, 10.0, 0.5)];
        for step in 0..100 {
            let x = step as f32 * 4.0;
            let (light, ..) = cell_light(step, 2, (x, 10.0), 5.0, Some((10.0, 10.0)), &trail, &ripples);
            assert!(light <= GLYPH_GAIN + 1e-6, "step {step}: {light}");
        }
        // Trail alone brightens a dim cell near its center.
        let quiet = cell_light(5, 5, (10.0, 10.0), 1.0, None, &[], &[]).0;
        let trailed = cell_light(5, 5, (10.0, 10.0), 1.0, None, &[(10.0, 10.0, 1.0)], &[]).0;
        assert!(trailed > quiet + 0.2, "{quiet} {trailed}");
    }

    #[test]
    fn glow_breath_stays_inside_contrast_safe_range() {
        for step in 0..200 {
            let b = glow_breath_at(step as f64 * 0.17);
            assert!((GLOW_BREATH_MIN..=GLOW_BREATH_MAX).contains(&b), "{b}");
        }
    }
}
