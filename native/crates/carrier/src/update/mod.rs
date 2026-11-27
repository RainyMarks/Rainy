//! Rainy's updates (3.x used electron-updater 6.8.9 through `updates.ts`): `latest.yml` of the latest GitHub release,
//! a strictly newer stable version, its installer downloaded into the updater cache and verified by size and SHA-512,
//! `latest.yml` itself by the release key (a build with keys refuses a release without its signature), then the
//! installer started as 3.x started it.
//! Calls that reach the network or hash files block; run them on a background thread.

mod feed_yaml;
mod win32;
#[cfg(test)]
mod tests;

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
use crate::fs_util;
use crate::keyring::Keyring;
use crate::paths;
use crate::text::quoted_executable;
use crate::trust;

pub use self::feed_yaml::parse_feed;

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
/// An update installed as Rainy quits: silently, without starting Rainy again.
pub const QUIT_ARGUMENTS: [&str; 2] = ["--updated", "/S"];
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

pub(super) fn invalid(detail: impl Into<String>) -> UpdateError {
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
    if !supported(feed.minimum_system_version.as_deref(), win32::os_version().as_ref()) {
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
    let text = serde_json::to_string(&record).map_err(|error| invalid(error.to_string()))? + "\n";
    fs_util::write_atomic(&path, text.as_bytes(), false).map_err(|error| io_error(&path, error))
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
/// Rainy installers (`.partial` and parallel `.partial.<n>` pieces are this updater's; electron-updater downloads into
/// `temp-<file>`). What electron-updater keeps in the same folder stays.
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

/// An unfinished download (`.partial` or parallel piece `.partial.<index>`) of a Rainy installer other than `keep`.
fn unfinished(name: &str, keep: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let keep = keep.to_ascii_lowercase();
    download::strip_partial_suffix(&lower).is_some_and(|base| base != keep && base.starts_with("rainy") && base.ends_with(".exe"))
}

/// Delete the regular file `name` from the cache, unless electron-updater's record names it.
fn remove_installer(cache: &Path, name: &str, what: &str) {
    if electron_updater_file(cache).is_some_and(|file| file.eq_ignore_ascii_case(name)) {
        log::info!("Update: {what} is kept, electron-updater's {ELECTRON_UPDATER_RECORD} names it");
        return;
    }
    let path = cache.join(name);
    if !matches!(fs::symlink_metadata(&path), Ok(metadata) if metadata.is_file() && !fs_util::is_reparse_point(&metadata)) {
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
    command_line_with(installer, &INSTALLER_ARGUMENTS)
}

/// The installer's command line with `arguments`.
pub fn command_line_with(installer: &Path, arguments: &[&str]) -> OsString {
    let mut line = OsString::from("\"");
    line.push(installer.as_os_str());
    line.push("\"");
    for argument in arguments {
        line.push(" ");
        line.push(argument);
    }
    line
}

/// Whether this Rainy installation is per-user (HKCU) or per-machine (HKLM, requiring UAC elevation to update).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallScope {
    PerUser,
    PerMachine,
}

/// Extract the folder of the quoted program in an NSIS `UninstallString` (mirrors `rainyQuotedFolder` in `rainy.nsi`).
pub(super) fn quoted_folder(uninstall_string: &str) -> Option<String> {
    let exe = quoted_executable(uninstall_string)?;
    let parent = Path::new(exe).parent()?;
    let text = parent.to_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn normalize_install_path(path: &str) -> String {
    let stripped = paths::without_verbatim(PathBuf::from(path.trim()));
    let normalized = stripped.to_string_lossy().replace('/', "\\");
    let trimmed = normalized.trim_end_matches('\\');
    if trimmed.len() == 2 && trimmed.as_bytes()[1] == b':' {
        format!("{trimmed}\\").to_ascii_lowercase()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

/// Decide whether `rainy.nsi` launched with [`QUIT_ARGUMENTS`] (`--updated /S`) would enter per-machine mode
/// (`$installMode == "all"`) and trigger a UAC prompt via `rainyElevate`.
pub fn install_scope_from(user_root: Option<&str>, machine_root: Option<&str>, current_install_dir: Option<&Path>) -> InstallScope {
    let user = user_root.map(str::trim).filter(|value| !value.is_empty());
    let machine = machine_root.map(str::trim).filter(|value| !value.is_empty());
    let Some(machine) = machine else {
        return InstallScope::PerUser;
    };
    if user.is_none() {
        return InstallScope::PerMachine;
    }
    if let Some(current) = current_install_dir.and_then(|dir| dir.to_str())
        && normalize_install_path(current) == normalize_install_path(machine)
    {
        return InstallScope::PerMachine;
    }
    InstallScope::PerUser
}

/// Detect whether Rainy is installed per-machine (HKLM) or per-user (HKCU).
pub fn install_scope() -> InstallScope {
    win32::install_scope()
}

/// Whether this installation is per-machine (HKLM) and would require UAC elevation during silent update.
pub fn is_per_machine_install() -> bool {
    install_scope() == InstallScope::PerMachine
}

/// Pre-launch safety check: the installer must be an existing non-empty regular `.exe` file with no reparse point.
fn validate_installer_path(installer: &Path) -> io::Result<()> {
    if !installer.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("exe")) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("refusing to launch non-executable installer: {}", installer.display())));
    }
    download::check_path(installer).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let metadata = fs::symlink_metadata(installer)?;
    if !metadata.is_file() || fs_util::is_reparse_point(&metadata) || metadata.len() == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("installer is missing, empty, or not a regular file: {}", installer.display())));
    }
    Ok(())
}

/// Start the installer detached from Rainy, in its own process group, with no handles of Rainy's and with its own
/// folder as the working folder, so nothing it starts keeps the installation folder in use; the process id. An
/// installer that needs elevation is started through the shell, which asks for it.
pub fn launch(installer: &Path) -> io::Result<u32> {
    win32::launch(installer)
}

/// Install as Rainy quits: silently, Rainy not started again, and never through an elevation prompt the user would
/// meet after closing the window; a per-machine (HKLM) installation or an installer that needs elevation is skipped
/// and left for the next interactive offer.
pub fn launch_on_quit(installer: &Path) -> io::Result<u32> {
    win32::launch_on_quit(installer)
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
