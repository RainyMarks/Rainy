//! The language of the carrier's own messages, which the interface shows as they are.

use std::sync::atomic::{AtomicBool, Ordering};

static CHINESE: AtomicBool = AtomicBool::new(true);

/// Follow the interface language.
pub fn set_chinese(chinese: bool) {
    CHINESE.store(chinese, Ordering::Relaxed);
}

/// The message in the interface language.
pub fn localized(chinese: &str, english: &str) -> String {
    if CHINESE.load(Ordering::Relaxed) { chinese } else { english }.to_owned()
}
