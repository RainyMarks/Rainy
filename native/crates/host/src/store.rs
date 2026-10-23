//! Files under the Host home, replaced atomically (3.x `files.ts`): a synced sibling file renamed over the old one, so a
//! crash leaves either version and never half of one.

use std::io::{self, Write as _};
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Read and parse a JSON file; `None` when it does not exist.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {error}", path.display()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Store a value as indented JSON with a final newline, as 3.x wrote it.
pub fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> io::Result<()> {
    let mut text = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    text.push('\n');
    write_atomic(path, text.as_bytes())
}

/// Replace a file's content atomically; parent folders are created. New files are private to the user on Linux.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let folder = path.parent().ok_or_else(|| io::Error::other("The file has no folder."))?;
    create_private_dir(folder)?;
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = folder.join(format!("{name}.{}.tmp", uuid::Uuid::new_v4()));
    let written = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        rename_retrying(&temporary, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// `rename`, retried a few times on Windows, where a scanner holding the destination open for a moment refuses it.
pub fn rename_retrying(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 1;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if cfg!(windows) && attempt < 6 && error.kind() == io::ErrorKind::PermissionDenied => {
                std::thread::sleep(Duration::from_millis(50 * attempt));
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Create a folder and its parents; new folders are private to the user on Linux.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)
}
