//! Copy buttons that show they worked: the one just used shows a check for a moment.

use std::cell::Cell;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

use gpui_kit::component::IconName;
use gpui_kit::*;

/// How long a copy button shows its check.
const SHOWN: Duration = Duration::from_millis(1500);

thread_local! {
    /// The button that copied last, and when.
    static LAST: Cell<Option<(u64, Instant)>> = const { Cell::new(None) };
}

/// What a copy button goes by: what it copies, or the row it belongs to.
pub fn key_of(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// The icon of the copy button `key`: a check right after it copied.
pub fn icon(key: u64) -> IconName {
    let copied = LAST.get().is_some_and(|(last, at)| last == key && at.elapsed() < SHOWN);
    if copied { IconName::Check } else { IconName::Copy }
}

/// Put `text` on the clipboard from the button `key`, which then shows a check for a moment.
pub fn copy(key: u64, text: &str, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
    LAST.set(Some((key, Instant::now())));
    cx.refresh_windows();
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_executor().timer(SHOWN).await;
        let _ = cx.update(|cx| cx.refresh_windows());
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
        assert!(matches!(icon(one), IconName::Copy));
        LAST.set(Some((one, Instant::now())));
        assert!(matches!(icon(one), IconName::Check) && matches!(icon(other), IconName::Copy));
        if let Some(earlier) = Instant::now().checked_sub(SHOWN) {
            LAST.set(Some((one, earlier)));
            assert!(matches!(icon(one), IconName::Copy));
        }
    }
}
