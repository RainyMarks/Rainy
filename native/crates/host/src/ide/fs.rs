//! Version-checked local file access for the editor (3.x `ide/files-fs.ts`). A version is
//! `dev:ino:size:mtimeNs:ctimeNs`, the same token Node's `stat({bigint: true})` gave 3.x, so versions a 3.x Host stored
//! in recovery buffers still match.

use std::fs;
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};

use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::paths::realpath;

pub fn not_file() -> HostError {
    HostError::new("not-file", l("所选路径不是普通文件。", "The selected path is not a regular file."))
}

pub fn too_large() -> HostError {
    HostError::new("too-large", l("文件超过了大小上限。", "The file exceeds the configured size limit."))
}

pub fn exists() -> HostError {
    HostError::new("already-exists", l("目标已存在。", "The destination already exists."))
}

/// The file on disk is not the version the editor opened.
pub fn changed() -> HostError {
    HostError::new("version-conflict", l("文件在打开后被修改了。请重新加载，或比较后再保存。", "The file changed after it was opened. Reload or compare before saving."))
}

fn rename_exists(destination: &Path) -> HostError {
    let name = destination.display();
    HostError::new("already-exists", l(&format!("无法重命名为“{name}”：目标已存在。"), &format!("Cannot rename to \"{name}\": the destination already exists.")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    Symlink,
    Other,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Other => "other",
        }
    }
}

/// One observation of a path entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub kind: Kind,
    pub size: u64,
    pub version: String,
}

fn absent(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory)
}

#[cfg(windows)]
fn identity(path: &Path, follow: bool) -> io::Result<(u64, u64, i128, i128)> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FileBasicInfo,
        GetFileInformationByHandle, GetFileInformationByHandleEx,
    };

    const UNIX_EPOCH_FILETIME: i128 = 116_444_736_000_000_000;
    let flags = FILE_FLAG_BACKUP_SEMANTICS | if follow { 0 } else { FILE_FLAG_OPEN_REPARSE_POINT };
    let file = fs::OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES).share_mode(7).custom_flags(flags).open(path)?;
    // SAFETY: plain data structures filled by the calls below while the handle is open.
    let (mut information, mut basic): (BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO) = unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
    // SAFETY: valid handle and writable output buffers of the declared sizes.
    unsafe {
        if GetFileInformationByHandle(file.as_raw_handle(), &mut information) == 0 {
            return Err(io::Error::last_os_error());
        }
        if GetFileInformationByHandleEx(file.as_raw_handle(), FileBasicInfo, (&raw mut basic).cast(), size_of::<FILE_BASIC_INFO>() as u32) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let index = (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    let nanos = |ticks: i64| (i128::from(ticks) - UNIX_EPOCH_FILETIME) * 100;
    Ok((u64::from(information.dwVolumeSerialNumber), index, nanos(basic.LastWriteTime), nanos(basic.ChangeTime)))
}

fn observe(path: &Path, follow: bool) -> io::Result<Option<Info>> {
    let metadata = match if follow { fs::metadata(path) } else { fs::symlink_metadata(path) } {
        Ok(metadata) => metadata,
        Err(error) if absent(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let file_type = metadata.file_type();
    let kind = if file_type.is_symlink() {
        Kind::Symlink
    } else if file_type.is_file() {
        Kind::File
    } else if file_type.is_dir() {
        Kind::Directory
    } else {
        Kind::Other
    };
    #[cfg(windows)]
    let (dev, ino, mtime, ctime) = match identity(path, follow) {
        Ok(identity) => identity,
        Err(error) if absent(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    #[cfg(unix)]
    let (dev, ino, mtime, ctime) = {
        use std::os::unix::fs::MetadataExt as _;
        (metadata.dev(), metadata.ino(), i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()), i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec()))
    };
    let size = metadata.len();
    Ok(Some(Info { kind, size, version: format!("{dev}:{ino}:{size}:{mtime}:{ctime}") }))
}

/// A path entry without following a final link; `None` when it or a parent is missing.
pub fn lstat(path: &Path) -> io::Result<Option<Info>> {
    observe(path, false)
}

/// A target after following links; `None` when missing. A link's kind is its target's.
pub fn stat(path: &Path) -> io::Result<Option<Info>> {
    observe(path, true)
}

/// Canonicalize a path; a missing path keeps its missing tail below the nearest existing ancestor.
pub fn resolve(path: &Path) -> HostResult<PathBuf> {
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        match realpath(&current) {
            Ok(real) => return Ok(missing.iter().rev().fold(real, |path, part| path.join(part))),
            Err(error) if error.kind() == io::ErrorKind::NotADirectory => return Err(HostError::new("not-found", l("上级路径中有一段不是文件夹。", "A parent path segment is not a directory."))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(parent) = current.parent().map(Path::to_path_buf) else { return Ok(path.to_path_buf()) };
                if parent == current {
                    return Ok(path.to_path_buf());
                }
                missing.push(current.file_name().map(ToOwned::to_owned).unwrap_or_default());
                current = parent;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// A whole regular file of at most `max` bytes.
pub fn read_bytes(path: &Path, max: u64) -> HostResult<Vec<u8>> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(not_file());
    }
    if metadata.len() > max {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)?.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(too_large());
    }
    Ok(bytes)
}

/// At most `length` bytes from `offset`.
pub fn read_range(path: &Path, offset: u64, length: u64) -> HostResult<Vec<u8>> {
    if !fs::metadata(path)?.is_file() {
        return Err(not_file());
    }
    let mut file = fs::File::open(path)?;
    file.seek(io::SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.take(length).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Open a regular file only if it is still the version that was observed.
pub fn open_stable(path: &Path, version: &str) -> HostResult<(fs::File, u64)> {
    let file = fs::File::open(path)?;
    let info = stat(path)?.ok_or_else(|| HostError::new("not-found", l("文件已不存在。", "The file no longer exists.")))?;
    if info.kind != Kind::File {
        return Err(not_file());
    }
    if info.version != version {
        return Err(HostError::new("version-conflict", l("文件在读取时被修改了。", "The file changed while it was opened.")));
    }
    Ok((file, info.size))
}

/// How a write may publish.
pub enum Intent<'a> {
    /// Only when nothing is there.
    CreateIfAbsent,
    /// Only over the observed target version.
    ReplaceIfVersion(&'a str),
}

static WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Publish complete content through a synced sibling staging file. A replacement keeps the file's POSIX mode; a new file
/// is published with a hard link, so a file another writer created after the check is never replaced.
pub fn write(path: &Path, content: &[u8], intent: Intent<'_>) -> HostResult<()> {
    let _serial = WRITES.lock().unwrap();
    let existing = stat(path)?;
    if existing.as_ref().is_some_and(|info| info.kind != Kind::File) {
        return Err(HostError::new("not-file", l("目标不是普通文件。", "The destination is not a regular file.")));
    }
    match (&intent, &existing) {
        (Intent::CreateIfAbsent, Some(_)) => return Err(exists()),
        (Intent::ReplaceIfVersion(version), Some(info)) if info.version == *version => {}
        (Intent::ReplaceIfVersion(_), _) => return Err(changed()),
        _ => {}
    }
    let folder = path.parent().ok_or_else(|| HostError::invalid("The destination has no folder."))?;
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = folder.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> HostResult<()> {
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        if existing.is_some() {
            fs::set_permissions(&temporary, fs::metadata(path)?.permissions())?;
        }
        match intent {
            Intent::CreateIfAbsent => match fs::hard_link(&temporary, path) {
                Ok(()) => {
                    let _ = fs::remove_file(&temporary);
                    Ok(())
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(exists()),
                // A filesystem without hard links: the check above is the best there is.
                Err(_) => {
                    if fs::symlink_metadata(path).is_ok() {
                        return Err(exists());
                    }
                    crate::store::rename_retrying(&temporary, path).map_err(Into::into)
                }
            },
            Intent::ReplaceIfVersion(_) => crate::store::rename_retrying(&temporary, path).map_err(Into::into),
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Rename without replacing an existing destination. Files (and links on Linux) move with a hard link, so a competing
/// destination fails; directories fall back to an absence check followed by `rename`.
pub fn rename_no_replace(source: &Path, destination: &Path, kind: Kind) -> HostResult<()> {
    if kind == Kind::File || (kind == Kind::Symlink && cfg!(unix)) {
        match fs::hard_link(source, destination) {
            Ok(()) => {
                if let Err(error) = fs::remove_file(source) {
                    let _ = fs::remove_file(destination);
                    return Err(error.into());
                }
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(rename_exists(&destination)),
            Err(_) => {}
        }
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err(rename_exists(&destination));
    }
    fs::rename(source, destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_change_with_content_and_writes_check_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        write(&path, b"one", Intent::CreateIfAbsent).unwrap();
        assert!(matches!(write(&path, b"x", Intent::CreateIfAbsent), Err(error) if error.code == "already-exists"));
        let first = stat(&path).unwrap().unwrap();
        assert_eq!((first.kind, first.size), (Kind::File, 3));
        assert_eq!(first.version.split(':').count(), 5);
        write(&path, b"two!", Intent::ReplaceIfVersion(&first.version)).unwrap();
        assert!(matches!(write(&path, b"x", Intent::ReplaceIfVersion(&first.version)), Err(error) if error.code == "version-conflict"));
        assert_eq!(fs::read(&path).unwrap(), b"two!");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "no staging file is left");
        let moved = dir.path().join("b.txt");
        rename_no_replace(&path, &moved, Kind::File).unwrap();
        assert!(!path.exists() && moved.exists());
        assert!(lstat(&path).unwrap().is_none());
        assert_eq!(resolve(&dir.path().join("x").join("y.txt")).unwrap(), realpath(dir.path()).unwrap().join("x").join("y.txt"));
    }
}
