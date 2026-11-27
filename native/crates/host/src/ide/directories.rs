//! In-app folder browser (`directories.list`, `directories.create`) and platform filesystem roots
//! (zero-thread `GetLogicalDrives()` on Windows, cached WSL drive discovery on Linux).

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::paths::fully_qualified;

const MAX_DIRECTORY_ENTRIES: usize = 10_000;
const ROOTS_CACHE_TTL: Duration = Duration::from_secs(10);

fn not_absolute() -> HostError {
    HostError::new("invalid-path", l("请选择一个绝对路径的文件夹。", "Choose an absolute directory path."))
}

/// Drive letters on Windows (via `GetLogicalDrives()` with 10s cache, zero OS threads); `/` and the home folder on Linux.
pub fn filesystem_roots() -> Vec<String> {
    #[cfg(not(windows))]
    {
        let home = crate::env::home_dir().display().to_string();
        let mut roots = if home == "/" { vec!["/".into()] } else { vec!["/".into(), home] };
        roots.extend(wsl::drive_roots());
        roots
    }
    #[cfg(windows)]
    {
        static CACHED: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);
        if let Ok(guard) = CACHED.lock()
            && let Some((when, roots)) = guard.as_ref()
            && when.elapsed() < ROOTS_CACHE_TTL
        {
            return roots.clone();
        }
        // SAFETY: no arguments; returns a bitmask of currently available logical drives (bit 0 = A:, ..., bit 25 = Z:).
        let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
        let roots: Vec<String> = (0..26u32)
            .filter(|bit| (mask & (1u32 << bit)) != 0)
            .map(|bit| format!("{}:\\", char::from(b'A' + bit as u8)))
            .collect();
        if let Ok(mut guard) = CACHED.lock() {
            *guard = Some((Instant::now(), roots.clone()));
        }
        roots
    }
}

/// One level of the in-app folder browser: child folders (following links to folders), sorted by name.
pub fn list_directories(path: Option<&str>, show_hidden: bool) -> HostResult<Value> {
    let target = match path {
        None => crate::env::home_dir(),
        Some(path) if fully_qualified(path) => crate::paths::normalize(Path::new(path)),
        Some(_) => return Err(not_absolute()),
    };
    let failure = |error: std::io::Error| match error.kind() {
        std::io::ErrorKind::NotFound => HostError::new("not-found", l(&format!("文件夹不存在：{}", target.display()), &format!("The directory does not exist: {}", target.display()))),
        std::io::ErrorKind::NotADirectory => HostError::new("not-directory", l(&format!("这个路径不是文件夹：{}", target.display()), &format!("The path is not a directory: {}", target.display()))),
        std::io::ErrorKind::PermissionDenied => HostError::new("permission-denied", l(&format!("无法读取文件夹：{}", target.display()), &format!("The directory cannot be read: {}", target.display()))),
        _ => HostError::new("io-error", error.to_string()),
    };
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&target).map_err(failure)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() || (kind.is_symlink() && std::fs::metadata(entry.path()).is_ok_and(|info| info.is_dir())) {
            names.push(name);
        }
    }
    names.sort_by_key(|name| name.to_lowercase());
    names.truncate(MAX_DIRECTORY_ENTRIES);
    let entries: Vec<Value> = names.iter().map(|name| json!({ "name": name, "path": target.join(name), "hidden": name.starts_with('.') })).collect();
    let parent = target.parent().filter(|parent| *parent != target && !parent.as_os_str().is_empty()).map(Path::to_path_buf);
    Ok(json!({ "path": target, "parent": parent, "roots": filesystem_roots(), "entries": entries }))
}

/// Create one folder whose parent exists, then list it.
pub fn create_directory(path: &str) -> HostResult<Value> {
    if !fully_qualified(path) {
        return Err(not_absolute());
    }
    let target = crate::paths::normalize(Path::new(path));
    if target.file_name().is_none() {
        return Err(HostError::new("invalid-path", l("请在已有的文件夹中新建文件夹。", "Choose a new directory inside an existing directory.")));
    }
    match std::fs::create_dir(&target) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(HostError::new("already-exists", l(&format!("文件夹已存在：{}", target.display()), &format!("The directory already exists: {}", target.display()))));
        }
        Err(error) => return Err(error.into()),
    }
    list_directories(Some(&target.to_string_lossy()), true)
}

/// Windows drives seen from WSL. WSL mounts them under `/mnt/<letter>` only when it starts, so a USB stick or a disk
/// image attached later stays out of reach; the folder browser mounts those (the Host runs as root in Rainy's own
/// distribution) and offers every mounted drive as a root.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) mod wsl {
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// How long a look at the drives holds: the folder browser lists roots with every folder it opens.
    const FRESH: Duration = Duration::from_secs(10);
    const MOUNT_TIMEOUT: Duration = Duration::from_secs(5);
    static SEEN: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);

    fn inside_wsl() -> bool {
        cfg!(target_os = "linux") && (std::env::var_os("WSL_DISTRO_NAME").is_some() || Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists())
    }

    /// The drive mount points of a `/proc/self/mounts` text: `/mnt/<letter>` with a Windows file system.
    pub fn mounted(mounts: &str) -> Vec<String> {
        let mut found: Vec<String> = mounts
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let (_, point, kind) = (fields.next()?, fields.next()?, fields.next()?);
                let letter = point.strip_prefix("/mnt/")?;
                (letter.len() == 1 && letter.chars().all(|c| c.is_ascii_lowercase()) && matches!(kind, "drvfs" | "9p" | "virtiofs")).then(|| point.to_owned())
            })
            .collect();
        found.sort();
        found.dedup();
        found
    }

    /// Run a short helper command synchronously with a bounded deadline, always reaping the child on timeout.
    fn run_with_timeout(command: &mut std::process::Command, timeout: Duration) -> Option<(bool, String, String)> {
        let mut child = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        };
        let mut out = String::new();
        let mut err = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = std::io::Read::read_to_string(&mut stdout, &mut out);
        }
        if let Some(mut stderr) = child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut stderr, &mut err);
        }
        Some((status.success(), out, err))
    }

    /// The letters of Windows' drives, asked through interop (`cmd.exe`), in at most two seconds.
    fn windows_letters() -> Vec<char> {
        let script = "for %d in (A B C D E F G H I J K L M N O P Q R S T U V W X Y Z) do @if exist %d:\\ echo %d";
        let mut cmd = std::process::Command::new("/mnt/c/Windows/System32/cmd.exe");
        cmd.args(["/d", "/c", script]).current_dir("/mnt/c");
        let Some((_, text, _)) = run_with_timeout(&mut cmd, Duration::from_secs(2)) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| line.trim().chars().next())
            .filter(char::is_ascii_alphabetic)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    }

    fn root() -> bool {
        std::fs::read_to_string("/proc/self/status").is_ok_and(|status| status.lines().any(|line| line.starts_with("Uid:") && line.split_whitespace().nth(1) == Some("0")))
    }

    /// Every Windows drive mounted under `/mnt`, after mounting the ones attached since WSL started.
    pub fn drive_roots() -> Vec<String> {
        if !inside_wsl() {
            return Vec::new();
        }
        if let Ok(seen) = SEEN.lock()
            && let Some((when, roots)) = seen.as_ref()
            && when.elapsed() < FRESH
        {
            return roots.clone();
        }
        let present = mounted(&std::fs::read_to_string("/proc/self/mounts").unwrap_or_default());
        let distro = std::env::var("WSL_DISTRO_NAME").unwrap_or_default();
        for letter in windows_letters() {
            let point = format!("/mnt/{letter}");
            if present.contains(&point) {
                continue;
            }
            let drive = format!("{}:", letter.to_ascii_uppercase());
            let script = format!("mkdir -p {point} && mount -t drvfs {drive} {point}");
            // As root the Host mounts it itself; a user's Host asks WSL to run the mount as root, which needs no password.
            let mut command = if root() {
                let mut command = std::process::Command::new("sh");
                command.args(["-c", &script]);
                command
            } else if !distro.is_empty() {
                let mut command = std::process::Command::new("/mnt/c/Windows/System32/wsl.exe");
                command.args(["-d", &distro, "-u", "root", "--exec", "sh", "-c", &script]).current_dir("/mnt/c");
                command
            } else {
                continue;
            };
            match run_with_timeout(&mut command, MOUNT_TIMEOUT) {
                Some((true, _, _)) => log::info!("mounted {drive} at {point}"),
                Some((false, _, stderr)) => log::warn!("{drive} could not be mounted at {point}: {}", stderr.trim()),
                None => log::warn!("{drive} mount at {point} timed out or failed"),
            }
        }
        let roots = mounted(&std::fs::read_to_string("/proc/self/mounts").unwrap_or_default());
        if let Ok(mut seen) = SEEN.lock() {
            *seen = Some((Instant::now(), roots.clone()));
        }
        roots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_windows_drives_wsl_mounted() {
        let mounts = "none /mnt/wsl tmpfs rw 0 0\nC:\\ /mnt/c 9p rw,dirsync 0 0\nD:\\ /mnt/d drvfs rw 0 0\n/dev/sdc / ext4 rw 0 0\nE: /mnt/e drvfs rw 0 0\ndrivers /usr/lib/wsl/drivers 9p ro 0 0\n";
        assert_eq!(wsl::mounted(mounts), ["/mnt/c", "/mnt/d", "/mnt/e"]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_roots_returns_system_drive_without_spawning_threads() {
        let roots = filesystem_roots();
        assert!(!roots.is_empty(), "at least one Windows drive should be detected");
        assert!(roots.iter().all(|r| r.len() == 3 && r.ends_with(":\\")));
    }

    /// Run inside WSL with a drive attached after WSL started (`RAINY_QA_WSL_DRIVE=r`).
    #[test]
    #[ignore]
    fn mounts_a_drive_attached_after_wsl_started() {
        let letter = std::env::var("RAINY_QA_WSL_DRIVE").unwrap_or_else(|_| "r".into());
        let roots = wsl::drive_roots();
        assert!(roots.contains(&format!("/mnt/{letter}")), "{roots:?}");
    }
}
