//! Rainy's own log, `<userData>\rainy.log`: what Rainy and its libraries write through `log`, and panics. The Host's
//! output goes to `host.log` instead.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use log::{Level, LevelFilter, Log, Metadata, Record};

/// The log's name in the user-data folder.
pub const FILE_NAME: &str = "rainy.log";
/// Past this size the log moves to `rainy.log.1`, replacing the one before.
const LIMIT: u64 = 4 * 1024 * 1024;

static LOGGER: OnceLock<FileLog> = OnceLock::new();

/// Write `log` records to `path` (Rainy's own from Info, other libraries' from Warn) and log every panic before the
/// panic hook that was installed before. Debug builds also print each line. Only the first call has an effect.
pub fn init(path: &Path) {
    if LOGGER.set(FileLog { path: path.to_path_buf(), file: Mutex::new(None) }).is_err() {
        return;
    }
    let logger = LOGGER.get().expect("set above");
    if log::set_logger(logger).is_err() {
        return;
    }
    log::set_max_level(LevelFilter::Info);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        logger.write(&format!("{} PANIC {}", timestamp(), panic_message(info)));
        previous(info);
    }));
}

/// What a panic said, and where.
pub fn panic_message(info: &PanicHookInfo<'_>) -> String {
    let message = info.payload_as_str().unwrap_or("Box<dyn Any>");
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("<unnamed>");
    match info.location() {
        Some(location) => format!("thread '{thread}' panicked at {location}: {message}"),
        None => format!("thread '{thread}' panicked: {message}"),
    }
}

fn timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string()
}

struct FileLog {
    path: PathBuf,
    /// The open log and its size.
    file: Mutex<Option<(File, u64)>>,
}

impl FileLog {
    fn write(&self, line: &str) {
        if cfg!(debug_assertions) {
            eprintln!("{line}");
        }
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        if file.is_none() {
            *file = open(&self.path);
        }
        if file.as_ref().is_some_and(|(_, size)| *size >= LIMIT) {
            *file = None;
            let mut older = OsString::from(&self.path);
            older.push(".1");
            let moved = std::fs::rename(&self.path, older).is_ok();
            // When the old log is in use the current one keeps growing; the next move is tried after another 4 MiB.
            *file = open(&self.path).map(|(handle, size)| (handle, if moved { size } else { 0 }));
        }
        if let Some((handle, size)) = file.as_mut()
            && handle.write_all(format!("{line}\n").as_bytes()).is_ok()
        {
            *size += line.len() as u64 + 1;
        }
    }
}

fn open(path: &Path) -> Option<(File, u64)> {
    let file = OpenOptions::new().create(true).append(true).open(path).ok()?;
    let size = file.metadata().map(|info| info.len()).unwrap_or(0);
    Some((file, size))
}

/// Whether a log target is in one of Rainy's own crates, whose Info records are kept: the app (`Rainy`) or `rainy_*`.
pub fn is_rainy(target: &str) -> bool {
    let krate = target.split("::").next().unwrap_or(target);
    krate.eq_ignore_ascii_case("rainy") || krate.starts_with("rainy_")
}

impl Log for FileLog {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= if is_rainy(metadata.target()) { Level::Info } else { Level::Warn }
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            self.write(&format!("{} {:<5} {}: {}", timestamp(), record.level(), record.target(), record.args()));
        }
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn moves_the_log_aside_past_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, vec![b'x'; LIMIT as usize]).unwrap();
        let log = FileLog { path: path.clone(), file: Mutex::new(None) };
        log.write("first");
        assert_eq!(std::fs::metadata(dir.path().join("rainy.log.1")).unwrap().len(), LIMIT);
        log.write("second");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");
    }

    #[test]
    fn keeps_rainy_info_and_other_warnings() {
        let log = FileLog { path: PathBuf::new(), file: Mutex::new(None) };
        let enabled = |target: &str, level: Level| log.enabled(&Metadata::builder().target(target).level(level).build());
        assert!(enabled("Rainy", Level::Info));
        assert!(enabled("Rainy::boot", Level::Info));
        assert!(enabled("rainy_carrier::wsl", Level::Info));
        assert!(enabled(module_path!(), Level::Info));
        assert!(enabled("rainy_protocol", Level::Info));
        assert!(!enabled("rainy_carrier::wsl", Level::Debug));
        assert!(!enabled("rainydays::cloud", Level::Info));
        assert!(!enabled("gpui::window", Level::Info));
        assert!(enabled("gpui::window", Level::Warn));
    }
}
