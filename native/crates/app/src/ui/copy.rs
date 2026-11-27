//! Copy buttons that show they worked: the one just used shows a check for a moment, with generation-debounced local
//! refresh and a subtle one-shot entrance micro-feedback.

use std::cell::Cell;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::*;

use crate::ui::motion;

/// How long a copy button shows its check.
const SHOWN: Duration = Duration::from_millis(1500);
/// Entrance duration for the check micro-feedback.
#[allow(dead_code)]
const CHECK_ENTER: Duration = Duration::from_millis(160);

thread_local! {
    /// Monotonic generation to cancel stale timers when copying again quickly.
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    /// The button that copied last, when, and its generation.
    static LAST: Cell<Option<(u64, Instant, u64)>> = const { Cell::new(None) };
}

/// What a copy button goes by: what it copies, or the row it belongs to.
pub fn key_of(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Whether `key` is currently inside its "just copied" confirmation window.
pub fn is_copied(key: u64) -> bool {
    LAST.get().is_some_and(|(last, at, _)| last == key && at.elapsed() < SHOWN)
}

/// Current generation for `key` if it is actively showing its check (used to key one-shot animations).
#[allow(dead_code)]
fn active_generation(key: u64) -> Option<u64> {
    LAST.get().filter(|(last, at, _)| *last == key && at.elapsed() < SHOWN).map(|(_, _, generation)| generation)
}

/// The icon of the copy button `key`: a check right after it copied.
pub fn icon(key: u64) -> IconName {
    if is_copied(key) { IconName::Check } else { IconName::Copy }
}

/// A readable success color for the check mark (at least WCAG 4.5:1 against the active background).
#[allow(dead_code)]
pub fn copied_color(cx: &App) -> Hsla {
    let theme = cx.theme();
    crate::ui::optics::look::readable(theme.success, theme.background, crate::ui::optics::look::MUTED)
}

/// Renders the copy/check icon for `key` with a subtle 160ms one-shot scale + fade confirmation when copied.
///
/// Respects reduced motion and stops requesting frames immediately after settling (0 frames at rest).
#[allow(dead_code)]
pub fn indicator(key: u64, cx: &App) -> AnyElement {
    let Some(generation) = active_generation(key) else {
        return Icon::new(IconName::Copy).xsmall().into_any_element();
    };
    let check = Icon::new(IconName::Check).xsmall().text_color(copied_color(cx));
    if !motion::allowed(cx) {
        return check.into_any_element();
    }
    check
        .with_animation(
            ElementId::NamedInteger(format!("copy-ok-{key}").into(), generation),
            Animation::new(CHECK_ENTER).with_easing(motion::ease_out_cubic),
            |icon, delta| {
                let scale = 0.80 + 0.20 * delta;
                icon.transform(Transformation::scale(size(scale, scale))).opacity(0.35 + 0.65 * delta)
            },
        )
        .into_any_element()
}

fn record(key: u64) -> u64 {
    let next = GENERATION.get().wrapping_add(1);
    GENERATION.set(next);
    LAST.set(Some((key, Instant::now(), next)));
    next
}

fn still_current(expected: u64) -> bool {
    GENERATION.get() == expected
}

/// Put `text` on the clipboard from the button `key`, which then shows a check for a moment.
///
/// Refreshes only the active window instead of broadcasting `cx.refresh_windows()`, and debounces rapid clicks so
/// superseded timers trigger no redundant redraw.
pub fn copy(key: u64, text: &str, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
    let generation = record(key);
    let target = cx.active_window().or_else(|| cx.windows().first().copied());
    if let Some(handle) = target {
        let _ = handle.update(cx, |_, window, _| window.refresh());
    }
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_executor().timer(SHOWN).await;
        if !still_current(generation) {
            return;
        }
        if let Some(handle) = target {
            let _ = handle.update(cx, |_, window, _| window.refresh());
        }
    })
    .detach();
}

/// Put `text` on the clipboard and notify only the current view on the given window (avoids full-window refresh).
#[allow(dead_code)]
pub fn copy_in(key: u64, text: &str, window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
    let generation = record(key);
    let view = window.current_view();
    cx.notify(view);
    window
        .spawn(cx, async move |cx| {
            cx.background_executor().timer(SHOWN).await;
            if !still_current(generation) {
                return;
            }
            let _ = cx.update(|_, cx| cx.notify(view));
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn only_the_button_that_copied_shows_the_check() {
        let (one, other) = (key_of("fn main() {}"), key_of("print(1)"));
        assert_ne!(one, other);
        LAST.set(None);
        assert!(matches!(icon(one), IconName::Copy));
        let gen1 = record(one);
        assert!(still_current(gen1));
        assert!(is_copied(one) && !is_copied(other));
        assert!(matches!(icon(one), IconName::Check) && matches!(icon(other), IconName::Copy));
        let gen2 = record(other);
        assert!(!still_current(gen1) && still_current(gen2));
        assert!(matches!(icon(other), IconName::Check) && matches!(icon(one), IconName::Copy));
        if let Some(earlier) = Instant::now().checked_sub(SHOWN) {
            LAST.set(Some((other, earlier, gen2)));
            assert!(matches!(icon(other), IconName::Copy));
            assert!(!is_copied(other));
        }
    }
}
