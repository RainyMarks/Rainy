//! The colors of the optical look, made from the theme: the characters, the glow, the glass and the text on them.
//! Every strength here is capped by contrast: body text keeps at least [`BODY`] and muted text at least [`MUTED`]
//! against the worst place it can sit. Text with muted colors sits only on glass, which is as opaque as the glow at its
//! peak and the brightest characters behind it require; on the page itself there is only body text (the greeting and
//! the project button), which keeps [`BODY`] with the brightest characters and as much glow as reaches it.
//!
//! GPUI blends sRGB values as they are (its render targets are UNORM), so the blends below match the pixels drawn.

use gpui_kit::component::Theme;
use gpui_kit::{Hsla, Rgba, hsla};
use rainy_protocol::reasoning::Level;

/// Contrast body text keeps.
pub const BODY: f32 = 7.0;
/// Contrast muted text and the level names keep.
pub const MUTED: f32 = 4.5;

/// sRGB channels of an opaque color.
fn channels(color: Hsla) -> [f32; 3] {
    let rgba = Rgba::from(Hsla { a: 1.0, ..color });
    [rgba.r, rgba.g, rgba.b]
}

/// WCAG relative luminance.
pub fn luminance(color: Hsla) -> f32 {
    let linear = |value: f32| if value <= 0.039_28 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) };
    let [r, g, b] = channels(color);
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/// WCAG contrast ratio of two opaque colors.
pub fn contrast(a: Hsla, b: Hsla) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `top` with its alpha drawn over the opaque `bottom`.
pub fn over(top: Hsla, bottom: Hsla) -> Hsla {
    let (t, b) = (channels(top), channels(bottom));
    let a = top.a.clamp(0.0, 1.0);
    let mix = |i: usize| t[i] * a + b[i] * (1.0 - a);
    Hsla::from(Rgba { r: mix(0), g: mix(1), b: mix(2), a: 1.0 })
}

/// `color` made darker on a light surface or lighter on a dark one, keeping its hue and saturation, until it has
/// `ratio` against `surface`. A color that already has it is returned as it is.
pub fn readable(color: Hsla, surface: Hsla, ratio: f32) -> Hsla {
    let color = Hsla { a: 1.0, ..color };
    if contrast(color, surface) >= ratio {
        return color;
    }
    let darker = luminance(surface) > 0.18;
    let (mut low, mut high) = if darker { (0.0, color.l) } else { (color.l, 1.0) };
    // The lightness at the far end always reaches the ratio for the ratios used here; search towards it.
    for _ in 0..24 {
        let middle = (low + high) / 2.0;
        let fits = contrast(Hsla { l: middle, ..color }, surface) >= ratio;
        match (darker, fits) {
            (true, true) => low = middle,
            (true, false) => high = middle,
            (false, true) => high = middle,
            (false, false) => low = middle,
        }
    }
    Hsla { l: if darker { low } else { high }, ..color }
}

/// The strongest alpha up to `max` at which `layers`, each drawn at that alpha in turn over `bottom`, keep every text
/// `(color, ratio)` readable; 0 when even a trace of them would not.
pub fn cap(bottom: Hsla, layers: &[Hsla], texts: &[(Hsla, f32)], max: f32) -> f32 {
    let fits = |alpha: f32| {
        let under = layers.iter().fold(bottom, |under, layer| over(Hsla { a: alpha, ..*layer }, under));
        texts.iter().all(|(text, ratio)| contrast(*text, under) >= *ratio)
    };
    if fits(max) {
        return max;
    }
    let (mut low, mut high) = (0.0, max);
    for _ in 0..24 {
        let middle = (low + high) / 2.0;
        if fits(middle) { low = middle } else { high = middle }
    }
    low
}

/// `color` turned by `degrees` on the color wheel.
fn turn(color: Hsla, degrees: f32) -> Hsla {
    Hsla { h: (color.h + degrees / 360.0).rem_euclid(1.0), ..color }
}

/// How strong the glow is drawn at most. Behind glass only the glass's alpha limits it; at the greeting, the share of
/// it that reaches there ([`super::glow_reach`]) may cap it.
const GLOW_LIGHT: f32 = 0.45;
const GLOW_DARK: f32 = 0.55;
/// The characters' alpha, away from the column of content and inside it.
const GLYPHS_LIGHT: (f32, f32) = (0.20, 0.10);
const GLYPHS_DARK: (f32, f32) = (0.17, 0.09);
/// How much brighter than its place's alpha a character may get: sparks, the pointer and ripples together.
pub const GLYPH_GAIN: f32 = 2.0;

/// The colors of one theme.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub dark: bool,
    /// The characters, opaque; their alpha is `glyph_alpha` away from the content and `glyph_alpha_column` behind it.
    pub glyph: Hsla,
    pub glyph_spark: Hsla,
    pub glyph_alpha: f32,
    pub glyph_alpha_column: f32,
    /// The three glow colors, each with its alpha.
    pub glow: [Hsla; 3],
    /// The fill of glass: the card and the composer.
    pub glass: Hsla,
    pub glass_border: Hsla,
    /// The bright line along the top edge of glass.
    pub glass_highlight: Hsla,
    /// The fill of a cell on glass (the usage numbers).
    pub cell: Hsla,
    pub cell_border: Hsla,
    /// Muted text on glass and in cells: the theme's, made stronger where it would fall under [`MUTED`].
    pub muted: Hsla,
    /// The activity map: no messages, then four steps.
    pub heat: [Hsla; 5],
    /// The thin contact shadow under glass.
    pub shadow: Hsla,
}

impl Look {
    pub fn of(theme: &Theme) -> Self {
        let dark = theme.mode.is_dark();
        let (background, foreground, muted) = (theme.background, theme.foreground, theme.muted_foreground);
        let accent = theme.primary;
        // The characters lean a little towards the accent, as the glow does.
        let glyph = over(Hsla { a: 0.25, ..accent }, muted);
        let glyph_spark = if dark { Hsla { l: accent.l.max(0.62), ..accent } } else { Hsla { l: accent.l.min(0.52), ..accent } };
        let (glyph_alpha, column_target) = if dark { GLYPHS_DARK } else { GLYPHS_LIGHT };
        // Blue, violet and pink for a blue accent; the same turns of the wheel for any other.
        let tint = |degrees: f32| {
            let color = turn(accent, degrees);
            if dark { Hsla { l: color.l.clamp(0.42, 0.55), s: color.s.max(0.70), ..color } } else { Hsla { l: color.l.clamp(0.62, 0.72), s: color.s.max(0.80), ..color } }
        };
        let tints = [tint(0.0), tint(40.0), tint(80.0)];
        // The greeting and the project button sit on the characters, as close to the glow as the layout lets them. The
        // characters behind the column get what the glow's share there leaves, then the glow what they leave.
        let body = [(foreground, BODY), (theme.secondary_foreground, BODY)];
        let target = if dark { GLOW_DARK } else { GLOW_LIGHT };
        let reach = super::glow_reach(super::greeting_clearance()).max(1e-4);
        let glowed = tints.iter().fold(background, |under, tint| over(Hsla { a: target * reach, ..*tint }, under));
        let glyph_alpha_column = cap(glowed, &[glyph_spark], &body, column_target * GLYPH_GAIN) / GLYPH_GAIN;
        let under_column = over(Hsla { a: glyph_alpha_column * GLYPH_GAIN, ..glyph_spark }, background);
        let glow_alpha = (cap(under_column, &tints, &body, target * reach) / reach).min(target);
        let glow = tints.map(|tint| Hsla { a: glow_alpha, ..tint });

        // The worst background under glass: the brightest characters and every glow color at its peak.
        let worst_under = glow.iter().fold(under_column, |under, glow| over(*glow, under));
        // Glass is the popover's color in light mode; in dark mode the window's, which keeps muted text farther from
        // its limit than the lighter popover does.
        let glass_base = if dark { background } else { theme.popover };
        let texts = [(foreground, BODY), (muted, MUTED)];
        let glass_alpha = (0..=40)
            .map(|step| 0.80 + step as f32 * 0.005)
            .find(|alpha| {
                let surface = over(Hsla { a: *alpha, ..glass_base }, worst_under);
                texts.iter().all(|(text, ratio)| contrast(*text, surface) >= *ratio)
            })
            .unwrap_or(1.0);
        let glass = Hsla { a: glass_alpha, ..glass_base };
        let glass_surface = over(glass, worst_under);
        let white = hsla(0.0, 0.0, 1.0, 1.0);
        let black = hsla(0.0, 0.0, 0.0, 1.0);
        let (cell, cell_border, glass_border, glass_highlight) = if dark {
            (Hsla { a: 0.035, ..white }, Hsla { a: 0.07, ..white }, Hsla { a: 0.13, ..white }, Hsla { a: 0.22, ..white })
        } else {
            (Hsla { a: 0.025, ..black }, Hsla { a: 0.06, ..black }, Hsla { a: 0.09, ..black }, Hsla { a: 0.9, ..white })
        };
        let cell_surface = over(cell, glass_surface);
        let muted = readable(readable(muted, glass_surface, MUTED), cell_surface, MUTED);
        let empty = if dark { Hsla { a: 0.07, ..white } } else { Hsla { a: 0.07, ..black } };
        let heat = [empty, Hsla { a: 0.28, ..accent }, Hsla { a: 0.48, ..accent }, Hsla { a: 0.72, ..accent }, Hsla { a: 1.0, ..accent }];
        let shadow = if dark { Hsla { a: 0.45, ..black } } else { Hsla { a: 0.10, ..turn(accent, 20.0) } };
        Self { dark, glyph, glyph_spark, glyph_alpha, glyph_alpha_column, glow, glass, glass_border, glass_highlight, cell, cell_border, muted, heat, shadow }
    }

    /// The opaque color text at the greeting can meet at worst: the brightest characters and the share of the glow
    /// that reaches there.
    #[cfg(test)]
    pub fn worst_greeting(&self, background: Hsla) -> Hsla {
        let reach = super::glow_reach(super::greeting_clearance());
        let under = over(Hsla { a: self.glyph_alpha_column * GLYPH_GAIN, ..self.glyph_spark }, background);
        self.glow.iter().fold(under, |under, glow| over(Hsla { a: glow.a * reach, ..*glow }, under))
    }

    /// The opaque color text on glass can meet at worst, for checks.
    pub fn worst_glass(&self, background: Hsla) -> Hsla {
        let under = over(Hsla { a: self.glyph_alpha_column * GLYPH_GAIN, ..self.glyph_spark }, background);
        let under = self.glow.iter().fold(under, |under, glow| over(*glow, under));
        over(self.glass, under)
    }
}

/// The hue of each reasoning level, from cool to warm as the model thinks longer: teal, cyan, blue, violet, magenta and
/// rose. Off has none (muted text).
fn level_hue(level: Level) -> Option<f32> {
    match level {
        Level::Off => None,
        Level::Minimal => Some(168.0),
        Level::Low => Some(192.0),
        Level::Medium => Some(218.0),
        Level::High => Some(262.0),
        Level::XHigh => Some(292.0),
        Level::Max => Some(328.0),
    }
}

/// The color of a level's name, readable at [`MUTED`] on every surface given (the menus and glass).
pub fn level_color(level: Level, theme: &Theme, surfaces: &[Hsla]) -> Hsla {
    let Some(hue) = level_hue(level) else { return theme.muted_foreground };
    let dark = theme.mode.is_dark();
    let start = hsla(hue / 360.0, 0.78, if dark { 0.68 } else { 0.50 }, 1.0);
    surfaces.iter().fold(start, |color, surface| readable(color, *surface, MUTED))
}

/// A level's color for the slider's track and stars, where it is a fill, not text.
pub fn level_fill(level: Level, dark: bool) -> Hsla {
    let hue = level_hue(level).unwrap_or(220.0);
    let saturation = if level == Level::Off { 0.15 } else { 0.80 };
    hsla(hue / 360.0, saturation, if dark { 0.58 } else { 0.56 }, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn contrast_matches_the_wcag_examples() {
        let (white, black) = (hsla(0.0, 0.0, 1.0, 1.0), hsla(0.0, 0.0, 0.0, 1.0));
        assert!((contrast(white, black) - 21.0).abs() < 0.01);
        assert!((contrast(white, white) - 1.0).abs() < 0.001);
        // #767676 on white is the classic 4.54.
        let gray = Hsla::from(Rgba { r: 0x76 as f32 / 255.0, g: 0x76 as f32 / 255.0, b: 0x76 as f32 / 255.0, a: 1.0 });
        assert!((contrast(gray, white) - 4.54).abs() < 0.02);
        assert_eq!(over(Hsla { a: 0.0, ..black }, white), white);
    }

    #[test]
    fn readable_reaches_the_ratio_and_keeps_the_hue() {
        let white = hsla(0.0, 0.0, 1.0, 1.0);
        let pale = hsla(0.6, 0.8, 0.8, 1.0);
        let fixed = readable(pale, white, MUTED);
        assert!(contrast(fixed, white) >= MUTED);
        assert!((fixed.h - pale.h).abs() < 1e-6 && fixed.l < pale.l);
        let night = hsla(0.0, 0.0, 0.08, 1.0);
        let deep = hsla(0.75, 0.8, 0.3, 1.0);
        let fixed = readable(deep, night, MUTED);
        assert!(contrast(fixed, night) >= MUTED && fixed.l > deep.l);
    }

    #[test]
    fn cap_stops_where_text_would_fail() {
        let white = hsla(0.0, 0.0, 1.0, 1.0);
        let gray = hsla(0.0, 0.0, 0.42, 1.0);
        let blue = hsla(0.6, 0.9, 0.5, 1.0);
        let alpha = cap(white, &[blue], &[(gray, MUTED)], 1.0);
        assert!(alpha > 0.0 && alpha < 1.0);
        assert!(contrast(gray, over(Hsla { a: alpha, ..blue }, white)) >= MUTED);
        assert!(contrast(gray, over(Hsla { a: alpha + 0.02, ..blue }, white)) < MUTED);
    }
}
