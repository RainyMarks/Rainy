//! Asking for the user's attention while the window is in the background.

use gpui_kit::*;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::UI::WindowsAndMessaging::{FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx};

/// Flash the taskbar button until the window comes to the front, as for a chat waiting for approval.
pub fn flash(window: &Window) {
    if window.is_window_active() {
        return;
    }
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    if let RawWindowHandle::Win32(handle) = handle.as_raw() {
        let info = FLASHWINFO {
            cbSize: size_of::<FLASHWINFO>() as u32,
            hwnd: handle.hwnd.get() as _,
            dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
            uCount: 0,
            dwTimeout: 0,
        };
        // SAFETY: `info` is a complete FLASHWINFO for this window's live handle.
        unsafe { FlashWindowEx(&info) };
    }
}
