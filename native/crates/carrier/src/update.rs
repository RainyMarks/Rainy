//! Rainy's updates (3.x used electron-updater 6.8.9 through `updates.ts`): `latest.yml` of the latest GitHub release,
//! a strictly newer stable version, its installer downloaded into the updater cache and verified by size and SHA-512,
//! `latest.yml` itself by the release key (a build with keys refuses a release without its signature), then the
//! installer started as 3.x started it.
//! Calls that reach the network or hash files block; run them on a background thread.

use std::cmp::Ordering;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
pub use semver::Version;
use serde::{Deserialize, Serialize};

use crate::download::{self, Digest, DownloadError, Expected, Options};
use crate::keyring::Keyring;
use crate::trust;

/// The same file 3.x reads, through the redirect GitHub keeps to the latest release.
pub const FEED: &str = "https://github.com/RainyMarks/Rainy/releases/latest/download/latest.yml";
/// A release's files, by its tag `v<version>`.
const RELEASE_DOWNLOADS: &str = "https://github.com/RainyMarks/Rainy/releases/download";
/// The release key's Ed25519 signature of `latest.yml`, as base64url; 3.x clients ignore it.
pub const SIGNATURE_FILE: &str = "latest.yml.sig";
/// What the release key signs before the bytes of `latest.yml`.
pub const SIGNATURE_DOMAIN: &[u8] = b"Rainy/update/v1\0";
/// The address of another `latest.yml`, for tests and QA; its release's files are read from the same folder, and a
/// loopback address may use plain HTTP.
pub const FEED_VARIABLE: &str = "RAINY_UPDATE_FEED";
/// Another folder for downloaded installers, for tests and QA.
pub const CACHE_VARIABLE: &str = "RAINY_UPDATE_CACHE";
/// In Rainy's user data folder: the installers of a feed named by [`FEED_VARIABLE`], kept apart from 3.x's cache.
const TEST_CACHE: &str = "update-cache";
/// electron-updater's record of the installer it downloaded into the same folder.
const ELECTRON_UPDATER_RECORD: &str = "update-info.json";
/// The command line every Rainy since 2.2 starts the installer with: update in place, silently, then start Rainy.
pub const INSTALLER_ARGUMENTS: [&str; 3] = ["--updated", "/S", "--force-run"];
/// Beside the installer: which release it is, so a later start can offer it without the network.
const RECORD_FILE: &str = "rainy-update.json";
const FEED_LIMIT: usize = 1024 * 1024;
const SIGNATURE_LIMIT: usize = 4096;
/// GitHub keeps release files under 2 GiB.
const MAX_INSTALLER: u64 = 2 * 1024 * 1024 * 1024;

/// Why an update could not be found or prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    Download(DownloadError),
    /// `latest.yml` is not what electron-builder writes: what is wrong.
    Invalid(String),
    /// `latest.yml` describes a web installer (`packages`), which Rainy does not publish.
    WebInstaller,
    /// The signature of `latest.yml` is missing or not the release key's.
    Signature,
    /// The updater cache could not be used: the path and the system's error.
    Io(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Download(error) => error.fmt(f),
            Self::Invalid(detail) => write!(f, "The update information is not valid: {detail}"),
            Self::WebInstaller => f.write_str("The update information describes a web installer, which Rainy does not use."),
            Self::Signature => f.write_str("The signature of the update information is not valid."),
            Self::Io(detail) => write!(f, "The update could not be stored: {detail}"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<DownloadError> for UpdateError {
    fn from(error: DownloadError) -> Self {
        Self::Download(error)
    }
}

fn invalid(detail: impl Into<String>) -> UpdateError {
    UpdateError::Invalid(detail.into())
}

fn io_error(path: &Path, error: io::Error) -> UpdateError {
    UpdateError::Io(format!("{}: {error}", path.display()))
}

/// Where `latest.yml` and the release's files are read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The address of `latest.yml`.
    pub feed: String,
    /// The folder of a feed named by [`FEED_VARIABLE`]; GitHub's releases keep their files under their tag instead.
    folder: Option<String>,
    /// Plain HTTP to this computer, which only a named feed may use.
    pub allow_loopback_http: bool,
}

impl Source {
    pub fn github() -> Self {
        Self { feed: FEED.to_owned(), folder: None, allow_loopback_http: false }
    }

    /// The `latest.yml` at `feed`, with its files beside it.
    pub fn at(feed: &str) -> Self {
        let address = feed.split(['?', '#']).next().unwrap_or(feed);
        let folder = address.rfind('/').map_or(address, |slash| &address[..=slash]);
        Self { feed: feed.to_owned(), folder: Some(folder.to_owned()), allow_loopback_http: true }
    }

    /// GitHub, unless [`FEED_VARIABLE`] names another feed.
    pub fn from_env() -> Self {
        match std::env::var(FEED_VARIABLE) {
            Ok(feed) if !feed.trim().is_empty() => Self::at(feed.trim()),
            _ => Self::github(),
        }
    }

    /// Whether the feed is a test feed rather than GitHub's.
    pub fn overridden(&self) -> bool {
        self.folder.is_some()
    }

    /// The address of one file of the release `version`.
    pub fn file_url(&self, version: &Version, file: &str) -> String {
        match &self.folder {
            Some(folder) => format!("{folder}{file}"),
            None => format!("{RELEASE_DOWNLOADS}/v{version}/{file}"),
        }
    }

    /// How requests for this source behave; `cancel` stops them.
    pub fn options(&self, cancel: Arc<AtomicBool>) -> Options {
        Options { allow_loopback_http: self.allow_loopback_http, ..Options::new(cancel) }
    }
}

/// `latest.yml` as electron-builder writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    pub version: Version,
    pub files: Vec<FeedFile>,
    /// The first file's name and digest again, for old updaters.
    pub path: Option<String>,
    pub sha512: Option<String>,
    pub release_date: Option<String>,
    /// The oldest Windows version (`10.0.<build>`) the release runs on.
    pub minimum_system_version: Option<String>,
}

/// One file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedFile {
    /// The file's name in the release.
    pub url: String,
    /// Base64 SHA-512 of the file.
    pub sha512: String,
    pub size: Option<u64>,
    pub block_map_size: Option<u64>,
}

/// A newer release's installer, to download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installer {
    pub version: Version,
    /// Its file name in the release and in the cache.
    pub file: String,
    pub url: String,
    pub expected: Expected,
}

/// A downloaded installer that matched its release when it was last checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub version: Version,
    pub path: PathBuf,
    pub expected: Expected,
}

impl Prepared {
    /// Whether the file still has its release's size and SHA-512.
    pub fn verify(&self, cancel: &AtomicBool) -> Result<bool, UpdateError> {
        Ok(download::matches(&self.path, &self.expected, cancel)?)
    }
}

/// A version as `latest.yml` and the record write it; a leading `v`, as in a tag, is allowed.
pub fn parse_version(text: &str) -> Option<Version> {
    let text = text.trim();
    Version::parse(text.strip_prefix(['v', 'V']).unwrap_or(text)).ok()
}

/// Whether `latest` is offered to `current`: a stable version strictly newer by semantic version precedence. No
/// prereleases, no downgrades, and build metadata alone is no update.
pub fn offered(latest: &Version, current: &Version) -> bool {
    latest.pre.is_empty() && latest.cmp_precedence(current) == Ordering::Greater
}

/// Whether Windows `system` is at least the release's `minimum`; like electron-updater, a version that cannot be read
/// does not stop the update.
pub fn supported(minimum: Option<&str>, system: Option<&Version>) -> bool {
    match (minimum.and_then(parse_version), system) {
        (Some(minimum), Some(system)) => system.cmp_precedence(&minimum) != Ordering::Less,
        _ => true,
    }
}

/// Read the newest release from `source`: the installer to download when it is offered to `current`, or `None`. A
/// release's `latest.yml` must carry the signature of a key in `keyring`; a build without keys does not
/// check signatures.
pub fn check(source: &Source, current: &Version, keyring: Option<&Keyring>, options: &Options) -> Result<Option<Installer>, UpdateError> {
    let bytes = download::fetch(&source.feed, FEED_LIMIT, options)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid("latest.yml is not UTF-8"))?;
    let feed = parse_feed(text)?;
    if !offered(&feed.version, current) {
        log::info!("Update check: the latest release is {}, this is {current}", feed.version);
        return Ok(None);
    }
    if !supported(feed.minimum_system_version.as_deref(), os_version().as_ref()) {
        log::info!("Update check: {} needs Windows {}", feed.version, feed.minimum_system_version.as_deref().unwrap_or_default());
        return Ok(None);
    }
    let installer = feed.installer(source)?;
    match keyring {
        None => log::warn!("Update {}: this build has no release keys, so the signature of latest.yml is not checked", feed.version),
        Some(keyring) => match download::fetch(&source.file_url(&feed.version, SIGNATURE_FILE), SIGNATURE_LIMIT, options) {
            // Every release since 4.0 signs it, and only newer ones are offered. The installer's SHA-512 comes from the
            // same unsigned file, so taking a release without the signature would let whoever can replace the release
            // files replace the installer too, just by leaving the signature out.
            Err(DownloadError::Status(404)) => {
                log::warn!("Update {}: the release has no {SIGNATURE_FILE}; the update is refused", feed.version);
                return Err(UpdateError::Signature);
            }
            Err(error) => return Err(error.into()),
            Ok(signature) if signature_valid(keyring, &bytes, &signature) => log::info!("Update {}: latest.yml is signed by the release key", feed.version),
            Ok(_) => {
                log::warn!("Update {}: {SIGNATURE_FILE} is not the release key's signature of latest.yml; the update is refused", feed.version);
                return Err(UpdateError::Signature);
            }
        },
    }
    log::info!("Update check: {} is available ({}, {} bytes)", feed.version, installer.file, installer.expected.bytes);
    Ok(Some(installer))
}

/// Whether `signature` (base64url, as `latest.yml.sig` holds it) is a release key's signature of the exact bytes of
/// `feed` under [`SIGNATURE_DOMAIN`].
pub fn signature_valid(keyring: &Keyring, feed: &[u8], signature: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(signature) else { return false };
    match URL_SAFE_NO_PAD.decode(text.trim().trim_end_matches('=')) {
        Ok(signature) => signature.len() == 64 && trust::signed_by(keyring, SIGNATURE_DOMAIN, feed, &signature),
        Err(_) => false,
    }
}

/// The folder installers from `source` are downloaded into: [`CACHE_VARIABLE`]; for a test feed, `update-cache` in
/// `user_data`; else 3.x's `%LOCALAPPDATA%\rainy-agent-updater\pending`, which electron-updater shares.
pub fn cache_dir(source: &Source, user_data: &Path) -> Option<PathBuf> {
    let test = source.overridden().then(|| user_data.join(TEST_CACHE));
    cache_dir_from(std::env::var_os(CACHE_VARIABLE), test, std::env::var_os("LOCALAPPDATA"))
}

fn cache_dir_from(named: Option<OsString>, test: Option<PathBuf>, local_app_data: Option<OsString>) -> Option<PathBuf> {
    if let Some(named) = named.filter(|named| !named.is_empty()) {
        return std::path::absolute(PathBuf::from(named)).ok();
    }
    test.or_else(|| local_app_data.filter(|folder| !folder.is_empty()).map(|folder| PathBuf::from(folder).join("rainy-agent-updater").join("pending")))
}

/// Download `installer` into `cache`, or keep the matching file an earlier start downloaded, then remember it in place
/// of the installer this updater recorded before, which is removed. `progress` gets the bytes present so far and the
/// total.
pub fn download(installer: &Installer, cache: &Path, options: &Options, progress: &dyn Fn(u64, u64)) -> Result<Prepared, UpdateError> {
    download::check_path(cache)?;
    fs::create_dir_all(cache).map_err(|error| io_error(cache, error))?;
    let target = cache.join(&installer.file);
    download::download(&installer.url, &target, &installer.expected, options, progress)?;
    log::info!("Update {}: {} is downloaded and verified", installer.version, target.display());
    let earlier = recorded_file(cache);
    match write_record(cache, installer) {
        Ok(()) => remove_earlier(cache, earlier.as_deref(), &installer.file),
        Err(error) => log::warn!("Update {}: the download was not recorded: {error}", installer.version),
    }
    Ok(Prepared { version: installer.version.clone(), path: target, expected: installer.expected.clone() })
}

/// `rainy-update.json`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    release: String,
    file: String,
    sha512: String,
    size: u64,
}

fn write_record(cache: &Path, installer: &Installer) -> Result<(), UpdateError> {
    let Digest::Sha512(digest) = &installer.expected.digest else { return Err(invalid("an installer needs a SHA-512")) };
    let record = Record { version: 1, release: installer.version.to_string(), file: installer.file.clone(), sha512: STANDARD.encode(digest), size: installer.expected.bytes };
    let path = cache.join(RECORD_FILE);
    let temporary = cache.join(format!("{RECORD_FILE}.tmp"));
    let text = serde_json::to_string(&record).map_err(|error| invalid(error.to_string()))? + "\n";
    fs::write(&temporary, text).map_err(|error| io_error(&temporary, error))?;
    download::rename(&temporary, &path).map_err(|error| io_error(&path, error))
}

/// The installer an earlier start downloaded, when it is still offered to `current` and still matches its release.
/// Once this version or a later one runs, the installer and its record are deleted.
pub fn pending(cache: &Path, current: &Version, cancel: &AtomicBool) -> Option<Prepared> {
    let record_path = cache.join(RECORD_FILE);
    let text = fs::read_to_string(&record_path).ok()?;
    let forget = |why: &str| {
        log::info!("Update: {why}; {} is removed", record_path.display());
        let _ = fs::remove_file(&record_path);
    };
    let record = match serde_json::from_str::<Record>(&text) {
        Ok(record) if record.version == 1 && safe_file_name(&record.file) => record,
        _ => {
            forget("the record of the downloaded installer is not valid");
            return None;
        }
    };
    let (Some(version), Some(digest)) = (parse_version(&record.release), Digest::sha512_base64(&record.sha512)) else {
        forget("the record of the downloaded installer is not valid");
        return None;
    };
    let path = cache.join(&record.file);
    if !offered(&version, current) {
        remove_installer(cache, &record.file, &format!("the installer of {version}, which is installed"));
        forget("this release is installed");
        return None;
    }
    let expected = Expected { bytes: record.size, digest };
    match download::matches(&path, &expected, cancel) {
        Ok(true) => Some(Prepared { version, path, expected }),
        Ok(false) => None,
        Err(error) => {
            log::warn!("Update {version}: the downloaded installer could not be checked: {error}");
            None
        }
    }
}

/// The installer `rainy-update.json` names, when the record is valid.
fn recorded_file(cache: &Path) -> Option<String> {
    let text = fs::read_to_string(cache.join(RECORD_FILE)).ok()?;
    serde_json::from_str::<Record>(&text).ok().filter(|record| record.version == 1 && safe_file_name(&record.file)).map(|record| record.file)
}

/// Remove what this updater left of other releases: the installer it recorded before, and unfinished downloads of
/// Rainy installers (`.partial` is this updater's; electron-updater downloads into `temp-<file>`). What
/// electron-updater keeps in the same folder stays.
fn remove_earlier(cache: &Path, recorded: Option<&str>, keep: &str) {
    if let Some(recorded) = recorded.filter(|recorded| !recorded.eq_ignore_ascii_case(keep)) {
        remove_installer(cache, recorded, &format!("the earlier installer {recorded}"));
    }
    let Ok(entries) = fs::read_dir(cache) else { return };
    for name in entries.flatten().filter_map(|entry| entry.file_name().into_string().ok()) {
        if unfinished(&name, keep) {
            remove_installer(cache, &name, &format!("the unfinished download {name}"));
        }
    }
}

/// An unfinished download of a Rainy installer other than `keep`.
fn unfinished(name: &str, keep: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.strip_suffix(".partial").is_some_and(|base| base != keep.to_ascii_lowercase() && base.starts_with("rainy") && base.ends_with(".exe"))
}

/// Delete the regular file `name` from the cache, unless electron-updater's record names it.
fn remove_installer(cache: &Path, name: &str, what: &str) {
    if electron_updater_file(cache).is_some_and(|file| file.eq_ignore_ascii_case(name)) {
        log::info!("Update: {what} is kept, electron-updater's {ELECTRON_UPDATER_RECORD} names it");
        return;
    }
    let path = cache.join(name);
    if !matches!(fs::symlink_metadata(&path), Ok(metadata) if metadata.is_file()) {
        return;
    }
    match fs::remove_file(&path) {
        Ok(()) => log::info!("Update: removed {what}"),
        Err(error) => log::warn!("Update: {what} was not removed: {error}"),
    }
}

/// The installer electron-updater prepared in the cache (`update-info.json`, `fileName`).
fn electron_updater_file(cache: &Path) -> Option<String> {
    let text = fs::read_to_string(cache.join(ELECTRON_UPDATER_RECORD)).ok()?;
    let info: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    info.get("fileName")?.as_str().map(str::to_owned)
}

/// `"<installer>" --updated /S --force-run`.
pub fn command_line(installer: &Path) -> OsString {
    let mut line = OsString::from("\"");
    line.push(installer.as_os_str());
    line.push("\"");
    for argument in INSTALLER_ARGUMENTS {
        line.push(" ");
        line.push(argument);
    }
    line
}

/// Start the installer detached from Rainy, in its own process group, with no handles of Rainy's and with its own
/// folder as the working folder, so nothing it starts keeps the installation folder in use; the process id. An
/// installer that needs elevation is started through the shell, which asks for it.
#[cfg(windows)]
pub fn launch(installer: &Path) -> io::Result<u32> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ELEVATION_REQUIRED};
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION, STARTUPINFOW,
    };

    let wide = |text: &std::ffi::OsStr| text.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let application = wide(installer.as_os_str());
    let mut command = wide(&command_line(installer));
    let folder = installer.parent().map(|folder| wide(folder.as_os_str()));
    // SAFETY: plain data; the size is set below.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = size_of::<STARTUPINFOW>() as u32;
    // SAFETY: plain data, filled by the call.
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated strings that outlive the call, a writable command line as CreateProcessW requires, and no
    // inherited handles.
    let started = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
            std::ptr::null(),
            folder.as_ref().map_or(std::ptr::null(), |folder| folder.as_ptr()),
            &startup,
            &mut process,
        )
    };
    if started == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED as i32) {
            return launch_elevated(installer);
        }
        return Err(error);
    }
    // SAFETY: handles the call returned, closed once; the installer keeps running.
    unsafe {
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    Ok(process.dwProcessId)
}

/// Through the shell, which asks for elevation (3.x used `elevate.exe`); the process id is not known, so 0.
#[cfg(windows)]
fn launch_elevated(installer: &Path) -> io::Result<u32> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |text: &std::ffi::OsStr| text.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (verb, file) = (wide("open".as_ref()), wide(installer.as_os_str()));
    let parameters = wide(INSTALLER_ARGUMENTS.join(" ").as_ref());
    let folder = installer.parent().map(|folder| wide(folder.as_os_str()));
    // SAFETY: NUL-terminated strings that outlive the call; no owner window.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            folder.as_ref().map_or(std::ptr::null(), |folder| folder.as_ptr()),
            SW_SHOWNORMAL,
        )
    };
    // Values up to 32 are errors.
    if result as usize <= 32 { Err(io::Error::last_os_error()) } else { Ok(0) }
}

#[cfg(not(windows))]
pub fn launch(_installer: &Path) -> io::Result<u32> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "updates need Windows"))
}

/// This Windows' version as Node's `os.release()` gives it, `10.0.<build>`.
#[cfg(windows)]
fn os_version() -> Option<Version> {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};

    use crate::text::wide;
    let key = wide("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let number = |name: &str| {
        let (name, mut value, mut size) = (wide(name), 0u32, 4u32);
        // SAFETY: NUL-terminated names and a writable DWORD of the size given.
        let read = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&raw mut value).cast(), &mut size) };
        (read == 0).then_some(value)
    };
    let (mut build, mut size) = ([0u16; 32], 64u32);
    let name = wide("CurrentBuildNumber");
    // SAFETY: NUL-terminated names and a writable buffer of the size given.
    let read = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), build.as_mut_ptr().cast(), &mut size) };
    if read != 0 {
        return None;
    }
    let build = String::from_utf16_lossy(&build[..(size as usize / 2).saturating_sub(1)]);
    Version::parse(&format!("{}.{}.{}", number("CurrentMajorVersionNumber")?, number("CurrentMinorVersionNumber")?, build.trim())).ok()
}

#[cfg(not(windows))]
fn os_version() -> Option<Version> {
    None
}

impl Feed {
    /// The installer to download: the first `.exe` whose name says `x64`, else the first `.exe` (electron-updater's
    /// choice), with its size and SHA-512.
    pub fn installer(&self, source: &Source) -> Result<Installer, UpdateError> {
        let installers: Vec<&FeedFile> = self.files.iter().filter(|file| file.url.to_ascii_lowercase().ends_with(".exe")).collect();
        let file = installers.iter().find(|file| file.url.contains("x64")).or(installers.first()).ok_or_else(|| invalid("no installer is listed"))?;
        // electron-updater requests the name with its spaces as dashes.
        let name = file.url.replace(' ', "-");
        if !safe_file_name(&name) {
            return Err(invalid(format!("the installer's name is not a plain file name: {}", file.url)));
        }
        let digest = Digest::sha512_base64(&file.sha512).ok_or_else(|| invalid(format!("the SHA-512 of {name} is not valid")))?;
        let bytes = file.size.ok_or_else(|| invalid(format!("the size of {name} is missing")))?;
        if bytes == 0 || bytes > MAX_INSTALLER {
            return Err(invalid(format!("the size of {name} is not valid: {bytes}")));
        }
        Ok(Installer { version: self.version.clone(), url: source.file_url(&self.version, &name), file: name, expected: Expected { bytes, digest } })
    }
}

/// A file name that is safe as a URL path part and on Windows: letters, digits, `.`, `_`, `-` and `+`, no device name.
fn safe_file_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
    (1..=200).contains(&name.len())
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
        && !name.starts_with('.')
        && !name.contains("..")
        && !device
}

/// One line that carries content: its number, indentation and text without trailing spaces.
#[derive(Debug, Clone, Copy)]
struct Line<'a> {
    number: usize,
    indent: usize,
    text: &'a str,
}

/// A `key: value` of a block mapping, with the lines nested below it.
struct Entry<'a> {
    line: usize,
    key: String,
    value: &'a str,
    nested: Vec<Line<'a>>,
}

impl Entry<'_> {
    /// A single-line scalar value.
    fn scalar(&self) -> Result<String, UpdateError> {
        if self.value.is_empty() || !self.nested.is_empty() {
            return Err(invalid(format!("line {}: {} is not a single value", self.line, self.key)));
        }
        scalar(self.value).map_err(|why| invalid(format!("line {}: {} {why}", self.line, self.key)))
    }

    fn number(&self) -> Result<u64, UpdateError> {
        let text = self.scalar()?;
        match text.parse::<u64>() {
            Ok(number) if text.bytes().all(|byte| byte.is_ascii_digit()) => Ok(number),
            _ => Err(invalid(format!("line {}: {} is not a whole number", self.line, self.key))),
        }
    }
}

/// Read `latest.yml` (the subset of YAML electron-builder writes: a block mapping of scalars and the `files` list of
/// mappings; scalars plain, single- or double-quoted). Unknown keys are ignored; `packages` is refused.
pub fn parse_feed(text: &str) -> Result<Feed, UpdateError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let content = raw.trim_start_matches(' ');
        let indent = raw.len() - content.len();
        let content = content.trim_end();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        if content.starts_with('\t') {
            return Err(invalid(format!("line {}: tabs are not allowed for indentation", index + 1)));
        }
        if indent == 0 && (content == "---" || content.starts_with("--- ")) {
            if lines.is_empty() {
                continue;
            }
            return Err(invalid("latest.yml holds more than one document"));
        }
        if indent == 0 && content == "..." {
            break;
        }
        lines.push(Line { number: index + 1, indent, text: content });
    }
    let entries = mapping(&lines, 0).map_err(invalid)?;
    if entries.iter().any(|entry| entry.key == "packages") {
        return Err(UpdateError::WebInstaller);
    }
    let find = |key: &str| entries.iter().find(|entry| entry.key == key);
    let optional = |key: &str| find(key).map(Entry::scalar).transpose();
    let version_text = find("version").ok_or_else(|| invalid("version is missing"))?.scalar()?;
    let version = parse_version(&version_text).ok_or_else(|| invalid(format!("version is not a semantic version: {version_text}")))?;
    let files = match find("files") {
        None => Vec::new(),
        Some(entry) if entry.value.is_empty() && !entry.nested.is_empty() => {
            let items = sequence(&entry.nested).map_err(|why| invalid(format!("files: {why}")))?;
            items.iter().map(|item| feed_file(item)).collect::<Result<_, _>>()?
        }
        Some(entry) => return Err(invalid(format!("line {}: files is not a list", entry.line))),
    };
    Ok(Feed {
        version,
        files,
        path: optional("path")?,
        sha512: optional("sha512")?,
        release_date: optional("releaseDate")?,
        minimum_system_version: optional("minimumSystemVersion")?,
    })
}

fn feed_file(item: &[Entry<'_>]) -> Result<FeedFile, UpdateError> {
    let find = |key: &str| item.iter().find(|entry| entry.key == key);
    let first = item.first().map_or(0, |entry| entry.line);
    let required = |key: &str| find(key).ok_or_else(|| invalid(format!("line {first}: a file has no {key}")))?.scalar();
    Ok(FeedFile {
        url: required("url")?,
        sha512: required("sha512")?,
        size: find("size").map(Entry::number).transpose()?,
        block_map_size: find("blockMapSize").map(Entry::number).transpose()?,
    })
}

fn is_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

/// The entries of a block mapping whose keys start at `column`. Deeper lines belong to the entry above them, as do list
/// items at the same column (`key:` followed by `- item`).
fn mapping<'a>(lines: &[Line<'a>], column: usize) -> Result<Vec<Entry<'a>>, String> {
    let mut entries: Vec<Entry<'a>> = Vec::new();
    for line in lines {
        let below = line.indent > column || (line.indent == column && is_item(line.text) && entries.last().is_some_and(|entry| entry.value.is_empty()));
        if below {
            entries.last_mut().ok_or_else(|| format!("line {}: unexpected indentation", line.number))?.nested.push(*line);
            continue;
        }
        if line.indent < column {
            return Err(format!("line {}: unexpected indentation", line.number));
        }
        let (key, value) = key_value(line.text).ok_or_else(|| format!("line {}: expected `key: value`", line.number))?;
        if entries.iter().any(|entry| entry.key == key) {
            return Err(format!("line {}: {key} appears twice", line.number));
        }
        entries.push(Entry { line: line.number, key, value, nested: Vec::new() });
    }
    Ok(entries)
}

/// The mappings of a block sequence (`- key: value` items).
fn sequence<'a>(lines: &[Line<'a>]) -> Result<Vec<Vec<Entry<'a>>>, String> {
    let column = lines.first().map_or(0, |line| line.indent);
    let mut items: Vec<Vec<Line<'a>>> = Vec::new();
    for line in lines {
        if line.indent > column {
            items.last_mut().ok_or_else(|| format!("line {}: expected a list item", line.number))?.push(*line);
            continue;
        }
        if line.indent < column || !is_item(line.text) {
            return Err(format!("line {}: expected a list item", line.number));
        }
        let rest = &line.text[1..];
        let content = rest.trim_start();
        let mut item = Vec::new();
        if !content.is_empty() {
            item.push(Line { number: line.number, indent: column + 1 + rest.len() - content.len(), text: content });
        }
        items.push(item);
    }
    items
        .iter()
        .map(|item| {
            let first = item.first().ok_or("a list item is empty")?;
            if is_item(first.text) {
                return Err(format!("line {}: nested lists are not supported", first.number));
            }
            mapping(item, first.indent)
        })
        .collect()
}

/// `key: value` or `key:`, with a plain or quoted key.
fn key_value(text: &str) -> Option<(String, &str)> {
    let (key, rest) = if text.starts_with(['\'', '"']) {
        let (key, rest) = quoted(text).ok()?;
        (key, rest.strip_prefix(':')?)
    } else {
        let end = text.find(": ").or_else(|| text.ends_with(':').then(|| text.len() - 1))?;
        let key = text[..end].trim_end();
        if key.is_empty() || key.starts_with(['-', '?', ':', '#', '&', '*', '!', '|', '>', '[', ']', '{', '}', ',', '@', '`', '%']) {
            return None;
        }
        (key.to_owned(), &text[end + 1..])
    };
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    let value = rest.trim_start();
    // `key: # comment` has no value.
    Some((key, if value.starts_with('#') { "" } else { value }))
}

/// One scalar on one line: plain (up to a ` #` comment), single-quoted or double-quoted.
fn scalar(value: &str) -> Result<String, String> {
    if value.starts_with(['\'', '"']) {
        let (text, rest) = quoted(value)?;
        let rest = rest.trim_start();
        if !(rest.is_empty() || rest.starts_with('#')) {
            return Err("has text after its closing quote".into());
        }
        return Ok(text);
    }
    if value.starts_with(['|', '>', '[', '{', '&', '*', '!', '%', '@', '`']) {
        return Err("is not a plain value".into());
    }
    let end = value.find(" #").unwrap_or(value.len());
    Ok(value[..end].trim_end().to_owned())
}

/// A quoted scalar at the start of `text` and what follows its closing quote.
fn quoted(text: &str) -> Result<(String, &str), String> {
    let mut chars = text.char_indices();
    let quote = chars.next().map(|(_, quote)| quote).ok_or("is empty")?;
    let mut out = String::new();
    while let Some((index, c)) = chars.next() {
        match (quote, c) {
            ('\'', '\'') => {
                if text[index + 1..].starts_with('\'') {
                    chars.next();
                    out.push('\'');
                } else {
                    return Ok((out, &text[index + 1..]));
                }
            }
            ('"', '"') => return Ok((out, &text[index + 1..])),
            ('"', '\\') => {
                let (_, escape) = chars.next().ok_or("ends inside an escape")?;
                let hex = |chars: &mut std::str::CharIndices<'_>, digits: usize| -> Result<char, String> {
                    let code: String = (0..digits).filter_map(|_| chars.next().map(|(_, c)| c)).collect();
                    u32::from_str_radix(&code, 16).ok().filter(|_| code.len() == digits).and_then(char::from_u32).ok_or_else(|| "has an invalid escape".to_owned())
                };
                out.push(match escape {
                    '\\' => '\\',
                    '"' => '"',
                    '/' => '/',
                    ' ' => ' ',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '0' => '\0',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'e' => '\u{1b}',
                    'a' => '\u{7}',
                    'v' => '\u{b}',
                    'x' => hex(&mut chars, 2)?,
                    'u' => hex(&mut chars, 4)?,
                    'U' => hex(&mut chars, 8)?,
                    _ => return Err("has an invalid escape".into()),
                });
            }
            _ => out.push(c),
        }
    }
    Err("has no closing quote".into())
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    use super::*;
    use crate::keyring::key_id;

    /// `release/latest.yml` of 3.9.1, byte for byte, as electron-builder 26.15.3 wrote it.
    const LATEST_3_9_1: &str = "version: 3.9.1\nfiles:\n  - url: Rainy-3.9.1-windows-x64-setup.exe\n    sha512: L8s/JYn7p3o+1bvmDfpMqsqwNIDRrlOa7mCBrhM2bBGEFQgoeXXue3C2DyO2iQU7d/g01824G/H7kagnYvfwKg==\n    size: 180169836\npath: Rainy-3.9.1-windows-x64-setup.exe\nsha512: L8s/JYn7p3o+1bvmDfpMqsqwNIDRrlOa7mCBrhM2bBGEFQgoeXXue3C2DyO2iQU7d/g01824G/H7kagnYvfwKg==\nreleaseDate: '2026-10-21T23:56:21.008Z'\n";
    const SHA512_3_9_1: &str = "L8s/JYn7p3o+1bvmDfpMqsqwNIDRrlOa7mCBrhM2bBGEFQgoeXXue3C2DyO2iQU7d/g01824G/H7kagnYvfwKg==";

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    /// A release key made for one test.
    struct Signer(Ed25519KeyPair);

    impl Signer {
        fn new() -> Self {
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
            Self(Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap())
        }

        fn keyring(&self) -> Keyring {
            let public: [u8; 32] = self.0.public_key().as_ref().try_into().unwrap();
            Keyring::from_keys(&[(key_id(&public).as_str(), public)])
        }

        /// `latest.yml.sig` for `feed`, as the release script writes it: base64url without padding and a newline.
        fn sign(&self, domain: &[u8], feed: &[u8]) -> Vec<u8> {
            format!("{}\n", URL_SAFE_NO_PAD.encode(self.0.sign(&[domain, feed].concat()).as_ref())).into_bytes()
        }
    }

    #[test]
    fn reads_the_latest_yml_of_3_9_1() {
        let feed = parse_feed(LATEST_3_9_1).unwrap();
        assert_eq!(feed.version, version("3.9.1"));
        assert_eq!(
            feed.files,
            [FeedFile { url: "Rainy-3.9.1-windows-x64-setup.exe".into(), sha512: SHA512_3_9_1.into(), size: Some(180_169_836), block_map_size: None }]
        );
        assert_eq!(feed.path.as_deref(), Some("Rainy-3.9.1-windows-x64-setup.exe"));
        assert_eq!(feed.sha512.as_deref(), Some(SHA512_3_9_1));
        assert_eq!(feed.release_date.as_deref(), Some("2026-10-21T23:56:21.008Z"));
        assert_eq!(feed.minimum_system_version, None);
        let installer = feed.installer(&Source::github()).unwrap();
        assert_eq!(installer.url, "https://github.com/RainyMarks/Rainy/releases/download/v3.9.1/Rainy-3.9.1-windows-x64-setup.exe");
        assert_eq!(installer.file, "Rainy-3.9.1-windows-x64-setup.exe");
        assert_eq!(installer.expected, Expected { bytes: 180_169_836, digest: Digest::sha512_base64(SHA512_3_9_1).unwrap() });
    }

    /// The one electron-builder wrote last in the main checkout, when it is there.
    #[test]
    fn reads_the_latest_yml_of_the_main_checkout() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../RainyAgent/release/latest.yml");
        let Ok(text) = fs::read_to_string(&path) else { return };
        let feed = parse_feed(&text).unwrap();
        let installer = feed.installer(&Source::github()).unwrap();
        assert!(installer.file.starts_with(&format!("Rainy-{}-", feed.version)), "{}", installer.file);
        assert_eq!(feed.path.as_deref(), Some(installer.file.as_str()));
    }

    #[test]
    fn tolerates_quoting_comments_and_unknown_keys() {
        let text = "\u{feff}---\r\n# written by hand\r\nversion: \"4.0.1\"   # the release\r\nreleaseName: 'Rainy 4.0.1'\r\nreleaseNotes: |\r\n  - fixed: things\r\n  version: 9.9.9\r\nstagingPercentage: 50\r\nfiles:\r\n- url: 'Rainy-4.0.1-windows-arm64-setup.exe'\r\n  sha512: \"AAAA\"\r\n  size: 1\r\n-\r\n  url: \"Rainy 4.0.1 windows x64 setup.exe\"\r\n  sha512: 'L8s/JYn7p3o+1bvmDfpMqsqwNIDRrlOa7mCBrhM2bBGEFQgoeXXue3C2DyO2iQU7d/g01824G/H7kagnYvfwKg=='\r\n  size: '42'\r\n  blockMapSize: 7\r\n  isAdminRightsRequired: false\r\n  extra:\r\n    nested: [1, 2]\r\n'path': Rainy-4.0.1-windows-x64-setup.exe\r\nminimumSystemVersion: 10.0.19041\r\nreleaseDate: \"2026-11-01T00:00:00.000Z\"\r\n...\r\nignored: after the end\r\n";
        let feed = parse_feed(text).unwrap();
        assert_eq!(feed.version, version("4.0.1"));
        assert_eq!(feed.files.len(), 2);
        assert_eq!(feed.files[1], FeedFile { url: "Rainy 4.0.1 windows x64 setup.exe".into(), sha512: SHA512_3_9_1.into(), size: Some(42), block_map_size: Some(7) });
        assert_eq!(feed.path.as_deref(), Some("Rainy-4.0.1-windows-x64-setup.exe"));
        assert_eq!(feed.minimum_system_version.as_deref(), Some("10.0.19041"));
        assert_eq!(feed.release_date.as_deref(), Some("2026-11-01T00:00:00.000Z"));
        // The x64 installer, named as electron-updater requests it.
        let installer = feed.installer(&Source::at("http://127.0.0.1:9/feed/latest.yml?x=1")).unwrap();
        assert_eq!(installer.file, "Rainy-4.0.1-windows-x64-setup.exe");
        assert_eq!(installer.url, "http://127.0.0.1:9/feed/Rainy-4.0.1-windows-x64-setup.exe");
        assert_eq!(installer.expected.bytes, 42);
        // Escapes in double quotes, and a `v` as in a tag.
        let feed = parse_feed("version: v4.2.0\nfiles:\n  - url: \"R\\x61iny-\\u0034.exe\"\n    sha512: x\n").unwrap();
        assert_eq!((feed.version, feed.files[0].url.as_str()), (version("4.2.0"), "Rainy-4.exe"));
        assert!(parse_feed("version: '4.0.0'\nnote: 'it''s'\n").unwrap().files.is_empty());
    }

    #[test]
    fn refuses_web_installers() {
        let text = format!("{LATEST_3_9_1}packages:\n  x64:\n    path: Rainy-3.9.1-x64.nsis.7z\n");
        assert_eq!(parse_feed(&text), Err(UpdateError::WebInstaller));
        assert_eq!(parse_feed("packages: {}\n"), Err(UpdateError::WebInstaller));
    }

    #[test]
    fn refuses_what_electron_builder_does_not_write() {
        let refused = |text: &str| assert!(matches!(parse_feed(text), Err(UpdateError::Invalid(_))), "{text:?} → {:?}", parse_feed(text));
        refused("");
        refused("files:\n  - url: a.exe\n    sha512: x\n");
        refused("version: 4.0\n");
        refused("version: 4.0.0\nversion: 4.0.1\n");
        refused("version: '4.0.0\n");
        refused("version: \"4.0.0\" trailing\n");
        refused("version: |\n  4.0.0\n");
        refused("version: [4, 0, 0]\n");
        refused("version: 4.0.0\nfiles: []\n");
        refused("version: 4.0.0\nfiles:\n  url: a.exe\n");
        refused("version: 4.0.0\nfiles:\n  - url: a.exe\n");
        refused("version: 4.0.0\nfiles:\n  - url: a.exe\n    sha512: x\n    size: -1\n");
        refused("version: 4.0.0\nfiles:\n  - url: a.exe\n    sha512: x\n    url: b.exe\n");
        refused("version: 4.0.0\nfiles:\n  - - url: a.exe\n");
        refused("version: 4.0.0\n\tfiles:\n");
        refused("version: 4.0.0\njust text\n");
        refused("  version: 4.0.0\nfiles:\n");
        refused("version: 4.0.0\n---\nversion: 4.0.1\n");
        let installer = |files: &str| parse_feed(&format!("version: 4.0.1\nfiles:\n{files}")).unwrap().installer(&Source::github());
        let good = format!("    sha512: {SHA512_3_9_1}\n    size: 5\n");
        assert!(installer(&format!("  - url: Rainy-4.0.1-windows-x64-setup.exe\n{good}")).is_ok());
        for bad in [
            format!("  - url: Rainy.zip\n{good}"),
            format!("  - url: ../Rainy.exe\n{good}"),
            format!("  - url: dir/Rainy.exe\n{good}"),
            format!("  - url: https://example.com/Rainy.exe\n{good}"),
            format!("  - url: CON.exe\n{good}"),
            "  - url: Rainy.exe\n    sha512: AAAA\n    size: 5\n".into(),
            format!("  - url: Rainy.exe\n    sha512: {SHA512_3_9_1}\n"),
            format!("  - url: Rainy.exe\n    sha512: {SHA512_3_9_1}\n    size: 0\n"),
            format!("  - url: Rainy.exe\n    sha512: {SHA512_3_9_1}\n    size: 3000000000\n"),
        ] {
            assert!(matches!(installer(&bad), Err(UpdateError::Invalid(_))), "{bad}");
        }
    }

    #[test]
    fn offers_only_newer_stable_versions() {
        let current = version("4.0.0");
        assert!(offered(&version("4.0.1"), &current));
        assert!(offered(&version("5.0.0"), &current));
        assert!(!offered(&version("4.0.0"), &current));
        assert!(!offered(&version("3.9.1"), &current));
        assert!(!offered(&version("4.1.0-beta.1"), &current));
        // Build metadata does not make a version newer.
        assert!(!offered(&version("4.0.0+20261101"), &current));
        assert!(offered(&version("4.1.0"), &version("4.1.0-beta.2")));
        assert_eq!(parse_version("v4.0.1"), Some(version("4.0.1")));
        assert_eq!(parse_version("4.0"), None);
        assert!(supported(None, Some(&version("10.0.19045"))));
        assert!(supported(Some("10.0.19041"), Some(&version("10.0.19045"))));
        assert!(supported(Some("10.0.19045"), Some(&version("10.0.19045"))));
        assert!(!supported(Some("10.0.22000"), Some(&version("10.0.19045"))));
        assert!(supported(Some("not a version"), Some(&version("10.0.19045"))));
        assert!(supported(Some("10.0.22000"), None));
        let windows = os_version();
        assert!(cfg!(not(windows)) || windows.is_some_and(|windows| windows.major >= 10));
    }

    #[test]
    fn checks_signatures_of_latest_yml() {
        let signer = Signer::new();
        let keyring = signer.keyring();
        let feed = LATEST_3_9_1.as_bytes();
        let signature = signer.sign(SIGNATURE_DOMAIN, feed);
        assert!(signature_valid(&keyring, feed, &signature));
        // Padding and surrounding spaces are tolerated.
        let padded = format!("  {}==\r\n", String::from_utf8_lossy(&signature).trim());
        assert!(signature_valid(&keyring, feed, padded.as_bytes()));
        // Another file, another domain, another key, or no signature at all.
        let mut changed = feed.to_vec();
        changed[9] = b'2';
        assert!(!signature_valid(&keyring, &changed, &signature));
        assert!(!signature_valid(&keyring, feed, &signer.sign(trust::DOMAIN, feed)));
        assert!(!signature_valid(&Signer::new().keyring(), feed, &signature));
        assert!(!signature_valid(&keyring, feed, b""));
        assert!(!signature_valid(&keyring, feed, b"not base64!"));
        assert!(!signature_valid(&keyring, feed, URL_SAFE_NO_PAD.encode([0u8; 64]).as_bytes()));
        assert!(!signature_valid(&keyring, feed, STANDARD.encode(&signature[..signature.len() - 1]).as_bytes()));
    }

    #[test]
    fn finds_the_files_of_a_release() {
        let github = Source::github();
        assert!(!github.overridden() && !github.allow_loopback_http);
        assert_eq!(github.file_url(&version("4.0.1"), SIGNATURE_FILE), "https://github.com/RainyMarks/Rainy/releases/download/v4.0.1/latest.yml.sig");
        let test = Source::at("http://127.0.0.1:8080/qa/feed/latest.yml#top");
        assert!(test.overridden() && test.allow_loopback_http);
        assert_eq!(test.file_url(&version("4.0.1"), SIGNATURE_FILE), "http://127.0.0.1:8080/qa/feed/latest.yml.sig");
        assert!(test.options(Arc::new(AtomicBool::new(false))).allow_loopback_http);
        assert!(!github.options(Arc::new(AtomicBool::new(false))).allow_loopback_http);
    }

    #[test]
    fn uses_the_updater_cache_of_3x_only_for_github() {
        assert_eq!(cache_dir_from(None, None, Some("C:/Users/me/AppData/Local".into())), Some(PathBuf::from("C:/Users/me/AppData/Local/rainy-agent-updater/pending")));
        assert_eq!(cache_dir_from(Some("".into()), None, Some("C:/L".into())), Some(PathBuf::from("C:/L/rainy-agent-updater/pending")));
        assert_eq!(cache_dir_from(Some("C:/qa/cache".into()), None, Some("C:/L".into())), Some(PathBuf::from("C:/qa/cache")));
        assert_eq!(cache_dir_from(None, None, None), None);
        // A test feed keeps its installers in the user data folder unless a cache is named.
        let test = Some(PathBuf::from("C:/ud/update-cache"));
        assert_eq!(cache_dir_from(None, test.clone(), Some("C:/L".into())), test);
        assert_eq!(cache_dir_from(Some("C:/qa/cache".into()), test, Some("C:/L".into())), Some(PathBuf::from("C:/qa/cache")));
    }

    #[test]
    fn starts_the_installer_as_3x_did() {
        let line = command_line(Path::new("C:\\Users\\me\\AppData\\Local\\rainy-agent-updater\\pending\\Rainy-4.0.1-windows-x64-setup.exe"));
        assert_eq!(line, "\"C:\\Users\\me\\AppData\\Local\\rainy-agent-updater\\pending\\Rainy-4.0.1-windows-x64-setup.exe\" --updated /S --force-run");
    }

    /// Record `file` of `release` as this updater does, with a placeholder digest.
    fn record(cache: &Path, release: &str, file: &str) {
        let record = Record { version: 1, release: release.into(), file: file.into(), sha512: SHA512_3_9_1.into(), size: 3 };
        fs::write(cache.join(RECORD_FILE), serde_json::to_string(&record).unwrap()).unwrap();
    }

    #[test]
    fn removes_only_what_this_updater_left() {
        let keep = "Rainy-4.0.2-windows-x64-setup.exe";
        for name in ["Rainy-4.0.1-windows-x64-setup.exe.partial", "RAINY-4.0.0-windows-x64-setup.exe.partial"] {
            assert!(unfinished(name, keep), "{name}");
        }
        for name in [
            "rainy-4.0.2-windows-x64-setup.exe.partial",
            "Rainy-4.0.1-windows-x64-setup.exe",
            "temp-Rainy-4.0.1-windows-x64-setup.exe",
            "temp-Rainy-4.0.1-windows-x64-setup.exe.partial",
            "Other-setup.exe.partial",
            "Rainy-notes.txt.partial",
            RECORD_FILE,
        ] {
            assert!(!unfinished(name, keep), "{name}");
        }

        let cache = tempfile::tempdir().unwrap();
        let file = |name: &str| cache.path().join(name);
        // electron-updater's: its installers, the one being downloaded, its record and blockmap.
        let electron_updater = ["RainyAgent-2.1.1-windows-x64-setup.exe", "Rainy-3.8.0-windows-x64-setup.exe", "temp-Rainy-4.0.2-windows-x64-setup.exe", "current.blockmap"];
        for name in electron_updater.into_iter().chain(["Rainy-4.0.1-windows-x64-setup.exe", "Rainy-4.0.0-windows-x64-setup.exe.partial", keep]) {
            fs::write(file(name), b"x").unwrap();
        }
        fs::write(file(ELECTRON_UPDATER_RECORD), r#"{"fileName":"RainyAgent-2.1.1-windows-x64-setup.exe","sha512":"x","isAdminRightsRequired":false}"#).unwrap();
        record(cache.path(), "4.0.1", "Rainy-4.0.1-windows-x64-setup.exe");
        assert_eq!(recorded_file(cache.path()).as_deref(), Some("Rainy-4.0.1-windows-x64-setup.exe"));
        remove_earlier(cache.path(), recorded_file(cache.path()).as_deref(), keep);
        assert!(!file("Rainy-4.0.1-windows-x64-setup.exe").exists());
        assert!(!file("Rainy-4.0.0-windows-x64-setup.exe.partial").exists());
        for name in electron_updater.into_iter().chain([keep, ELECTRON_UPDATER_RECORD]) {
            assert!(file(name).exists(), "{name}");
        }
        // The file electron-updater prepared is kept even when this updater recorded the same name.
        record(cache.path(), "2.1.1", "RainyAgent-2.1.1-windows-x64-setup.exe");
        remove_earlier(cache.path(), recorded_file(cache.path()).as_deref(), keep);
        assert!(file("RainyAgent-2.1.1-windows-x64-setup.exe").exists());
        assert_eq!(pending(cache.path(), &version("4.0.0"), &AtomicBool::new(false)), None);
        assert!(file("RainyAgent-2.1.1-windows-x64-setup.exe").exists());
        assert!(!file(RECORD_FILE).exists());
        // Without a valid record nothing is recorded to remove.
        fs::write(file(RECORD_FILE), "{}").unwrap();
        assert_eq!(recorded_file(cache.path()), None);
    }

    #[cfg(windows)]
    mod network {
        use ::core::prelude::v1::test;
        use std::sync::Mutex;

        use sha2::{Digest as _, Sha512};

        use super::super::*;
        use super::{LATEST_3_9_1, Signer, version};
        use crate::test_server::{Reply, Server};

        const SETUP: &str = "Rainy-4.0.1-windows-x64-setup.exe";

        fn installer_bytes() -> Vec<u8> {
            (0..300_000).map(|index| (index * 13 % 251) as u8).collect()
        }

        fn latest_yml(release: &str, setup: &[u8]) -> String {
            let sha512 = STANDARD.encode(Sha512::digest(setup));
            format!(
                "version: {release}\nfiles:\n  - url: {SETUP}\n    sha512: {sha512}\n    size: {}\npath: {SETUP}\nsha512: {sha512}\nreleaseDate: '2026-11-01T00:00:00.000Z'\n",
                setup.len()
            )
        }

        /// A release folder: `latest.yml`, its signature when given, and the installer.
        fn serve(feed: String, signature: Option<Vec<u8>>, setup: Vec<u8>) -> Server {
            Server::start(move |seen| match seen.path.as_str() {
                "/feed/latest.yml" => Reply::ok(feed.clone().into_bytes()),
                "/feed/latest.yml.sig" => signature.clone().map_or_else(|| Reply::status(404), Reply::ok),
                path if path == format!("/feed/{SETUP}") => Reply::ok(setup.clone()),
                _ => Reply::status(404),
            })
        }

        fn options() -> Options {
            Source::at("http://127.0.0.1/").options(Arc::new(AtomicBool::new(false)))
        }

        fn paths(server: &Server) -> Vec<String> {
            server.seen().into_iter().map(|seen| seen.path).collect()
        }

        /// The whole flow as Rainy runs it, with the feed and the cache named by the environment as QA names them.
        #[test]
        fn checks_downloads_and_offers_a_signed_release() {
            let setup = installer_bytes();
            let feed = latest_yml("4.0.1", &setup);
            let signer = Signer::new();
            let server = serve(feed.clone(), Some(signer.sign(SIGNATURE_DOMAIN, feed.as_bytes())), setup.clone());
            let (cache, user_data) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            fs::write(cache.path().join("Rainy-3.9.1-windows-x64-setup.exe"), b"old").unwrap();
            fs::write(cache.path().join("update-info.json"), b"{}").unwrap();
            fs::write(cache.path().join("Rainy-4.0.0-windows-x64-setup.exe"), b"recorded").unwrap();
            super::record(cache.path(), "4.0.0", "Rainy-4.0.0-windows-x64-setup.exe");
            // SAFETY: Windows serializes access to the environment, and no other test reads these two variables.
            unsafe {
                std::env::set_var(FEED_VARIABLE, server.url("/feed/latest.yml"));
                std::env::set_var(CACHE_VARIABLE, cache.path());
            }
            let (source, folder) = (Source::from_env(), cache_dir(&Source::from_env(), user_data.path()).unwrap());
            // SAFETY: as above.
            unsafe { std::env::remove_var(CACHE_VARIABLE) };
            let unnamed = cache_dir(&source, user_data.path());
            // SAFETY: as above.
            unsafe { std::env::remove_var(FEED_VARIABLE) };
            assert_eq!(folder, cache.path());
            assert!(source.overridden());
            // Without a named cache, a test feed never uses 3.x's folder.
            assert_eq!(unnamed, Some(user_data.path().join("update-cache")));
            assert_eq!(cache_dir(&Source::from_env(), user_data.path()), cache_dir_from(None, None, std::env::var_os("LOCALAPPDATA")));
            let current = version("4.0.0");
            let options = source.options(Arc::new(AtomicBool::new(false)));
            let installer = check(&source, &current, Some(&signer.keyring()), &options).unwrap().unwrap();
            assert_eq!(installer.version, version("4.0.1"));
            assert_eq!(installer.url, server.url(&format!("/feed/{SETUP}")));
            let seen = Mutex::new(Vec::new());
            let prepared = download(&installer, &folder, &options, &|done, total| seen.lock().unwrap().push((done, total))).unwrap();
            assert_eq!(prepared.path, folder.join(SETUP));
            assert_eq!(fs::read(&prepared.path).unwrap(), setup);
            assert_eq!(seen.lock().unwrap().last(), Some(&(300_000, 300_000)));
            assert_eq!(paths(&server), ["/feed/latest.yml", "/feed/latest.yml.sig", format!("/feed/{SETUP}").as_str()]);
            // The installer recorded before is gone; what electron-updater keeps beside it stays.
            assert!(!folder.join("Rainy-4.0.0-windows-x64-setup.exe").exists());
            assert!(folder.join("Rainy-3.9.1-windows-x64-setup.exe").exists());
            assert!(folder.join("update-info.json").exists());
            assert!(prepared.verify(&AtomicBool::new(false)).unwrap());

            // A later start offers it without the network, and a second download reuses it.
            assert_eq!(pending(&folder, &current, &AtomicBool::new(false)), Some(prepared.clone()));
            assert_eq!(download(&installer, &folder, &options, &|_, _| {}).unwrap(), prepared);
            assert_eq!(server.seen().len(), 3);
            // A changed file is no longer offered, nor accepted before installing.
            fs::write(&prepared.path, b"changed").unwrap();
            assert_eq!(pending(&folder, &current, &AtomicBool::new(false)), None);
            assert!(!prepared.verify(&AtomicBool::new(false)).unwrap());
            download(&installer, &folder, &options, &|_, _| {}).unwrap();
            assert_eq!(fs::read(&prepared.path).unwrap(), setup);
            // Once 4.0.1 runs, its installer and the record are deleted.
            assert_eq!(pending(&folder, &version("4.0.1"), &AtomicBool::new(false)), None);
            assert!(!prepared.path.exists());
            assert!(!folder.join(RECORD_FILE).exists());
        }

        #[test]
        fn refuses_a_release_with_an_invalid_signature() {
            let setup = installer_bytes();
            let feed = latest_yml("4.0.1", &setup);
            let signer = Signer::new();
            // Signed by another key, and the signature of another file.
            for signature in [Signer::new().sign(SIGNATURE_DOMAIN, feed.as_bytes()), signer.sign(SIGNATURE_DOMAIN, LATEST_3_9_1.as_bytes()), b"garbage".to_vec()] {
                let server = serve(feed.clone(), Some(signature), setup.clone());
                let source = Source::at(&server.url("/feed/latest.yml"));
                assert_eq!(check(&source, &version("4.0.0"), Some(&signer.keyring()), &options()), Err(UpdateError::Signature));
                assert_eq!(paths(&server), ["/feed/latest.yml", "/feed/latest.yml.sig"]);
            }
        }

        #[test]
        fn refuses_an_unsigned_release() {
            // Leaving the signature out must not be a way around it.
            let setup = installer_bytes();
            let server = serve(latest_yml("4.0.1", &setup), None, setup.clone());
            let source = Source::at(&server.url("/feed/latest.yml"));
            assert_eq!(check(&source, &version("4.0.0"), Some(&Signer::new().keyring()), &options()), Err(UpdateError::Signature));
            assert_eq!(paths(&server), ["/feed/latest.yml", "/feed/latest.yml.sig"]);
            // A build without keys does not ask for the signature.
            let server = serve(latest_yml("4.0.1", &setup), Some(b"garbage".to_vec()), setup.clone());
            assert!(check(&Source::at(&server.url("/feed/latest.yml")), &version("4.0.0"), None, &options()).unwrap().is_some());
            assert_eq!(paths(&server), ["/feed/latest.yml"]);
        }

        #[test]
        fn offers_nothing_to_a_current_version() {
            let setup = installer_bytes();
            let server = serve(latest_yml("4.0.1", &setup), None, setup.clone());
            let source = Source::at(&server.url("/feed/latest.yml"));
            for current in ["4.0.1", "4.1.0", "4.0.1+local"] {
                assert_eq!(check(&source, &version(current), None, &options()), Ok(None));
            }
            let server = serve(latest_yml("4.1.0-beta.1", &setup), None, setup);
            assert_eq!(check(&Source::at(&server.url("/feed/latest.yml")), &version("4.0.0"), None, &options()), Ok(None));
        }

        #[test]
        fn reports_failures() {
            let setup = installer_bytes();
            let mut wrong = setup.clone();
            wrong[0] ^= 1;
            let server = serve(latest_yml("4.0.1", &setup), None, wrong);
            let source = Source::at(&server.url("/feed/latest.yml"));
            let installer = check(&source, &version("4.0.0"), None, &options()).unwrap().unwrap();
            let cache = tempfile::tempdir().unwrap();
            assert_eq!(download(&installer, cache.path(), &options(), &|_, _| {}), Err(UpdateError::Download(DownloadError::Mismatch)));
            assert!(!cache.path().join(SETUP).exists());
            assert_eq!(pending(cache.path(), &version("4.0.0"), &AtomicBool::new(false)), None);
            let server = serve(format!("{LATEST_3_9_1}packages:\n  x64: {{}}\n"), None, Vec::new());
            assert_eq!(check(&Source::at(&server.url("/feed/latest.yml")), &version("3.0.0"), None, &options()), Err(UpdateError::WebInstaller));
            assert_eq!(check(&Source::at(&server.url("/missing/latest.yml")), &version("3.0.0"), None, &options()), Err(UpdateError::Download(DownloadError::Status(404))));
            // Plain HTTP only to this computer and only for a named feed.
            let github_like = Source { feed: server.url("/feed/latest.yml"), ..Source::github() };
            assert!(matches!(check(&github_like, &version("3.0.0"), None, &github_like.options(Arc::new(AtomicBool::new(false)))), Err(UpdateError::Download(DownloadError::Insecure(_)))));
        }
    }
}
