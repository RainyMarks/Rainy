//! Win32 HKEY_CURRENT_USER registry adapter, QA-redirected hive, and Default Apps launcher.

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryInfoKeyW,
    RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify, ShellExecuteW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::catalog::{DEFAULT_APPS, DEFAULT_APPS_LIST};
use super::plan::Registry;
use crate::locale::localized;
use crate::registry_win;
use crate::text::wide;

/// HKEY_CURRENT_USER through the Win32 registry API.
pub struct CurrentUser;

/// HKEY_CURRENT_USER with every key below `Software\RainyQA\<name>`: a development build run with
/// `RAINY_QA_REGISTRY=<name>` writes its Explorer integration there, so QA sees exactly what Rainy would register
/// while Windows acts on none of it.
pub struct Redirected {
    prefix: String,
}

impl Redirected {
    /// None for a name that is not letters, digits, `-` and `_`.
    pub fn new(name: &str) -> Option<Self> {
        let valid = !name.is_empty()
            && name.len() <= 64
            && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
        valid.then(|| Self {
            prefix: format!(r"Software\RainyQA\{name}\"),
        })
    }

    fn key(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }
}

impl Registry for Redirected {
    fn read(&self, key: &str, name: &str) -> Result<Option<String>> {
        CurrentUser.read(&self.key(key), name)
    }
    fn exists(&self, key: &str) -> Result<bool> {
        CurrentUser.exists(&self.key(key))
    }
    fn has_value(&self, key: &str, name: &str) -> Result<bool> {
        CurrentUser.has_value(&self.key(key), name)
    }
    fn empty(&self, key: &str) -> Result<bool> {
        CurrentUser.empty(&self.key(key))
    }
    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<()> {
        CurrentUser.set(&self.key(key), name, value)
    }
    fn delete_tree(&mut self, key: &str) -> Result<()> {
        CurrentUser.delete_tree(&self.key(key))
    }
    fn delete_key(&mut self, key: &str) -> Result<()> {
        CurrentUser.delete_key(&self.key(key))
    }
    fn delete_value(&mut self, key: &str, name: &str) -> Result<()> {
        CurrentUser.delete_value(&self.key(key), name)
    }
    /// Windows is not told: nothing real changed.
    fn associations_changed(&mut self) {}
}

fn failed(status: WIN32_ERROR, doing: &str, key: &str) -> anyhow::Error {
    let chinese = match doing {
        "Opening" => format!(r"无法打开 HKCU\{key}"),
        "Reading" => format!(r"无法读取 HKCU\{key}"),
        "Creating" => format!(r"无法创建 HKCU\{key}"),
        "Writing" => format!(r"无法写入 HKCU\{key}"),
        "Deleting" => format!(r"无法删除 HKCU\{key}"),
        _ => format!(r"无法删除 HKCU\{key} 中的值"),
    };
    anyhow::Error::new(std::io::Error::from_raw_os_error(status as i32)).context(localized(&chinese, &format!(r"{doing} HKCU\{key}")))
}

/// An open key, closed when dropped.
struct Key(HKEY);

impl Key {
    /// `HKCU\<path>` opened with `access`, or `None` when it does not exist.
    fn open(path: &str, access: REG_SAM_FLAGS) -> Result<Option<Self>> {
        let name = wide(path);
        let mut handle = std::ptr::null_mut();
        // SAFETY: `name` ends with NUL; the handle is closed by `Key`.
        match unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, name.as_ptr(), 0, access, &mut handle) } {
            ERROR_SUCCESS => Ok(Some(Self(handle))),
            status if registry_win::is_missing(status) => Ok(None),
            status => Err(failed(status, "Opening", path)),
        }
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle was opened for this `Key` and is closed once.
        unsafe { RegCloseKey(self.0) };
    }
}

impl Registry for CurrentUser {
    fn read(&self, key: &str, name: &str) -> Result<Option<String>> {
        registry_win::read_string(HKEY_CURRENT_USER, key, name, false).map_err(|status| failed(status, "Reading", key))
    }

    fn exists(&self, key: &str) -> Result<bool> {
        Ok(Key::open(key, KEY_READ)?.is_some())
    }

    fn has_value(&self, key: &str, name: &str) -> Result<bool> {
        let Some(handle) = Key::open(key, KEY_QUERY_VALUE)? else { return Ok(false) };
        let value = wide(name);
        let null = std::ptr::null_mut();
        // SAFETY: asks only whether the value exists; nothing is read into memory.
        match unsafe { RegQueryValueExW(handle.0, value.as_ptr(), std::ptr::null(), null, null.cast(), null) } {
            ERROR_SUCCESS => Ok(true),
            status if registry_win::is_missing(status) => Ok(false),
            status => Err(failed(status, "Reading", key)),
        }
    }

    fn empty(&self, key: &str) -> Result<bool> {
        let Some(handle) = Key::open(key, KEY_READ)? else { return Ok(false) };
        let (mut keys, mut values) = (0u32, 0u32);
        let null = std::ptr::null_mut::<u32>();
        // SAFETY: only the two counts are asked for.
        let status = unsafe { RegQueryInfoKeyW(handle.0, null.cast(), null, std::ptr::null(), &mut keys, null, null, &mut values, null, null, null, null.cast()) };
        if status != ERROR_SUCCESS {
            return Err(failed(status, "Reading", key));
        }
        Ok(keys == 0 && values == 0)
    }

    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<()> {
        let (path, name, data) = (wide(key), wide(name), wide(value));
        let mut handle = std::ptr::null_mut();
        // SAFETY: `path` ends with NUL; the handle is closed by `Key`.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut handle,
                std::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(failed(status, "Creating", key));
        }
        let handle = Key(handle);
        // SAFETY: `data` is the text with its NUL, passed with its length in bytes.
        let status = unsafe { RegSetValueExW(handle.0, name.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), u32::try_from(data.len() * 2)?) };
        if status != ERROR_SUCCESS {
            return Err(failed(status, "Writing", key));
        }
        Ok(())
    }

    fn delete_tree(&mut self, key: &str) -> Result<()> {
        let path = wide(key);
        // SAFETY: `path` ends with NUL.
        match unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) } {
            ERROR_SUCCESS => Ok(()),
            status if registry_win::is_missing(status) => Ok(()),
            status => Err(failed(status, "Deleting", key)),
        }
    }

    fn delete_key(&mut self, key: &str) -> Result<()> {
        let path = wide(key);
        // SAFETY: `path` ends with NUL.
        match unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, path.as_ptr()) } {
            ERROR_SUCCESS => Ok(()),
            status if registry_win::is_missing(status) => Ok(()),
            status => Err(failed(status, "Deleting", key)),
        }
    }

    fn delete_value(&mut self, key: &str, name: &str) -> Result<()> {
        let Some(handle) = Key::open(key, KEY_SET_VALUE)? else { return Ok(()) };
        let value = wide(name);
        // SAFETY: `value` ends with NUL.
        match unsafe { RegDeleteValueW(handle.0, value.as_ptr()) } {
            ERROR_SUCCESS => Ok(()),
            status if registry_win::is_missing(status) => Ok(()),
            status => Err(failed(status, "Deleting a value of", key)),
        }
    }

    fn associations_changed(&mut self) {
        // SAFETY: SHCNE_ASSOCCHANGED takes no items.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, std::ptr::null(), std::ptr::null()) };
    }
}

/// Open Windows' Default apps settings on Rainy's page (`rainy:default-apps`).
pub fn open_default_apps() -> Result<()> {
    let verb = wide("open");
    let mut last_code = 0usize;
    for target in [DEFAULT_APPS, DEFAULT_APPS_LIST] {
        let file = wide(target);
        // SAFETY: both strings end with NUL; there is no owner window, parameter or folder.
        let instance = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
        let code = instance as usize;
        if code > 32 {
            return Ok(());
        }
        last_code = code;
    }
    let error = std::io::Error::last_os_error();
    let detail = if error.raw_os_error().is_some_and(|raw| raw != 0) {
        error.to_string()
    } else {
        format!("ShellExecuteW code {last_code}")
    };
    bail!(localized(&format!("Windows 无法打开“默认应用”设置：{detail}"), &format!("Windows could not open the Default apps settings: {detail}")))
}
