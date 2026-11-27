//! The pixel pet on the edge of a new chat's composer: a small white-haired cat girl with an amber hair clip set with
//! a blue gem, a blue sailor jacket, an orange scarf and a white tail, as 44 x 54 pixel art. She blinks, twitches an
//! ear and looks towards the pointer; now and then she strolls a few steps along the composer and back; pointed at,
//! she hops and a heart rises; while the page's clock is paused she sleeps. Everything follows the page's clock
//! ([`Backdrop`]), so she costs nothing while it stands still.

use gpui_kit::*;

use super::backdrop::{Backdrop, motion_allowed};

const WIDTH: usize = 44;
const HEIGHT: usize = 54;
/// A pixel of the art in window pixels at most: it is drawn as a whole number of the screen's pixels, so its edges
/// stay sharp at any scaling (two of them at 150 % and at 200 %).
pub const PIXEL: f32 = 1.34;
/// How far she strolls to the left of her place, and how high she hops, in pixels of the art.
const STROLL: f32 = 48.0;
const HOP: f32 = 7.0;

/// `o` outline, `w`/`s`/`S` hair, `q`/`l` its pink and blue tips, `p`/`P` inner ear, `y`/`Y` amber clip and scarf,
/// `x` the scarf's star, `j`/`J` gem, `f`/`F` skin, `D`/`d`/`v`/`i`/`I`/`h` eyes, `b`/`B` blush, `m` mouth,
/// `c`/`C` jacket, `k`/`K` collar, `n` its stripe and the cuffs, `u` frill and socks.
const ART: [&str; HEIGHT] = [
    "...................ooooo....................",
    "..................oyyyyYo...................",
    ".........o........oyJjyYo.........o.........",
    "........owo.......oyJjyYo........owo........",
    "........owwoo.....oyJjyYo......oowwo........",
    "........owpwwo....oyjjyYo.....owwpwo........",
    "........owppwwo...oyjjyYo....owwppwo........",
    "........owPppwwooooyjjyYooooowwppPwo........",
    "........owPPpppwwwwyyyyYwwwwwpppPPwo........",
    "........owPPppppwwwyyyyYwwwwppppPPwo........",
    "........owPppppwwwwwsssswwwwwppppPwo........",
    "........owpppwwwwwwwwwwwwwwwwwwpppwo........",
    "........owpwwwwwwwwwwwwwwwwwwwwwwpwo........",
    "........owwwwwwwwwwwwwwwwwwwwwwwwwwo........",
    ".........owwwwwwwwwwwwwwwwwwwwsssso.........",
    "........owwwwwwwwwwwwwwwwwwwwwssssso........",
    "........owwwwwwwwwwwwwwwwwwwwwssssso........",
    "........owwwwwwwwwwwwwwwwwwwwwssssso........",
    ".......owwwswwwwwwwwwwwwwwwwwwsswssso.......",
    ".......owwwswswwwwwwwwwwwwwwwwsswssso.......",
    ".......owwwsSfSwwwsswwwwsswwwSfSwssso.......",
    ".......owwwsfffSwSffSwwSffSwSfffwssso.......",
    "........owwsffffSfffSwwSfffSffffwsso........",
    "........owwsfDDDDfffSwwSfffDDDDfwsso........",
    "........owwsfdvvvDfffSSfffDvvvdfwsso........",
    "........owwsfvhhvvfffSSfffvvhhvfwsso........",
    "........owwsfvhvviffffffffivvhvfwsso........",
    "........owwsfiiiIiffffffffiIiiifwsso........",
    "........owwsffIIIffffffffffIIIffwsso........",
    "........owwsSbBbBffffffffffBbBbSwsso........",
    "........owwswSbbfffffmmfffffbbSswsso........",
    "........owwswwSffffffffffffffSsswsso........",
    "........owwswwwwSfffFFFFfffSwwsswsso........",
    ".........owsssskknKKuuuuKKnkkssswsso...o....",
    ".........owssssccnKKuuuuKKnccssswsso..olo...",
    ".........owsssccccnKuuuuKnccccsswsso.olllo..",
    ".........owssscccknyyxxyynkcccsswso...ollo..",
    ".........oqqscccckkyyxxyykkccccsllo...osso..",
    ".........oqqccccckkkYyyYkkkcccccllo..owsso..",
    ".........oqqcccccckkyyyykkccccccllo..owso...",
    ".........oqqccccccccyYYyccccccccllo..owso...",
    ".........oqnnncccccccyycccccccnnnlooowwso...",
    ".........oqnnnnccccccccccccccnnnnlowwwwo....",
    "..........offffccccccccccccccfffflssssso....",
    "...........offfccccccccccccccfffSssssso.....",
    "...........offfCCCCCCCCCCCCCCfffosssoo......",
    "...........oCCCCCfffCCCCfffCCCCCoooo........",
    "............ooooofffoooofffooooo............",
    "................offfo..offfo................",
    "................ouuuo..ouuuo................",
    "...............okkkkkookkkkko...............",
    "...............okkkkkookkkkko...............",
    "...............okkkkkookkkkko...............",
    "................ooooo..ooooo................",
];
/// The eyes: 5 x 6 blocks on rows 23 to 28, the left one from column 13, the right one from column 26 and drawn
/// mirrored; open, closed and smiling.
const EYE_ROW: usize = 23;
const EYES_AT: [(usize, bool); 2] = [(13, false), (26, true)];
const EYE_WIDTH: usize = 5;
const EYE_HEIGHT: usize = 6;
const EYES_OPEN: [&str; EYE_HEIGHT] = [
    "DDDDf",
    "dvvvD",
    "vhhvv",
    "vhvvi",
    "iiiIi",
    "fIIIf",
];
const EYES_CLOSED: [&str; EYE_HEIGHT] = [
    "fffff",
    "fffff",
    "fffff",
    "DfffD",
    "fDDDf",
    "fffff",
];
const EYES_HAPPY: [&str; EYE_HEIGHT] = [
    "fffff",
    "fffff",
    "fDDDf",
    "DfffD",
    "fffff",
    "fffff",
];
/// The legs (the rows from `LEG_ROW` down) standing and in the two steps of a walk.
const LEG_ROW: usize = 48;
const LEGS_STAND: [&str; HEIGHT - LEG_ROW] = [
    "................offfo..offfo................",
    "................ouuuo..ouuuo................",
    "...............okkkkkookkkkko...............",
    "...............okkkkkookkkkko...............",
    "...............okkkkkookkkkko...............",
    "................ooooo..ooooo................",
];
const LEGS_LEFT: [&str; HEIGHT - LEG_ROW] = [
    "................ouuuo..offfo................",
    "...............okkkkko.ouuuo................",
    "...............okkkkkookkkkko...............",
    "...............okkkkkookkkkko...............",
    "................ooooo.okkkkko...............",
    ".......................ooooo................",
];
const LEGS_RIGHT: [&str; HEIGHT - LEG_ROW] = [
    "................offfo..ouuuo................",
    "................ouuuo.okkkkko...............",
    "...............okkkkkookkkkko...............",
    "...............okkkkkookkkkko...............",
    "...............okkkkko.ooooo................",
    "................ooooo.......................",
];
/// The right ear's top bent outwards for a twitch: these rows from column 30, row 2.
const FLICK_AT: (usize, usize) = (30, 2);
const FLICK: [&str; 4] = [
    "....oo.",
    "...oowo",
    ".ooowwo",
    ".owwpwo",
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

fn color(mark: char) -> Option<Hsla> {
    let rgb = |r: u8, g: u8, b: u8| Some(Hsla::from(Rgba { r: f32::from(r) / 255.0, g: f32::from(g) / 255.0, b: f32::from(b) / 255.0, a: 1.0 }));
    match mark {
        'o' => rgb(70, 72, 120),
        'w' => rgb(250, 250, 255),
        's' => rgb(218, 220, 248),
        'S' => rgb(186, 190, 232),
        'q' => rgb(250, 196, 220),
        'l' => rgb(176, 208, 250),
        'p' => rgb(168, 206, 248),
        'P' => rgb(216, 236, 255),
        'y' => rgb(250, 182, 70),
        'Y' => rgb(222, 138, 46),
        'x' => rgb(255, 232, 124),
        'j' => rgb(60, 138, 236),
        'J' => rgb(160, 216, 255),
        'f' => rgb(255, 241, 234),
        'F' => rgb(246, 210, 200),
        'D' => rgb(96, 34, 70),
        'd' => rgb(176, 50, 88),
        'v' => rgb(50, 60, 158),
        'i' => rgb(98, 124, 230),
        'I' => rgb(164, 192, 254),
        'h' | 'u' | 'n' => rgb(255, 255, 255),
        'b' => rgb(255, 190, 204),
        'B' => rgb(244, 150, 174),
        'm' => rgb(206, 102, 124),
        'c' => rgb(100, 150, 234),
        'C' => rgb(70, 110, 205),
        'k' => rgb(40, 56, 140),
        'K' => rgb(64, 84, 170),
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
    let towards_pointer = pointer.map_or(0, |dx| if dx < -10.0 { -1 } else if dx > 10.0 { 1 } else { 0 });
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
    if (EYE_ROW..EYE_ROW + EYE_HEIGHT).contains(&y) {
        for (column, mirrored) in EYES_AT {
            // The eye and the skin around it, which the eye moves into when it looks aside.
            if (column - 1..=column + EYE_WIDTH).contains(&x) {
                let block = match pose.eyes {
                    Eyes::Open => EYES_OPEN,
                    Eyes::Closed => EYES_CLOSED,
                    Eyes::Happy => EYES_HAPPY,
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
                let near = pointer.map(|point| (f32::from(point.x) - middle) / unit).filter(|dx| dx.abs() < 300.0);
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
                // One layer for her pixels and what floats over her head: one place in the frame's order.
                let reach = Bounds::new(point(px(left - unit), px(top - 20.0 * unit)), size(px((WIDTH as f32 + 2.0) * unit), px((HEIGHT as f32 + 20.0) * unit)));
                window.paint_layer(reach, |window| {
                    // A run of one colour along a row is one quad: a third of the quads, the same pixels.
                    for y in 0..HEIGHT {
                        let mut x = 0;
                        while x < WIDTH {
                            let here = mark(&pose, x, y);
                            let mut end = x + 1;
                            while end < WIDTH && mark(&pose, end, y) == here {
                                end += 1;
                            }
                            if let Some(shade) = color(here) {
                                let run = Bounds::new(point(px(left + x as f32 * unit), px(top + y as f32 * unit)), size(px((end - x) as f32 * unit), px(unit)));
                                window.paint_quad(fill(run, shade));
                            }
                            x = end;
                        }
                    }
                    if let Some(rise) = pose.heart {
                        picture(window, &HEART, 30.0, -9.0 - rise * 6.0, 1.0 - rise * rise);
                    }
                    if pose.asleep {
                        picture(window, &ZED, 36.0, -6.0, 1.0);
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
        assert!(EYES_OPEN.iter().chain(&EYES_CLOSED).chain(&EYES_HAPPY).all(|row| row.len() == EYE_WIDTH));
        // The ear's twitch and what floats over her stay inside her width.
        assert!(FLICK.iter().all(|row| row.len() == FLICK[0].len()) && FLICK_AT.0 + FLICK[0].len() <= WIDTH);
        assert!(30 + HEART[0].len() <= WIDTH && 36 + ZED[0].len() <= WIDTH);
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
