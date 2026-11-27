//! The GPUI interface.

pub mod actions;
pub mod agent;
pub mod attention;
pub mod changes;
pub mod composer;
pub mod copy;
pub mod chat;
pub mod history;
pub mod markdown;
pub mod metrics;
pub mod motion;
pub mod notify;
pub mod optics;
pub mod palette;
pub mod prompt;
pub mod quota;
pub mod ide;
pub mod save_dialog;
pub mod settings;
pub mod ssh;
pub mod tool_title;
pub mod transcript;
pub mod tunnels;
pub mod update;
pub mod workbench;

use gpui_kit::{App, SharedString, Window};

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

/// A Host failure as a notification in the interface language: its meaning, and the system's own words below it
/// when they were replaced (`i18n::describe_error`).
pub fn error_notification(cx: &App, error: &rainy_protocol::RpcError) -> gpui_kit::component::notification::Notification {
    use gpui_kit::component::notification::Notification;
    match i18n::describe_error(locale(cx), error) {
        (text, Some(detail)) => Notification::error(detail).title(text),
        (text, None) => Notification::error(text),
    }
}

/// A Host failure as one text in the interface language, the system's own words on the next line.
pub fn error_text(cx: &App, error: &rainy_protocol::RpcError) -> SharedString {
    i18n::error_text(locale(cx), error).into()
}

/// Inject quoted context (such as terminal output or a diagnostic) into the active chat composer without sending it yet.
pub fn insert_into_composer(snippet: String, window: &mut Window, cx: &mut App) {
    if let Some(workbench) = cx.try_global::<crate::ui::workbench::WorkbenchHandle>().and_then(|h| h.0.upgrade()) {
        workbench.update(cx, |workbench, cx| workbench.inject_into_chat(snippet, window, cx));
    } else {
        ask_ai(snippet, window, cx);
    }
}

/// Send a question to the selected chat, or a new one in the project, and show the chat (3.x `onAsk`).
pub fn ask_ai(text: String, window: &mut Window, cx: &mut App) {
    window.dispatch_action(Box::new(crate::ui::ide::ShowChat), cx);
    let store = Store::global(cx);
    let workspace = crate::store::ide::Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
    let (session, since) = (store.read(cx).selected.clone(), store.read(cx).selections);
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let failed = |error: rainy_protocol::RpcError, cx: &mut gpui_kit::AsyncApp| {
            let _ = handle.update(cx, |_, window, cx| {
                use gpui_kit::component::WindowExt as _;
                let text = trf(cx, "chat.sendFailed", &[("message", &error_text(cx, &error))]);
                window.push_notification(gpui_kit::component::notification::Notification::error(text), cx)
            });
        };
        let session = match session {
            Some(id) => id,
            None => match store.update(cx, |store, cx| store.create_chat(workspace, cx)).await {
                Ok(id) => {
                    store.update(cx, |store, cx| {
                        if store.selections == since {
                            store.select(Some(id.clone()), cx);
                        }
                    });
                    id
                }
                Err(error) => return failed(error, cx),
            },
        };
        let call = store.read_with(cx, |store, _| store.call("chat.send", serde_json::json!({ "sessionId": session, "text": text, "mode": "queue" })));
        if let Err(error) = call.await {
            failed(error, cx);
        }
    })
    .detach();
}

/// Follow a link in a reply: web pages open in the browser, project files in the editor, anything else nothing.
/// Other schemes (`file:`, custom protocols) could start programs, so they are never handed to the system.
pub fn open_link(url: &str, window: &mut Window, cx: &mut App) {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        cx.open_url(url);
        return;
    }
    if lower.contains("://") || lower.starts_with("javascript:") || lower.starts_with("data:") || lower.starts_with("mailto:") {
        return;
    }
    // A path with an optional `#L12` line, relative to the project or absolute inside it.
    let (path, line) = match url.split_once("#L") {
        Some((path, line)) => (path, line.parse::<u32>().ok()),
        None => (url, None),
    };
    let ide = crate::store::ide::Ide::global(cx);
    let key = {
        let ide = ide.read(cx);
        if ide.workspace.is_none() {
            return;
        }
        ide.key_from_absolute(path).or_else(|| (!path.starts_with('/') && !path.contains(':')).then(|| path.trim_start_matches("./").to_owned()))
    };
    let Some(key) = key else { return };
    ide.update(cx, |ide, cx| match line {
        Some(line) => ide.reveal(key, line.saturating_sub(1), 0, window, cx),
        None => ide.open(key, true, window, cx),
    });
    crate::ui::agent::reveal_editor(cx);
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

const KIB: f64 = 1024.0;
const MIB: f64 = KIB * 1024.0;
const GIB: f64 = MIB * 1024.0;
const TIB: f64 = GIB * 1024.0;

/// Formats a scaled quantity with 1 decimal below `10.0` and whole digits from `10.0` upward.
fn format_scaled_unit(value: f64, sep: &str, unit: &str) -> String {
    if value < 10.0 {
        format!("{value:.1}{sep}{unit}")
    } else {
        format!("{}{sep}{unit}", value.round())
    }
}

/// Binary byte size for logs and file panels: `512 B`, `1.5 KiB`, `2.0 MiB`, `1.25 GiB`, `1.50 TiB`.
pub fn format_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= TIB {
        format!("{:.2} TiB", b / TIB)
    } else if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Compact status-bar byte size: `512M`, `2.5G`, `48G`, `1.2T`.
pub fn format_bytes_compact(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= TIB {
        format!("{:.1}T", b / TIB)
    } else if b >= GIB {
        format_scaled_unit(b / GIB, "", "G")
    } else {
        format!("{:.0}M", b / MIB)
    }
}

/// Compact 3.x-compatible binary size without spaces: `512B`, `9.5KB`, `36MB`, `1.2GB`.
pub fn format_bytes_short(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut value = bytes as f64 / KIB;
    for unit in ["KB", "MB"] {
        if value < KIB {
            return format_scaled_unit(value, "", unit);
        }
        value /= KIB;
    }
    format_scaled_unit(value, "", "GB")
}

/// Small-file binary size in bytes or `KB`: `900 B`, `2.0 KB`.
pub fn format_bytes_kb(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{:.1} KB", bytes as f64 / KIB)
    }
}

/// Decimal byte size for model catalogs and downloads: `620 MB`, `1.5 GB`, `12 GB`.
pub fn format_bytes_decimal(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 {
        format_gb(gb)
    } else {
        format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

/// Decimal gigabytes: `1.5 GB`, `12 GB`.
pub fn format_gb(gb: f64) -> String {
    format_scaled_unit(gb, " ", "GB")
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
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.0 MiB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.00 GiB");
        assert_eq!(format_bytes_compact(512 * 1024 * 1024), "512M");
        assert_eq!(format_bytes_compact(2 * 1024 * 1024 * 1024 + 512 * 1024 * 1024), "2.5G");
        assert_eq!(format_bytes_compact(48 * 1024 * 1024 * 1024), "48G");
        assert_eq!(format_bytes_compact(2 * 1024 * 1024 * 1024 * 1024), "2.0T");
        assert_eq!(format_bytes_short(512), "512B");
        assert_eq!(format_bytes_short(9_728), "9.5KB");
        assert_eq!(format_bytes_short(38_191_707), "36MB");
        assert_eq!(format_bytes_short(1_288_490_189), "1.2GB");
        assert_eq!((format_bytes_kb(900), format_bytes_kb(2048)), ("900 B".into(), "2.0 KB".into()));
        assert_eq!(format_bytes_decimal(620_430_878), "620 MB");
        assert_eq!(format_bytes_decimal(1_500_000_000), "1.5 GB");
        assert_eq!(format_bytes_decimal(12_300_000_000), "12 GB");
    }
}
