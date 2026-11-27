//! The optical look (Settings → Appearance): behind a new chat in Agent mode a field of drifting characters and a
//! glow coming from behind the composer, a usage overview, a pixel pet on the composer; glass for the card and the
//! composer; colored reasoning levels and a reasoning slider in the model card.
//!
//! Rainy draws all of it itself, from its own data: no web view, no extra process. The only thing that moves on its
//! own is the page's clock ([`backdrop`]), which stops whenever nobody is watching. Contrast is computed, not hoped
//! for: [`look`] caps the glow and the glass by the text on them.

pub mod backdrop;
pub mod look;
pub mod pet;
pub mod slider;
pub mod usage;

use std::sync::OnceLock;

use gpui_kit::*;

use crate::store::Store;
use backdrop::Backdrop;
use look::Look;

/// Whether the optical look is chosen.
pub fn enabled(cx: &App) -> bool {
    Store::global(cx).read(cx).prefs.optics
}

/// Give `element` the look of glass: a translucent fill (as opaque as its text needs, see [`Look`]), a fine border,
/// a bright line along the top edge and a thin contact shadow. It becomes the anchor of the line, so it is positioned.
/// No wide soft shadow: GPUI orders a shadow by its unblurred box ([`paint_lit`]), so its blur would be drawn over
/// what stands next to the glass (the pet, a suggestion list), and on the dark page it was hardly seen anyway.
pub fn glaze<E: Styled + ParentElement>(element: E, look: &Look, radius: Pixels) -> E {
    let highlight = look.glass_highlight;
    let line = |from: Hsla, to: Hsla| div().flex_1().h_full().bg(linear_gradient(90.0, linear_color_stop(from, 0.0), linear_color_stop(to, 1.0)));
    let clear = highlight.opacity(0.0);
    element
        .relative()
        .rounded(radius)
        .bg(look.glass)
        .border_1()
        .border_color(look.glass_border)
        .shadow(vec![BoxShadow::new(px(0.), px(1.), look.shadow.opacity(0.6)).blur_radius(px(2.))])
        .child(div().absolute().top_0().left(radius).right(radius).h(px(1.)).flex().child(line(clear, highlight)).child(line(highlight, clear)))
}

/// A glass panel laid out as a column.
pub fn glass(look: &Look, radius: Pixels) -> Div {
    glaze(div().flex().flex_col(), look, radius)
}

/// Where the glow's box starts below the composer's top edge, and how far it reaches past its sides and bottom.
const GLOW_TOP: f32 = 36.0;
const GLOW_SIDES: f32 = 72.0;
const GLOW_BOTTOM: f32 = -28.0;
/// The glow's three lights: horizontal and vertical offset, blur (GPUI blurs with a Gaussian of this sigma) and spread.
const GLOW_LIGHTS: [(f32, f32, f32, f32); 3] = [(-150.0, 10.0, 64.0, -12.0), (0.0, 24.0, 72.0, 0.0), (150.0, 10.0, 64.0, -12.0)];
/// Active breathing modulates glow alpha strictly downward (`<= 1.0`), so WCAG contrast budgets are never exceeded.
pub const GLOW_BREATH_MIN: f32 = 0.88;
pub const GLOW_BREATH_MAX: f32 = 1.0;
/// Maximum horizontal pointer parallax applied to the composer glow (purely horizontal, preserving vertical clearance).
pub const GLOW_PARALLAX_MAX: f32 = 18.0;
/// The gap under the greeting. With the pet's row it keeps the greeting this far above the composer when the usage
/// overview is left out ([`greeting_clearance`]).
pub const GREETING_GAP: f32 = 12.0;

/// How far the greeting's text is at least above the composer's top edge.
pub fn greeting_clearance() -> f32 {
    GREETING_GAP + pet::row_height()
}

/// The share of the glow's peak that reaches text `clearance` above the composer: the tail beyond the top edge of the
/// nearest light, which is at most what an endless edge gives.
pub fn glow_reach(clearance: f32) -> f32 {
    GLOW_LIGHTS
        .iter()
        .map(|(_, dy, blur, spread)| {
            let distance = clearance + GLOW_TOP + dy - spread;
            0.5 * erfc(distance / (blur * std::f32::consts::SQRT_2))
        })
        .fold(0.0, f32::max)
}

/// The complementary error function (Abramowitz and Stegun 7.1.26, within 1.5e-7).
fn erfc(x: f32) -> f32 {
    if x < 0.0 {
        return 2.0 - erfc(-x);
    }
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t * (0.254_829_6 + t * (-0.284_496_7 + t * (1.421_413_7 + t * (-1.453_152 + t * 1.061_405_4))));
    poly * (-x * x).exp()
}

/// Builds the three stack-allocated drop shadows for the composer glow, clamping `breath` to `[GLOW_BREATH_MIN, 1.0]`
/// and horizontal `parallax_x` to `[-GLOW_PARALLAX_MAX, GLOW_PARALLAX_MAX]`.
fn glow_shadows(look: &Look, breath: f32, parallax_x: f32) -> [BoxShadow; 3] {
    let breath = breath.clamp(GLOW_BREATH_MIN, GLOW_BREATH_MAX);
    let shift = parallax_x.clamp(-GLOW_PARALLAX_MAX, GLOW_PARALLAX_MAX);
    std::array::from_fn(|i| {
        let (dx, dy, blur, spread) = GLOW_LIGHTS[i];
        let color = look.glow[i];
        let modulated = Hsla { a: color.a * breath, ..color };
        BoxShadow::new(px(dx + shift), px(dy), modulated).blur_radius(px(blur)).spread_radius(px(spread))
    })
}

/// The glow behind the composer: blue, violet and pink light (for a blue accent) spreading from under it, lower and
/// wider than it. Placed first in the composer's positioned wrapper, so the composer is drawn over its brightest part;
/// the page paints that wrapper before everything above it, so nothing else is drawn under the light.
#[allow(dead_code)]
pub fn glow(look: &Look) -> impl IntoElement {
    glow_reactive(look, 1.0, 0.0)
}

/// Reactive variant of [`glow`]: applies subtle downward alpha breathing (`0.88..=1.0`) and horizontal pointer parallax
/// (`±18px`) without allocating a heap `Vec`.
pub fn glow_reactive(look: &Look, breath: f32, parallax_x: f32) -> impl IntoElement {
    let lights = glow_shadows(look, breath, parallax_x);
    div()
        .absolute()
        .top(px(GLOW_TOP))
        .bottom(px(GLOW_BOTTOM))
        .left(px(GLOW_SIDES))
        .right(px(GLOW_SIDES))
        .child(canvas(|_, _, _| (), move |bounds, (), window, _| paint_lit(window, bounds, &lights)).size_full())
}

/// Convenience wrapper deriving [`glow_reactive`] parameters directly from an active [`Backdrop`].
pub fn glow_for(look: &Look, backdrop: &Backdrop, window: &Window) -> impl IntoElement {
    glow_reactive(look, backdrop.glow_breath(window), backdrop.glow_parallax_x())
}

/// Paint drop shadows so that whatever is painted after them over their blur is drawn over it. GPUI gives a shadow its
/// place among the frame's primitives by its unblurred box, so an element painted later that meets only the blur
/// could be drawn under it (the pet came out tinted by the glow); a layer as large as the blur orders them by where
/// they are seen.
fn paint_lit(window: &mut Window, bounds: Bounds<Pixels>, shadows: &[BoxShadow]) {
    let reach = shadows
        .iter()
        .map(|shadow| (bounds + shadow.offset).dilate(shadow.spread_radius + shadow.blur_radius * 3.0))
        .reduce(|all, one| all.union(&one));
    if let Some(reach) = reach {
        window.paint_layer(reach, |window| window.paint_drop_shadows(bounds, Corners::default(), shadows));
    }
}

fn cached_user_name() -> &'static Option<SharedString> {
    static NAME: OnceLock<Option<SharedString>> = OnceLock::new();
    NAME.get_or_init(|| {
        std::env::var("USERNAME")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty() && name.chars().count() <= 40)
            .map(SharedString::from)
    })
}

/// The name the greeting uses: the Windows account's, cached once per process.
pub fn user_name() -> Option<String> {
    cached_user_name().as_ref().map(|name| name.to_string())
}

/// Zero-allocation shared variant of [`user_name`].
#[allow(dead_code)]
pub fn user_name_shared() -> Option<SharedString> {
    cached_user_name().clone()
}

#[cfg(test)]
mod tests {
    use super::look::{BODY, MUTED, contrast, over};
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_kit::component::Theme;

    /// Every scheme and accent in both modes: text on glass and in its cells, the greeting on the characters and the
    /// glow, and the level names keep their contrast, and the glow is not capped away.
    #[test]
    fn the_look_keeps_text_readable_in_every_theme() {
        let mut report = Vec::new();
        for scheme in ["graphite", "slate", "midnight", "sand", "forest", "rose", "black"] {
            for accent in ["auto", "blue", "violet", "green", "orange", "pink", "teal"] {
                let (light, dark) = crate::theme::themes_for(scheme, accent);
                for config in [light, dark] {
                    let mut theme = Theme::default();
                    theme.apply_config(&config);
                    let look = Look::of(&theme);
                    let glass = look.worst_glass(theme.background);
                    let cell = over(look.cell, glass);
                    let name = format!("{scheme}/{accent}/{}", if look.dark { "dark" } else { "light" });
                    assert!(contrast(theme.foreground, glass) >= BODY, "{name}: body on glass");
                    assert!(contrast(theme.muted_foreground, glass) >= MUTED, "{name}: muted on glass");
                    assert!(contrast(look.muted, glass) >= MUTED && contrast(look.muted, cell) >= MUTED, "{name}: muted in cells");
                    assert!(contrast(theme.foreground, cell) >= BODY, "{name}: numbers in cells");
                    // The greeting and the project button on the brightest characters with as much glow as reaches them.
                    let page = look.worst_greeting(theme.background);
                    assert!(contrast(theme.foreground, page) >= BODY && contrast(theme.secondary_foreground, page) >= BODY, "{name}: greeting");
                    assert!(look.glyph_alpha_column > 0.04, "{name}: characters behind the column {}", look.glyph_alpha_column);
                    for level in rainy_protocol::reasoning::Level::ALL {
                        let color = look::level_color(level, &theme, &[theme.popover, glass]);
                        assert!(contrast(color, theme.popover) >= MUTED - 0.01 && contrast(color, glass) >= MUTED - 0.01, "{name}: {level:?}");
                        assert_eq!(color, look::cached_level_color(level, &theme, true));
                        assert_eq!(look::level_color(level, &theme, &[theme.popover]), look::cached_level_color(level, &theme, false));
                    }
                    assert!(look.glow[0].a > 0.3, "{name}: glow {}", look.glow[0].a);
                    report.push(format!("{name}: glass {:.3} glow {:.3} column {:.3} muted {:.2}", look.glass.a, look.glow[0].a, look.glyph_alpha_column, contrast(look.muted, cell)));
                }
            }
        }
        println!("{}", report.join("\n"));
    }

    #[test]
    fn look_cache_matches_fresh_computation_across_theme_switches() {
        for (scheme, accent) in [("graphite", "blue"), ("midnight", "violet"), ("sand", "orange"), ("rose", "pink")] {
            let (light, dark) = crate::theme::themes_for(scheme, accent);
            for config in [light, dark] {
                let mut theme = Theme::default();
                theme.apply_config(&config);
                let fresh = Look::compute(&theme);
                let first = Look::of(&theme);
                let second = Look::of(&theme);
                assert_eq!(first, fresh);
                assert_eq!(second, fresh);
            }
        }
    }

    #[test]
    fn glow_breathing_clamps_downward_and_preserves_contrast_budget() {
        let (light, dark) = crate::theme::themes_for("graphite", "blue");
        for config in [light, dark] {
            let mut theme = Theme::default();
            theme.apply_config(&config);
            let look = Look::of(&theme);
            let over_bright = glow_shadows(&look, 1.5, 99.0);
            let under_dim = glow_shadows(&look, 0.2, -99.0);
            for i in 0..3 {
                assert!((over_bright[i].color.a - look.glow[i].a).abs() < 1e-6);
                assert!((under_dim[i].color.a - look.glow[i].a * GLOW_BREATH_MIN).abs() < 1e-6);
                assert_eq!(over_bright[i].offset.x, px(GLOW_LIGHTS[i].0 + GLOW_PARALLAX_MAX));
                assert_eq!(under_dim[i].offset.x, px(GLOW_LIGHTS[i].0 - GLOW_PARALLAX_MAX));
                assert_eq!(over_bright[i].offset.y, px(GLOW_LIGHTS[i].1));
            }
        }
    }

    #[test]
    fn the_glow_fades_before_the_greeting() {
        assert!((erfc(0.0) - 1.0).abs() < 1e-6 && (erfc(1.0) - 0.157_299).abs() < 1e-5 && (erfc(-1.0) - 1.842_701).abs() < 1e-5);
        // Right at the composer's edge most of the light is there; at the greeting a few percent at most.
        assert!(glow_reach(-GLOW_TOP - 40.0) > 0.5);
        let reach = glow_reach(greeting_clearance());
        assert!(reach > 0.0 && reach < 0.08, "{reach}");
    }
}
