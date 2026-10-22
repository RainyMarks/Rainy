//! The main window's place on the screen (`initialPlacement`, the geometry tracking, `fitDisplay` and `afterMove` in 3.x
//! `main.ts`): it opens where it was while a display still shows that place, else at the default; its normal bounds and
//! maximized state go to `window-state.json` when it closes and when Rainy quits; after a display change it is moved
//! back into the work area of its display, and after the user moves it, only when no display shows its top edge.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::*;
use rainy_carrier::window_state::{self, Rect, WindowState};
use raw_window_handle::RawWindowHandle;
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowRect, HWND_BOTTOM, IsIconic, IsWindowVisible, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    SWP_NOZORDER, SetWindowPos,
};

/// Windows reports a scaling change as several events in a row; the window is fitted once they settle.
const REFIT_DELAY: Duration = Duration::from_millis(250);

/// What the check after a geometry change does; a display change outranks a move while both wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Refit {
    /// The user moved or resized the window: it is brought back only when it could not be grabbed again.
    Moved,
    /// A display was connected, removed or rescaled: the window is fitted into its display's work area.
    Displays,
}

/// Where and how the window opens.
#[derive(Clone, Copy)]
pub struct Opening {
    pub bounds: WindowBounds,
    pub display: Option<DisplayId>,
    pub min_size: Size<Pixels>,
}

impl Opening {
    /// Where the window opens on the displays connected now.
    pub fn new(user_data: &Path, cx: &App) -> Self {
        let saved = window_state::read(&user_data.join(window_state::FILE_NAME));
        let displays = cx.displays();
        let areas: Vec<Bounds<Pixels>> = displays.iter().map(|display| display.visible_bounds()).collect();
        let primary = cx.primary_display().and_then(|primary| displays.iter().position(|display| display.id() == primary.id()));
        let (placement, display, minimum) = choose(saved.as_ref(), &areas, primary);
        log::info!(
            "The window opens at {:?}{} on display {} of {} ({} saved place)",
            placement.bounds,
            if placement.maximized { ", maximized" } else { "" },
            display.map_or(0, |index| index + 1),
            areas.len(),
            if saved.is_some() { "with a" } else { "no" }
        );
        let Rect { x, y, width, height } = placement.bounds;
        let bounds = Bounds::new(point(px(x as f32), px(y as f32)), size(px(width as f32), px(height as f32)));
        Self {
            bounds: if placement.maximized { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) },
            display: display.map(|index| displays[index].id()),
            min_size: size(px(minimum.width as f32), px(minimum.height as f32)),
        }
    }

    /// Set the window's options. It opens hidden and `show` shows it in its place, already maximized when it opens
    /// maximized.
    pub fn apply(&self, options: &mut WindowOptions) {
        options.window_bounds = Some(self.bounds);
        options.display_id = self.display;
        options.window_min_size = Some(self.min_size);
        options.show = false;
        // Test runs on the developer's desktop show the window behind the others, without taking the focus.
        if background() {
            options.window_bounds = Some(WindowBounds::Windowed(self.bounds.get_bounds()));
            options.show = true;
            options.focus = false;
        }
    }

    pub fn show(&self, window: &mut Window) {
        if background() {
            if let Some(hwnd) = hwnd(window) {
                // SAFETY: a plain z-order change of the window's live handle.
                unsafe { SetWindowPos(hwnd, HWND_BOTTOM, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) };
            }
            return;
        }
        window.activate_window();
    }
}

/// Whether this is a test run that must not take the focus (`RAINY_QA_BACKGROUND`, debug builds only).
pub fn background() -> bool {
    cfg!(debug_assertions) && std::env::var_os("RAINY_QA_BACKGROUND").is_some()
}

/// The placement on displays with these work areas (logical pixels, the primary one's index if known), the display it
/// is on, and the minimum window size there.
fn choose(saved: Option<&WindowState>, displays: &[Bounds<Pixels>], primary: Option<usize>) -> (window_state::Placement, Option<usize>, window_state::Size) {
    let areas: Vec<Rect> = displays.iter().copied().map(work_area).collect();
    let primary = primary.or((!areas.is_empty()).then_some(0));
    let fallback = Rect { x: 0, y: 0, width: window_state::PREFERRED.width, height: window_state::PREFERRED.height };
    let primary_area = primary.map_or(fallback, |index| areas[index]);
    let placement = window_state::initial_placement(saved, &areas, primary_area, window_state::PREFERRED, window_state::MINIMUM);
    let display = window_state::display_matching(placement.bounds, &areas);
    let area = display.map_or(primary_area, |index| areas[index]);
    (placement, display, gpui_minimum(window_state::minimum_for(area, window_state::MINIMUM), area))
}

/// GPUI adds the window's invisible borders to the minimum it reports to Windows, so a minimum close to the whole work
/// area (950×650 on a laptop screen at 225%) would ask for more than the screen has; four fifths leaves room for them.
fn gpui_minimum(minimum: window_state::Size, area: Rect) -> window_state::Size {
    window_state::Size { width: minimum.width.min(area.width * 4 / 5), height: minimum.height.min(area.height * 4 / 5) }
}

/// A display's work area in whole logical pixels, inside the real one: at a scaling such as 225% it ends in a fraction.
fn work_area(bounds: Bounds<Pixels>) -> Rect {
    let (x, y) = (f64::from(f32::from(bounds.origin.x)), f64::from(f32::from(bounds.origin.y)));
    let (width, height) = (f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height)));
    let left = (x - 0.01).ceil() as i64;
    let top = (y - 0.01).ceil() as i64;
    let right = (x + width + 0.01).floor() as i64;
    let bottom = (y + height + 0.01).floor() as i64;
    Rect { x: left, y: top, width: (right - left).max(1), height: (bottom - top).max(1) }
}

/// Window bounds in whole logical pixels.
fn rect_of(bounds: Bounds<Pixels>) -> Rect {
    let round = |value: Pixels| f32::from(value).round() as i64;
    Rect { x: round(bounds.origin.x), y: round(bounds.origin.y), width: round(bounds.size.width).max(1), height: round(bounds.size.height).max(1) }
}

/// Follows the window's geometry and keeps it on its display.
struct Tracker {
    path: PathBuf,
    window: AnyWindowHandle,
    /// The last normal bounds and maximized state seen while the window was shown and not minimized.
    state: Option<WindowState>,
    /// The check waiting for the geometry to settle.
    pending: Option<Refit>,
    refit: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

struct GlobalTracker(Entity<Tracker>);
impl Global for GlobalTracker {}

/// Remember the window's geometry for `save`; fit the window to its display after a display is connected, removed or
/// rescaled, and bring it back after the user moved it only when no display shows its top edge.
pub fn track(user_data: &Path, window: &mut Window, cx: &mut App) {
    let path = user_data.join(window_state::FILE_NAME);
    let handle = window.window_handle();
    let tracker = cx.new(|cx: &mut Context<Tracker>| {
        let weak = cx.weak_entity();
        let shown = weak.clone();
        let subscriptions = vec![
            cx.observe_window_bounds(window, |tracker, window, cx| {
                tracker.record(window);
                tracker.schedule(Refit::Moved, cx);
            }),
            // Shown in its place without a move or resize, or restored from the taskbar.
            window.observe_window_visibility(move |visibility, window, cx| {
                if visibility.is_visible() {
                    let _ = shown.update(cx, |tracker, _| tracker.record(window));
                }
            }),
            cx.observe_displays(move |_, cx| {
                let _ = weak.update(cx, |tracker, cx| tracker.schedule(Refit::Displays, cx));
            }),
        ];
        Tracker { path, window: handle, state: None, pending: None, refit: None, _subscriptions: subscriptions }
    });
    cx.set_global(GlobalTracker(tracker));
}

/// Write the window's last geometry (`writeWindowState`); called when the window closes and when Rainy quits, after
/// which there is nothing left to write.
pub fn save(cx: &mut App) {
    let Some(tracker) = cx.try_global::<GlobalTracker>().map(|global| global.0.clone()) else { return };
    let (path, state) = tracker.update(cx, |tracker, _| (tracker.path.clone(), tracker.state.take()));
    if let Some(state) = state
        && let Err(error) = window_state::write(&path, &state)
    {
        log::warn!("Window state was not saved: {error:#}");
    }
}

impl Tracker {
    fn record(&mut self, window: &Window) {
        // Minimized, the placement no longer says whether the window was maximized; hidden, it is not placed yet.
        if !shown(window) {
            return;
        }
        let (bounds, maximized) = match window.window_bounds() {
            WindowBounds::Windowed(bounds) | WindowBounds::Fullscreen(bounds) => (bounds, false),
            WindowBounds::Maximized(bounds) => (bounds, true),
        };
        self.state = Some(WindowState { bounds: rect_of(bounds), maximized });
    }

    /// Check the window once its geometry settles; each change starts the wait again.
    fn schedule(&mut self, refit: Refit, cx: &mut Context<Self>) {
        self.pending = self.pending.max(Some(refit));
        let window = self.window;
        self.refit = Some(cx.spawn(async move |this: WeakEntity<Tracker>, cx: &mut AsyncApp| {
            // Not while a mouse button is down, as while the window is dragged to another display.
            loop {
                cx.background_executor().timer(REFIT_DELAY).await;
                if !buttons_down() {
                    break;
                }
            }
            let Ok(Some(refit)) = this.update(cx, |tracker, _| tracker.pending.take()) else { return };
            let placing = window.update(cx, |_, window, cx| match refit {
                Refit::Displays => fit_display(window, cx),
                Refit::Moved => after_move(window, cx),
            });
            if let Ok(Some(placing)) = placing {
                placing.apply();
            }
        }));
    }
}

/// Where a normal window moves and shrinks to stay in the work area of its display (`fitDisplay`). GPUI keeps the
/// minimum size the window opened with.
fn fit_display(window: &mut Window, cx: &mut App) -> Option<Placing> {
    if window.is_maximized() || window.is_fullscreen() || !shown(window) {
        return None;
    }
    let display = window.display(cx)?;
    let current = rect_of(window.bounds());
    let fitted = window_state::fit_bounds(current, work_area(display.visible_bounds()), window_state::MINIMUM);
    move_to(window, current, fitted, "to stay on its display")
}

/// After the user moved the window (`afterMove`): as any Windows window it may hang partly off screen, and it is brought
/// back only when no display shows its top edge any more, so it could not be grabbed again. In 3.x the minimum size also
/// followed the display the window is on; GPUI fixes the minimum when it creates the window (`window_min_size`, kept in
/// `WindowsWindowState::min_size` with no way to change it), so here it stays the one the window opened with.
fn after_move(window: &mut Window, cx: &mut App) -> Option<Placing> {
    if window.is_maximized() || window.is_fullscreen() || !shown(window) {
        return None;
    }
    let areas: Vec<Rect> = cx.displays().iter().map(|display| work_area(display.visible_bounds())).collect();
    let current = rect_of(window.bounds());
    let fitted = pulled_back(current, &areas)?;
    move_to(window, current, fitted, "as no display showed its top edge")
}

/// Where a window the user moved goes: nowhere while a display shows its top edge ([`window_state::grip_area`], as when
/// restoring), else into the work area of the display it is on.
fn pulled_back(bounds: Rect, areas: &[Rect]) -> Option<Rect> {
    if window_state::grip_area(bounds, areas).is_some() {
        return None;
    }
    let display = window_state::display_matching(bounds, areas)?;
    Some(window_state::fit_bounds(bounds, areas[display], window_state::MINIMUM))
}

/// The move from `current` to `fitted`, if it is one.
fn move_to(window: &Window, current: Rect, fitted: Rect, reason: &str) -> Option<Placing> {
    if !worth_moving(current, fitted) {
        return None;
    }
    let placing = Placing { hwnd: hwnd(window)?, scale: f64::from(window.scale_factor()), bounds: fitted };
    log::info!("The window moves from {current:?} to {fitted:?} {reason}");
    Some(placing)
}

/// Rounding to whole pixels is not a reason to move.
fn worth_moving(current: Rect, fitted: Rect) -> bool {
    let off = (fitted.x - current.x).abs().max((fitted.y - current.y).abs()).max((fitted.width - current.width).abs()).max((fitted.height - current.height).abs());
    off > 1
}

fn hwnd(window: &Window) -> Option<HWND> {
    match raw_window_handle::HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}

/// Whether the window is shown and not minimized.
fn shown(window: &Window) -> bool {
    // SAFETY: plain queries on the window's live handle.
    hwnd(window).is_some_and(|hwnd| unsafe { IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 })
}

fn buttons_down() -> bool {
    // SAFETY: plain queries; the high bit is set while the button is down.
    unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON)) < 0 || GetAsyncKeyState(i32::from(VK_RBUTTON)) < 0 }
}

/// A move of the window's client area to logical bounds at the window's scale, decided inside `window.update`.
struct Placing {
    hwnd: HWND,
    scale: f64,
    bounds: Rect,
}

impl Placing {
    /// Give the window's client area its new bounds; the frame's invisible borders around it stay as they are. Only
    /// while the App is not borrowed: `SetWindowPos` runs GPUI's move, resize and display-change handlers before it
    /// returns, and they update the App (a display change panics on a borrowed one; a move would not reach `record`).
    fn apply(self) {
        let Self { hwnd, scale, bounds } = self;
        let physical = |value: i64| (value as f64 * scale).round() as i32;
        let (mut frame, mut client, mut origin) = (RECT::default(), RECT::default(), POINT::default());
        // SAFETY: the window's live handle and valid out pointers.
        let measured = unsafe { GetWindowRect(hwnd, &mut frame) != 0 && GetClientRect(hwnd, &mut client) != 0 && ClientToScreen(hwnd, &mut origin) != 0 };
        if !measured {
            return;
        }
        let (left, top) = (origin.x - frame.left, origin.y - frame.top);
        let (right, bottom) = (frame.right - (origin.x + client.right), frame.bottom - (origin.y + client.bottom));
        let (x, y, width, height) = (physical(bounds.x), physical(bounds.y), physical(bounds.width), physical(bounds.height));
        // SAFETY: the window's live handle; no change of order or activation.
        unsafe {
            SetWindowPos(hwnd, std::ptr::null_mut(), x - left, y - top, width + left + right, height + top + bottom, SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE)
        };
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    fn logical(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    #[test]
    fn takes_whole_pixels_inside_each_work_area() {
        // 2560×1600 at 225% with a 108 px taskbar: 2560×1492 physical.
        assert_eq!(work_area(logical(0., 0., 2560. / 2.25, 1492. / 2.25)), Rect { x: 0, y: 0, width: 1137, height: 663 });
        // A display to the left at 150%, starting at -1920 physical.
        assert_eq!(work_area(logical(-1920. / 1.5, 7. / 1.5, 1920. / 1.5, 1032. / 1.5)), Rect { x: -1280, y: 5, width: 1280, height: 687 });
        assert_eq!(work_area(logical(0., 0., 1920., 1032.)), Rect { x: 0, y: 0, width: 1920, height: 1032 });
        assert_eq!(rect_of(logical(12.4, -7.6, 1023.5, 649.6)), Rect { x: 12, y: -8, width: 1024, height: 650 });
    }

    #[test]
    fn places_the_window_on_fake_displays() {
        // The 225% laptop alone: maximized over 90% of it, with a minimum that leaves room for GPUI's borders.
        let laptop = logical(0., 0., 2560. / 2.25, 1492. / 2.25);
        let (placement, display, minimum) = choose(None, &[laptop], Some(0));
        assert_eq!(placement, window_state::Placement { bounds: Rect { x: 57, y: 7, width: 1023, height: 650 }, maximized: true });
        assert_eq!((display, minimum), (Some(0), window_state::Size { width: 909, height: 530 }));
        // A saved place on a second monitor that is still there opens there, normal.
        let monitor = logical(1138., 0., 1920., 1040.);
        let saved = WindowState { bounds: Rect { x: 1300, y: 40, width: 1380, height: 920 }, maximized: false };
        let (placement, display, _) = choose(Some(&saved), &[laptop, monitor], Some(0));
        assert_eq!(placement, window_state::Placement { bounds: saved.bounds, maximized: false });
        assert_eq!(display, Some(1));
        // Once the monitor is gone, the default on the laptop.
        let (placement, display, _) = choose(Some(&saved), &[laptop], Some(0));
        assert!(placement.maximized);
        assert_eq!(display, Some(0));
        // The primary display need not be the first; a smaller one lowers the minimum.
        let small = logical(-1092., 0., 1092., 574.);
        let (placement, display, minimum) = choose(None, &[monitor, small], Some(1));
        assert_eq!(placement, window_state::Placement { bounds: Rect { x: -1037, y: 0, width: 983, height: 574 }, maximized: true });
        assert_eq!((display, minimum), (Some(1), window_state::Size { width: 873, height: 459 }));
        // A large monitor keeps the 3.x minimum.
        let (_, _, minimum) = choose(None, &[monitor], Some(0));
        assert_eq!(minimum, window_state::Size { width: 950, height: 650 });
        // No display reported: the preferred size at the origin.
        let (placement, display, _) = choose(None, &[], None);
        assert_eq!((placement.bounds, placement.maximized, display), (Rect { x: 0, y: 0, width: 1380, height: 920 }, false, None));
    }

    #[test]
    fn a_moved_window_stays_where_it_was_dropped_while_its_top_edge_can_be_grabbed() {
        let rect = |x, y, width, height| Rect { x, y, width, height };
        let primary = rect(0, 0, 1920, 1040);
        let second = rect(1920, 0, 2560, 1400);
        // Mostly below the screen or past its left edge, as in the reported recording: not moved, not shrunk.
        assert_eq!(pulled_back(rect(300, 900, 1300, 870), &[primary]), None);
        assert_eq!(pulled_back(rect(-1000, 100, 1300, 870), &[primary]), None);
        // Dragged onto the second display with a little left on the first.
        assert_eq!(pulled_back(rect(1900, 100, 1300, 870), &[primary, second]), None);
        // Its top edge below the work area, or only a sliver left: back into the work area, at its size.
        assert_eq!(pulled_back(rect(300, 1020, 1300, 870), &[primary]), Some(rect(300, 170, 1300, 870)));
        assert_eq!(pulled_back(rect(1850, 100, 1300, 870), &[primary]), Some(rect(620, 100, 1300, 870)));
        // Off every display: into the nearest one.
        assert_eq!(pulled_back(rect(300, 1100, 1300, 870), &[primary, second]), Some(rect(300, 170, 1300, 870)));
        assert_eq!(pulled_back(rect(5000, 300, 1300, 870), &[primary, second]), Some(rect(3180, 300, 1300, 870)));
        assert_eq!(pulled_back(rect(0, 0, 10, 10), &[]), None);
        // Dropped on the taskbars across the seam of two equal displays, mostly on the second: onto the first, which
        // need not be the display GPUI had it on.
        let right = rect(1920, 0, 1920, 1040);
        assert_eq!(pulled_back(rect(1300, 1050, 1300, 870), &[primary, right]), Some(rect(620, 170, 1300, 870)));
    }

    #[test]
    fn moves_only_by_more_than_rounding() {
        let rect = |x, y, width, height| Rect { x, y, width, height };
        assert!(!worth_moving(rect(10, 10, 1300, 870), rect(11, 9, 1301, 869)));
        assert!(worth_moving(rect(10, 10, 1300, 870), rect(12, 10, 1300, 870)));
        assert!(worth_moving(rect(10, 10, 1300, 870), rect(10, 10, 1300, 867)));
    }
}
