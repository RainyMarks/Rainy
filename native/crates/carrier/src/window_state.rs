//! The main window's place and size across launches (`window-state.ts` in 3.x): `<userData>\window-state.json` holds
//! the last normal (unmaximized) bounds in integer device-independent pixels and whether the window was maximized, in
//! exactly the shape 3.x reads and writes, and the rules choose where the window opens.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde_json::{Map, Value};

/// The record's name in the user-data folder.
pub const FILE_NAME: &str = "window-state.json";
/// The smallest window the layout is designed for (`MINIMUM_WINDOW`); a smaller work area lowers it to fit.
pub const MINIMUM: Size = Size { width: 950, height: 650 };
/// The size a window opens at without a saved place.
pub const PREFERRED: Size = Size { width: 1380, height: 920 };
/// A window keeps this much of its top edge on a display, enough to grab and move it.
const VISIBLE_GRIP: Size = Size { width: 120, height: 40 };
/// JavaScript's `Number.MAX_SAFE_INTEGER`: larger numbers are not integers to 3.x.
const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

/// A screen rectangle in device-independent pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

/// A window width and height in device-independent pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: i64,
    pub height: i64,
}

/// The last normal bounds and whether the window was maximized (`{version: 1, bounds, maximized}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowState {
    pub bounds: Rect,
    pub maximized: bool,
}

/// Where the window opens: its normal bounds, and whether it is maximized over them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub bounds: Rect,
    pub maximized: bool,
}

/// The saved state, or `None` when the record is missing or not exactly the 3.x shape, which keeps the default placement.
pub fn read(path: &Path) -> Option<WindowState> {
    parse(&std::fs::read_to_string(path).ok()?)
}

/// The 3.x schema, strictly: `version` 1, integer bounds with a positive size, a boolean `maximized`, and no other field
/// at any level.
pub fn parse(text: &str) -> Option<WindowState> {
    let value: Value = serde_json::from_str(text).ok()?;
    let state = exactly(&value, &["version", "bounds", "maximized"])?;
    if integer(&state["version"])? != 1 {
        return None;
    }
    let bounds = exactly(&state["bounds"], &["x", "y", "width", "height"])?;
    let bounds = Rect { x: integer(&bounds["x"])?, y: integer(&bounds["y"])?, width: integer(&bounds["width"])?, height: integer(&bounds["height"])? };
    if bounds.width <= 0 || bounds.height <= 0 {
        return None;
    }
    Some(WindowState { bounds, maximized: state["maximized"].as_bool()? })
}

/// An object with exactly these keys.
fn exactly<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Map<String, Value>> {
    let object = value.as_object()?;
    (object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))).then_some(object)
}

/// A JSON number that JavaScript takes for a safe integer, `2.0` included.
fn integer(value: &Value) -> Option<i64> {
    let number = value.as_number()?;
    let integer = match number.as_i64() {
        Some(integer) => integer,
        None => {
            let float = number.as_f64()?;
            if float.fract() != 0.0 || float.abs() > MAX_SAFE_INTEGER as f64 {
                return None;
            }
            float as i64
        }
    };
    (integer.abs() <= MAX_SAFE_INTEGER).then_some(integer)
}

/// The record as 3.x writes it: compact JSON and a line feed.
pub fn serialize(state: &WindowState) -> String {
    let Rect { x, y, width, height } = state.bounds;
    format!(r#"{{"version":1,"bounds":{{"x":{x},"y":{y},"width":{width},"height":{height}}},"maximized":{}}}"#, state.maximized) + "\n"
}

/// Write the record through a temporary file in the same folder, so a crash leaves the old one or the new one.
pub fn write(path: &Path, state: &WindowState) -> Result<()> {
    let folder = path.parent().context("The window state has no folder")?;
    let temporary = folder.join(format!(".window-state-{}.tmp", random_uuid()?));
    let written = std::fs::write(&temporary, serialize(state)).and_then(|()| std::fs::rename(&temporary, path));
    let _ = std::fs::remove_file(&temporary);
    written.with_context(|| format!("Writing {}", path.display()))
}

/// A random (version 4) UUID, as `randomUUID` names the temporary file.
fn random_uuid() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("{error}"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]))
}

/// The smallest window on one display: the designed minimum, or the whole work area when that is smaller, as on a
/// 1366×768 screen at 125% scaling or a 2560×1600 screen at 225%.
pub fn minimum_for(area: Rect, minimum: Size) -> Size {
    Size { width: minimum.width.min(area.width), height: minimum.height.min(area.height) }
}

/// Move and shrink bounds so they lie inside one work area, keeping at least the minimum size that fits it.
pub fn fit_bounds(bounds: Rect, area: Rect, minimum: Size) -> Rect {
    let floor = minimum_for(area, minimum);
    let width = floor.width.max(bounds.width.min(area.width));
    let height = floor.height.max(bounds.height.min(area.height));
    Rect {
        x: bounds.x.max(area.x).min(area.x + area.width - width),
        y: bounds.y.max(area.y).min(area.y + area.height - height),
        width,
        height,
    }
}

/// The first work area that shows enough of the window's top edge to grab and move it (`gripArea`): 120 px of its
/// width, its top between 20 px above the area and 40 px above its bottom.
pub fn grip_area(bounds: Rect, areas: &[Rect]) -> Option<Rect> {
    areas.iter().copied().find(|work| {
        let shared = (bounds.x + bounds.width).min(work.x + work.width) - bounds.x.max(work.x);
        shared >= VISIBLE_GRIP.width
            && bounds.y >= work.y - VISIBLE_GRIP.height / 2
            && bounds.y <= work.y + work.height - VISIBLE_GRIP.height
    })
}

/// Saved bounds placed on the first work area that still shows the window's top edge ([`grip_area`]), shrunk to that
/// area; `None` when no display shows it.
pub fn restored_bounds(state: &WindowState, areas: &[Rect], minimum: Size) -> Option<Rect> {
    let area = grip_area(state.bounds, areas)?;
    Some(fit_bounds(state.bounds, area, minimum))
}

/// Where the window opens: the saved place while a display still shows it, else the preferred size centered on the
/// primary work area; a work area too small for the preferred size opens maximized over 90% of it.
pub fn initial_placement(state: Option<&WindowState>, areas: &[Rect], primary: Rect, preferred: Size, minimum: Size) -> Placement {
    if let Some(state) = state
        && let Some(bounds) = restored_bounds(state, areas, minimum)
    {
        return Placement { bounds, maximized: state.maximized };
    }
    let area = primary;
    let floor = minimum_for(area, minimum);
    let fits = preferred.width <= area.width && preferred.height <= area.height;
    let width = if fits { preferred.width } else { floor.width.max(js_round(area.width as f64 * 0.9)) };
    let height = if fits { preferred.height } else { floor.height.max(js_round(area.height as f64 * 0.9)) };
    let x = area.x + 0.max(js_round((area.width - width) as f64 / 2.0));
    let y = area.y + 0.max(js_round((area.height - height) as f64 / 2.0));
    Placement { bounds: Rect { x, y, width, height }, maximized: !fits }
}

/// The work area a rectangle is on (`screen.getDisplayMatching`): the one it overlaps most, or the nearest.
pub fn display_matching(bounds: Rect, areas: &[Rect]) -> Option<usize> {
    let gap = |low: i64, high: i64, start: i64, end: i64| (start - high).max(low - end).max(0);
    let score = |area: &Rect| {
        let width = (bounds.x + bounds.width).min(area.x + area.width) - bounds.x.max(area.x);
        let height = (bounds.y + bounds.height).min(area.y + area.height) - bounds.y.max(area.y);
        let overlap = width.max(0) * height.max(0);
        let dx = gap(bounds.x, bounds.x + bounds.width, area.x, area.x + area.width);
        let dy = gap(bounds.y, bounds.y + bounds.height, area.y, area.y + area.height);
        (overlap, -(dx * dx + dy * dy))
    };
    // The first of equals, as Electron picks.
    areas.iter().enumerate().rev().max_by_key(|(_, area)| score(area)).map(|(index, _)| index)
}

/// `Math.round`: halves round up.
fn js_round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    const fn rect(x: i64, y: i64, width: i64, height: i64) -> Rect {
        Rect { x, y, width, height }
    }

    #[test]
    fn reads_and_writes_the_3x_record() {
        let text = "{\"version\":1,\"bounds\":{\"x\":120,\"y\":64,\"width\":1380,\"height\":920},\"maximized\":false}\n";
        let state = parse(text).unwrap();
        assert_eq!(state, WindowState { bounds: rect(120, 64, 1380, 920), maximized: false });
        assert_eq!(serialize(&state), text);
        let maximized = WindowState { bounds: rect(-1700, -8, 1024, 650), maximized: true };
        assert_eq!(serialize(&maximized), "{\"version\":1,\"bounds\":{\"x\":-1700,\"y\":-8,\"width\":1024,\"height\":650},\"maximized\":true}\n");
        assert_eq!(parse(&serialize(&maximized)), Some(maximized));
        // Any order, white space, and numbers JavaScript reads as integers.
        let loose = r#" { "maximized": true, "bounds": { "height": 6.5e2, "width": 1024.0, "y": -0, "x": 3 }, "version": 1.0 } "#;
        assert_eq!(parse(loose), Some(WindowState { bounds: rect(3, 0, 1024, 650), maximized: true }));
    }

    #[test]
    fn rejects_anything_but_the_exact_shape() {
        let good = r#"{"version":1,"bounds":{"x":0,"y":0,"width":10,"height":10},"maximized":false}"#;
        assert!(parse(good).is_some());
        for bad in [
            r#"{"version":1,"bounds":{"x":0,"y":0,"width":10,"height":10},"maximized":false,"display":"x"}"#,
            r#"{"version":1,"bounds":{"x":0,"y":0,"width":10,"height":10,"scale":2},"maximized":false}"#,
            r#"{"version":1,"bounds":{"x":0,"y":0,"width":10},"maximized":false}"#,
            r#"{"version":1,"bounds":{"x":0,"y":0,"width":10,"height":10}}"#,
            r#"{"version":2,"bounds":{"x":0,"y":0,"width":10,"height":10},"maximized":false}"#,
            r#"{"version":"1","bounds":{"x":0,"y":0,"width":10,"height":10},"maximized":false}"#,
            r#"{"version":1,"bounds":{"x":0.5,"y":0,"width":10,"height":10},"maximized":false}"#,
            r#"{"version":1,"bounds":{"x":0,"y":0,"width":0,"height":10},"maximized":false}"#,
            r#"{"version":1,"bounds":{"x":0,"y":0,"width":10,"height":-4},"maximized":false}"#,
            r#"{"version":1,"bounds":{"x":"0","y":0,"width":10,"height":10},"maximized":false}"#,
            r#"{"version":1,"bounds":{"x":9007199254740992,"y":0,"width":10,"height":10},"maximized":false}"#,
            r#"{"version":1,"bounds":{"x":0,"y":0,"width":10,"height":10},"maximized":0}"#,
            r#"{"version":1,"bounds":[0,0,10,10],"maximized":false}"#,
            "[]",
            "",
            "\u{feff}{\"version\":1,\"bounds\":{\"x\":0,\"y\":0,\"width\":10,\"height\":10},\"maximized\":false}",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn writes_through_a_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        assert_eq!(read(&path), None);
        std::fs::write(&path, "damaged").unwrap();
        assert_eq!(read(&path), None);
        let state = WindowState { bounds: rect(10, 20, 1200, 800), maximized: true };
        write(&path, &state).unwrap();
        assert_eq!(read(&path), Some(state));
        let names: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, [FILE_NAME]);
        let uuid = random_uuid().unwrap();
        assert_eq!((uuid.len(), &uuid[14..15], uuid.matches('-').count()), (36, "4", 4));
        assert!(write(&dir.path().join("missing").join(FILE_NAME), &state).is_err());
    }

    #[test]
    fn keeps_the_top_edge_on_a_display() {
        let primary = rect(0, 0, 1920, 1040);
        let left = rect(-1280, 200, 1280, 984);
        let areas = [primary, left];
        let state = |bounds: Rect| WindowState { bounds, maximized: false };
        // Inside a display: unchanged.
        assert_eq!(restored_bounds(&state(rect(100, 50, 1380, 920)), &areas, MINIMUM), Some(rect(100, 50, 1380, 920)));
        // 120 px of the top edge on the primary display is enough; the window is moved into it.
        assert_eq!(restored_bounds(&state(rect(1800, 10, 1000, 700)), &areas, MINIMUM), Some(rect(920, 10, 1000, 700)));
        assert_eq!(restored_bounds(&state(rect(1801, 10, 1000, 700)), &areas, MINIMUM), None);
        // The top may be 20 px above the work area, and must be 40 px above its bottom.
        assert_eq!(restored_bounds(&state(rect(0, -20, 1000, 700)), &areas, MINIMUM), Some(rect(0, 0, 1000, 700)));
        assert_eq!(restored_bounds(&state(rect(0, -21, 1000, 700)), &[primary], MINIMUM), None);
        assert_eq!(restored_bounds(&state(rect(0, 1000, 1000, 700)), &[primary], MINIMUM), Some(rect(0, 340, 1000, 700)));
        assert_eq!(restored_bounds(&state(rect(0, 1001, 1000, 700)), &[primary], MINIMUM), None);
        // On the display to the left, shrunk to its work area.
        assert_eq!(restored_bounds(&state(rect(-1300, 300, 1400, 1100)), &areas, MINIMUM), Some(rect(-1280, 200, 1280, 984)));
        // A display that went away.
        assert_eq!(restored_bounds(&state(rect(2000, 100, 1200, 800)), &areas, MINIMUM), None);
    }

    #[test]
    fn lets_a_moved_window_hang_partly_off_screen_while_its_top_edge_can_still_be_grabbed() {
        let primary = rect(0, 0, 1920, 1040);
        // Dragged down until most of the window is below the screen, as in the reported recording: it stays where it was
        // dropped.
        assert_eq!(grip_area(rect(300, 900, 1300, 870), &[primary]), Some(primary));
        assert_eq!(grip_area(rect(-1000, 100, 1300, 870), &[primary]), Some(primary));
        // Its top edge below the work area, or only a sliver left on screen: it has to be brought back.
        assert_eq!(grip_area(rect(300, 1020, 1300, 870), &[primary]), None);
        assert_eq!(grip_area(rect(1850, 100, 1300, 870), &[primary]), None);
        // The first display that shows it, as when restoring.
        let left = rect(-1280, 200, 1280, 984);
        assert_eq!(grip_area(rect(-200, 300, 1300, 870), &[primary, left]), Some(primary));
        assert_eq!(grip_area(rect(-1300, 300, 1300, 870), &[primary, left]), Some(left));
    }

    #[test]
    fn falls_back_to_the_default_placement_when_only_a_sliver_would_remain() {
        let primary = rect(0, 0, 1920, 1040);
        let state = WindowState { bounds: rect(2100, 80, 1600, 1000), maximized: false };
        assert_eq!(restored_bounds(&state, &[primary], MINIMUM), None);
        let sliver = WindowState { bounds: rect(1850, 80, 1600, 1000), ..state };
        assert_eq!(restored_bounds(&sliver, &[primary], MINIMUM), None);
    }

    #[test]
    fn fits_bounds_without_going_below_the_minimum_that_fits() {
        let area = rect(0, 0, 1137, 663);
        assert_eq!(minimum_for(area, MINIMUM), Size { width: 950, height: 650 });
        assert_eq!(minimum_for(rect(0, 0, 1092, 574), MINIMUM), Size { width: 950, height: 574 });
        assert_eq!(fit_bounds(rect(-50, 600, 400, 300), area, MINIMUM), rect(0, 13, 950, 650));
        assert_eq!(fit_bounds(rect(500, 100, 2000, 2000), area, MINIMUM), rect(0, 0, 1137, 663));
        assert_eq!(fit_bounds(rect(10, 10, 1000, 650), area, MINIMUM), rect(10, 10, 1000, 650));
    }

    #[test]
    fn opens_at_the_default_place_deterministically() {
        // 1920×1080 at 100% with the taskbar: 1380×920 fits and is centered.
        let full_hd = rect(0, 0, 1920, 1032);
        assert_eq!(initial_placement(None, &[full_hd], full_hd, PREFERRED, MINIMUM), Placement { bounds: rect(270, 56, 1380, 920), maximized: false });
        // 2560×1600 at 225% (work area 2560×1492 physical): 90% of the area, at least the minimum, maximized.
        let scaled = rect(0, 0, 1137, 663);
        assert_eq!(initial_placement(None, &[scaled], scaled, PREFERRED, MINIMUM), Placement { bounds: rect(57, 7, 1023, 650), maximized: true });
        // 3840×2160 at 225%: 912 px high is not enough for 920.
        let uhd = rect(0, 0, 1706, 912);
        assert_eq!(initial_placement(None, &[uhd], uhd, PREFERRED, MINIMUM), Placement { bounds: rect(86, 46, 1535, 821), maximized: true });
        // A primary display that is not at the origin.
        let shifted = rect(1920, 48, 2560, 1392);
        assert_eq!(initial_placement(None, &[full_hd, shifted], shifted, PREFERRED, MINIMUM).bounds, rect(2510, 284, 1380, 920));
        // Smaller than the minimum: the whole work area.
        let tiny = rect(0, 0, 800, 560);
        assert_eq!(initial_placement(None, &[tiny], tiny, PREFERRED, MINIMUM), Placement { bounds: rect(0, 0, 800, 560), maximized: true });
    }

    #[test]
    fn restores_the_saved_place_and_falls_back_when_it_is_off_screen() {
        let primary = rect(0, 0, 1920, 1032);
        let second = rect(1920, 0, 1280, 984);
        let saved = WindowState { bounds: rect(2000, 40, 1100, 800), maximized: true };
        assert_eq!(initial_placement(Some(&saved), &[primary, second], primary, PREFERRED, MINIMUM), Placement { bounds: rect(2000, 40, 1100, 800), maximized: true });
        // The second display was unplugged: the default, not maximized because it fits.
        assert_eq!(initial_placement(Some(&saved), &[primary], primary, PREFERRED, MINIMUM), Placement { bounds: rect(270, 56, 1380, 920), maximized: false });
    }

    #[test]
    fn finds_the_display_a_window_is_on() {
        let areas = [rect(0, 0, 1920, 1032), rect(1920, 0, 1280, 984), rect(-1280, 0, 1280, 984)];
        assert_eq!(display_matching(rect(100, 100, 800, 600), &areas), Some(0));
        assert_eq!(display_matching(rect(1800, 100, 800, 600), &areas), Some(1));
        assert_eq!(display_matching(rect(-300, 100, 800, 600), &areas), Some(0));
        assert_eq!(display_matching(rect(-700, 100, 800, 600), &areas), Some(2));
        // Nowhere: the nearest.
        assert_eq!(display_matching(rect(3300, 1500, 100, 100), &areas), Some(1));
        // Equal overlap: the first.
        assert_eq!(display_matching(rect(1820, 100, 200, 100), &areas), Some(0));
        assert_eq!(display_matching(rect(0, 0, 10, 10), &[]), None);
    }
}
