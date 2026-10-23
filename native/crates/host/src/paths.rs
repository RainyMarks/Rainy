//! Path rules every file operation shares (3.x `realpath.ts`, `ide/files-core.ts`).

use std::io;
use std::path::{Component, Path, PathBuf};

use crate::error::{HostError, HostResult};

/// Resolve links to a canonical path, without Windows' `\\?\` prefix. On Windows, a volume the mount manager does not
/// know (forensic image mounters such as 火眼, RAM disks, some virtual drives) has no final path name even though the
/// path is there; when the path exists, its absolute spelling answers instead.
pub fn realpath(path: &Path) -> io::Result<PathBuf> {
    match dunce::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if cfg!(windows) => {
            // A missing path, or one below a file, keeps the error: callers walk up on NotFound.
            std::fs::metadata(path).map_err(|_| error)?;
            std::path::absolute(path)
        }
        Err(error) => Err(error),
    }
}

fn comparable(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.trim_end_matches('/').to_owned();
    if cfg!(windows) { text.to_lowercase() } else { text }
}

/// Whether `target` is `root` or inside it; both absolute and resolved.
pub fn contains(root: &Path, target: &Path) -> bool {
    let (root, target) = (comparable(root), comparable(target));
    target == root || target.starts_with(&format!("{root}/")) || (root.ends_with(':') && target.starts_with(&root))
}

/// Whether two folder paths name the same folder spelling, as the platform compares them.
pub fn same_path(left: &Path, right: &Path) -> bool {
    comparable(left) == comparable(right)
}

/// The portable relative spelling of editor paths: `/`-separated, inside the project, no `.`/`..`, `\`, `:` or control
/// characters. An empty path names the root only when `allow_root`.
pub fn relative_path(path: &str, allow_root: bool) -> HostResult<&str> {
    if path.is_empty() && allow_root {
        return Ok(path);
    }
    let bad = path.is_empty()
        || path.chars().any(|c| (c as u32) < 0x20 || c == '\\' || c == ':')
        || path.starts_with('/')
        || path.split('/').any(|part| part.is_empty() || part == "." || part == "..");
    if bad {
        return Err(HostError::new("invalid-path", "IDE paths must be slash-separated paths inside the selected workspace."));
    }
    Ok(path)
}

/// `root` joined with a checked relative path.
pub fn join_relative(root: &Path, path: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        out.push(part);
    }
    out
}

/// Whether a path names one fixed location: absolute on Linux; drive-qualified (`C:\…`) or complete UNC on Windows.
pub fn fully_qualified(path: &str) -> bool {
    if path.contains('\0') {
        return false;
    }
    if cfg!(windows) {
        let bytes = path.as_bytes();
        let drive = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/');
        let unc = (path.starts_with("\\\\") || path.starts_with("//")) && path[2..].split(['\\', '/']).filter(|part| !part.is_empty()).count() >= 2;
        drive || unc
    } else {
        path.starts_with('/')
    }
}

/// `path` with `.` and `..` resolved lexically.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// A path's `/`-separated spelling relative to `root`, when it is inside it.
pub fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let (root_text, path_text) = (comparable(root), comparable(path));
    if path_text == root_text {
        return Some(String::new());
    }
    let prefix = format!("{root_text}/");
    if !path_text.starts_with(&prefix) {
        return None;
    }
    let original = path.to_string_lossy().replace('\\', "/");
    Some(original[prefix.len()..].trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_relative_spellings() {
        assert!(relative_path("src/main.rs", false).is_ok());
        assert!(relative_path("", true).is_ok());
        for bad in ["", "/a", "a//b", "a/./b", "a/../b", "a\\b", "C:x", "a\u{1}"] {
            assert!(relative_path(bad, false).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn contains_only_inside() {
        let root = Path::new(if cfg!(windows) { r"C:\work\proj" } else { "/work/proj" });
        let inside = root.join("src");
        let sibling = PathBuf::from(format!("{}2", root.display()));
        assert!(contains(root, root) && contains(root, &inside) && !contains(root, &sibling));
        assert_eq!(relative_to(root, &inside.join("a.rs")).as_deref(), Some("src/a.rs"));
    }
}
