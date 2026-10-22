//! Downloads of release files for optional modules and updates (`release-download.ts` in 3.x): WinHTTP with the Windows
//! system proxy and PAC (read, never changed), HTTPS only, redirects followed, no cookies or credentials. A file arrives
//! in `<file>.partial`, which a later attempt resumes with `Range`, and takes its name only after its size and digest
//! matched. Calls block; run them on a background thread.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine as _;
use sha2::{Digest as _, Sha256, Sha512};

/// How long a download may receive nothing before it fails.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REDIRECTS: u32 = 10;
const CHUNK: usize = 256 * 1024;

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

fn io_error(path: &Path, error: io::Error) -> DownloadError {
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
        (text.len() == 64 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then_some(())?;
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
}

impl Options {
    pub fn new(cancel: Arc<AtomicBool>) -> Self {
        Self { stall_timeout: STALL_TIMEOUT, resume: true, allow_loopback_http: false, cancel }
    }

    fn cancelled(&self) -> Result<(), DownloadError> {
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
    watched(&options.cancel, |live| receive(url, &partial, offset, expected, options, live, progress))?;
    rename(&partial, target).map_err(|error| io_error(target, error))
}

/// A small document such as `latest.yml`, read into memory; more than `limit` bytes fail.
pub fn fetch(url: &str, limit: usize, options: &Options) -> Result<Vec<u8>, DownloadError> {
    watched(&options.cancel, |live| {
        let mut response = get(url, 0, options, live)?;
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
            if body.len() + read > limit {
                return Err(DownloadError::TooLarge);
            }
            body.extend_from_slice(&buffer[..read]);
        }
    })
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
fn hash_from(reader: &mut impl io::Read, limit: u64, sink: &mut HashSink, cancel: &AtomicBool) -> io::Result<u64> {
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
        if metadata.file_type().is_symlink() || (metadata.is_file() && link_count(&cursor).map_err(|error| io_error(&cursor, error))? > 1) {
            return Err(DownloadError::Link(cursor.display().to_string()));
        }
        if (index + 1 < parts.len() && !metadata.is_dir()) || !(metadata.is_dir() || metadata.is_file()) {
            return Err(DownloadError::Io(format!("{}: not a regular file or folder", cursor.display())));
        }
    }
    Ok(())
}

/// How many names a file has.
#[cfg(windows)]
fn link_count(path: &Path) -> io::Result<u32> {
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
    // SAFETY: plain data, filled by the call below.
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle stays open for the call, and `information` is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(information.nNumberOfLinks)
}

#[cfg(not(windows))]
fn link_count(path: &Path) -> io::Result<u32> {
    use std::os::unix::fs::MetadataExt as _;

    Ok(fs::symlink_metadata(path)?.nlink() as u32)
}

/// Rename, retrying for about two seconds while another program holds the source or the replaced target open
/// (`renameToolPackPath`).
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 1;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if cfg!(windows) && attempt < 10 && held(&error) => {
                std::thread::sleep(Duration::from_millis(attempt * 50));
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Access denied, sharing or lock violation: what Windows reports while another handle is open.
fn held(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::ResourceBusy) || matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

/// Receive the body into `partial`, from `offset` when the server resumes there, and check it.
fn receive(
    url: &str,
    partial: &Path,
    offset: u64,
    expected: &Expected,
    options: &Options,
    live: &transport::Live,
    progress: &dyn Fn(u64, u64),
) -> Result<(), DownloadError> {
    let mut response = get(url, offset, options, live)?;
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
        if received + read as u64 > expected.bytes {
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

/// GET `url`, following redirects; a resumed download asks for the bytes from `offset` on.
fn get<'a>(url: &str, offset: u64, options: &Options, live: &'a transport::Live) -> Result<transport::Response<'a>, DownloadError> {
    let mut address = Address::parse(url, options)?;
    for _ in 0..=MAX_REDIRECTS {
        options.cancelled()?;
        let response = transport::request(&address, offset, options, live)?;
        if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        let location = response.location.clone().ok_or(DownloadError::InvalidResponse)?;
        address = Address::parse(&address.join(location.trim()), options)?;
    }
    Err(DownloadError::Redirects)
}

/// Run `work` while a watcher ends its blocked network call as soon as the download is cancelled.
fn watched<T>(cancel: &AtomicBool, work: impl FnOnce(&transport::Live) -> Result<T, DownloadError>) -> Result<T, DownloadError> {
    let live = transport::Live::default();
    let (done, finished) = std::sync::mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let watching = &live;
        scope.spawn(move || {
            while finished.recv_timeout(Duration::from_millis(50)) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                if cancel.load(Ordering::Relaxed) {
                    watching.abort();
                    return;
                }
            }
        });
        let result = work(&live);
        let _ = done.send(());
        result
    })
}

/// A parsed download address.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Address {
    secure: bool,
    /// Lowercase; IPv6 addresses keep their brackets.
    host: String,
    port: u16,
    /// Path and query, starting with `/`.
    path: String,
}

impl Address {
    /// Accept `https://`, and `http://` only for a loopback host when the caller allows it; no user names or passwords.
    fn parse(text: &str, options: &Options) -> Result<Self, DownloadError> {
        let insecure = || DownloadError::Insecure(text.to_owned());
        let (scheme, rest) = text.split_once("://").ok_or_else(insecure)?;
        let secure = match scheme.to_ascii_lowercase().as_str() {
            "https" => true,
            "http" => false,
            _ => return Err(insecure()),
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let (authority, path) = rest.split_at(rest.find(['/', '?']).unwrap_or(rest.len()));
        if authority.is_empty() || authority.contains('@') || authority.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(insecure());
        }
        let (host, port) = match authority.strip_prefix('[') {
            Some(inner) => {
                let (address, after) = inner.split_once(']').ok_or_else(insecure)?;
                (format!("[{address}]"), after.strip_prefix(':'))
            }
            None => match authority.rsplit_once(':') {
                Some((host, port)) => (host.to_owned(), Some(port)),
                None => (authority.to_owned(), None),
            },
        };
        let port = match port {
            Some(digits) => digits.parse::<u16>().ok().filter(|port| *port > 0).ok_or_else(insecure)?,
            None if secure => 443,
            None => 80,
        };
        let host = host.to_ascii_lowercase();
        if host.is_empty() || (!secure && !(options.allow_loopback_http && is_loopback(&host))) {
            return Err(insecure());
        }
        let path = if path.starts_with('/') { path.to_owned() } else { format!("/{path}") };
        if path.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(insecure());
        }
        Ok(Self { secure, host, port, path })
    }

    fn origin(&self) -> String {
        let (scheme, default) = if self.secure { ("https", 443) } else { ("http", 80) };
        if self.port == default { format!("{scheme}://{}", self.host) } else { format!("{scheme}://{}:{}", self.host, self.port) }
    }

    fn url(&self) -> String {
        format!("{}{}", self.origin(), self.path)
    }

    /// A redirect's `Location`, which may be relative to this address.
    fn join(&self, location: &str) -> String {
        let scheme_end = location.find("://");
        if scheme_end.is_some_and(|end| !location[..end].contains(['/', '?'])) {
            return location.to_owned();
        }
        if location.starts_with("//") {
            return format!("{}:{location}", if self.secure { "https" } else { "http" });
        }
        if location.starts_with('/') {
            return format!("{}{location}", self.origin());
        }
        let path = self.path.split('?').next().unwrap_or("/");
        if location.starts_with('?') {
            return format!("{}{path}{location}", self.origin());
        }
        let folder = &path[..path.rfind('/').map_or(0, |slash| slash + 1)];
        format!("{}{folder}{location}", self.origin())
    }
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

#[cfg(windows)]
mod transport {
    use std::ffi::c_void;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError};
    use windows_sys::Win32::Networking::WinHttp::*;

    use super::{Address, DownloadError, Options, is_loopback};

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// A session or connection handle, closed when dropped.
    struct Handle(*mut c_void);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: a handle WinHTTP returned, closed once.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }

    /// The open request handle. Closing it is how WinHTTP ends a blocked synchronous call, so a cancelled download
    /// closes it from the watcher thread; every call takes the handle from here and fails once it is gone.
    #[derive(Default)]
    pub(super) struct Live {
        request: Mutex<Option<usize>>,
        aborted: AtomicBool,
    }

    impl Live {
        pub(super) fn abort(&self) {
            let mut request = self.request.lock().unwrap();
            self.aborted.store(true, Ordering::Relaxed);
            if let Some(handle) = request.take() {
                // SAFETY: the request handle, closed once; a call blocked on it returns with an error.
                unsafe { WinHttpCloseHandle(handle as *mut c_void) };
            }
        }

        fn set(&self, handle: *mut c_void) -> Result<(), DownloadError> {
            let mut request = self.request.lock().unwrap();
            if self.aborted.load(Ordering::Relaxed) {
                // SAFETY: a new request handle nothing else uses.
                unsafe { WinHttpCloseHandle(handle) };
                return Err(DownloadError::Cancelled);
            }
            *request = Some(handle as usize);
            Ok(())
        }

        fn get(&self) -> Result<*mut c_void, DownloadError> {
            self.request.lock().unwrap().map(|handle| handle as *mut c_void).ok_or(DownloadError::Cancelled)
        }

        fn release(&self) {
            if let Some(handle) = self.request.lock().unwrap().take() {
                // SAFETY: the request handle, closed once.
                unsafe { WinHttpCloseHandle(handle as *mut c_void) };
            }
        }
    }

    /// The answer to one request: its status and the headers a download needs, with the body still to read.
    pub(super) struct Response<'a> {
        pub status: u16,
        pub location: Option<String>,
        pub content_range: Option<String>,
        live: &'a Live,
        _connection: Handle,
        _session: Handle,
    }

    impl Drop for Response<'_> {
        fn drop(&mut self) {
            // The request goes before its connection and session.
            self.live.release();
        }
    }

    impl Response<'_> {
        /// The next part of the body, as soon as some of it arrived; 0 at its end. A synchronous read alone would wait
        /// until the whole buffer is filled.
        pub fn read(&mut self, buffer: &mut [u8], options: &Options) -> Result<usize, DownloadError> {
            let handle = self.live.get()?;
            let mut available = 0u32;
            // SAFETY: a live request handle and a writable count.
            if unsafe { WinHttpQueryDataAvailable(handle, &mut available) } == 0 {
                return Err(failure(options));
            }
            if available == 0 {
                return Ok(0);
            }
            let handle = self.live.get()?;
            let wanted = buffer.len().min(available as usize).min(u32::MAX as usize) as u32;
            let mut read = 0u32;
            // SAFETY: `buffer` is writable for at least `wanted` bytes.
            if unsafe { WinHttpReadData(handle, buffer.as_mut_ptr().cast(), wanted, &mut read) } == 0 {
                return Err(failure(options));
            }
            Ok(read as usize)
        }
    }

    /// The error of the WinHTTP call that just failed.
    fn failure(options: &Options) -> DownloadError {
        // SAFETY: reads the calling thread's last error.
        let code = unsafe { GetLastError() };
        if options.cancel.load(Ordering::Relaxed) {
            return DownloadError::Cancelled;
        }
        if code == ERROR_WINHTTP_TIMEOUT {
            return DownloadError::Stalled(options.stall_timeout.as_secs());
        }
        DownloadError::Network(describe(code))
    }

    /// WinHTTP's own description of an error, in the system's language, with its code.
    fn describe(code: u32) -> String {
        use windows_sys::Win32::System::Diagnostics::Debug::{FORMAT_MESSAGE_FROM_HMODULE, FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW};
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

        let mut buffer = [0u16; 512];
        let module = wide("winhttp.dll");
        // SAFETY: `buffer` is writable for its length; no inserts are read.
        let length = unsafe {
            FormatMessageW(
                FORMAT_MESSAGE_FROM_HMODULE | FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                GetModuleHandleW(module.as_ptr()) as *const c_void,
                code,
                0,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                std::ptr::null(),
            )
        };
        let text = String::from_utf16_lossy(&buffer[..length as usize]).trim().trim_end_matches(['.', '。']).to_owned();
        if text.is_empty() { format!("WinHTTP {code}") } else { format!("{text}, WinHTTP {code}") }
    }

    /// A session for `address`: the system proxy and PAC script as WinHTTP resolves them for each request (Windows 8.1
    /// and later), else the proxy the system settings name for this address; never one for a loopback address.
    fn session(address: &Address) -> Result<Handle, DownloadError> {
        let agent = wide("Rainy");
        let open = |access: WINHTTP_ACCESS_TYPE, proxy: Option<&str>| {
            let (proxy, bypass) = (proxy.map(wide), wide("<local>"));
            let (proxy_ptr, bypass_ptr) = match &proxy {
                Some(proxy) => (proxy.as_ptr(), bypass.as_ptr()),
                None => (std::ptr::null(), std::ptr::null()),
            };
            // SAFETY: NUL-terminated strings that outlive the call.
            unsafe { WinHttpOpen(agent.as_ptr(), access, proxy_ptr, bypass_ptr, 0) }
        };
        let handle = if is_loopback(&address.host) {
            open(WINHTTP_ACCESS_TYPE_NO_PROXY, None)
        } else {
            let automatic = open(WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, None);
            if !automatic.is_null() {
                automatic
            } else {
                match crate::proxy::system_proxy(&address.url()) {
                    Some(proxy) => open(WINHTTP_ACCESS_TYPE_NAMED_PROXY, Some(proxy.trim_start_matches("http://").trim_start_matches("https://"))),
                    None => open(WINHTTP_ACCESS_TYPE_NO_PROXY, None),
                }
            }
        };
        if handle.is_null() {
            // SAFETY: reads the calling thread's last error.
            return Err(DownloadError::Network(describe(unsafe { GetLastError() })));
        }
        Ok(Handle(handle))
    }

    fn header(handle: *mut c_void, info: u32) -> Option<String> {
        let mut size = 0u32;
        // SAFETY: asks for the size only; the second call writes at most `size` bytes into `buffer`.
        unsafe {
            if WinHttpQueryHeaders(handle, info, std::ptr::null(), std::ptr::null_mut(), &mut size, std::ptr::null_mut()) != 0
                || GetLastError() != ERROR_INSUFFICIENT_BUFFER
            {
                return None;
            }
            let mut buffer = vec![0u16; size as usize / 2 + 1];
            if WinHttpQueryHeaders(handle, info, std::ptr::null(), buffer.as_mut_ptr().cast(), &mut size, std::ptr::null_mut()) == 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&buffer[..size as usize / 2]))
        }
    }

    /// One GET without following redirects; no cookies are kept or sent.
    pub(super) fn request<'a>(address: &Address, offset: u64, options: &Options, live: &'a Live) -> Result<Response<'a>, DownloadError> {
        let session = session(address)?;
        let host = wide(address.host.trim_start_matches('[').trim_end_matches(']'));
        // SAFETY: a valid session and a NUL-terminated host name.
        let connection = unsafe { WinHttpConnect(session.0, host.as_ptr(), address.port, 0) };
        if connection.is_null() {
            return Err(failure(options));
        }
        let connection = Handle(connection);
        let (verb, path) = (wide("GET"), wide(&address.path));
        let flags = if address.secure { WINHTTP_FLAG_SECURE } else { 0 };
        // SAFETY: a valid connection and NUL-terminated strings; null version, referrer and accept types are defaults.
        let request = unsafe { WinHttpOpenRequest(connection.0, verb.as_ptr(), path.as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null(), flags) };
        if request.is_null() {
            return Err(failure(options));
        }
        live.set(request)?;
        let mut response = Response { status: 0, location: None, content_range: None, live, _connection: connection, _session: session };
        let handle = live.get()?;
        let features: u32 = WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_REDIRECTS;
        let timeout = options.stall_timeout.as_millis().min(i32::MAX as u128) as i32;
        let range: Vec<u16> = if offset > 0 { format!("Range: bytes={offset}-\r\n").encode_utf16().collect() } else { Vec::new() };
        // SAFETY: a live request handle; `features` and `range` outlive the calls.
        let sent = unsafe {
            WinHttpSetOption(handle, WINHTTP_OPTION_DISABLE_FEATURE, (&raw const features).cast(), 4) != 0
                && WinHttpSetTimeouts(handle, timeout, timeout, timeout, timeout) != 0
                && WinHttpSendRequest(
                    handle,
                    if range.is_empty() { std::ptr::null() } else { range.as_ptr() },
                    range.len() as u32,
                    std::ptr::null(),
                    0,
                    0,
                    0,
                ) != 0
        };
        if !sent {
            return Err(failure(options));
        }
        let handle = live.get()?;
        // SAFETY: a request that was sent.
        if unsafe { WinHttpReceiveResponse(handle, std::ptr::null_mut()) } == 0 {
            return Err(failure(options));
        }
        let handle = live.get()?;
        let mut status = 0u32;
        let mut size = 4u32;
        // SAFETY: `status` is a writable u32, as WINHTTP_QUERY_FLAG_NUMBER asks.
        let queried = unsafe {
            WinHttpQueryHeaders(handle, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, std::ptr::null(), (&raw mut status).cast(), &mut size, std::ptr::null_mut())
        };
        if queried == 0 {
            return Err(DownloadError::InvalidResponse);
        }
        response.status = u16::try_from(status).map_err(|_| DownloadError::InvalidResponse)?;
        response.location = header(handle, WINHTTP_QUERY_LOCATION);
        response.content_range = header(handle, WINHTTP_QUERY_CONTENT_RANGE);
        Ok(response)
    }
}

#[cfg(not(windows))]
mod transport {
    use super::{Address, DownloadError, Options};

    #[derive(Default)]
    pub(super) struct Live;

    impl Live {
        pub(super) fn abort(&self) {}
    }

    pub(super) struct Response<'a> {
        pub status: u16,
        pub location: Option<String>,
        pub content_range: Option<String>,
        _live: &'a Live,
    }

    impl Response<'_> {
        pub fn read(&mut self, _buffer: &mut [u8], _options: &Options) -> Result<usize, DownloadError> {
            Err(DownloadError::Network("downloads need Windows".into()))
        }
    }

    pub(super) fn request<'a>(_address: &Address, _offset: u64, _options: &Options, _live: &'a Live) -> Result<Response<'a>, DownloadError> {
        Err(DownloadError::Network("downloads need Windows".into()))
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    fn options() -> Options {
        Options { allow_loopback_http: true, ..Options::new(Arc::new(AtomicBool::new(false))) }
    }

    fn sha256(bytes: &[u8]) -> Digest {
        let mut sink = HashSink::new(&Digest::Sha256([0; 32]));
        sink.update(bytes);
        sink.finish()
    }

    #[test]
    fn reads_digests() {
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(Digest::sha256_hex(empty), Some(sha256(b"")));
        assert_eq!(Digest::sha256_hex(&empty.to_uppercase()), None);
        assert_eq!(Digest::sha256_hex(&empty[1..]), None);
        let abc = "3a81oZNherrMQXNJriBBMRLm+k6JqX6iCp7u5ktV05ohkpkqJ0/BqDa6PCOj/uu9RU1EI2Q86A4qmslPpUyknw==";
        let mut sink = HashSink::new(&Digest::Sha512([0; 64]));
        sink.update(b"abc");
        assert_eq!(Digest::sha512_base64(abc), Some(sink.finish()));
        assert_eq!(Digest::sha512_base64("abc"), None);
    }

    #[test]
    fn accepts_only_secure_addresses() {
        let strict = Options::new(Arc::new(AtomicBool::new(false)));
        let address = Address::parse("https://GitHub.com/RainyMarks/Rainy/releases/download/v1.0.0-resources/x.001#frag", &strict).unwrap();
        assert_eq!((address.secure, address.host.as_str(), address.port), (true, "github.com", 443));
        assert_eq!(address.path, "/RainyMarks/Rainy/releases/download/v1.0.0-resources/x.001");
        assert_eq!(Address::parse("https://example.com:8443?x=1", &strict).unwrap().path, "/?x=1");
        for refused in ["http://example.com/x", "http://127.0.0.1/x", "ftp://example.com/x", "https://user:pw@example.com/", "https:///x", "https://a b/", "example.com"] {
            assert_eq!(Address::parse(refused, &strict), Err(DownloadError::Insecure(refused.to_owned())), "{refused}");
        }
        let loopback = options();
        for allowed in ["http://127.0.0.1:8080/a", "http://localhost/a", "http://[::1]:9/a"] {
            assert!(Address::parse(allowed, &loopback).is_ok(), "{allowed}");
        }
        assert!(Address::parse("http://example.com/x", &loopback).is_err());
        assert!(Address::parse("http://127.0.0.2/x", &loopback).is_err());
    }

    #[test]
    fn resolves_redirect_locations() {
        let base = Address::parse("https://github.com/a/b/file.001?x=1", &options()).unwrap();
        assert_eq!(base.join("https://objects.example.com/z?sig=1"), "https://objects.example.com/z?sig=1");
        assert_eq!(base.join("//cdn.example.com/z"), "https://cdn.example.com/z");
        assert_eq!(base.join("/root"), "https://github.com/root");
        assert_eq!(base.join("other.002"), "https://github.com/a/b/other.002");
        assert_eq!(base.join("?y=2"), "https://github.com/a/b/file.001?y=2");
        assert_eq!(base.join("path/with://inside"), "https://github.com/a/b/path/with://inside");
    }

    #[test]
    fn names_partial_files() {
        assert_eq!(partial_path(Path::new("C:/m/.downloads/php.tar.gz")), PathBuf::from("C:/m/.downloads/php.tar.gz.partial"));
    }

    #[test]
    fn refuses_hard_linked_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert_eq!(check_path(&file), Ok(()));
        assert_eq!(check_path(&dir.path().join("missing").join("deeper")), Ok(()));
        fs::hard_link(&file, dir.path().join("second")).unwrap();
        assert!(matches!(check_path(&file), Err(DownloadError::Link(_))));
        assert!(matches!(check_path(&file.join("below")), Err(DownloadError::Link(_))));
    }

    #[cfg(windows)]
    mod network {
        use ::core::prelude::v1::test;
        use std::sync::Mutex;
        use std::time::Instant;

        use super::super::*;
        use super::{options, sha256};
        use crate::test_server::{Reply, Server};

        fn body(length: usize) -> Vec<u8> {
            (0..length).map(|index| (index * 7 % 251) as u8).collect()
        }

        fn expected(bytes: &[u8]) -> Expected {
            Expected { bytes: bytes.len() as u64, digest: sha256(bytes) }
        }

        #[test]
        fn downloads_a_whole_file() {
            let data = body(700_000);
            let served = data.clone();
            let server = Server::start(move |_| Reply::ok(served.clone()));
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("file.bin");
            let seen = Mutex::new(Vec::new());
            download(&server.url("/file"), &target, &expected(&data), &options(), &|done, total| seen.lock().unwrap().push((done, total))).unwrap();
            assert_eq!(fs::read(&target).unwrap(), data);
            assert!(!partial_path(&target).exists());
            let seen = seen.into_inner().unwrap();
            assert_eq!(seen.first(), Some(&(0, 700_000)));
            assert_eq!(seen.last(), Some(&(700_000, 700_000)));
            assert!(seen.windows(2).all(|pair| pair[0].0 <= pair[1].0));
            // A file that is already there and matches needs no request.
            download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}).unwrap();
            assert_eq!(server.seen().len(), 1);
        }

        #[test]
        fn resumes_with_the_exact_range() {
            let data = body(300_000);
            let served = data.clone();
            let server = Server::start(move |seen| Reply::ranged(seen, &served));
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("file.bin");
            fs::write(partial_path(&target), &data[..120_000]).unwrap();
            let first = Mutex::new(None);
            download(&server.url("/file"), &target, &expected(&data), &options(), &|done, _| {
                first.lock().unwrap().get_or_insert(done);
            })
            .unwrap();
            assert_eq!(fs::read(&target).unwrap(), data);
            assert_eq!(*first.lock().unwrap(), Some(120_000));
            assert_eq!(server.seen()[0].range.as_deref(), Some("bytes=120000-"));
        }

        #[test]
        fn starts_over_when_the_server_ignores_the_range() {
            let data = body(200_000);
            let served = data.clone();
            let server = Server::start(move |_| Reply::ok(served.clone()));
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("file.bin");
            // Different bytes than the server's: a restart must not keep them.
            fs::write(partial_path(&target), vec![0xEE; 50_000]).unwrap();
            download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}).unwrap();
            assert_eq!(fs::read(&target).unwrap(), data);
            assert_eq!(server.seen()[0].range.as_deref(), Some("bytes=50000-"));
        }

        #[test]
        fn refuses_another_content_range() {
            let data = body(10_000);
            let served = data.clone();
            let server = Server::start(move |_| Reply::partial(served[1000..].to_vec(), "bytes 1000-9999/10000"));
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("file.bin");
            fs::write(partial_path(&target), &data[..2000]).unwrap();
            assert_eq!(download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::InvalidResume));
            assert_eq!(fs::read(partial_path(&target)).unwrap(), &data[..2000]);
            assert!(!target.exists());
        }

        #[test]
        fn follows_redirects() {
            let data = body(5000);
            let served = data.clone();
            let server = Server::start(move |seen| match seen.path.as_str() {
                "/start" => Reply::redirect(302, "/middle/next"),
                "/middle/next" => Reply::redirect(307, "final?x=1"),
                "/middle/final?x=1" => Reply::redirect(301, &format!("http://127.0.0.1:{}/file", seen.port)),
                "/file" => Reply::ok(served.clone()),
                "/away" => Reply::redirect(302, "http://example.com/file"),
                "/loop" => Reply::redirect(302, "/loop"),
                _ => Reply::status(404),
            });
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("file.bin");
            download(&server.url("/start"), &target, &expected(&data), &options(), &|_, _| {}).unwrap();
            assert_eq!(fs::read(&target).unwrap(), data);
            let paths: Vec<String> = server.seen().into_iter().map(|seen| seen.path).collect();
            assert_eq!(paths, ["/start", "/middle/next", "/middle/final?x=1", "/file"]);
            // Leaving HTTPS (here: loopback) for plain HTTP elsewhere is refused before any connection.
            let other = dir.path().join("other.bin");
            assert_eq!(download(&server.url("/away"), &other, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Insecure("http://example.com/file".into())));
            assert_eq!(download(&server.url("/loop"), &other, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Redirects));
            assert_eq!(download(&server.url("/missing"), &other, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Status(404)));
        }

        #[test]
        fn stops_when_too_many_bytes_arrive() {
            let data = body(4000);
            let longer = body(9000);
            let server = Server::start(move |_| Reply::ok(longer.clone()));
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("file.bin");
            assert_eq!(download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::TooLarge));
            assert!(!target.exists());
            assert!(fs::metadata(partial_path(&target)).unwrap().len() <= 4000);
        }

        #[test]
        fn cancelling_keeps_the_partial_file() {
            let data = body(400_000);
            let served = data.clone();
            let server = Server::start(move |_| Reply::ok(served.clone()).stall_after(100_000, Duration::from_secs(20)));
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("file.bin");
            let options = options();
            let cancel = options.cancel.clone();
            let started = Instant::now();
            // Cancelled from another thread while the download waits for the stalled server.
            let result = download(&server.url("/file"), &target, &expected(&data), &options, &|done, _| {
                if done >= 100_000 {
                    let cancel = cancel.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(300));
                        cancel.store(true, Ordering::Relaxed);
                    });
                }
            });
            assert_eq!(result, Err(DownloadError::Cancelled));
            assert!(started.elapsed() < Duration::from_secs(10), "cancelling took {:?}", started.elapsed());
            assert_eq!(fs::read(partial_path(&target)).unwrap(), &data[..100_000]);
            assert!(!target.exists());
        }

        #[test]
        fn a_stalled_download_fails_and_keeps_its_part() {
            let data = body(50_000);
            let served = data.clone();
            let server = Server::start(move |_| Reply::ok(served.clone()).stall_after(20_000, Duration::from_secs(6)));
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("file.bin");
            let options = Options { stall_timeout: Duration::from_secs(1), ..options() };
            assert_eq!(download(&server.url("/file"), &target, &expected(&data), &options, &|_, _| {}), Err(DownloadError::Stalled(1)));
            assert_eq!(fs::read(partial_path(&target)).unwrap(), &data[..20_000]);
        }

        #[test]
        fn a_slow_body_within_the_timeout_arrives() {
            let data = body(40_000);
            let served = data.clone();
            let server = Server::start(move |_| Reply::ok(served.clone()).paced(8_000, Duration::from_millis(150)));
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("file.bin");
            let options = Options { stall_timeout: Duration::from_secs(2), ..options() };
            download(&server.url("/file"), &target, &expected(&data), &options, &|_, _| {}).unwrap();
            assert_eq!(fs::read(&target).unwrap(), data);
        }

        #[test]
        fn rejects_a_digest_mismatch() {
            let data = body(30_000);
            let mut wrong = data.clone();
            wrong[29_999] ^= 1;
            let server = Server::start(move |_| Reply::ok(wrong.clone()));
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("file.bin");
            assert_eq!(download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Mismatch));
            assert!(!target.exists());
            assert!(!partial_path(&target).exists());
            // A complete partial file that does not match is replaced, not trusted.
            fs::write(partial_path(&target), vec![1u8; 30_000]).unwrap();
            assert_eq!(download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Mismatch));
            assert_eq!(server.seen().last().unwrap().range, None);
            // A partial file larger than the file (another version's) is replaced from the start.
            fs::write(partial_path(&target), vec![1u8; 30_001]).unwrap();
            assert_eq!(download(&server.url("/file"), &target, &expected(&data), &options(), &|_, _| {}), Err(DownloadError::Mismatch));
            assert_eq!(server.seen().last().unwrap().range, None);
        }

        #[test]
        fn verifies_installers_by_sha512() {
            let data = body(3000);
            let served = data.clone();
            let server = Server::start(move |_| Reply::ok(served.clone()));
            let mut sink = HashSink::new(&Digest::Sha512([0; 64]));
            sink.update(&data);
            let digest = sink.finish();
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("setup.exe");
            download(&server.url("/setup.exe"), &target, &Expected { bytes: 3000, digest: digest.clone() }, &options(), &|_, _| {}).unwrap();
            assert_eq!(file_digest(&target, &digest, &AtomicBool::new(false)).unwrap(), digest);
        }

        #[test]
        fn fetches_small_documents() {
            let server = Server::start(|seen| match seen.path.as_str() {
                "/latest.yml" => Reply::ok(b"version: 4.0.0\n".to_vec()),
                "/moved" => Reply::redirect(302, "/latest.yml"),
                _ => Reply::status(404),
            });
            assert_eq!(fetch(&server.url("/moved"), 1024, &options()).unwrap(), b"version: 4.0.0\n");
            assert_eq!(fetch(&server.url("/latest.yml"), 4, &options()), Err(DownloadError::TooLarge));
            assert_eq!(fetch(&server.url("/nothing"), 1024, &options()), Err(DownloadError::Status(404)));
            let strict = Options::new(Arc::new(AtomicBool::new(false)));
            assert!(matches!(fetch(&server.url("/latest.yml"), 1024, &strict), Err(DownloadError::Insecure(_))));
        }
    }
}
