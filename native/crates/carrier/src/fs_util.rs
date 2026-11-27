//! Shared filesystem helpers: atomic writes with Windows sharing retries, reparse-point checks,
//! Win32 file identity/link counts, archive relative-path validation, and sensitive-memory zeroing.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{Ordering, compiler_fence};
use std::time::Duration;

use crate::text::uuid_v4_lossy;

/// Windows file times count 100 ns intervals from 1601; Unix timestamps count nanoseconds from 1970.
const UNIX_EPOCH_FILETIME: i128 = 116_444_736_000_000_000;

/// Rename `from` to `to`, retrying briefly on Windows when another process (scanner/indexer) holds a handle.
pub(crate) fn rename_retried(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 1u64;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if cfg!(windows) && attempt < 10 && is_sharing_violation(&error) => {
                std::thread::sleep(Duration::from_millis(attempt * 50));
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Access denied, sharing or lock violation: what Windows reports while another handle is open.
pub(crate) fn is_sharing_violation(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::ResourceBusy) || matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

/// Atomically write `bytes` to `path` via a unique temporary file in the same directory:
/// `create_new` + `write_all` + `sync_all` + retried `rename`, removing the temp file on failure.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], create_parents: bool) -> io::Result<()> {
    let directory = path.parent().ok_or_else(|| io::Error::other("No parent directory"))?;
    if create_parents {
        fs::create_dir_all(directory)?;
    }
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("record");
    let temporary = directory.join(format!(".{name}.{}.tmp", uuid_v4_lossy()));
    let written = fs::OpenOptions::new().write(true).create_new(true).open(&temporary).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        rename_retried(&temporary, path)
    });
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// Atomically replace a private regular file, refusing symlinks, reparse points or directories at `path`.
pub(crate) fn write_private_record(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() || is_reparse_point(&metadata) => {
            return Err(io::Error::other("The local record path is not a regular file."));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    write_atomic(path, bytes, true)
}

/// Whether `metadata` describes a symlink or (on Windows) any reparse point such as a junction.
#[cfg(windows)]
pub(crate) fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    metadata.file_type().is_symlink() || (metadata.file_attributes() & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

#[cfg(not(windows))]
pub(crate) fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// Low-level file identity read without following symlinks or reparse points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileInfo {
    pub links: u32,
    pub mtime_ns: i128,
    pub file_index: u64,
}

#[cfg(windows)]
pub(crate) fn file_info(path: &Path) -> io::Result<FileInfo> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        GetFileInformationByHandle,
    };

    let file = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    // SAFETY: plain data, filled by GetFileInformationByHandle below.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle stays open for the call, and `info` is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let written = (u64::from(info.ftLastWriteTime.dwHighDateTime) << 32) | u64::from(info.ftLastWriteTime.dwLowDateTime);
    let file_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok(FileInfo {
        links: info.nNumberOfLinks,
        mtime_ns: (i128::from(written) - UNIX_EPOCH_FILETIME) * 100,
        file_index,
    })
}

#[cfg(not(windows))]
pub(crate) fn file_info(path: &Path) -> io::Result<FileInfo> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = fs::symlink_metadata(path)?;
    Ok(FileInfo {
        links: metadata.nlink() as u32,
        mtime_ns: i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()),
        file_index: metadata.ino(),
    })
}

/// Whether a single archive path segment is portable and safe on Windows (no reserved device names,
/// control characters, wildcards, or trailing dots/spaces).
pub(crate) fn is_portable_segment(part: &str) -> bool {
    const RESERVED: [&str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
        "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let stem = part.split('.').next().unwrap_or_default().trim_end().to_ascii_lowercase();
    !part.is_empty()
        && !part.chars().any(|c| c.is_control() || "<>\"|?*".contains(c))
        && !part.ends_with(['.', ' '])
        && !RESERVED.contains(&stem.as_str())
}

/// Validate an archive entry name as a safe relative path below the extraction root.
/// Returns `Ok(None)` for root entries such as `.` or `./`, or `Err(reason)` for absolute paths,
/// drive letters, `..` traversal, or non-portable Windows segments.
pub(crate) fn safe_archive_relative(name: &str) -> Result<Option<PathBuf>, &'static str> {
    if name.starts_with(['/', '\\']) || name.contains(':') {
        return Err("is an absolute path");
    }
    let mut path = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return Err("leaves the destination"),
            part if !is_portable_segment(part) => return Err("is not a valid Windows file name"),
            part => path.push(part),
        }
    }
    Ok((!path.as_os_str().is_empty()).then_some(path))
}

/// Overwrite sensitive memory bytes with zeros in a way the optimizer will not elide.
pub(crate) fn zeroize_bytes(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: `byte` is a valid, aligned mutable reference to a `u8`.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn validates_archive_paths_and_zeroizes_buffers() {
        assert_eq!(safe_archive_relative("a/b/c.txt").unwrap(), Some(PathBuf::from("a").join("b").join("c.txt")));
        assert_eq!(safe_archive_relative("./").unwrap(), None);
        assert!(safe_archive_relative("/etc/passwd").is_err());
        assert!(safe_archive_relative("C:foo").is_err());
        assert!(safe_archive_relative("a/../b").is_err());
        assert!(safe_archive_relative("dir/CON.txt").is_err());
        assert!(safe_archive_relative("dir/nul").is_err());
        assert!(safe_archive_relative("dir/trailing.").is_err());
        assert!(safe_archive_relative("dir/bad\x01name").is_err());

        let mut secret = vec![0xAAu8; 32];
        zeroize_bytes(&mut secret);
        assert!(secret.iter().all(|&b| b == 0));
    }

    #[test]
    fn writes_atomically_and_cleans_up_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested").join("data.json");
        write_atomic(&target, b"{\"ok\":true}\n", true).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"{\"ok\":true}\n");
        let entries: Vec<_> = fs::read_dir(target.parent().unwrap()).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1);
    }
}
