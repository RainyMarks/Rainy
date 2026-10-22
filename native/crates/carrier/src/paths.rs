//! Canonical paths that also work on Windows drives the mount manager does not know (`src/host/realpath.ts` in 3.9.2).

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf, Prefix};

/// Links a walk follows before it gives up: a loop of links never ends.
const MAX_LINKS: usize = 64;

/// The path with links resolved, as Node's `realpath` gives it: without the `\\?\` prefix Windows adds. Windows answers
/// through GetFinalPathNameByHandle, which fails on volumes the mount manager does not know (forensic image mounters such
/// as 火眼, RAM disks, some virtual drives) even though the path is there. When the path exists, a walk that reads each
/// link on the way answers instead, as Node's JavaScript `realpath` does, and the plain absolute path as a last resort.
pub fn canonical(path: &Path) -> io::Result<PathBuf> {
    resolve(path, cfg!(windows), |path| fs::canonicalize(path), walk)
}

/// [`canonical`] with the native call and the walk given, on Windows' filesystem or another.
fn resolve(
    path: &Path,
    windows: bool,
    native: impl FnOnce(&Path) -> io::Result<PathBuf>,
    walk: impl FnOnce(&Path) -> io::Result<PathBuf>,
) -> io::Result<PathBuf> {
    let error = match native(path) {
        Ok(resolved) => return Ok(without_verbatim(resolved)),
        Err(error) => error,
    };
    // A missing path, or one below a file, keeps the native error: callers walk up on NotFound and NotADirectory.
    if !windows || fs::metadata(path).is_err() {
        return Err(error);
    }
    match walk(path).or_else(|_| std::path::absolute(path)) {
        Ok(resolved) => Ok(without_verbatim(resolved)),
        Err(_) => Err(error),
    }
}

/// Node's JavaScript `realpath` on Windows: the absolute path read one component at a time, where a symbolic link or a
/// junction to a drive or a share is read and resolved against the folder holding it, and the walk starts over.
fn walk(path: &Path) -> io::Result<PathBuf> {
    let mut pending = std::path::absolute(path)?;
    let mut links = 0;
    'restart: loop {
        let mut resolved = PathBuf::new();
        let mut components = pending.components();
        while let Some(component) = components.next() {
            match component {
                Component::Prefix(_) | Component::RootDir => resolved.push(component),
                Component::CurDir => {}
                Component::ParentDir => {
                    resolved.pop();
                }
                Component::Normal(name) => {
                    resolved.push(name);
                    if !fs::symlink_metadata(&resolved)?.file_type().is_symlink() {
                        continue;
                    }
                    // A volume mounted into a folder has no path a program could use: it stays a folder, as in Node.
                    let Some(target) = fs::read_link(&resolved).ok().map(without_verbatim).filter(|target| followable(target)) else { continue };
                    links += 1;
                    if links > MAX_LINKS {
                        return Err(io::Error::other(format!("Too many links in {}", path.display())));
                    }
                    resolved.pop();
                    pending = std::path::absolute(resolved.join(target))?.join(components.as_path());
                    continue 'restart;
                }
            }
        }
        return Ok(resolved);
    }
}

/// Whether a link's target names a place on a drive or a share, or one relative to the link.
fn followable(target: &Path) -> bool {
    match target.components().next() {
        Some(Component::Prefix(prefix)) => matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(..)),
        _ => true,
    }
}

/// The path without the `\\?\` prefix of a drive or a share; any other path stays as it is.
pub fn without_verbatim(path: PathBuf) -> PathBuf {
    let mut components = path.components();
    let mut plain = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(letter) => PathBuf::from(format!("{}:\\", char::from(letter))),
            Prefix::VerbatimUNC(server, share) => {
                let mut root = std::ffi::OsString::from(r"\\");
                root.push(server);
                root.push(r"\");
                root.push(share);
                root.push(r"\");
                PathBuf::from(root)
            }
            _ => return path,
        },
        _ => return path,
    };
    for component in components {
        if let Component::Normal(name) = component {
            plain.push(name);
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    /// GetFinalPathNameByHandle fails with ERROR_INVALID_FUNCTION on such a volume, or with ERROR_PATH_NOT_FOUND when the
    /// volume has no DOS name. Paths containing "virtual-drive" behave like that here.
    fn mount_manager(path: &Path) -> io::Result<PathBuf> {
        let text = path.to_string_lossy();
        if text.contains("virtual-drive-invalid") {
            return Err(io::Error::from_raw_os_error(1));
        }
        if text.contains("virtual-drive-missing") {
            return Err(io::Error::from_raw_os_error(3));
        }
        fs::canonicalize(path)
    }

    /// A scratch folder and its canonical path.
    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let base = canonical(dir.path()).unwrap();
        (dir, base)
    }

    #[cfg(windows)]
    #[test]
    fn resolves_paths_as_node_does() {
        assert_eq!(without_verbatim(PathBuf::from(r"\\?\C:\Programs\Rainy\resources")), PathBuf::from(r"C:\Programs\Rainy\resources"));
        assert_eq!(without_verbatim(PathBuf::from(r"\\?\UNC\server\share\Rainy")), PathBuf::from(r"\\server\share\Rainy"));
        assert_eq!(without_verbatim(PathBuf::from(r"C:\Programs")), PathBuf::from(r"C:\Programs"));
        assert_eq!(without_verbatim(PathBuf::from(r"\\?\Volume{0b1c}\case")), PathBuf::from(r"\\?\Volume{0b1c}\case"));
        let (_dir, base) = scratch();
        assert!(base.is_absolute() && !base.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(canonical(&base).unwrap(), base);
        assert!(followable(Path::new(r"C:\evidence")) && followable(Path::new(r"..\evidence")) && followable(Path::new(r"\\server\share")));
        assert!(!followable(Path::new(r"\\?\Volume{0b1c}\")));
    }

    #[test]
    fn answers_with_the_walked_path_when_the_native_call_fails_but_the_folder_is_there() {
        let (_dir, base) = scratch();
        for name in ["virtual-drive-invalid", "virtual-drive-missing"] {
            let folder = base.join(name).join("case 1");
            fs::create_dir_all(&folder).unwrap();
            assert_eq!(resolve(&folder, true, mount_manager, walk).unwrap(), folder);
            let file = folder.join("notes.md");
            fs::write(&file, "x").unwrap();
            assert_eq!(resolve(&folder.join(".").join("..").join("case 1").join("notes.md"), true, mount_manager, walk).unwrap(), file);
        }
    }

    #[test]
    fn keeps_the_native_error_for_a_path_that_is_not_there_and_off_windows() {
        let (_dir, base) = scratch();
        let missing = resolve(&base.join("virtual-drive-missing").join("missing"), true, mount_manager, walk).unwrap_err();
        assert_eq!(missing.raw_os_error(), Some(3));
        let folder = base.join("virtual-drive-invalid").join("linux");
        fs::create_dir_all(&folder).unwrap();
        assert_eq!(resolve(&folder, false, mount_manager, walk).unwrap_err().raw_os_error(), Some(1));
    }

    #[test]
    fn falls_back_to_the_absolute_path_when_the_walk_fails() {
        let (_dir, base) = scratch();
        let folder = base.join("virtual-drive-invalid").join("unwalkable");
        fs::create_dir_all(&folder).unwrap();
        let failing = |_: &Path| Err(io::Error::other("The walk failed"));
        assert_eq!(resolve(&folder.join("..").join("unwalkable"), true, mount_manager, failing).unwrap(), folder);
    }

    /// A junction needs no privilege, unlike a symbolic link; where `mklink /J` cannot make one, there is nothing to check.
    #[cfg(windows)]
    #[test]
    fn follows_a_junction_on_the_way_like_node() {
        let (_dir, base) = scratch();
        let evidence = base.join("evidence");
        fs::create_dir_all(evidence.join("case")).unwrap();
        let drive = base.join("virtual-drive-invalid");
        fs::create_dir_all(&drive).unwrap();
        let link = drive.join("link");
        let made = std::process::Command::new("cmd.exe").args(["/D", "/C", "mklink", "/J"]).arg(&link).arg(&evidence).output();
        if !made.is_ok_and(|output| output.status.success()) {
            eprintln!("mklink /J failed; the junction case was not checked");
            return;
        }
        assert_eq!(resolve(&link.join("case"), true, mount_manager, walk).unwrap(), evidence.join("case"));
        // The native call says the same where it works.
        assert_eq!(canonical(&link.join("case")).unwrap(), evidence.join("case"));
    }
}
