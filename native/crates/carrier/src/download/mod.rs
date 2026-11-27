//! Downloads of release files for optional modules and updates (`release-download.ts` in 3.x): WinHTTP with the Windows
//! system proxy and PAC (read, never changed), HTTPS only, redirects followed, no cookies or credentials. A file arrives
//! in `<file>.partial`, which a later attempt resumes with `Range`, and takes its name only after its size and digest
//! matched. Calls block; run them on a background thread.

mod parallel;
mod url;
mod winhttp;
#[cfg(test)]
mod tests;

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine as _;
use sha2::{Digest as _, Sha256, Sha512};

use crate::fs_util;
use crate::text::is_hex;
use self::parallel::{parallel as download_parallel, remove_pieces};
use self::url::{Address, Span};

/// How long a download may receive nothing before it fails.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REDIRECTS: u32 = 10;
const CHUNK: usize = 256 * 1024;
/// A single connection is often held to a fraction of the line (a proxy, a far-away mirror); several share it out.
const CONNECTIONS: u64 = 6;
/// Smaller files are not worth more than one connection.
const PARALLEL_FROM: u64 = 8 * 1024 * 1024;

/// Why a download failed. The texts are 3.x's (`release-download.ts`, `toolpack-files.ts`); the interface shows them
/// in its language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// Not an HTTPS address, nor a loopback one the caller allowed: the address.
    Insecure(String),
    Redirects,
    /// The server could not be reached or the connection failed: what the system said.
    Network(String),
    /// Nothing arrived for this many seconds.
    Stalled(u64),
    /// The connection ended before the whole file arrived; what arrived is kept.
    Interrupted,
    Status(u16),
    /// A resumed download was answered with another range.
    InvalidResume,
    /// A success status other than 200 or 206.
    InvalidResponse,
    TooLarge,
    Mismatch,
    /// Pieces joined into a file that is not the expected one.
    FileMismatch,
    /// A `.partial` file larger than expected, or not a regular file.
    InvalidCache,
    /// A link, junction or hard-linked file on the path.
    Link(String),
    Cancelled,
    /// A local file could not be read or written: the path and the system's error.
    Io(String),
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Insecure(url) => write!(f, "The download address is not secure: {url}"),
            Self::Redirects => f.write_str("The download was redirected too many times."),
            Self::Network(detail) => write!(f, "The download failed ({detail}). Check your connection and try again."),
            Self::Stalled(seconds) => write!(f, "No data arrived for {seconds} seconds. Check your connection and try again."),
            Self::Interrupted => f.write_str("The download connection was interrupted. Check your connection and try again."),
            Self::Status(status) => write!(f, "The download failed (HTTP {status}). Check your connection and try again."),
            Self::InvalidResume => f.write_str("The server sent an invalid response when resuming the download."),
            Self::InvalidResponse => f.write_str("The server sent an invalid download response."),
            Self::TooLarge => f.write_str("The download is larger than expected."),
            Self::Mismatch => f.write_str("The download failed verification. Try again."),
            Self::FileMismatch => f.write_str("The downloaded file failed verification. Try again."),
            Self::InvalidCache => f.write_str("The download cache is invalid. Clear the download cache and try again."),
            Self::Link(path) => write!(f, "The installation path contains a link or junction: {path}"),
            Self::Cancelled => f.write_str("The tool installation was cancelled. Verified staged files will be reused when you retry."),
            Self::Io(detail) => write!(f, "A file could not be read or written: {detail}"),
        }
    }
}

impl std::error::Error for DownloadError {}

pub(super) fn io_error(path: &Path, error: io::Error) -> DownloadError {
    DownloadError::Io(format!("{}: {error}", path.display()))
}

/// A digest a file must have: SHA-256 for module pieces, SHA-512 for installers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Digest {
    Sha256([u8; 32]),
    Sha512([u8; 64]),
}

impl Digest {
    /// Lowercase hex SHA-256, as `optional-modules.json` writes it.
    pub fn sha256_hex(text: &str) -> Option<Self> {
        let mut bytes = [0u8; 32];
        is_hex(text, 64).then_some(())?;
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(Self::Sha256(bytes))
    }

    /// Base64 SHA-512, as `latest.yml` writes it.
    pub fn sha512_base64(text: &str) -> Option<Self> {
        let decoded = base64::engine::general_purpose::STANDARD.decode(text).ok()?;
        Some(Self::Sha512(decoded.try_into().ok()?))
    }
}

/// Computes the digest of what passes through it, of the kind it was made for.
pub enum HashSink {
    Sha256(Sha256),
    Sha512(Sha512),
}

impl HashSink {
    pub fn new(like: &Digest) -> Self {
        match like {
            Digest::Sha256(_) => Self::Sha256(Sha256::new()),
            Digest::Sha512(_) => Self::Sha512(Sha512::new()),
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(hash) => hash.update(bytes),
            Self::Sha512(hash) => hash.update(bytes),
        }
    }

    pub fn finish(self) -> Digest {
        match self {
            Self::Sha256(hash) => {
                let mut digest = [0u8; 32];
                digest.copy_from_slice(&hash.finalize());
                Digest::Sha256(digest)
            }
            Self::Sha512(hash) => {
                let mut digest = [0u8; 64];
                digest.copy_from_slice(&hash.finalize());
                Digest::Sha512(digest)
            }
        }
    }
}

/// What a downloaded file must be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub bytes: u64,
    pub digest: Digest,
}

/// How downloads behave.
#[derive(Debug, Clone)]
pub struct Options {
    /// A download that receives nothing for this long fails.
    pub stall_timeout: Duration,
    /// Resume an existing `.partial` file with `Range` rather than start over.
    pub resume: bool,
    /// Also accept `http://127.0.0.1`, `http://localhost` and `http://[::1]`: tests and local test feeds.
    pub allow_loopback_http: bool,
    /// Set from any thread to stop; what arrived stays in the `.partial` file.
    pub cancel: Arc<AtomicBool>,
    /// How many connections a large file is received over at once.
    pub connections: u64,
    /// Files of at least this many bytes are received over several connections.
    pub parallel_from: u64,
}

impl Options {
    pub fn new(cancel: Arc<AtomicBool>) -> Self {
        Self {
            stall_timeout: STALL_TIMEOUT,
            resume: true,
            allow_loopback_http: false,
            cancel,
            connections: CONNECTIONS,
            parallel_from: PARALLEL_FROM,
        }
    }

    pub(super) fn cancelled(&self) -> Result<(), DownloadError> {
        if self.cancel.load(Ordering::Relaxed) { Err(DownloadError::Cancelled) } else { Ok(()) }
    }
}

/// Download `url` into `target` (`fetchPiece`): a `target` that already matches is kept; otherwise `<target>.partial`
/// is resumed or started, and renamed to `target` once its size and digest match. `progress` gets the bytes present
/// so far, resumed ones included, and the total.
pub fn download(url: &str, target: &Path, expected: &Expected, options: &Options, progress: &dyn Fn(u64, u64)) -> Result<(), DownloadError> {
    if matches(target, expected, &options.cancel)? {
        progress(expected.bytes, expected.bytes);
        return Ok(());
    }
    let partial = partial_path(target);
    check_path(&partial)?;
    let contiguous = fs::symlink_metadata(&partial).is_ok();
    if contiguous {
        // A file being received over one connection, or one whose pieces were being joined: it goes on that way.
        remove_pieces(target, options.connections);
    } else if options.connections > 1 && expected.bytes >= options.parallel_from && options.resume {
        match download_parallel(url, target, expected, options, progress) {
            Ok(true) => return rename(&partial, target).map_err(|error| io_error(target, error)),
            // The server sends whole files only.
            Ok(false) => {
                log::info!("{url} is not served in ranges; downloading it over one connection");
                remove_pieces(target, options.connections);
            }
            Err(error) => return Err(error),
        }
    }
    let mut offset = match fs::symlink_metadata(&partial) {
        Ok(metadata) if !metadata.is_file() => return Err(DownloadError::InvalidCache),
        // Left by a version whose file under the same name was larger: start over instead of failing every attempt.
        Ok(metadata) if metadata.len() > expected.bytes => {
            fs::remove_file(&partial).map_err(|error| io_error(&partial, error))?;
            None
        }
        Ok(metadata) => Some(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_error(&partial, error)),
    };
    if offset == Some(expected.bytes) {
        if matches(&partial, expected, &options.cancel)? {
            rename(&partial, target).map_err(|error| io_error(target, error))?;
            progress(expected.bytes, expected.bytes);
            return Ok(());
        }
        fs::remove_file(&partial).map_err(|error| io_error(&partial, error))?;
        offset = None;
    }
    let offset = if options.resume { offset.unwrap_or(0) } else { 0 };
    match receive(url, &partial, offset, expected, options, progress) {
        // A server or proxy that cannot resume, or a partial file of another version of the file under the same name,
        // would fail every attempt the same way: start over once.
        Err(DownloadError::InvalidResume | DownloadError::Status(416) | DownloadError::Mismatch) if offset > 0 => {
            log::info!("{} could not be resumed from byte {offset}; downloading it again", target.display());
            match fs::remove_file(&partial) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(io_error(&partial, error)),
                _ => {}
            }
            receive(url, &partial, 0, expected, options, progress)?;
        }
        result => result?,
    }
    rename(&partial, target).map_err(|error| io_error(target, error))
}

/// A small document such as `latest.yml`, read into memory; more than `limit` bytes fail.
pub fn fetch(url: &str, limit: usize, options: &Options) -> Result<Vec<u8>, DownloadError> {
    let mut response = get(url, Span::WHOLE, options)?;
    match response.status {
        200 => {}
        201..=299 => return Err(DownloadError::InvalidResponse),
        status => return Err(DownloadError::Status(status)),
    }
    let (mut body, mut buffer) = (Vec::new(), vec![0u8; CHUNK]);
    loop {
        options.cancelled()?;
        let read = response.read(&mut buffer, options)?;
        if read == 0 {
            return Ok(body);
        }
        if body.len().saturating_add(read) > limit {
            return Err(DownloadError::TooLarge);
        }
        body.extend_from_slice(&buffer[..read]);
    }
}

/// Whether the file at `path` has the expected size and digest (`releaseFileMatches`); links are refused.
pub fn matches(path: &Path, expected: &Expected, cancel: &AtomicBool) -> Result<bool, DownloadError> {
    check_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() == expected.bytes => Ok(file_digest(path, &expected.digest, cancel)? == expected.digest),
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(path, error)),
    }
}

/// The digest of a file, of the same kind as `like`.
pub fn file_digest(path: &Path, like: &Digest, cancel: &AtomicBool) -> Result<Digest, DownloadError> {
    let mut file = File::open(path).map_err(|error| io_error(path, error))?;
    let mut sink = HashSink::new(like);
    hash_from(&mut file, u64::MAX, &mut sink, cancel).map_err(|error| if cancel.load(Ordering::Relaxed) { DownloadError::Cancelled } else { io_error(path, error) })?;
    Ok(sink.finish())
}

/// Feed up to `limit` bytes of `reader` to `sink`; the number read.
pub(super) fn hash_from(reader: &mut impl io::Read, limit: u64, sink: &mut HashSink, cancel: &AtomicBool) -> io::Result<u64> {
    let mut buffer = vec![0u8; CHUNK];
    let mut total = 0;
    while total < limit {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let wanted = buffer.len().min(usize::try_from(limit - total).unwrap_or(usize::MAX));
        match reader.read(&mut buffer[..wanted]) {
            Ok(0) => break,
            Ok(read) => {
                sink.update(&buffer[..read]);
                total += read as u64;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(total)
}

/// `<target>.partial`.
pub fn partial_path(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}

/// Strip a `.partial` or parallel piece `.partial.<digits>` suffix, returning the base file name.
pub(crate) fn strip_partial_suffix(name: &str) -> Option<&str> {
    if let Some(base) = name.strip_suffix(".partial") {
        return (!base.is_empty()).then_some(base);
    }
    let (prefix, digits) = name.rsplit_once('.')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let base = prefix.strip_suffix(".partial")?;
    (!base.is_empty()).then_some(base)
}

/// Refuse links, junctions and hard-linked files on every existing part of `path`, and parts that are neither files nor
/// folders (`assertToolPackPath`). The parts after the first missing one are not looked at.
pub fn check_path(path: &Path) -> Result<(), DownloadError> {
    let absolute = std::path::absolute(path).map_err(|error| io_error(path, error))?;
    let parts: Vec<Component> = absolute.components().collect();
    let mut cursor = PathBuf::new();
    for (index, part) in parts.iter().enumerate() {
        cursor.push(part);
        if !matches!(part, Component::Normal(_)) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&cursor) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_error(&cursor, error)),
        };
        if fs_util::is_reparse_point(&metadata)
            || (metadata.is_file() && fs_util::file_info(&cursor).map_err(|error| io_error(&cursor, error))?.links > 1)
        {
            return Err(DownloadError::Link(cursor.display().to_string()));
        }
        if (index + 1 < parts.len() && !metadata.is_dir()) || !(metadata.is_dir() || metadata.is_file()) {
            return Err(DownloadError::Io(format!("{}: not a regular file or folder", cursor.display())));
        }
    }
    Ok(())
}

/// Rename, retrying for about two seconds while another program holds the source or the replaced target open
/// (`renameToolPackPath`).
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    fs_util::rename_retried(from, to)
}

/// Receive the body into `partial`, from `offset` when the server resumes there, and check it.
fn receive(
    url: &str,
    partial: &Path,
    offset: u64,
    expected: &Expected,
    options: &Options,
    progress: &dyn Fn(u64, u64),
) -> Result<(), DownloadError> {
    let mut response = get(url, Span::from(offset), options)?;
    let mut offset = offset;
    match response.status {
        206 => {
            let wanted = expected.bytes.checked_sub(1).map(|last| format!("bytes {offset}-{last}/{}", expected.bytes));
            if wanted.is_none() || response.content_range != wanted {
                return Err(DownloadError::InvalidResume);
            }
        }
        200 => offset = 0,
        201..=299 => return Err(DownloadError::InvalidResponse),
        status => return Err(DownloadError::Status(status)),
    }
    let failed = |error| io_error(partial, error);
    let mut sink = HashSink::new(&expected.digest);
    let mut file = if offset == 0 {
        File::create(partial).map_err(failed)?
    } else {
        // What is already there counts toward the digest.
        let mut kept = File::open(partial).map_err(failed)?;
        let hashed = hash_from(&mut kept, offset, &mut sink, &options.cancel).map_err(|error| {
            if options.cancel.load(Ordering::Relaxed) { DownloadError::Cancelled } else { failed(error) }
        })?;
        if hashed != offset {
            return Err(DownloadError::InvalidCache);
        }
        fs::OpenOptions::new().append(true).open(partial).map_err(failed)?
    };
    let mut received = offset;
    progress(received, expected.bytes);
    let mut buffer = vec![0u8; CHUNK];
    loop {
        options.cancelled()?;
        let read = response.read(&mut buffer, options)?;
        if read == 0 {
            break;
        }
        if received.saturating_add(read as u64) > expected.bytes {
            return Err(DownloadError::TooLarge);
        }
        file.write_all(&buffer[..read]).map_err(failed)?;
        sink.update(&buffer[..read]);
        received += read as u64;
        progress(received, expected.bytes);
    }
    file.sync_all().map_err(failed)?;
    drop(file);
    if received < expected.bytes {
        return Err(DownloadError::Interrupted);
    }
    if sink.finish() != expected.digest {
        let _ = fs::remove_file(partial);
        return Err(DownloadError::Mismatch);
    }
    Ok(())
}

/// GET `url`, following redirects, for the bytes of `span`.
fn get(
    url: &str,
    span: Span,
    options: &Options,
) -> Result<winhttp::Response, DownloadError> {
    let mut address = Address::parse(url, options)?;
    for _ in 0..=MAX_REDIRECTS {
        options.cancelled()?;
        let response = winhttp::request(&address, span, options)?;
        if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        let location = response.location.clone().ok_or(DownloadError::InvalidResponse)?;
        address = Address::parse(&address.join(location.trim()), options)?;
    }
    Err(DownloadError::Redirects)
}
