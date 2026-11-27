//! The pixel pet on the edge of a new chat's composer (and an optional low-distraction compact companion in active
//! chats): a small white-haired cat girl with dot eyes and round blush, an ahoge, a leaf clip, a white dress with a
//! pale blue sailor collar, and a white tail, as 44 x 54 pixel art.
//!
//! She blinks, twitches an ear, wags her tail and looks towards the pointer from her live position; she strolls along
//! the composer and stops attentively while the user types; hovered or patted she hops with a heart (three quick pats
//! trigger a double-hop easter egg); during AI thinking/responding she looks up expectantly and celebrates briefly on
//! completion; while the page's clock is paused she sleeps with 0 frames / 0 CPU at rest.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gpui_kit::*;

use super::backdrop::{Backdrop, CELEBRATE_HOLD, EXCITED_HOLD, PAT_HOLD, PetActivity, TYPING_HOLD, motion_allowed};

const WIDTH: usize = 44;
const HEIGHT: usize = 54;
/// A pixel of the art in window pixels at most: it is drawn as a whole number of the screen's pixels, so its edges
/// stay sharp at any scaling (two of them at 150 % and at 200 %).
pub const PIXEL: f32 = 1.34;
/// Compact companion scale for active chats (`0.72×`, stationary on the composer's right corner).
#[allow(dead_code)]
pub const MINI_PIXEL: f32 = PIXEL * 0.72;
/// How far she strolls to the left of her place, and how high she hops, in pixels of the art.
const STROLL: f32 = 48.0;
const HOP: f32 = 7.0;

/// `o` outline, `w`/`s` hair, `p`/`P` inner ear and its fur, `g`/`G` leaf and stem, `f`/`F` skin and the shadow under
/// the bangs, `e` eyes, `b` blush, `c`/`C` collar and skirt hem, `n` the collar's star, `u`/`U` dress, `K` shoes,
/// `r`/`h` heart, `z` sleep Z, `y` sparkle gold, `a` approval amber.
const ART: [&str; HEIGHT] = [
    "......................oooooo................",
    ".....................owwwwwwo...............",
    ".......o............owwwoowwwo......o.......",
    "......owoo..........owoo..oowo....oowo......",
    "......owwwo........owwo.....o....owwwo......",
    "......owwwwoo......owwo........oowwwwo......",
    "......owppwwwo.....owwo.......owwwppwo......",
    "......owpppwwwoo.oooowooooo.oowwwpppwo......",
    "......owppppwwwwowwwwwwwwwwowwwwppppwo......",
    "......owppppppwwwwwwwwwwwwwwwwppppppwo......",
    "......owpppppppwwwwwwwwwwwwwwpppppppwo......",
    "......owpPppppwwwwwwwwwwwwwwwwppppPpwo......",
    "......owPPpppwwwwwwwwwwwwwwwwwwpppPPwo......",
    "......owppPPwwwwwwwwwwwwwwwwwwwwPPppwo......",
    "......owPPPwwwwwwwwwwwwwwwwwwwwwwPgPwo......",
    "......owwwwwwwwwwwwwwwwwwwwwwwwwwggwwo......",
    "......owwwwwwwwwwwwwwwwwwwwwwwwwwgwwwo......",
    ".......owwwwwwwwwwwwwwwwwwwwwwgwwGwgo.......",
    ".......owwwswwwwwwswwwwwwswwwwggsGggo.......",
    ".......owwwswswwwwswwwwwwswwwwsgGgwwo.......",
    "......owwwwswswwwwswwwwwwswwwgswGggwwo......",
    "......owwwwswswwwwswwwwwwswwwwggGgwwwo......",
    "......owwwwswswwwwswwwwwwswwwwsGswwwwo......",
    "......owwwwswswwwwswwwwwwswwwwsGswwwwo......",
    ".......owwwswwwwwwwwwwwwwwwwwwwGswwwo.......",
    ".......owwwsFwwFFFFFwwwwFFFFFwwFswwwo.......",
    ".......owwwsfFFfffffFwwFfffffFFfswwwo.......",
    ".......owwwsfffeeffffFFffffeefffswwwo.......",
    ".......owwwsfffeeffffffffffeefffswwwo.......",
    ".......owwwsfffeeffffffffffeefffswwwo.......",
    ".......owwwsfffeeffffffffffeefffswwwo.......",
    ".......owwwsfffeeffffffffffeefffswwwo.......",
    ".......owwwsbbffffffffffffffffbbswwwo.......",
    ".......owwwbbbbffffffffffffffbbbbwwwo.......",
    ".......owwwsbbwffffffffffffffwbbswwwo..o....",
    ".......owwwswwwwffffffffffffwwwwswwwo.oso...",
    ".......owwwswwwwwwwwffffwwwwwwwwswwwoosso...",
    ".......owwwswwccccccccccccccccwwswwwoossso..",
    ".......owwwswwccccccccccccccccwwswwwoossso..",
    ".......owwwswccccccccnnccccccccwswwwoossso..",
    ".......owwwswCCCccccnnnnccccCCCwswwwoosso...",
    ".......owwwwwuuuCCCccnnccCCCuuuwwsssoosso...",
    ".......owwwwuuuuuuuCnccnCuUuuuuwwsssossso...",
    ".......ossssuuuuuuuuCCCCuuUuuuuusssssssso...",
    ".......ossssuuuuuuuuuuuuuuUUuuuussssssso....",
    ".......ossssffffuuuuuuuuuuUUffffsssssso.....",
    ".......ossssffffuuuuuuuuuuUUffffssssso......",
    "........oooofffccccccccccccccfffossoo.......",
    "............occcccccccccccccccco.oo.........",
    "...........occcccfffccccfffccccco...........",
    "............ooooofffoooofffooooo............",
    "...............oKKKKKooKKKKKo...............",
    "...............oKKKKKooKKKKKo...............",
    "...............oKKKKKooKKKKKo...............",
];
/// The eyes: 4 x 5 blocks on rows 27 to 31 holding a dot eye with room to look aside, the left one from column 14,
/// the right one from column 26 and drawn mirrored.
const EYE_ROW: usize = 27;
const EYES_AT: [(usize, bool); 2] = [(14, false), (26, true)];
const EYE_WIDTH: usize = 4;
const EYE_HEIGHT: usize = 5;
const EYES_OPEN: [&str; EYE_HEIGHT] = [
    "feef",
    "feef",
    "feef",
    "feef",
    "feef",
];
const EYES_CLOSED: [&str; EYE_HEIGHT] = [
    "ffff",
    "ffff",
    "ffff",
    "eeee",
    "ffff",
];
const EYES_HAPPY: [&str; EYE_HEIGHT] = [
    "ffff",
    "ffff",
    "feef",
    "effe",
    "ffff",
];
/// Focused / expectant eyes with a tiny white highlight dot.
const EYES_FOCUSED: [&str; EYE_HEIGHT] = [
    "feef",
    "fhef",
    "feef",
    "feef",
    "ffff",
];
/// Cheerful raised arc eyes for celebration and excited easter-egg bounce.
const EYES_CHEER: [&str; EYE_HEIGHT] = [
    "ffff",
    "feef",
    "effe",
    "ffff",
    "ffff",
];
/// The legs (the rows from `LEG_ROW` down) standing and in the two steps of a walk.
const LEG_ROW: usize = 49;
const LEGS_STAND: [&str; HEIGHT - LEG_ROW] = [
    "...........occcccfffccccfffccccco...........",
    "............ooooofffoooofffooooo............",
    "...............oKKKKKooKKKKKo...............",
    "...............oKKKKKooKKKKKo...............",
    "...............oKKKKKooKKKKKo...............",
];
const LEGS_LEFT: [&str; HEIGHT - LEG_ROW] = [
    "...........occcoofffooccfffccccco...........",
    "............ooooKKKKKooofffooooo............",
    "...............oKKKKKooKKKKKo...............",
    "...............oKKKKKooKKKKKo...............",
    "......................oKKKKKo...............",
];
const LEGS_RIGHT: [&str; HEIGHT - LEG_ROW] = [
    "...........occcccfffccoofffooccco...........",
    "............ooooofffoooKKKKKoooo............",
    "...............oKKKKKooKKKKKo...............",
    "...............oKKKKKooKKKKKo...............",
    "...............oKKKKKo......................",
];
/// The right ear's top bent outwards for a twitch: these rows from column 33, row 2.
const FLICK_AT: (usize, usize) = (33, 2);
const FLICK: [&str; 4] = [
    "...oo.",
    ".ooowo",
    "oowwwo",
    "owwwwo",
];
/// Gentle tail wag patch (rows 34..41, columns 37..44, swaying the tail tip slightly rightward).
const TAIL_AT: (usize, usize) = (37, 34);
const TAIL_WAG: [&str; 7] = [
    "...o...",
    "..oso..",
    ".osso..",
    "oossso.",
    ".ossso.",
    ".ossso.",
    "osso...",
];
const HEART: [&str; 8] = [
    ".rr...rr.",
    "rhrr.rrrr",
    "rrrrrrrrr",
    "rrrrrrrrr",
    ".rrrrrrr.",
    "..rrrrr..",
    "...rrr...",
    "....r....",
];
const ZED: [&str; 5] = [
    "zzzz",
    "...z",
    "..z.",
    ".z..",
    "zzzz",
];
/// Small floating sparkle for thinking / excited states.
const SPARKLE: [&str; 7] = [
    "...y...",
    "...y...",
    "..yhy..",
    "yyhhhyy",
    "..yhy..",
    "...y...",
    "...y...",
];
/// Small musical note for completion celebration.
const NOTE: [&str; 8] = [
    "...KKKK",
    "...K..K",
    "...K..K",
    "...K...",
    ".KKK...",
    "KKKK...",
    "KKKK...",
    ".KK....",
];
/// Small calm amber mark for waiting-approval alertness.
const EXCLAIM: [&str; 8] = [
    ".a.",
    "aaa",
    "aaa",
    ".a.",
    ".a.",
    "...",
    "aaa",
    ".a.",
];

/// Precomputed ASCII lookup table (`[Option<Hsla>; 128]`), eliminating per-pixel color conversion in hot paint loops.
fn palette() -> &'static [Option<Hsla>; 128] {
    static PALETTE: OnceLock<[Option<Hsla>; 128]> = OnceLock::new();
    PALETTE.get_or_init(|| {
        let mut table = [None; 128];
        let mut put = |mark: u8, r: u8, g: u8, b: u8| {
            table[mark as usize] = Some(Hsla::from(Rgba { r: f32::from(r) / 255.0, g: f32::from(g) / 255.0, b: f32::from(b) / 255.0, a: 1.0 }));
        };
        put(b'o', 86, 92, 140);
        put(b'w', 240, 243, 253);
        put(b's', 212, 219, 245);
        put(b'p', 241, 176, 182);
        put(b'P', 255, 255, 255);
        put(b'g', 116, 176, 122);
        put(b'G', 74, 132, 86);
        put(b'f', 253, 238, 232);
        put(b'F', 240, 214, 208);
        put(b'e', 38, 38, 48);
        put(b'b', 244, 170, 166);
        put(b'c', 168, 198, 242);
        put(b'C', 126, 160, 222);
        put(b'n', 255, 255, 255);
        put(b'u', 255, 255, 255);
        put(b'U', 226, 232, 248);
        put(b'K', 108, 142, 214);
        put(b'r', 236, 84, 116);
        put(b'h', 255, 255, 255);
        put(b'z', 132, 136, 186);
        put(b'y', 238, 192, 92);
        put(b'a', 228, 142, 58);
        table
    })
}

#[inline]
fn color(mark: char) -> Option<Hsla> {
    let code = mark as u32;
    (code < 128).then(|| palette()[code as usize]).flatten()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eyes {
    Open,
    Closed,
    Happy,
    Focused,
    Cheer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Legs {
    Stand,
    Left,
    Right,
}

/// How she is at one moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// How far left of her place she is, in pixels of the art.
    pub stroll: f32,
    /// How high she is off the composer, in pixels of the art.
    pub lift: f32,
    pub eyes: Eyes,
    /// Where her eyes look: -1 left, 0 ahead, 1 right.
    pub glance: i32,
    pub legs: Legs,
    pub flick: bool,
    pub tail_wag: bool,
    /// The heart's rise, 0 to 1, after she was pointed at or patted.
    pub heart: Option<f32>,
    /// Floating sparkle progress, 0 to 1, while thinking or excited.
    pub sparkle: Option<f32>,
    /// Floating musical note progress, 0 to 1, during completion celebration.
    pub note: Option<f32>,
    /// Calm amber mark while a tool waits for approval.
    pub exclaim: bool,
    pub asleep: bool,
}

/// All dynamic inputs that determine a [`Pose`].
#[derive(Clone, Copy, Debug, Default)]
pub struct PetContext {
    pub clock: f32,
    pub poked: f32,
    pub asleep: bool,
    pub pointer: Option<f32>,
    pub typing_age: Option<f32>,
    pub pat_age: Option<f32>,
    pub excited_age: Option<f32>,
    pub celebrate_age: Option<f32>,
    pub activity: PetActivity,
    /// When `true` (active-chat mini companion), she stays planted without strolling.
    pub compact: bool,
}

/// Computes horizontal stroll displacement & walking direction independently so pointer gaze can be calculated from
/// her true rendered center rather than her resting home coordinate.
fn stroll_at(clock: f32, compact: bool, hold_still: bool) -> (f32, Option<i32>) {
    if compact {
        return (0.0, None);
    }
    let t = clock.rem_euclid(30.0);
    let (stroll, walking) = match t {
        t if t < 10.0 => (0.0, None),
        t if t < 13.0 => ((t - 10.0) / 3.0 * STROLL, Some(-1)),
        t if t < 20.0 => (STROLL, None),
        t if t < 23.0 => ((23.0 - t) / 3.0 * STROLL, Some(1)),
        _ => (0.0, None),
    };
    if hold_still { (stroll, None) } else { (stroll, walking) }
}

/// Full pose computation supporting typing attentiveness, pat/click easter egg, and AI turn reactions.
pub fn pose_with(ctx: PetContext) -> Pose {
    let typing = ctx.typing_age.is_some_and(|age| (0.0..TYPING_HOLD).contains(&age));
    let holding = typing
        || ctx.pat_age.is_some()
        || ctx.excited_age.is_some()
        || ctx.celebrate_age.is_some()
        || ctx.activity != PetActivity::Idle;
    let (stroll, walking) = stroll_at(ctx.clock, ctx.compact, holding);
    if ctx.asleep {
        return Pose {
            stroll,
            lift: 0.0,
            eyes: Eyes::Closed,
            glance: 0,
            legs: Legs::Stand,
            flick: false,
            tail_wag: false,
            heart: None,
            sparkle: None,
            note: None,
            exclaim: false,
            asleep: true,
        };
    }

    let t = ctx.clock.rem_euclid(30.0);
    let step = (t * 4.0) as i32 % 2 == 0;
    let (legs, mut lift) = match walking {
        Some(_) if step => (Legs::Left, 1.0),
        Some(_) => (Legs::Right, 0.0),
        None => (Legs::Stand, 0.0),
    };

    let looking_around = if !ctx.compact && (15.0..16.5).contains(&t) {
        1
    } else if !ctx.compact && (16.5..18.0).contains(&t) {
        -1
    } else {
        0
    };
    let towards_pointer = ctx.pointer.map_or(0, |dx| if dx < -10.0 { -1 } else if dx > 10.0 { 1 } else { 0 });
    let mut glance = walking.unwrap_or(if towards_pointer != 0 { towards_pointer } else { looking_around });

    let since_poke = ctx.clock - ctx.poked;
    let poke_happy = (0.0..1.2).contains(&since_poke);
    if (0.0..0.45).contains(&since_poke) {
        lift += HOP * (std::f32::consts::PI * since_poke / 0.45).sin();
    }

    let pat_rise = ctx.pat_age.filter(|age| (0.0..PAT_HOLD).contains(age)).map(|age| {
        if age < 0.48 {
            lift = lift.max(HOP * 1.25 * (std::f32::consts::PI * age / 0.48).sin());
        }
        age / PAT_HOLD
    });

    let excited_rise = ctx.excited_age.filter(|age| (0.0..EXCITED_HOLD).contains(age)).map(|age| {
        let bounce = (std::f32::consts::TAU * age / EXCITED_HOLD).sin().abs();
        lift = lift.max(HOP * 1.3 * bounce);
        age / EXCITED_HOLD
    });

    let celebrate_rise = ctx.celebrate_age.filter(|age| (0.0..CELEBRATE_HOLD).contains(age)).map(|age| {
        if age < 0.55 {
            lift = lift.max(HOP * 1.1 * (std::f32::consts::PI * age / 0.55).sin());
        }
        age / CELEBRATE_HOLD
    });

    let thinking_or_responding = matches!(ctx.activity, PetActivity::Thinking | PetActivity::Responding);
    let waiting_approval = ctx.activity == PetActivity::WaitingApproval;
    let thinking_sparkle = thinking_or_responding.then(|| (ctx.clock * 0.55).rem_euclid(1.0));

    let blinking = (ctx.clock + 0.7).rem_euclid(3.7) < 0.15;
    let eyes = if excited_rise.is_some() || celebrate_rise.is_some() {
        Eyes::Cheer
    } else if poke_happy || pat_rise.is_some() {
        Eyes::Happy
    } else if typing || thinking_or_responding || waiting_approval {
        Eyes::Focused
    } else if blinking && walking.is_none() {
        Eyes::Closed
    } else {
        Eyes::Open
    };

    if excited_rise.is_some() || celebrate_rise.is_some() || poke_happy || pat_rise.is_some() {
        glance = 0;
    } else if typing {
        // Looking attentively towards the composer to her left.
        glance = -1;
    }

    let idle_flick = (ctx.clock + 2.3).rem_euclid(6.1) < 0.2;
    let typing_flick = ctx.typing_age.is_some_and(|age| (age * 2.5).rem_euclid(1.0) < 0.16);
    let flick = idle_flick || typing_flick || waiting_approval;

    let wag_active = typing || thinking_or_responding || excited_rise.is_some() || celebrate_rise.is_some() || pat_rise.is_some();
    let tail_wag = wag_active && (ctx.clock * 3.2).rem_euclid(1.0) < 0.45;

    let heart = pat_rise.or_else(|| poke_happy.then_some(since_poke / 1.2));
    let sparkle = excited_rise.or(thinking_sparkle);
    let note = celebrate_rise;

    Pose {
        stroll,
        lift,
        eyes,
        glance,
        legs,
        flick,
        tail_wag,
        heart,
        sparkle,
        note,
        exclaim: waiting_approval,
        asleep: false,
    }
}

/// Her pose at `clock` (the page's clock, seconds). `poked`: when she was last pointed at, on the same clock;
/// `asleep`: the clock is paused; `pointer`: how far right of her middle the pointer is, in pixels of the art.
#[allow(dead_code)]
pub fn pose(clock: f32, poked: f32, asleep: bool, pointer: Option<f32>) -> Pose {
    pose_with(PetContext { clock, poked, asleep, pointer, ..PetContext::default() })
}

/// The mark of the art at column `x`, row `y` in `pose`.
fn mark(pose: &Pose, x: usize, y: usize) -> char {
    if (EYE_ROW..EYE_ROW + EYE_HEIGHT).contains(&y) {
        for (column, mirrored) in EYES_AT {
            // The eye and the skin around it, which the eye moves into when it looks aside.
            if (column - 1..=column + EYE_WIDTH).contains(&x) {
                let block = match pose.eyes {
                    Eyes::Open => EYES_OPEN,
                    Eyes::Closed => EYES_CLOSED,
                    Eyes::Happy => EYES_HAPPY,
                    Eyes::Focused => EYES_FOCUSED,
                    Eyes::Cheer => EYES_CHEER,
                };
                let offset = x as i32 - column as i32 - pose.glance;
                if !(0..EYE_WIDTH as i32).contains(&offset) {
                    return 'f';
                }
                let index = if mirrored { EYE_WIDTH - 1 - offset as usize } else { offset as usize };
                return block[y - EYE_ROW].as_bytes()[index] as char;
            }
        }
    }
    if y >= LEG_ROW {
        let legs = match pose.legs {
            Legs::Stand => LEGS_STAND,
            Legs::Left => LEGS_LEFT,
            Legs::Right => LEGS_RIGHT,
        };
        return legs[y - LEG_ROW].as_bytes()[x] as char;
    }
    let (fx, fy) = FLICK_AT;
    if pose.flick && (fy..fy + FLICK.len()).contains(&y) && (fx..fx + FLICK[0].len()).contains(&x) {
        return FLICK[y - fy].as_bytes()[x - fx] as char;
    }
    let (tx, ty) = TAIL_AT;
    if pose.tail_wag && (ty..ty + TAIL_WAG.len()).contains(&y) && (tx..tx + TAIL_WAG[0].len()).contains(&x) {
        return TAIL_WAG[y - ty].as_bytes()[x - tx] as char;
    }
    ART[y].as_bytes()[x] as char
}

/// The size of a pixel of the art on a screen with `scale` physical pixels per window pixel.
pub fn unit(scale: f32) -> f32 {
    unit_for(PIXEL, scale)
}

fn unit_for(base_pixel: f32, scale: f32) -> f32 {
    (base_pixel * scale).floor().max(1.0) / scale
}

/// The box she lives in: her place on the right, the stroll to the left, room to hop above.
pub fn box_size() -> (f32, f32) {
    ((WIDTH as f32 + STROLL) * PIXEL, row_height())
}

/// The height of the row she stands in.
pub fn row_height() -> f32 {
    (HEIGHT as f32 + HOP) * PIXEL
}

/// Size of the compact active-chat companion (`0.72×`, no horizontal stroll).
#[allow(dead_code)]
pub fn mini_size() -> (f32, f32) {
    (WIDTH as f32 * MINI_PIXEL, (HEIGHT as f32 + HOP) * MINI_PIXEL)
}

/// Paints a small pixel overlay sprite using horizontal run-length merging to minimize quad count.
fn paint_sprite_rle(window: &mut Window, rows: &[&str], origin: (f32, f32), offset: (f32, f32), unit: f32, alpha: f32) {
    let (left, top) = origin;
    let (ox, oy) = offset;
    let base_y = oy.round();
    for (dy, row) in rows.iter().enumerate() {
        let bytes = row.as_bytes();
        let mut x = 0;
        while x < bytes.len() {
            let here = bytes[x] as char;
            let mut end = x + 1;
            while end < bytes.len() && bytes[end] as char == here {
                end += 1;
            }
            if let Some(shade) = color(here) {
                let run = Bounds::new(
                    point(px(left + (ox + x as f32) * unit), px(top + (base_y + dy as f32) * unit)),
                    size(px((end - x) as f32 * unit), px(unit)),
                );
                window.paint_quad(fill(run, shade.opacity(alpha)));
            }
            x = end;
        }
    }
}

/// Paints the pet body and active head overlays inside a single `paint_layer`.
fn paint_pose(window: &mut Window, pose: &Pose, left: f32, top: f32, unit: f32) {
    let reach = Bounds::new(
        point(px(left - unit), px(top - 22.0 * unit)),
        size(px((WIDTH as f32 + 2.0) * unit), px((HEIGHT as f32 + 22.0) * unit)),
    );
    window.paint_layer(reach, |window| {
        // Horizontal RLE reduces quad count by ~3x while drawing identical pixels.
        for y in 0..HEIGHT {
            let mut x = 0;
            while x < WIDTH {
                let here = mark(pose, x, y);
                let mut end = x + 1;
                while end < WIDTH && mark(pose, end, y) == here {
                    end += 1;
                }
                if let Some(shade) = color(here) {
                    let run = Bounds::new(
                        point(px(left + x as f32 * unit), px(top + y as f32 * unit)),
                        size(px((end - x) as f32 * unit), px(unit)),
                    );
                    window.paint_quad(fill(run, shade));
                }
                x = end;
            }
        }
        let origin = (left, top);
        if let Some(rise) = pose.heart {
            paint_sprite_rle(window, &HEART, origin, (30.0, -9.0 - rise * 6.0), unit, 1.0 - rise * rise);
        }
        if let Some(rise) = pose.sparkle {
            let alpha = (rise * std::f32::consts::PI).sin().clamp(0.0, 1.0);
            paint_sprite_rle(window, &SPARKLE, origin, (6.0, -7.0 - rise * 7.0), unit, alpha);
        }
        if let Some(rise) = pose.note {
            paint_sprite_rle(window, &NOTE, origin, (31.0, -9.0 - rise * 7.0), unit, 1.0 - rise * rise);
        }
        if pose.exclaim {
            paint_sprite_rle(window, &EXCLAIM, origin, (37.0, -9.0), unit, 1.0);
        }
        if pose.asleep {
            paint_sprite_rle(window, &ZED, origin, (36.0, -6.0), unit, 1.0);
        }
    });
}

/// The pet, standing with her feet on the bottom of her box.
pub fn render(backdrop: &Entity<Backdrop>, window: &Window, cx: &App) -> impl IntoElement + use<> {
    let page = backdrop.read(cx);
    let motion = motion_allowed();
    let asleep = motion && page.dozing(window);
    let (clock, poked) = if motion { (page.clock(), page.poked()) } else { (0.0, f32::NEG_INFINITY) };
    let pointer = page.pointer();
    let typing_age = if motion { page.typing_age() } else { None };
    let pat_age = if motion { page.pat_age() } else { None };
    let excited_age = if motion { page.excited_age() } else { None };
    let celebrate_age = if motion { page.celebrate_age() } else { None };
    let activity = page.ai_activity();
    let last_inside = Rc::new(Cell::new(page.pet_inside()));
    let (width, height) = box_size();
    let weak = backdrop.downgrade();
    div().id("optics-pet").w(px(width)).h(px(height)).child(
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, _| {
                let scale = window.scale_factor();
                let unit = unit(scale);
                let snap = |value: f32| (value * scale).round() / scale;
                let home = f32::from(bounds.right()) - WIDTH as f32 * unit;
                // Compute her live stroll position first so pointer gaze tracks from where she actually stands.
                let holding = typing_age.is_some()
                    || pat_age.is_some()
                    || excited_age.is_some()
                    || celebrate_age.is_some()
                    || activity != PetActivity::Idle;
                let (stroll, _) = stroll_at(clock, false, holding);
                let left = snap(home - stroll * unit);
                let actual_middle = left + WIDTH as f32 * unit * 0.5;
                let near = pointer.map(|point| (f32::from(point.x) - actual_middle) / unit).filter(|dx| dx.abs() < 300.0);
                let pose = pose_with(PetContext {
                    clock,
                    poked,
                    asleep,
                    pointer: near,
                    typing_age,
                    pat_age,
                    excited_age,
                    celebrate_age,
                    activity,
                    compact: false,
                });
                let top = snap(f32::from(bounds.bottom()) - (HEIGHT as f32 + pose.lift) * unit);
                paint_pose(window, &pose, left, top, unit);

                // Gate pointer hit-testing by `hitbox.is_hovered(window)` so modal overlays/menus occlude the pet.
                let body = Bounds::new(point(px(left), px(top)), size(px(WIDTH as f32 * unit), px(HEIGHT as f32 * unit)));
                let weak_move = weak.clone();
                let inside_cell = last_inside.clone();
                let hitbox_move = hitbox.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if !phase.bubble() {
                        return;
                    }
                    let inside = hitbox_move.is_hovered(window) && body.contains(&event.position);
                    if inside != inside_cell.get() {
                        inside_cell.set(inside);
                        let _ = weak_move.update(cx, |page, cx| page.pet_hovered(inside, window, cx));
                    }
                });
                let weak_click = weak.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase.bubble() && event.button == MouseButton::Left && hitbox.is_hovered(window) && body.contains(&event.position) {
                        cx.stop_propagation();
                        let _ = weak_click.update(cx, |page, cx| page.pet_clicked(window, cx));
                    }
                });
            },
        )
        .size_full(),
    )
}

/// Stop the mini companion's ticker if it has not been painted for this long (e.g. hidden behind settings or optics off).
#[allow(dead_code)]
const MINI_UNSEEN: Duration = Duration::from_millis(600);

/// Whether [`MiniPet`]'s 10 fps ticker should stay alive.
#[allow(dead_code)]
fn mini_tick_rule(motion: bool, window_active: bool, shown: bool, active_or_burst: bool) -> bool {
    motion && window_active && shown && active_or_burst
}

/// Low-distraction stationary mini companion (`0.72×`) for active chats when optics are enabled.
///
/// Consumes **0 frames / 0 CPU at rest** (both `Idle` and `WaitingApproval` are completely static) and stops its 10 fps
/// ticker within [`MINI_UNSEEN`] whenever hidden or unrendered. Starts only while visible + thinking/responding
/// (window-active gated) or during finite reaction bursts (1.2s pat, 1.5s triple-click, 1.8s celebration, 2.0s typing).
#[allow(dead_code)]
pub struct MiniPet {
    clock: f64,
    last_tick: Option<Instant>,
    woken_at: Option<Instant>,
    painted: Rc<Cell<Option<Instant>>>,
    activity: PetActivity,
    last_typed: f64,
    patted: f64,
    pat_streak: u8,
    excited_at: f64,
    excited_cooldown_until: f64,
    celebrated_at: f64,
    ticker: Option<Task<()>>,
    _activation: Subscription,
}

#[allow(dead_code)]
impl MiniPet {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() && this.is_active_or_burst() {
                this.wake_ticker(window, cx);
            }
        });
        Self {
            clock: 0.0,
            last_tick: None,
            woken_at: None,
            painted: Rc::default(),
            activity: PetActivity::Idle,
            last_typed: f64::NEG_INFINITY,
            patted: f64::NEG_INFINITY,
            pat_streak: 0,
            excited_at: f64::NEG_INFINITY,
            excited_cooldown_until: f64::NEG_INFINITY,
            celebrated_at: f64::NEG_INFINITY,
            ticker: None,
            _activation: activation,
        }
    }

    /// Updates AI turn status; transitioning out of `Thinking`/`Responding` into `Idle` triggers a 1.8s celebration.
    pub fn set_activity(&mut self, activity: PetActivity, window: &mut Window, cx: &mut Context<Self>) {
        if self.activity == activity {
            return;
        }
        let was_active = matches!(self.activity, PetActivity::Thinking | PetActivity::Responding);
        self.activity = activity;
        if was_active && activity == PetActivity::Idle && motion_allowed() {
            self.celebrated_at = self.clock;
        }
        if self.is_active_or_burst() {
            self.wake_ticker(window, cx);
        } else {
            cx.notify();
        }
    }

    pub fn notify_typed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !motion_allowed() {
            return;
        }
        self.last_typed = self.clock;
        self.wake_ticker(window, cx);
    }

    pub fn clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !motion_allowed() {
            return;
        }
        if self.clock - self.patted <= 3.0 {
            self.pat_streak = self.pat_streak.saturating_add(1);
        } else {
            self.pat_streak = 1;
        }
        self.patted = self.clock;
        if self.pat_streak >= 3 && self.clock >= self.excited_cooldown_until {
            self.excited_at = self.clock;
            self.excited_cooldown_until = self.clock + EXCITED_HOLD as f64 + 4.0;
            self.pat_streak = 0;
        }
        self.wake_ticker(window, cx);
    }

    fn wake_ticker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.woken_at = Some(Instant::now());
        cx.notify();
        self.ensure_ticking(window, cx);
    }

    fn shown(&self, now: Instant) -> bool {
        self.painted.get().is_some_and(|at| now.saturating_duration_since(at) < MINI_UNSEEN)
            || self.woken_at.is_some_and(|at| now.saturating_duration_since(at) < MINI_UNSEEN)
    }

    fn has_finite_burst(&self) -> bool {
        (0.0..TYPING_HOLD as f64).contains(&(self.clock - self.last_typed))
            || (0.0..PAT_HOLD as f64).contains(&(self.clock - self.patted))
            || (0.0..EXCITED_HOLD as f64).contains(&(self.clock - self.excited_at))
            || (0.0..CELEBRATE_HOLD as f64).contains(&(self.clock - self.celebrated_at))
    }

    fn is_active_or_burst(&self) -> bool {
        matches!(self.activity, PetActivity::Thinking | PetActivity::Responding) || self.has_finite_burst()
    }

    fn should_tick(&self, window: &Window, now: Instant) -> bool {
        mini_tick_rule(motion_allowed(), window.is_window_active(), self.shown(now), self.is_active_or_burst())
    }

    fn ensure_ticking(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ticker.is_some() || !mini_tick_rule(motion_allowed(), window.is_window_active(), true, self.is_active_or_burst()) {
            return;
        }
        self.last_tick = Some(Instant::now());
        let step = Duration::from_millis(100);
        self.ticker = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(step).await;
                let going = this
                    .update_in(cx, |this, window, cx| {
                        let now = Instant::now();
                        let elapsed = this.last_tick.map_or(0.0, |last| now.duration_since(last).as_secs_f64());
                        this.last_tick = Some(now);
                        this.clock += elapsed.min(0.2);
                        if !this.should_tick(window, now) {
                            let still_visible = this.shown(now);
                            this.ticker = None;
                            this.last_tick = None;
                            this.woken_at = None;
                            if still_visible {
                                cx.notify();
                            }
                            return false;
                        }
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

impl Render for MiniPet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let motion = motion_allowed();
        // Resume the 10 fps ticker automatically when the mini pet becomes visible again while active.
        if motion && self.ticker.is_none() && self.is_active_or_burst() && window.is_window_active() {
            self.woken_at = Some(Instant::now());
            self.ensure_ticking(window, cx);
        }
        let clock = if motion { self.clock as f32 } else { 0.0 };
        let typing_age = motion.then_some((self.clock - self.last_typed) as f32).filter(|age| (0.0..TYPING_HOLD).contains(age));
        let pat_age = motion.then_some((self.clock - self.patted) as f32).filter(|age| (0.0..PAT_HOLD).contains(age));
        let excited_age = motion.then_some((self.clock - self.excited_at) as f32).filter(|age| (0.0..EXCITED_HOLD).contains(age));
        let celebrate_age = motion.then_some((self.clock - self.celebrated_at) as f32).filter(|age| (0.0..CELEBRATE_HOLD).contains(age));
        let pose = pose_with(PetContext {
            clock,
            poked: f32::NEG_INFINITY,
            asleep: false,
            pointer: None,
            typing_age,
            pat_age,
            excited_age,
            celebrate_age,
            activity: self.activity,
            compact: true,
        });
        let (width, height) = mini_size();
        let weak = cx.weak_entity();
        let painted = self.painted.clone();
        div().id("optics-mini-pet").w(px(width)).h(px(height)).cursor_pointer().child(
            canvas(
                |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |bounds, hitbox, window, _| {
                    painted.set(Some(Instant::now()));
                    let scale = window.scale_factor();
                    let unit = unit_for(MINI_PIXEL, scale);
                    let snap = |value: f32| (value * scale).round() / scale;
                    let left = snap(f32::from(bounds.right()) - WIDTH as f32 * unit);
                    let top = snap(f32::from(bounds.bottom()) - (HEIGHT as f32 + pose.lift) * unit);
                    paint_pose(window, &pose, left, top, unit);
                    let body = Bounds::new(point(px(left), px(top)), size(px(WIDTH as f32 * unit), px(HEIGHT as f32 * unit)));
                    let weak = weak.clone();
                    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                        if phase.bubble() && event.button == MouseButton::Left && hitbox.is_hovered(window) && body.contains(&event.position) {
                            cx.stop_propagation();
                            let _ = weak.update(cx, |pet, cx| pet.clicked(window, cx));
                        }
                    });
                },
            )
            .size_full(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn the_art_is_whole_and_every_mark_has_a_color() {
        let all = ART
            .iter()
            .chain(&EYES_OPEN)
            .chain(&EYES_CLOSED)
            .chain(&EYES_HAPPY)
            .chain(&EYES_FOCUSED)
            .chain(&EYES_CHEER)
            .chain(&LEGS_STAND)
            .chain(&LEGS_LEFT)
            .chain(&LEGS_RIGHT)
            .chain(&HEART)
            .chain(&ZED)
            .chain(&FLICK)
            .chain(&TAIL_WAG)
            .chain(&SPARKLE)
            .chain(&NOTE)
            .chain(&EXCLAIM);
        for row in all {
            assert!(row.chars().all(|mark| mark == '.' || color(mark).is_some()), "{row}");
        }
        assert!(ART.iter().all(|row| row.len() == WIDTH) && LEGS_STAND.iter().chain(&LEGS_LEFT).chain(&LEGS_RIGHT).all(|row| row.len() == WIDTH));
        // The art's own legs are the standing ones, and the eyes (the right one mirrored) sit in skin they can move into.
        assert_eq!(&ART[LEG_ROW..], &LEGS_STAND[..]);
        for (column, mirrored) in EYES_AT {
            for y in EYE_ROW..EYE_ROW + EYE_HEIGHT {
                let row = ART[y].as_bytes();
                assert_eq!((row[column - 1], row[column + EYE_WIDTH]), (b'f', b'f'), "row {y}");
                let open = EYES_OPEN[y - EYE_ROW];
                let open: String = if mirrored { open.chars().rev().collect() } else { open.to_string() };
                assert_eq!(&ART[y][column..column + EYE_WIDTH], open, "row {y}");
            }
        }
        assert!(EYES_OPEN.iter().chain(&EYES_CLOSED).chain(&EYES_HAPPY).chain(&EYES_FOCUSED).chain(&EYES_CHEER).all(|row| row.len() == EYE_WIDTH));
        // The ear's twitch, tail wag and floating sprites stay inside her width.
        assert!(FLICK.iter().all(|row| row.len() == FLICK[0].len()) && FLICK_AT.0 + FLICK[0].len() <= WIDTH);
        assert!(TAIL_WAG.iter().all(|row| row.len() == TAIL_WAG[0].len()) && TAIL_AT.0 + TAIL_WAG[0].len() <= WIDTH && TAIL_AT.1 + TAIL_WAG.len() <= HEIGHT);
        assert!(30 + HEART[0].len() <= WIDTH && 36 + ZED[0].len() <= WIDTH);
        assert!(6 + SPARKLE[0].len() <= WIDTH && 31 + NOTE[0].len() <= WIDTH && 37 + EXCLAIM[0].len() <= WIDTH);
    }

    #[test]
    fn she_strolls_out_and_back_and_blinks_now_and_then() {
        let poses: Vec<Pose> = (0..300).map(|step| pose(step as f32 * 0.1, f32::NEG_INFINITY, false, None)).collect();
        assert!(poses.iter().any(|pose| pose.stroll >= STROLL - 0.01));
        assert_eq!(poses[0].stroll, 0.0);
        assert!(poses.iter().any(|pose| pose.legs != Legs::Stand));
        let blinks = poses.iter().filter(|pose| pose.eyes == Eyes::Closed).count();
        assert!((4..=20).contains(&blinks), "{blinks}");
        // Walking, she looks where she goes.
        let out = pose(11.0, f32::NEG_INFINITY, false, None);
        assert_eq!((out.glance, out.eyes), (-1, Eyes::Open));
        // Standing, she looks towards the pointer.
        assert_eq!(pose(5.0, f32::NEG_INFINITY, false, Some(40.0)).glance, 1);
    }

    #[test]
    fn pointed_at_she_hops_with_a_heart_and_asleep_she_keeps_still() {
        let hop = pose(20.2, 20.0, false, None);
        assert_eq!(hop.eyes, Eyes::Happy);
        assert!(hop.lift > HOP * 0.8 && hop.heart.is_some_and(|rise| rise < 0.2));
        assert!(pose(22.0, 20.0, false, None).heart.is_none());
        let asleep = pose(20.2, 20.0, true, Some(40.0));
        assert_eq!((asleep.eyes, asleep.lift, asleep.heart, asleep.glance), (Eyes::Closed, 0.0, None, 0));
        // Looking aside moves the eye one pixel and leaves skin where it was.
        let aside = Pose { glance: -1, ..pose(5.0, f32::NEG_INFINITY, false, None) };
        let (left, _) = EYES_AT[0];
        assert_eq!(mark(&aside, left - 1, EYE_ROW), EYES_OPEN[0].as_bytes()[0] as char);
        assert_eq!(mark(&aside, left + EYE_WIDTH - 1, EYE_ROW), 'f');
        // The right eye is the left one mirrored.
        let ahead = pose(5.0, f32::NEG_INFINITY, false, None);
        let (right, _) = EYES_AT[1];
        assert_eq!(mark(&ahead, right + EYE_WIDTH - 1, EYE_ROW + 2), mark(&ahead, left, EYE_ROW + 2));
    }

    #[test]
    fn typing_pat_and_ai_states_drive_distinct_poses() {
        let typing = pose_with(PetContext { clock: 11.2, typing_age: Some(0.3), ..PetContext::default() });
        assert_eq!(typing.eyes, Eyes::Focused);
        assert_eq!(typing.legs, Legs::Stand);
        assert_eq!(typing.glance, -1);

        let excited = pose_with(PetContext { clock: 4.0, excited_age: Some(0.35), ..PetContext::default() });
        assert_eq!(excited.eyes, Eyes::Cheer);
        assert!(excited.lift > HOP * 0.5 && excited.sparkle.is_some());

        let celebrate = pose_with(PetContext { clock: 4.0, celebrate_age: Some(0.25), ..PetContext::default() });
        assert_eq!(celebrate.eyes, Eyes::Cheer);
        assert!(celebrate.note.is_some() && celebrate.lift > 0.0);

        let waiting = pose_with(PetContext { clock: 4.0, activity: PetActivity::WaitingApproval, compact: true, ..PetContext::default() });
        assert_eq!(waiting.stroll, 0.0);
        assert!(waiting.exclaim && waiting.flick && waiting.eyes == Eyes::Focused);
    }

    #[test]
    fn mini_pet_stops_when_unseen_or_idle_or_reduced_motion() {
        assert!(mini_tick_rule(true, true, true, true));
        assert!(!mini_tick_rule(true, true, false, true));
        assert!(!mini_tick_rule(true, false, true, true));
        assert!(!mini_tick_rule(false, true, true, true));
        assert!(!mini_tick_rule(true, true, true, false));
    }

    #[test]
    fn a_pixel_is_whole_screen_pixels_and_never_larger_than_planned() {
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 3.0] {
            let unit = unit(scale);
            assert!(unit <= PIXEL + 1e-6, "{scale}");
            assert!(((unit * scale) - (unit * scale).round()).abs() < 1e-4, "{scale}");
        }
        // Two screen pixels to a pixel of the art at 150 % and at 200 %.
        assert!((unit(1.5) * 1.5 - 2.0).abs() < 1e-4 && (unit(2.0) * 2.0 - 2.0).abs() < 1e-4);
    }
}
