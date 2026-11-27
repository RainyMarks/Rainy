//! Shared Windows Registry read helpers used by shell integration, updater scope detection, and diagnostics.

#![cfg(windows)]

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, ERROR_UNSUPPORTED_TYPE, WIN32_ERROR};
use windows_sys::Win32::System::Registry::{
    HKEY, KEY_READ, KEY_WOW64_64KEY, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey, RegGetValueW, RegOpenKeyExW,
};

use crate::text::wide;

/// Maximum REG_SZ value buffer size we ever allocate (1 MiB in bytes) to guard against runaway growth.
const MAX_REG_STRING_BYTES: usize = 1024 * 1024;
const MAX_GROWTH_ATTEMPTS: usize = 8;

/// Decode a UTF-16 slice up to the first NUL terminator.
pub(crate) fn until_nul(units: &[u16]) -> String {
    let end = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

pub(crate) fn is_missing(status: WIN32_ERROR) -> bool {
    matches!(status, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
}

/// Read a `REG_SZ` string from `root\subkey` value `name` (`""` for default value).
/// Returns `Ok(None)` when the key/value does not exist or has a non-string type; `Err(win32_status)` on other errors.
pub(crate) fn read_string(root: HKEY, subkey: &str, name: &str, wow64_64: bool) -> Result<Option<String>, WIN32_ERROR> {
    let key_path = wide(subkey);
    let value_name = wide(name);
    if wow64_64 {
        let mut handle = std::ptr::null_mut();
        // SAFETY: `key_path` is NUL-terminated; handle is closed before returning.
        let status = unsafe { RegOpenKeyExW(root, key_path.as_ptr(), 0, KEY_READ | KEY_WOW64_64KEY, &mut handle) };
        if is_missing(status) {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            return Err(status);
        }
        let result = read_string_from_handle(handle, std::ptr::null(), value_name.as_ptr());
        // SAFETY: handle opened above, closed once.
        unsafe { RegCloseKey(handle) };
        result
    } else {
        read_string_from_handle(root, key_path.as_ptr(), value_name.as_ptr())
    }
}

fn read_string_from_handle(root: HKEY, subkey: *const u16, value_name: *const u16) -> Result<Option<String>, WIN32_ERROR> {
    let mut buffer = vec![0u16; 256];
    for _ in 0..MAX_GROWTH_ATTEMPTS {
        let Ok(mut size) = u32::try_from(buffer.len() * 2) else {
            return Err(ERROR_MORE_DATA);
        };
        // SAFETY: `subkey` (null or NUL-terminated) and `value_name` (NUL-terminated) outlive the call;
        // `buffer` holds `size` writable bytes.
        let status = unsafe {
            RegGetValueW(root, subkey, value_name, RRF_RT_REG_SZ, std::ptr::null_mut(), buffer.as_mut_ptr().cast(), &mut size)
        };
        match status {
            ERROR_SUCCESS => {
                let units = &buffer[..(size as usize / 2).min(buffer.len())];
                return Ok(Some(until_nul(units)));
            }
            ERROR_MORE_DATA => {
                let needed_bytes = (size as usize).saturating_add(2);
                if needed_bytes > MAX_REG_STRING_BYTES {
                    return Err(ERROR_MORE_DATA);
                }
                buffer.resize(needed_bytes.div_ceil(2), 0);
            }
            ERROR_UNSUPPORTED_TYPE => return Ok(None),
            status if is_missing(status) => return Ok(None),
            status => return Err(status),
        }
    }
    Err(ERROR_MORE_DATA)
}

/// Read a `REG_DWORD` value from `root\subkey` value `name`.
pub(crate) fn read_dword(root: HKEY, subkey: &str, name: &str) -> Option<u32> {
    let (key, value) = (wide(subkey), wide(name));
    let mut number = 0u32;
    let mut size = 4u32;
    // SAFETY: NUL-terminated names and writable DWORD of `size` bytes.
    let status = unsafe {
        RegGetValueW(root, key.as_ptr(), value.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&raw mut number).cast(), &mut size)
    };
    (status == ERROR_SUCCESS).then_some(number)
}
