//! Shared process utilities: hidden Windows console creation, process-tree termination via System32 `taskkill.exe`,
//! and a shared Windows Job Object (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) so child trees never outlive Rainy.

use std::process::{Child, Command, Stdio};

#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Suppress creating a visible console window for child processes on Windows.
pub(crate) fn hide(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Path to Windows' built-in `taskkill.exe` under `%SystemRoot%\System32`, avoiding PATH hijacking.
#[cfg(windows)]
fn taskkill_path() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").map_or_else(|| std::path::PathBuf::from(r"C:\Windows"), std::path::PathBuf::from);
    root.join("System32").join("taskkill.exe")
}

/// Forcefully terminate `pid` and its entire descendant tree on Windows.
pub(crate) fn kill_process_tree(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(windows)]
    {
        let mut command = Command::new(taskkill_path());
        command
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide(&mut command);
        let _ = command.status();
    }
    #[cfg(not(windows))]
    {
        let _ = (pid, Stdio::null);
    }
}

/// Assign `child` (and any descendants it spawns) to Rainy's kill-on-close Job Object so a crash or hard termination
/// of Rainy never leaves orphan Host, SSH tunnel, WSL or Strata processes behind.
#[cfg(windows)]
pub(crate) fn contain(child: &Child) {
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

    let job = shared_job();
    // SAFETY: both handles are open: the job handle lives for the process lifetime, and `child` owns its handle.
    if job.is_null() || unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) } == 0 {
        log::debug!("Child process {} was not assigned to Rainy's job object: {}", child.id(), std::io::Error::last_os_error());
    }
}

#[cfg(windows)]
pub(crate) fn shared_job() -> windows_sys::Win32::Foundation::HANDLE {
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    static JOB: OnceLock<usize> = OnceLock::new();
    // SAFETY: unnamed job object with non-inheritable handle; kept open for the process lifetime.
    let job = *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return 0;
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        if SetInformationJobObject(job, JobObjectExtendedLimitInformation, (&raw const limits).cast(), size) == 0 {
            CloseHandle(job);
            return 0;
        }
        job as usize
    });
    job as windows_sys::Win32::Foundation::HANDLE
}

#[cfg(not(windows))]
pub(crate) fn contain(_child: &Child) {}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[cfg(windows)]
    #[test]
    fn resolves_system32_taskkill_and_creates_job() {
        let path = taskkill_path();
        assert!(path.ends_with(r"System32\taskkill.exe"), "{}", path.display());
        assert!(!shared_job().is_null());
    }
}
