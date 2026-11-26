//! The pixel pet on the edge of a new chat's composer: a small white-haired cat girl with a leaf clip and a sailor
//! collar, drawn after the user's own avatar as 26 x 30 pixel art. She blinks, twitches an ear and looks towards the
//! pointer; now and then she strolls a few steps along the composer and back; pointed at, she hops and a heart rises;
//! while the page's clock is paused she sleeps. Everything follows the page's clock ([`Backdrop`]), so she costs
//! nothing while it stands still.

use gpui_kit::*;

use super::backdrop::{Backdrop, motion_allowed};

const WIDTH: usize = 26;
const HEIGHT: usize = 30;
/// A pixel of the art in window pixels at most: it is drawn as a whole number of the screen's pixels, so its edges
/// stay sharp at any scaling.
pub const PIXEL: f32 = 2.0;
/// How far she strolls to the left of her place, and how high she hops, in pixels of the art.
const STROLL: f32 = 28.0;
const HOP: f32 = 4.0;

/// `o` outline, `w`/`s`/`S` hair, `p`/`P` inner ear, `f`/`F` skin, `d`/`e`/`E`/`h` eyes, `b` blush, `m` mouth,
/// `g`/`G` leaf clip, `c`/`C`/`n` collar and skirt, `u` shirt, `k` bow.
const ART: [&str; HEIGHT] = [
    "..........................",
    "..o...........oo.......o..",
    "..oo.........owo......oo..",
    ".opo........owo.......opo.",
    ".oppo.......o........oppo.",
    ".oPppo.oooooooooooo.oppPo.",
    ".oPpppowwwwwwwwwwwwopppPo.",
    ".oppppwwwwwwwwwwwwwwppppo.",
    "oppPwwwwwwwwwwwwwwwggwPppo",
    "owwwwwwwwwwSwwSwwwgGgwwwwo",
    "owwwwwwwwwwwwwwwwwwwGgwwwo",
    "owwwSwwwwSwwwwwwSwwgwSwwwo",
    "owwSfSwwSffSwwSffSwwSfSwwo",
    "owsfffffffffFFfffffffffswo",
    "owsffdddffffffffffdddffswo",
    "owsffheeffffffffffheeffswo",
    "owsffedeffffffffffedeffswo",
    "owsffEEEffffffffffEEEffswo",
    "owsfbbbffffffffffffbbbfswo",
    "owswffffffffmmffffffffwswo",
    "owsSwfffffffFFfffffffwSswo",
    "owsS.offffffffffffffo.Sswo",
    ".osS..onccccccccccno..Sso.",
    ".owS.ocnnccukkuccnnco.Swo.",
    ".owS.ocuuuukkkkuuuuco.Swo.",
    "..oS.ofuuuuukkuuuuufo.So..",
    "..oo.ooCCCCCCCCCCCCoo.oo..",
    "......oCcCcCccCcCcCo......",
    ".........FF....FF.........",
    "........ooo....ooo........",
];
/// The eyes: 3 x 4 blocks whose left column is 5 and 18 on rows 14 to 17, open, closed and smiling.
const EYE_ROW: usize = 14;
const EYE_COLUMNS: [usize; 2] = [5, 18];
const EYES_OPEN: [&str; 4] = ["ddd", "hee", "ede", "EEE"];
const EYES_CLOSED: [&str; 4] = ["fff", "fff", "ddd", "fff"];
const EYES_HAPPY: [&str; 4] = ["fff", "fdf", "dfd", "fff"];
/// The legs (the last two rows) standing and in the two steps of a walk.
const LEGS_STAND: [&str; 2] = [".........FF....FF.........", "........ooo....ooo........"];
const LEGS_LEFT: [&str; 2] = [".........oo....FF.........", "...............ooo........"];
const LEGS_RIGHT: [&str; 2] = [".........FF....oo.........", "........ooo..............."];
/// The right ear's tip, bent out for a twitch (rows 1 and 2 from column 22).
const FLICK: [&str; 2] = ["..o", ".oo"];
const HEART: [&str; 6] = [".rr.rr.", "rrrrrrr", "rrrrrrr", ".rrrrr.", "..rrr..", "...r..."];
const ZED: [&str; 4] = ["zzz", "..z", ".z.", "zzz"];

fn color(mark: char) -> Option<Hsla> {
    let rgb = |r: u8, g: u8, b: u8| Some(Hsla::from(Rgba { r: f32::from(r) / 255.0, g: f32::from(g) / 255.0, b: f32::from(b) / 255.0, a: 1.0 }));
    match mark {
        'o' => rgb(88, 92, 138),
        'w' => rgb(246, 247, 255),
        's' => rgb(205, 210, 245),
        'S' => rgb(178, 184, 232),
        'p' => rgb(245, 168, 188),
        'P' => rgb(255, 214, 224),
        'f' => rgb(255, 238, 230),
        'F' => rgb(236, 196, 188),
        'd' => rgb(150, 40, 72),
        'e' => rgb(226, 86, 118),
        'E' => rgb(246, 150, 168),
        'h' | 'u' | 'n' => rgb(255, 255, 255),
        'b' => rgb(255, 182, 196),
        'm' => rgb(196, 100, 126),
        'g' => rgb(104, 184, 100),
        'G' => rgb(58, 132, 70),
        'c' => rgb(122, 178, 242),
        'C' => rgb(74, 130, 212),
        'k' => rgb(66, 112, 196),
        'r' => rgb(236, 84, 116),
        'z' => rgb(132, 136, 186),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eyes {
    Open,
    Closed,
    Happy,
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
    /// The heart's rise, 0 to 1, after she was pointed at.
    pub heart: Option<f32>,
    pub asleep: bool,
}

/// Her pose at `clock` (the page's clock, seconds). `poked`: when she was last pointed at, on the same clock;
/// `asleep`: the clock is paused; `pointer`: how far right of her middle the pointer is, in pixels of the art.
pub fn pose(clock: f32, poked: f32, asleep: bool, pointer: Option<f32>) -> Pose {
    // A stroll every 30 seconds: out to the left in 3 s, a look around, and back.
    let t = clock.rem_euclid(30.0);
    let (stroll, walking) = match t {
        t if t < 10.0 => (0.0, None),
        t if t < 13.0 => ((t - 10.0) / 3.0 * STROLL, Some(-1)),
        t if t < 20.0 => (STROLL, None),
        t if t < 23.0 => ((23.0 - t) / 3.0 * STROLL, Some(1)),
        _ => (0.0, None),
    };
    if asleep {
        return Pose { stroll, lift: 0.0, eyes: Eyes::Closed, glance: 0, legs: Legs::Stand, flick: false, heart: None, asleep: true };
    }
    let step = (t * 4.0) as i32 % 2 == 0;
    let (legs, mut lift) = match walking {
        Some(_) if step => (Legs::Left, 1.0),
        Some(_) => (Legs::Right, 0.0),
        None => (Legs::Stand, 0.0),
    };
    let looking_around = if (15.0..16.5).contains(&t) { 1 } else if (16.5..18.0).contains(&t) { -1 } else { 0 };
    let towards_pointer = pointer.map_or(0, |dx| if dx < -6.0 { -1 } else if dx > 6.0 { 1 } else { 0 });
    let glance = walking.unwrap_or(if towards_pointer != 0 { towards_pointer } else { looking_around });
    let since = clock - poked;
    let happy = (0.0..1.2).contains(&since);
    if (0.0..0.45).contains(&since) {
        lift += HOP * (std::f32::consts::PI * since / 0.45).sin();
    }
    let blinking = (clock + 0.7).rem_euclid(3.7) < 0.15;
    let eyes = if happy { Eyes::Happy } else if blinking && walking.is_none() { Eyes::Closed } else { Eyes::Open };
    Pose {
        stroll,
        lift,
        eyes,
        glance: if happy { 0 } else { glance },
        legs,
        flick: (clock + 2.3).rem_euclid(6.1) < 0.2,
        heart: happy.then_some(since / 1.2),
        asleep: false,
    }
}

/// The mark of the art at column `x`, row `y` in `pose`.
fn mark(pose: &Pose, x: usize, y: usize) -> char {
    if (EYE_ROW..EYE_ROW + 4).contains(&y) {
        for column in EYE_COLUMNS {
            // The eye and the skin around it, which the eye moves into when it looks aside.
            if (column - 1..=column + 3).contains(&x) {
                let block = match pose.eyes {
                    Eyes::Open => EYES_OPEN,
                    Eyes::Closed => EYES_CLOSED,
                    Eyes::Happy => EYES_HAPPY,
                };
                let offset = x as i32 - column as i32 - pose.glance;
                return if (0..3).contains(&offset) { block[y - EYE_ROW].as_bytes()[offset as usize] as char } else { 'f' };
            }
        }
    }
    if y >= HEIGHT - 2 {
        let legs = match pose.legs {
            Legs::Stand => LEGS_STAND,
            Legs::Left => LEGS_LEFT,
            Legs::Right => LEGS_RIGHT,
        };
        return legs[y - (HEIGHT - 2)].as_bytes()[x] as char;
    }
    if pose.flick && (1..=2).contains(&y) && x >= 22 && x < 25 {
        return FLICK[y - 1].as_bytes()[x - 22] as char;
    }
    ART[y].as_bytes()[x] as char
}

/// The size of a pixel of the art on a screen with `scale` physical pixels per window pixel.
pub fn unit(scale: f32) -> f32 {
    (PIXEL * scale).floor().max(1.0) / scale
}

/// The box she lives in: her place on the right, the stroll to the left, room to hop above.
pub fn box_size() -> (f32, f32) {
    ((WIDTH as f32 + STROLL) * PIXEL, row_height())
}

/// The height of the row she stands in.
pub fn row_height() -> f32 {
    (HEIGHT as f32 + HOP) * PIXEL
}

/// The pet, standing with her feet on the bottom of her box.
pub fn render(backdrop: &Entity<Backdrop>, window: &Window, cx: &App) -> impl IntoElement + use<> {
    let page = backdrop.read(cx);
    let motion = motion_allowed();
    let asleep = motion && page.dozing(window);
    let (clock, poked) = if motion { (page.clock(), page.poked()) } else { (0.0, f32::NEG_INFINITY) };
    let pointer = page.pointer();
    let (width, height) = box_size();
    let weak = backdrop.downgrade();
    div().id("optics-pet").w(px(width)).h(px(height)).child(
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let scale = window.scale_factor();
                let unit = unit(scale);
                let snap = |value: f32| (value * scale).round() / scale;
                // Where she would stand, to know where the pointer is from her middle.
                let home = f32::from(bounds.right()) - WIDTH as f32 * unit;
                let middle = home + WIDTH as f32 * unit / 2.0;
                let near = pointer.map(|point| (f32::from(point.x) - middle) / unit).filter(|dx| dx.abs() < 160.0);
                let pose = pose(clock, poked, asleep, near);
                let left = snap(home - pose.stroll * unit);
                let top = snap(f32::from(bounds.bottom()) - (HEIGHT as f32 + pose.lift) * unit);
                let cell = |x: f32, y: f32| Bounds::new(point(px(left + x * unit), px(top + y * unit)), size(px(unit), px(unit)));
                let picture = |window: &mut Window, rows: &[&str], x: f32, y: f32, alpha: f32| {
                    for (dy, row) in rows.iter().enumerate() {
                        for (dx, mark) in row.chars().enumerate() {
                            if let Some(shade) = color(mark) {
                                window.paint_quad(fill(cell(x + dx as f32, (y + dy as f32).round()), shade.opacity(alpha)));
                            }
                        }
                    }
                };
                // One layer for her few hundred pixels and what floats over her head: one place in the frame's order.
                let reach = Bounds::new(point(px(left - unit), px(top - 14.0 * unit)), size(px((WIDTH as f32 + 4.0) * unit), px((HEIGHT as f32 + 14.0) * unit)));
                window.paint_layer(reach, |window| {
                    for y in 0..HEIGHT {
                        for x in 0..WIDTH {
                            if let Some(shade) = color(mark(&pose, x, y)) {
                                window.paint_quad(fill(cell(x as f32, y as f32), shade));
                            }
                        }
                    }
                    if let Some(rise) = pose.heart {
                        picture(window, &HEART, 18.0, -7.0 - rise * 4.0, 1.0 - rise * rise);
                    }
                    if pose.asleep {
                        picture(window, &ZED, 24.0, -6.0, 1.0);
                    }
                });
                // Pointing at her (not at the empty path beside her) makes her hop.
                let body = Bounds::new(point(px(left), px(top)), size(px(WIDTH as f32 * unit), px(HEIGHT as f32 * unit)));
                let weak = weak.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if !phase.bubble() {
                        return;
                    }
                    let inside = body.contains(&event.position);
                    let _ = weak.update(cx, |page, cx| page.pet_hovered(inside, window, cx));
                });
            },
        )
        .size_full(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn the_art_is_whole_and_every_mark_has_a_color() {
        let all = ART.iter().chain(&EYES_OPEN).chain(&EYES_CLOSED).chain(&EYES_HAPPY).chain(&LEGS_STAND).chain(&LEGS_LEFT).chain(&LEGS_RIGHT).chain(&HEART).chain(&ZED).chain(&FLICK);
        for row in all {
            assert!(row.chars().all(|mark| mark == '.' || color(mark).is_some()), "{row}");
        }
        assert!(ART.iter().all(|row| row.len() == WIDTH) && LEGS_STAND.iter().chain(&LEGS_LEFT).chain(&LEGS_RIGHT).all(|row| row.len() == WIDTH));
        // The art's own legs are the standing ones, and the eyes sit in skin they can move into.
        assert_eq!([ART[HEIGHT - 2], ART[HEIGHT - 1]], LEGS_STAND);
        for column in EYE_COLUMNS {
            for y in EYE_ROW..EYE_ROW + 4 {
                let row = ART[y].as_bytes();
                assert_eq!((row[column - 1], row[column + 3]), (b'f', b'f'), "row {y}");
                assert_eq!(&ART[y][column..column + 3], EYES_OPEN[y - EYE_ROW], "row {y}");
            }
        }
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
        assert_eq!(mark(&aside, 4, EYE_ROW), 'd');
        assert_eq!(mark(&aside, 7, EYE_ROW), 'f');
    }

    #[test]
    fn a_pixel_is_whole_screen_pixels_and_never_larger_than_planned() {
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 3.0] {
            let unit = unit(scale);
            assert!(unit <= PIXEL + 1e-6, "{scale}");
            assert!(((unit * scale) - (unit * scale).round()).abs() < 1e-4, "{scale}");
        }
        assert!((unit(2.25) * 2.25 - 4.0).abs() < 1e-4);
    }
}
