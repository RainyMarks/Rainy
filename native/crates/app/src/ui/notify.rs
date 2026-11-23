//! Windows notifications for chats that finish or wait for an approval while Rainy is in the background, as the Codex
//! app sends them. They are the balloon of a notification-area icon that is there only while one shows, which Windows
//! 10 and 11 present as an ordinary notification: nothing is registered with Windows (no app identity, no shortcut).
//! A click on one brings Rainy to the front with that chat.
//!
//! The icon belongs to a message-only window on a thread of its own, which receives the clicks; the interface hands it
//! notifications through [`show`] and gets clicks back through the channel given to [`start`].

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    ExtractIconExW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_LARGE_ICON, NIIF_USER, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HICON, HWND_MESSAGE, MSG,
    PostMessageW, RegisterClassW, WM_APP, WM_LBUTTONUP, WNDCLASSW,
};

/// A notification to show: its title, its line and the chat a click opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub title: String,
    pub text: String,
    pub chat: String,
}

/// The interface asks the icon window to show what is queued.
const SHOW: u32 = WM_APP + 1;
/// The icon's own messages: the low word of `lParam` says what happened (`NOTIFYICON_VERSION_4`).
const CALLBACK: u32 = WM_APP + 2;
/// A click or Enter on the icon itself (`NOTIFYICON_VERSION_4`).
const NIN_SELECT: u32 = 0x0400;
const NIN_KEYSELECT: u32 = 0x0401;
const NIN_BALLOONUSERCLICK: u32 = 0x0405;
const ICON_ID: u32 = 1;

struct State {
    window: HWND,
    queue: VecDeque<Note>,
    /// The chat of the notification shown now.
    shown: Option<String>,
    icon: HICON,
    shown_icon: bool,
    clicks: async_channel::Sender<String>,
}

// SAFETY: the window and icon handles are only used on the icon thread; other threads only queue notes and post a
// message to the window, which Windows allows from any thread.
unsafe impl Send for State {}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn wide(text: &str, into: &mut [u16]) {
    let mut units: Vec<u16> = text.encode_utf16().take(into.len() - 1).collect();
    units.push(0);
    into[..units.len()].copy_from_slice(&units);
}

fn data(state: &State) -> NOTIFYICONDATAW {
    // SAFETY: an all-zero NOTIFYICONDATAW is valid; the fields used are set below.
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = state.window;
    data.uID = ICON_ID;
    data
}

fn remove_icon(state: &mut State) {
    if state.shown_icon {
        let data = data(state);
        // SAFETY: a complete NOTIFYICONDATAW naming this window's icon.
        unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        state.shown_icon = false;
    }
    state.shown = None;
}

/// Show the next queued note as the icon's balloon, adding the icon when it is not there.
fn show_next(state: &mut State) {
    let Some(note) = state.queue.pop_back() else {
        return;
    };
    state.queue.clear();
    let mut data = data(state);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_INFO;
    data.uCallbackMessage = CALLBACK;
    data.hIcon = state.icon;
    data.hBalloonIcon = state.icon;
    data.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
    wide("Rainy", &mut data.szTip);
    wide(&note.title, &mut data.szInfoTitle);
    wide(&note.text, &mut data.szInfo);
    // SAFETY: complete NOTIFYICONDATAW values for this window's icon.
    unsafe {
        // An icon Explorer lost when it restarted cannot be changed: it is added again.
        if state.shown_icon && Shell_NotifyIconW(NIM_MODIFY, &data) == 0 {
            state.shown_icon = false;
        }
        if !state.shown_icon {
            if Shell_NotifyIconW(NIM_ADD, &data) != 0 {
                data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                Shell_NotifyIconW(NIM_SETVERSION, &data);
                state.shown_icon = true;
            } else {
                log::warn!(
                    "a notification could not be shown: error {}",
                    windows_sys::Win32::Foundation::GetLastError()
                );
            }
        }
    }
    if state.shown_icon {
        log::info!("Notification shown for chat {}", note.chat);
    }
    state.shown = Some(note.chat);
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if let Some(state) = STATE.get() {
        match message {
            SHOW => {
                show_next(&mut state.lock().unwrap());
                return 0;
            }
            CALLBACK => {
                let event = (lparam as u32) & 0xffff;
                let mut state = state.lock().unwrap();
                match event {
                    // The balloon's timing out is not the end: with Focus Assist or a full-screen game it times out at
                    // once and waits in the notification center, which loses it when the icon goes. The icon stays
                    // until the notification or the icon is clicked, or Rainy comes to the front.
                    NIN_BALLOONUSERCLICK | NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONUP => {
                        if let Some(chat) = state.shown.clone() {
                            let _ = state.clicks.try_send(chat);
                        }
                        remove_icon(&mut state);
                    }
                    _ => {}
                }
                return 0;
            }
            _ => {}
        }
    }
    // SAFETY: the default handling of a message of this window.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Start the icon window; clicks on a notification arrive on `clicks` as the chat's id. Once per process.
pub fn start(clicks: async_channel::Sender<String>) {
    if STATE.get().is_some() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("notifications".into())
        .spawn(move || {
            let class: Vec<u16> = "RainyNotifications\0".encode_utf16().collect();
            // SAFETY: a window class and a message-only window of this module, with a NUL-terminated class name; the
            // icon comes from Rainy's own executable.
            unsafe {
                let instance = GetModuleHandleW(std::ptr::null());
                let mut definition: WNDCLASSW = std::mem::zeroed();
                definition.lpfnWndProc = Some(procedure);
                definition.hInstance = instance;
                definition.lpszClassName = class.as_ptr();
                RegisterClassW(&definition);
                let window = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                if window.is_null() {
                    log::warn!("notifications are unavailable: no message window");
                    return;
                }
                let mut icon: HICON = std::ptr::null_mut();
                if let Ok(exe) = std::env::current_exe() {
                    let path: Vec<u16> = exe
                        .as_os_str()
                        .to_string_lossy()
                        .encode_utf16()
                        .chain(Some(0))
                        .collect();
                    ExtractIconExW(path.as_ptr(), 0, &mut icon, std::ptr::null_mut(), 1);
                }
                let _ = STATE.set(Mutex::new(State {
                    window,
                    queue: VecDeque::new(),
                    shown: None,
                    icon,
                    shown_icon: false,
                    clicks,
                }));
                let mut message: MSG = std::mem::zeroed();
                while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                    DispatchMessageW(&message);
                }
            }
        });
}

/// Whether the person is looking at Rainy: the foreground window is one of this process's. GPUI's active window is
/// the thread's own, which a window that never came to the front can still be.
pub fn in_front() -> bool {
    // SAFETY: two queries about the foreground window; `process` is writable.
    unsafe {
        let window = GetForegroundWindow();
        let mut process = 0u32;
        !window.is_null() && GetWindowThreadProcessId(window, &mut process) != 0 && process == std::process::id()
    }
}

/// Show a notification; a newer one replaces the one shown.
pub fn show(note: Note) {
    let Some(state) = STATE.get() else { return };
    let window = {
        let mut state = state.lock().unwrap();
        state.queue.push_back(note);
        state.window
    };
    // SAFETY: posting a message to a live window is allowed from any thread.
    unsafe { PostMessageW(window, SHOW, 0, 0) };
}

/// Take back the notification once Rainy is in front again: the sidebar's unread marks say the rest.
pub fn dismiss() {
    let Some(state) = STATE.get() else { return };
    remove_icon(&mut state.lock().unwrap());
}

/// What to say about a chat that changed while Rainy was in the background: it finished, or it waits for an
/// approval. None when there is nothing to say.
pub fn note_for(
    title: &str,
    was_running: bool,
    now_idle: bool,
    newly_waiting: bool,
    zh: bool,
) -> Option<(String, String)> {
    let locale = if zh { crate::i18n::Locale::Zh } else { crate::i18n::Locale::En };
    let title = if title.trim().is_empty() {
        crate::i18n::text(locale, "native.notifyUntitledChat")
    } else {
        title.trim()
    };
    if newly_waiting {
        return Some((
            crate::i18n::text(locale, "native.notifyApprovalTitle").to_owned(),
            crate::i18n::format(locale, "native.notifyApprovalBody", &[("title", &title)]),
        ));
    }
    (was_running && now_idle).then(|| {
        (
            crate::i18n::text(locale, "native.notifyDoneTitle").to_owned(),
            crate::i18n::format(locale, "native.notifyDoneBody", &[("title", &title)]),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_what_happened_in_the_interface_language() {
        assert_eq!(
            note_for("修复登录", true, true, false, true),
            Some(("对话已完成".into(), "「修复登录」已经回复完".into()))
        );
        assert_eq!(
            note_for("", false, false, true, false),
            Some((
                "Waiting for approval".into(),
                "\"New chat\" needs your approval".into()
            ))
        );
        assert_eq!(note_for("x", false, true, false, true), None);
        assert_eq!(note_for("x", true, false, false, true), None);
    }

    /// Shows a real notification and clicks it through the icon's own message: the click comes back as the chat. Run
    /// by hand (it puts up a notification): `cargo test -p rainy shows_a_note -- --ignored`.
    #[test]
    #[ignore]
    fn shows_a_note_and_reports_its_click() {
        let (clicks, clicked) = async_channel::unbounded();
        start(clicks);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while STATE.get().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        show(Note {
            title: "对话已完成".into(),
            text: "「测试」已经回复完".into(),
            chat: "chat-1".into(),
        });
        // Long enough for a balloon that Focus Assist sends straight to the notification center to time out.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let window = {
            let state = STATE.get().expect("the icon window").lock().unwrap();
            assert!(state.shown_icon, "the notification icon was not added");
            assert_eq!(state.shown.as_deref(), Some("chat-1"));
            state.window
        };
        // SAFETY: the icon window lives for the process.
        unsafe { PostMessageW(window, CALLBACK, 0, NIN_BALLOONUSERCLICK as isize) };
        let chat = wait_for(&clicked);
        assert_eq!(chat.as_deref(), Some("chat-1"));
        let state = STATE.get().unwrap().lock().unwrap();
        assert!(
            !state.shown_icon && state.shown.is_none(),
            "a clicked notification goes away"
        );
    }

    fn wait_for(clicked: &async_channel::Receiver<String>) -> Option<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Ok(chat) = clicked.try_recv() {
                return Some(chat);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        None
    }

    #[test]
    fn a_title_fits_the_balloon() {
        let mut buffer = [0u16; 8];
        wide("一二三四五六七八九十", &mut buffer);
        assert_eq!(buffer[7], 0);
        assert_eq!(String::from_utf16_lossy(&buffer[..7]), "一二三四五六七");
    }
}
