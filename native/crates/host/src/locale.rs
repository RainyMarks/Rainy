//! Messages written in the interface language chosen in Settings (3.x `shared/locale.ts`).

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

/// Follow the language Settings saved: `zh` or `en`.
pub fn set(locale: &str) {
    ENGLISH.store(locale == "en", Ordering::Relaxed);
}

pub fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// The message in the interface language.
pub fn l(zh: &str, en: &str) -> String {
    if english() { en.to_owned() } else { zh.to_owned() }
}
