//! Windows-specific updater integration: HKCU/HKLM install scope detection, OS version check, and installer launch.

use std::io;
use std::path::Path;

use semver::Version;

use super::InstallScope;

#[cfg(windows)]
mod platform {
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::Path;

    use semver::Version;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ELEVATION_REQUIRED};
    use windows_sys::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION, STARTUPINFOW,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::super::{
        INSTALLER_ARGUMENTS, InstallScope, QUIT_ARGUMENTS, command_line_with, install_scope_from, is_per_machine_install,
        quoted_folder, validate_installer_path,
    };
    use crate::registry_win;

    /// Registry keys where Rainy records its installation folder (shared with 3.x and `native/installer/rainy.nsi`).
    const INSTALL_KEY: &str = r"Software\2b4d0a43-506b-52c1-a15c-818e3e1af64d";
    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\2b4d0a43-506b-52c1-a15c-818e3e1af64d";

    fn read_registry_string_64(root: HKEY, subkey: &str, name: &str) -> Option<String> {
        registry_win::read_string(root, subkey, name, true)
            .ok()
            .flatten()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    }

    fn find_install_root(root: HKEY) -> Option<String> {
        read_registry_string_64(root, INSTALL_KEY, "InstallLocation")
            .or_else(|| read_registry_string_64(root, UNINSTALL_KEY, "UninstallString").and_then(|value| quoted_folder(&value)))
    }

    pub(super) fn install_scope() -> InstallScope {
        let user_root = find_install_root(HKEY_CURRENT_USER);
        let machine_root = find_install_root(HKEY_LOCAL_MACHINE);
        let current_dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
        install_scope_from(user_root.as_deref(), machine_root.as_deref(), current_dir.as_deref())
    }

    pub(super) fn launch(installer: &Path) -> io::Result<u32> {
        validate_installer_path(installer)?;
        launch_with(installer, &INSTALLER_ARGUMENTS, true)
    }

    pub(super) fn launch_on_quit(installer: &Path) -> io::Result<u32> {
        validate_installer_path(installer)?;
        if is_per_machine_install() {
            log::info!(
                "Update: skipping silent install on quit because Rainy is installed per-machine (HKLM) and would prompt for UAC elevation; the update will be offered on next launch"
            );
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "per-machine installation requires elevation; skipping silent install on quit",
            ));
        }
        launch_with(installer, &QUIT_ARGUMENTS, false)
    }

    fn wide_os(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    fn launch_with(installer: &Path, arguments: &[&str], elevate: bool) -> io::Result<u32> {
        let application = wide_os(installer.as_os_str());
        let mut command = wide_os(&command_line_with(installer, arguments));
        let folder = installer.parent().map(|folder| wide_os(folder.as_os_str()));
        // SAFETY: plain data; the size is set below.
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        // SAFETY: plain data, filled by the call.
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: NUL-terminated strings that outlive the call, a writable command line as CreateProcessW requires, and no
        // inherited handles.
        let started = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
                std::ptr::null(),
                folder.as_ref().map_or(std::ptr::null(), |folder| folder.as_ptr()),
                &startup,
                &mut process,
            )
        };
        if started == 0 {
            let error = io::Error::last_os_error();
            if elevate && error.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED as i32) {
                return launch_elevated(installer);
            }
            return Err(error);
        }
        // SAFETY: handles the call returned, closed once; the installer keeps running.
        unsafe {
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        }
        Ok(process.dwProcessId)
    }

    /// Through the shell, which asks for elevation (3.x used `elevate.exe`); the process id is not known, so 0.
    fn launch_elevated(installer: &Path) -> io::Result<u32> {
        let (verb, file) = (wide_os("open".as_ref()), wide_os(installer.as_os_str()));
        let parameters = wide_os(INSTALLER_ARGUMENTS.join(" ").as_ref());
        let folder = installer.parent().map(|folder| wide_os(folder.as_os_str()));
        // SAFETY: NUL-terminated strings that outlive the call; no owner window.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                parameters.as_ptr(),
                folder.as_ref().map_or(std::ptr::null(), |folder| folder.as_ptr()),
                SW_SHOWNORMAL,
            )
        };
        // Values up to 32 are ShellExecute error codes, not guaranteed to set GetLastError().
        let code = result as usize;
        if code <= 32 {
            let last = io::Error::last_os_error();
            if last.raw_os_error().is_some_and(|raw| raw != 0) {
                return Err(last);
            }
            let kind = if code == 5 { io::ErrorKind::PermissionDenied } else { io::ErrorKind::Other };
            return Err(io::Error::new(kind, format!("ShellExecuteW failed with code {code}")));
        }
        Ok(0)
    }

    /// This Windows' version as Node's `os.release()` gives it, `10.0.<build>`.
    pub(super) fn os_version() -> Option<Version> {
        const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        let major = registry_win::read_dword(HKEY_LOCAL_MACHINE, KEY, "CurrentMajorVersionNumber")?;
        let minor = registry_win::read_dword(HKEY_LOCAL_MACHINE, KEY, "CurrentMinorVersionNumber")?;
        let build = registry_win::read_string(HKEY_LOCAL_MACHINE, KEY, "CurrentBuildNumber", false).ok().flatten()?;
        Version::parse(&format!("{major}.{minor}.{}", build.trim())).ok()
    }
}

#[cfg(not(windows))]
mod platform {
    use std::io;
    use std::path::Path;

    use semver::Version;

    use super::super::InstallScope;

    pub(super) fn install_scope() -> InstallScope {
        InstallScope::PerUser
    }

    pub(super) fn launch(_installer: &Path) -> io::Result<u32> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "updates need Windows"))
    }

    pub(super) fn launch_on_quit(_installer: &Path) -> io::Result<u32> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "updates need Windows"))
    }

    pub(super) fn os_version() -> Option<Version> {
        None
    }
}

pub(super) fn install_scope() -> InstallScope {
    platform::install_scope()
}

pub(super) fn launch(installer: &Path) -> io::Result<u32> {
    platform::launch(installer)
}

pub(super) fn launch_on_quit(installer: &Path) -> io::Result<u32> {
    platform::launch_on_quit(installer)
}

pub(super) fn os_version() -> Option<Version> {
    platform::os_version()
}
