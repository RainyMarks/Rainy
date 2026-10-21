//! The GPUI interface.

pub mod actions;
pub mod changes;
pub mod composer;
pub mod chat;
pub mod history;
pub mod prompt;
pub mod ide;
pub mod settings;
pub mod tool_title;
pub mod transcript;
pub mod workbench;

use gpui_kit::{App, SharedString};

use crate::i18n::{self, Locale};
use crate::store::Store;

/// The interface language.
pub fn locale(cx: &App) -> Locale {
    Store::global(cx).read(cx).locale()
}

/// One interface string, such as `chat.send`.
pub fn tr(cx: &App, key: &str) -> SharedString {
    SharedString::new_static(i18n::text(locale(cx), key))
}

/// One interface string with its `{name}` placeholders filled.
pub fn trf(cx: &App, key: &str, vars: &[(&str, &dyn std::fmt::Display)]) -> SharedString {
    i18n::format(locale(cx), key, vars).into()
}

/// `12s`, `3m 4s` or `1h 2m` (`formatDuration` in `Transcript.tsx`).
pub fn format_duration(ms: f64) -> String {
    let seconds = (ms / 1000.0).round().max(0.0) as u64;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m {}s", seconds % 60);
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// `950`, `1.2k`, `12k` or `1.2M` (`formatTokens` in `Transcript.tsx`).
pub fn format_tokens(tokens: f64) -> String {
    if tokens < 1000.0 {
        format!("{tokens}")
    } else if tokens < 1_000_000.0 {
        if tokens < 10_000.0 { format!("{:.1}k", tokens / 1000.0) } else { format!("{:.0}k", tokens / 1000.0) }
    } else {
        format!("{:.1}M", tokens / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_duration(1234.0), "1s");
        assert_eq!(format_duration(45_000.0), "45s");
        assert_eq!(format_duration(185_000.0), "3m 5s");
        assert_eq!(format_duration(3_720_000.0), "1h 2m");
        assert_eq!(format_tokens(950.0), "950");
        assert_eq!(format_tokens(1_234.0), "1.2k");
        assert_eq!(format_tokens(12_345.0), "12k");
        assert_eq!(format_tokens(1_260_000.0), "1.3M");
    }
}
