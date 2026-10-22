//! The GPUI interface.

pub mod actions;
pub mod attention;
pub mod changes;
pub mod composer;
pub mod chat;
pub mod history;
pub mod markdown;
pub mod palette;
pub mod prompt;
pub mod ide;
pub mod save_dialog;
pub mod settings;
pub mod ssh;
pub mod tool_title;
pub mod transcript;
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

/// Send a question to the selected chat, or a new one in the project, and show the chat (3.x `onAsk`).
pub fn ask_ai(text: String, window: &mut Window, cx: &mut App) {
    window.dispatch_action(Box::new(crate::ui::ide::ShowChat), cx);
    let store = Store::global(cx);
    let workspace = crate::store::ide::Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
    let session = store.read(cx).selected.clone();
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let session = match session {
            Some(id) => Some(id),
            None => store.update(cx, |store, cx| store.create_chat(workspace, cx)).await,
        };
        let Some(session) = session else { return };
        let call = store.read_with(cx, |store, _| store.call("chat.send", serde_json::json!({ "sessionId": session, "text": text, "mode": "queue" })));
        if let Err(error) = call.await {
            let _ = handle.update(cx, |_, window, cx| {
                use gpui_kit::component::WindowExt as _;
                window.push_notification(gpui_kit::component::notification::Notification::error(error.message), cx)
            });
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
