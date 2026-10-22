//! Whether the installed resources are the ones Rainy was released with (`release-integrity.ts` in 3.x). The build signs
//! a list of every resource file (`resources\release-manifest.signed.json`); the first start of each installed version
//! checks every file against it and records that in `<userData>\release-verified.json`, and a daily recheck compares the
//! files' metadata with what that check saw.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::keyring::Keyring;
use crate::paths::canonical;

/// The signed list, in the resources folder.
pub const MANIFEST_FILE: &str = "release-manifest.signed.json";
/// The record of the last full check, in the user-data folder.
pub const STAMP_FILE: &str = "release-verified.json";
/// Prefixed to the list before signing, so no other signature by the release key can pass for one.
pub const DOMAIN: &[u8] = b"RainyAgent/release-manifest/v1\0";
/// How long a check is trusted before the recheck looks at the files again.
pub const RECHECK_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;

/// 3.x writes version 2 and treats any other version as no record, so going back to 3.x checks everything again.
const STAMP_VERSION: u64 = 3;
const WORKERS: usize = 8;
const CHUNK: usize = 1024 * 1024;
const MAX_PAYLOAD: usize = 32 * 1024 * 1024;
const MAX_SIGNATURE: usize = 128;
const MAX_FILES: usize = 100_000;
/// Windows file times count 100 ns from 1601; Node's `mtimeNs` counts nanoseconds from 1970.
const UNIX_EPOCH_FILETIME: i128 = 116_444_736_000_000_000;

/// Why the installed resources were not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustError {
    /// The signed list is not the JSON a release build writes; the detail says how.
    Invalid(String),
    /// The list or its signature is not canonical base64url, or the signature is not 64 bytes.
    Encoding,
    /// No release key has the list's key id, or the signature does not match.
    Signature,
    DuplicatePaths,
    /// A listed path leads outside the resources folder.
    Outside,
    /// These carry the listed path.
    Missing(String),
    Damaged(String),
    Replaced(String),
    Mismatch(String),
    /// A file could not be read, or the record not written: the path and the system's error.
    Io(String),
}

impl TrustError {
    /// A file differs from the list (3.x `ReleaseIntegrityError`): the installation was changed or damaged.
    pub fn is_integrity(&self) -> bool {
        matches!(self, Self::Outside | Self::Missing(_) | Self::Damaged(_) | Self::Replaced(_) | Self::Mismatch(_))
    }
}

impl fmt::Display for TrustError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(detail) => write!(f, "The Rainy file list is not valid: {detail}"),
            Self::Encoding => f.write_str("The Rainy file list is not encoded correctly."),
            Self::Signature => f.write_str("The Rainy file list has an invalid signature."),
            Self::DuplicatePaths => f.write_str("The file list contains duplicate paths."),
            Self::Outside => f.write_str("A Rainy file path points outside the installation."),
            Self::Missing(path) => write!(f, "A Rainy file is missing: {path}"),
            Self::Damaged(path) => write!(f, "A Rainy file is damaged: {path}"),
            Self::Replaced(path) => write!(f, "The folder of a Rainy file was replaced: {path}"),
            Self::Mismatch(path) => write!(f, "A Rainy file failed verification: {path}"),
            Self::Io(detail) => write!(f, "A Rainy file could not be read: {detail}"),
        }
    }
}

impl std::error::Error for TrustError {}

/// One listed file: its path below the resources folder (`/`-separated), SHA-256 and size.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestFile {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

/// A signed list whose signature and shape were checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub build_version: String,
    pub created_at: String,
    pub key_id: String,
    pub files: Vec<ManifestFile>,
}

/// The list as signed: `{version, product, buildVersion, createdAt, keyId, files}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Payload {
    version: u64,
    product: String,
    build_version: String,
    created_at: String,
    key_id: String,
    files: Vec<ManifestFile>,
}

/// The signed file: `{version, payload, signature}`, the last two base64url without padding.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u64,
    payload: String,
    signature: String,
}

/// An authenticated list, with the SHA-256 of its payload text, which the record names it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authenticated {
    pub manifest: Manifest,
    pub manifest_sha256: String,
}

/// Check the text of a signed list as strictly as `authenticateReleaseManifest`: the envelope, canonical base64url, the
/// list's shape, then its Ed25519 signature over [`DOMAIN`] and the list's bytes by the key it names.
pub fn authenticate(text: &str, keyring: &Keyring) -> Result<Authenticated, TrustError> {
    let envelope: Envelope = serde_json::from_str(text).map_err(|error| TrustError::Invalid(error.to_string()))?;
    if envelope.version != 1 {
        return Err(TrustError::Invalid("the signed file is not version 1".into()));
    }
    if js_length(&envelope.payload) > MAX_PAYLOAD || js_length(&envelope.signature) > MAX_SIGNATURE {
        return Err(TrustError::Invalid("the signed file is too long".into()));
    }
    let bytes = canonical_base64(&envelope.payload).ok_or(TrustError::Encoding)?;
    let signature = canonical_base64(&envelope.signature).filter(|signature| signature.len() == 64).ok_or(TrustError::Encoding)?;
    let payload: Payload = serde_json::from_slice(&bytes).map_err(|error| TrustError::Invalid(error.to_string()))?;
    let manifest = checked(payload)?;
    let key = keyring.get(&manifest.key_id).ok_or(TrustError::Signature)?;
    if !verifies(key, DOMAIN, &bytes, &signature) {
        return Err(TrustError::Signature);
    }
    Ok(Authenticated { manifest, manifest_sha256: hex(&Sha256::digest(envelope.payload.as_bytes())) })
}

/// Whether `signature` is an Ed25519 signature by one of the release keys over `domain` followed by `message`: for
/// other files the release key signs, each under its own domain (such as `Rainy/update/v1\0`).
pub fn signed_by(keyring: &Keyring, domain: &[u8], message: &[u8], signature: &[u8]) -> bool {
    keyring.keys().any(|(_, key)| verifies(key, domain, message, signature))
}

fn verifies(key: &[u8; 32], domain: &[u8], message: &[u8], signature: &[u8]) -> bool {
    UnparsedPublicKey::new(&ED25519, key).verify(&[domain, message].concat(), signature).is_ok()
}

/// Read and authenticate `resources\release-manifest.signed.json`.
pub fn read_manifest(resources: &Path, keyring: &Keyring) -> Result<Authenticated, TrustError> {
    let bytes = fs::read(resources.join(MANIFEST_FILE)).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => TrustError::Missing(MANIFEST_FILE.into()),
        _ => unreadable(MANIFEST_FILE, error),
    })?;
    let text = String::from_utf8(bytes).map_err(|_| TrustError::Invalid("the signed file is not UTF-8".into()))?;
    authenticate(&text, keyring)
}

/// The decoded bytes when `text` is exactly their base64url encoding without padding.
fn canonical_base64(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text).ok().filter(|bytes| URL_SAFE_NO_PAD.encode(bytes) == text)
}

/// The rules of `releaseManifestSchema`.
fn checked(payload: Payload) -> Result<Manifest, TrustError> {
    let invalid = |detail: &str| Err(TrustError::Invalid(detail.into()));
    if payload.version != 1 {
        return invalid("the file list is not version 1");
    }
    if payload.product != "RainyAgent" {
        return invalid("the file list is not Rainy's");
    }
    if !(1..=100).contains(&js_length(&payload.build_version)) {
        return invalid("the file list has an invalid build version");
    }
    if !is_iso_datetime(&payload.created_at) {
        return invalid("the file list has an invalid creation time");
    }
    if !is_hex(&payload.key_id, 32) {
        return invalid("the file list has an invalid key id");
    }
    if !(1..=MAX_FILES).contains(&payload.files.len()) {
        return invalid("the file list has no files or too many");
    }
    if let Some(file) = payload.files.iter().find(|file| !is_listed_path(&file.path) || !is_hex(&file.sha256, 64)) {
        return Err(TrustError::Invalid(format!("the file list has an invalid entry {:?}", file.path)));
    }
    let mut seen = HashSet::with_capacity(payload.files.len());
    if !payload.files.iter().all(|file| seen.insert(file.path.to_lowercase())) {
        return Err(TrustError::DuplicatePaths);
    }
    Ok(Manifest { build_version: payload.build_version, created_at: payload.created_at, key_id: payload.key_id, files: payload.files })
}

/// A relative `/`-separated path with no empty, `.` or `..` segment, and no `\`, NUL or `:`.
fn is_listed_path(path: &str) -> bool {
    (1..=4096).contains(&js_length(path))
        && !path.contains(['\\', '\0', ':'])
        && !path.starts_with('/')
        && path.split('/').all(|segment| !matches!(segment, "" | "." | ".."))
}

/// `z.iso.datetime()`: `YYYY-MM-DDTHH:MM:SS[.fraction]Z` on a real calendar date.
fn is_iso_datetime(text: &str) -> bool {
    let bytes = text.as_bytes();
    let number = |start: usize, length: usize| -> Option<u32> {
        let digits = bytes.get(start..start + length)?;
        digits.iter().all(u8::is_ascii_digit).then(|| digits.iter().fold(0, |value, digit| value * 10 + u32::from(digit - b'0')))
    };
    let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) =
        (number(0, 4), number(5, 2), number(8, 2), number(11, 2), number(14, 2), number(17, 2))
    else {
        return false;
    };
    if separators.iter().any(|(at, separator)| bytes[*at] != *separator) {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    if !(1..=days).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    match &bytes[19..] {
        b"Z" => true,
        [b'.', fraction @ .., b'Z'] => !fraction.is_empty() && fraction.iter().all(u8::is_ascii_digit),
        _ => false,
    }
}

fn is_hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// The length JavaScript gives a string, in UTF-16 units.
fn js_length(text: &str) -> usize {
    text.encode_utf16().count()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes.iter().flat_map(|byte| [DIGITS[usize::from(byte >> 4)], DIGITS[usize::from(byte & 15)]]).map(char::from).collect()
}

fn unreadable(path: &str, error: io::Error) -> TrustError {
    TrustError::Io(format!("{path}: {error}"))
}

/// The listed files below one resolved resources folder.
struct Walk {
    root: PathBuf,
    /// Whether each folder resolves to itself; folders are shared by many files.
    folders: Mutex<HashMap<PathBuf, bool>>,
}

impl Walk {
    fn new(root: PathBuf) -> Self {
        Self { root, folders: Mutex::default() }
    }

    /// Find one listed file, refusing paths outside the root, links, other sizes and folders that resolve elsewhere; its
    /// path and metadata signature (`path\0size\0mtimeNs\0fileIndex`).
    fn locate(&self, file: &ManifestFile) -> Result<(PathBuf, String), TrustError> {
        let mut path = self.root.clone();
        for segment in file.path.split('/') {
            path.push(segment);
        }
        if path == self.root || !path.starts_with(&self.root) {
            return Err(TrustError::Outside);
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => {
                return Err(TrustError::Missing(file.path.clone()));
            }
            Err(error) => return Err(unreadable(&file.path, error)),
        };
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() || metadata.len() != file.bytes {
            return Err(TrustError::Damaged(file.path.clone()));
        }
        let parent = path.parent().unwrap_or(&self.root).to_path_buf();
        let known = self.folders.lock().unwrap().get(&parent).copied();
        let same = match known {
            Some(same) => same,
            None => {
                let same = canonical(&parent).map_err(|error| unreadable(&file.path, error))? == parent;
                self.folders.lock().unwrap().insert(parent, same);
                same
            }
        };
        if !same {
            return Err(TrustError::Replaced(file.path.clone()));
        }
        let (modified, index) = identity(&path).map_err(|error| unreadable(&file.path, error))?;
        let signature = format!("{}\0{}\0{modified}\0{index}", file.path, metadata.len());
        Ok((path, signature))
    }
}

/// The last write time (nanoseconds since 1970) and file index of a file, read without following a link.
#[cfg(windows)]
fn identity(path: &Path) -> io::Result<(i128, u64)> {
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
    let written = (u64::from(information.ftLastWriteTime.dwHighDateTime) << 32) | u64::from(information.ftLastWriteTime.dwLowDateTime);
    let index = (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok(((i128::from(written) - UNIX_EPOCH_FILETIME) * 100, index))
}

#[cfg(not(windows))]
fn identity(path: &Path) -> io::Result<(i128, u64)> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = fs::symlink_metadata(path)?;
    Ok((i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()), metadata.ino()))
}

/// Run `work` over the listed files on up to eight threads; the first failure stops the rest. The results keep the
/// list's order.
fn each_file<T: Send>(
    files: &[ManifestFile],
    work: impl Fn(&ManifestFile, &mut Vec<u8>) -> Result<T, TrustError> + Sync,
) -> Result<Vec<T>, TrustError> {
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let failure = Mutex::new(None);
    let done: Vec<Vec<(usize, T)>> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..WORKERS.min(files.len()).max(1))
            .map(|_| {
                scope.spawn(|| {
                    let (mut buffer, mut done) = (Vec::new(), Vec::new());
                    while !failed.load(Ordering::Relaxed) {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(file) = files.get(index) else { break };
                        match work(file, &mut buffer) {
                            Ok(value) => done.push((index, value)),
                            Err(error) => {
                                failed.store(true, Ordering::Relaxed);
                                failure.lock().unwrap().get_or_insert(error);
                            }
                        }
                    }
                    done
                })
            })
            .collect();
        workers.into_iter().map(|worker| worker.join().expect("a verification thread panicked")).collect()
    });
    if let Some(error) = failure.into_inner().unwrap() {
        return Err(error);
    }
    let mut slots: Vec<Option<T>> = files.iter().map(|_| None).collect();
    for (index, value) in done.into_iter().flatten() {
        slots[index] = Some(value);
    }
    Ok(slots.into_iter().map(|slot| slot.expect("every file was checked")).collect())
}

/// Hash every listed file and compare it with the list; the metadata fingerprint of the files checked.
fn verify_files(walk: &Walk, files: &[ManifestFile], progress: &(dyn Fn(usize, usize) + Sync)) -> Result<String, TrustError> {
    let completed = Mutex::new(0);
    let signatures = each_file(files, |file, buffer| {
        let (path, signature) = walk.locate(file)?;
        if buffer.is_empty() {
            buffer.resize(CHUNK, 0);
        }
        if digest(&path, buffer).map_err(|error| unreadable(&file.path, error))? != file.sha256 {
            return Err(TrustError::Mismatch(file.path.clone()));
        }
        // Reported in order, one at a time.
        let mut completed = completed.lock().unwrap();
        *completed += 1;
        progress(*completed, files.len());
        Ok(signature)
    })?;
    Ok(fingerprint(&signatures))
}

/// Metadata signatures of every file, without reading them.
fn walk_files(walk: &Walk, files: &[ManifestFile]) -> Result<String, TrustError> {
    let signatures = each_file(files, |file, _| walk.locate(file).map(|(_, signature)| signature))?;
    Ok(fingerprint(&signatures))
}

fn fingerprint(signatures: &[String]) -> String {
    hex(&Sha256::digest(signatures.join("\n").as_bytes()))
}

fn digest(path: &Path, buffer: &mut [u8]) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    loop {
        match file.read(buffer) {
            Ok(0) => return Ok(hex(&hash.finalize())),
            Ok(read) => hash.update(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// The record of a full check: `{version, manifestSha256, root, fingerprint, checkedAt}`, as 3.x writes it with
/// version 3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Stamp {
    pub version: u64,
    /// SHA-256 of the signed list's payload text.
    pub manifest_sha256: String,
    /// The resources folder, resolved.
    pub root: String,
    pub fingerprint: String,
    /// Milliseconds since 1970.
    pub checked_at: u64,
}

/// The record at `path`; `None` when it is missing, damaged or another version's, which only costs a full check.
pub fn read_stamp(path: &Path) -> Option<Stamp> {
    let stamp: Stamp = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (stamp.version == STAMP_VERSION && is_hex(&stamp.manifest_sha256, 64) && !stamp.root.is_empty() && is_hex(&stamp.fingerprint, 64))
        .then_some(stamp)
}

pub fn write_stamp(path: &Path, stamp: &Stamp) -> io::Result<()> {
    let mut text = serde_json::to_string(stamp).map_err(io::Error::other)?;
    text.push('\n');
    write_record(path, text.as_bytes())
}

/// Replace `path` with `bytes` in one step (`writePrivateRecord`): a new file in the same folder, flushed, then renamed
/// over it. A link or folder at `path` is refused, not followed.
fn write_record(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let folder = path.parent().ok_or_else(|| io::Error::other("The local record has no folder."))?;
    fs::create_dir_all(folder)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => return Err(io::Error::other("The local record path is not a regular file.")),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut suffix = [0u8; 16];
    getrandom::fill(&mut suffix).map_err(|error| io::Error::other(error.to_string()))?;
    let temporary = folder.join(format!(".rainy-{}.tmp", hex(&suffix)));
    let written = fs::OpenOptions::new().write(true).create_new(true).open(&temporary).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    });
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// Milliseconds since 1970, as records store them.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// What to check, and where the record of the check is kept.
pub struct Options<'a> {
    pub resources: &'a Path,
    pub keyring: &'a Keyring,
    pub stamp: &'a Path,
    /// Called as files are checked, with the number checked and the total.
    pub progress: &'a (dyn Fn(usize, usize) + Sync),
}

/// How startup accepted the resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verified {
    /// Every file was hashed.
    Full,
    /// The record of an earlier full check of the same list in the same folder was found.
    Stamp,
}

impl fmt::Display for Verified {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Full => "full",
            Self::Stamp => "stamp",
        })
    }
}

/// What a recheck did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recheck {
    /// The last check is less than a day old.
    Skipped,
    /// No file's metadata changed.
    Unchanged,
    /// Metadata changed, and every file was hashed again and matched.
    Reverified,
}

/// Accepted resources and their list, for the daily recheck.
#[derive(Debug)]
pub struct Integrity {
    pub verified: Verified,
    pub manifest: Manifest,
    root: PathBuf,
    stamp_path: PathBuf,
    current: Mutex<Stamp>,
}

/// Accept the resources before anything from them runs (`ensureReleaseIntegrity`). The signature is checked every
/// time; the files are hashed only when no record names this list in this folder, so once per installed version.
pub fn ensure(options: &Options<'_>, now: u64) -> Result<Integrity, TrustError> {
    let Authenticated { manifest, manifest_sha256 } = read_manifest(options.resources, options.keyring)?;
    let root = canonical(options.resources).map_err(|error| unreadable(&options.resources.display().to_string(), error))?;
    let root_text = root.to_string_lossy().into_owned();
    let (verified, stamp) = match read_stamp(options.stamp) {
        Some(stamp) if stamp.manifest_sha256 == manifest_sha256 && stamp.root == root_text => (Verified::Stamp, stamp),
        _ => {
            let fingerprint = verify_files(&Walk::new(root.clone()), &manifest.files, options.progress)?;
            let stamp = Stamp { version: STAMP_VERSION, manifest_sha256, root: root_text, fingerprint, checked_at: now };
            // Without the record the next start checks every file again.
            if let Err(error) = write_stamp(options.stamp, &stamp) {
                log::warn!("The record of the verified installation was not written: {error}");
            }
            (Verified::Full, stamp)
        }
    };
    Ok(Integrity { verified, manifest, root, stamp_path: options.stamp.to_path_buf(), current: Mutex::new(stamp) })
}

impl Integrity {
    /// Once `interval` has passed since the last check, compare the files' metadata with it, and hash every file again
    /// when it changed. A failure removes the record, so the next start checks every file before using any.
    pub fn recheck(&self, now: u64, interval: u64) -> Result<Recheck, TrustError> {
        let mut current = self.current.lock().unwrap();
        if now.saturating_sub(current.checked_at) < interval {
            return Ok(Recheck::Skipped);
        }
        let walk = Walk::new(self.root.clone());
        let checked = walk_files(&walk, &self.manifest.files).and_then(|fingerprint| {
            let (outcome, fingerprint) = if fingerprint == current.fingerprint {
                (Recheck::Unchanged, fingerprint)
            } else {
                (Recheck::Reverified, verify_files(&walk, &self.manifest.files, &|_, _| {})?)
            };
            let next = Stamp { fingerprint, checked_at: now, ..current.clone() };
            write_stamp(&self.stamp_path, &next).map_err(|error| unreadable(STAMP_FILE, error))?;
            *current = next;
            Ok(outcome)
        });
        if checked.is_err() {
            let _ = fs::remove_file(&self.stamp_path);
        }
        checked
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use std::time::{Duration, SystemTime};

    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair as _};
    use serde_json::Value;

    use super::*;
    use crate::keyring::key_id;

    const CREATED: &str = "2026-10-21T23:55:45.824Z";

    /// A release key made for one test.
    struct Signer {
        pair: Ed25519KeyPair,
        id: String,
    }

    impl Signer {
        fn new() -> Self {
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
            let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
            let public: [u8; 32] = pair.public_key().as_ref().try_into().unwrap();
            Self { pair, id: key_id(&public) }
        }

        fn public(&self) -> [u8; 32] {
            self.pair.public_key().as_ref().try_into().unwrap()
        }

        fn keyring(&self) -> Keyring {
            Keyring::from_keys(&[(self.id.as_str(), self.public())])
        }

        /// The signed file as `sign-release.mjs` writes it: `JSON.stringify(signed) + '\n'`.
        fn envelope(&self, payload: &[u8], domain: &[u8]) -> String {
            let signature = self.pair.sign(&[domain, payload].concat());
            format!(
                "{{\"version\":1,\"payload\":\"{}\",\"signature\":\"{}\"}}\n",
                URL_SAFE_NO_PAD.encode(payload),
                URL_SAFE_NO_PAD.encode(signature.as_ref())
            )
        }
    }

    type Listed = (String, String, u64);

    /// `JSON.stringify` of a manifest with the fields in `sign-release.mjs`'s order.
    fn payload(build: &str, key_id: &str, files: &[Listed]) -> String {
        let files: Vec<String> = files
            .iter()
            .map(|(path, sha256, bytes)| format!("{{\"path\":{},\"sha256\":\"{sha256}\",\"bytes\":{bytes}}}", serde_json::to_string(path).unwrap()))
            .collect();
        format!(
            "{{\"version\":1,\"product\":\"RainyAgent\",\"buildVersion\":\"{build}\",\"createdAt\":\"{CREATED}\",\"keyId\":\"{key_id}\",\"files\":[{}]}}",
            files.join(",")
        )
    }

    fn listed(path: &str, content: &[u8]) -> Listed {
        (path.to_owned(), hex(&Sha256::digest(content)), content.len() as u64)
    }

    /// An installed resources folder, signed by its own key, and a user-data folder for the record.
    struct Installation {
        dir: tempfile::TempDir,
        resources: PathBuf,
        stamp: PathBuf,
        signer: Signer,
        files: Vec<Listed>,
    }

    impl Installation {
        fn new(files: &[(&str, &[u8])]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let resources = dir.path().join("resources");
            for (path, content) in files {
                let full = resources.join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(full, content).unwrap();
            }
            let files: Vec<Listed> = files.iter().map(|(path, content)| listed(path, content)).collect();
            let stamp = dir.path().join("user-data").join(STAMP_FILE);
            let installation = Self { dir, resources, stamp, signer: Signer::new(), files };
            installation.sign(&installation.files);
            installation
        }

        fn sign(&self, files: &[Listed]) {
            self.write_manifest(&self.signer.envelope(payload("4.0.0", &self.signer.id, files).as_bytes(), DOMAIN));
        }

        fn write_manifest(&self, text: &str) {
            fs::write(self.resources.join(MANIFEST_FILE), text).unwrap();
        }

        fn path(&self, path: &str) -> PathBuf {
            path.split('/').fold(self.resources.clone(), |full, segment| full.join(segment))
        }

        fn ensure_with(&self, keyring: &Keyring, now: u64) -> Result<Integrity, TrustError> {
            ensure(&Options { resources: &self.resources, keyring, stamp: &self.stamp, progress: &|_, _| {} }, now)
        }

        fn ensure(&self, now: u64) -> Result<Integrity, TrustError> {
            self.ensure_with(&self.signer.keyring(), now)
        }
    }

    fn installation() -> Installation {
        Installation::new(&[
            ("windows-host/node/node.exe", b"node"),
            ("windows-host/app/dist/host.js", b"host"),
            ("windows-host/runtime.json", b"{}"),
            ("icon.ico", b"icon"),
        ])
    }

    /// A directory junction, which any user may create; false where `mklink` is unavailable.
    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(target).output().is_ok_and(|output| output.status.success())
    }

    #[test]
    fn verifies_an_installation_once_then_trusts_the_record() {
        let installation = installation();
        let seen = Mutex::new(Vec::new());
        let options = Options {
            resources: &installation.resources,
            keyring: &installation.signer.keyring(),
            stamp: &installation.stamp,
            progress: &|done, total| seen.lock().unwrap().push((done, total)),
        };
        let integrity = ensure(&options, 1_000).unwrap();
        assert_eq!(integrity.verified, Verified::Full);
        assert_eq!(integrity.manifest.build_version, "4.0.0");
        assert_eq!(integrity.manifest.files.len(), 4);
        assert_eq!(*seen.lock().unwrap(), [(1, 4), (2, 4), (3, 4), (4, 4)]);
        let stamp = read_stamp(&installation.stamp).unwrap();
        assert_eq!(stamp.version, 3);
        assert_eq!(stamp.checked_at, 1_000);
        assert_eq!(stamp.root, canonical(&installation.resources).unwrap().to_string_lossy());
        assert!(!stamp.root.starts_with(r"\\?\"));
        assert_eq!(installation.ensure(2_000).unwrap().verified, Verified::Stamp);
        assert_eq!(read_stamp(&installation.stamp).unwrap().checked_at, 1_000);
    }

    #[test]
    fn refuses_a_changed_list() {
        let installation = installation();
        let keyring = installation.signer.keyring();
        let text = fs::read_to_string(installation.path(MANIFEST_FILE)).unwrap();
        let envelope: Value = serde_json::from_str(&text).unwrap();
        let (payload_text, signature) = (envelope["payload"].as_str().unwrap(), envelope["signature"].as_str().unwrap());
        let with = |payload: &str, signature: &str| format!("{{\"version\":1,\"payload\":\"{payload}\",\"signature\":\"{signature}\"}}\n");
        assert!(authenticate(&with(payload_text, signature), &keyring).is_ok());

        // Another valid list under the old signature.
        let mut files = installation.files.clone();
        files[0].2 += 1;
        let tampered = URL_SAFE_NO_PAD.encode(payload("4.0.0", &installation.signer.id, &files));
        assert_eq!(authenticate(&with(&tampered, signature), &keyring), Err(TrustError::Signature));
        // One bit of the signature.
        let mut flipped = URL_SAFE_NO_PAD.decode(signature).unwrap();
        flipped[10] ^= 1;
        assert_eq!(authenticate(&with(payload_text, &URL_SAFE_NO_PAD.encode(flipped)), &keyring), Err(TrustError::Signature));
        // Not canonical base64url: padding, the standard alphabet, a short signature.
        assert_eq!(authenticate(&with(&format!("{payload_text}="), signature), &keyring), Err(TrustError::Encoding));
        let standard = base64::engine::general_purpose::STANDARD_NO_PAD.encode(URL_SAFE_NO_PAD.decode(signature).unwrap());
        if standard != signature {
            assert_eq!(authenticate(&with(payload_text, &standard), &keyring), Err(TrustError::Encoding));
        }
        assert_eq!(authenticate(&with(payload_text, &signature[..84]), &keyring), Err(TrustError::Encoding));
        // A tampered list on disk stops startup before any file is read.
        installation.write_manifest(&with(&tampered, signature));
        assert_eq!(installation.ensure(0).unwrap_err(), TrustError::Signature);
        assert!(!installation.stamp.exists());
    }

    #[test]
    fn refuses_a_signature_for_another_purpose() {
        let installation = installation();
        let bytes = payload("4.0.0", &installation.signer.id, &installation.files);
        for domain in [&b"Rainy/update/v1\0"[..], b"RainyAgent/release-manifest/v2\0", b"RainyAgent/release-manifest/v1", b""] {
            let text = installation.signer.envelope(bytes.as_bytes(), domain);
            assert_eq!(authenticate(&text, &installation.signer.keyring()), Err(TrustError::Signature));
        }
        // Other signed files are checked under their own domain.
        let update = b"version: 4.0.1\n";
        let signature = installation.signer.pair.sign(&[&b"Rainy/update/v1\0"[..], update].concat());
        let other = Signer::new();
        let keyring = Keyring::from_keys(&[(other.id.as_str(), other.public()), (installation.signer.id.as_str(), installation.signer.public())]);
        assert!(signed_by(&keyring, b"Rainy/update/v1\0", update, signature.as_ref()));
        assert!(!signed_by(&keyring, DOMAIN, update, signature.as_ref()));
        assert!(!signed_by(&keyring, b"Rainy/update/v1\0", b"version: 4.0.2\n", signature.as_ref()));
        assert!(!signed_by(&other.keyring(), b"Rainy/update/v1\0", update, signature.as_ref()));
    }

    #[test]
    fn refuses_unknown_keys() {
        let installation = installation();
        // Signed by the right key, but naming another.
        let other = "0123456789abcdef0123456789abcdef";
        let text = installation.signer.envelope(payload("4.0.0", other, &installation.files).as_bytes(), DOMAIN);
        assert_eq!(authenticate(&text, &installation.signer.keyring()), Err(TrustError::Signature));
        // A keyring whose key under this id is another one.
        let impostor = Signer::new();
        let keyring = Keyring::from_keys(&[(installation.signer.id.as_str(), impostor.public())]);
        assert_eq!(installation.ensure_with(&keyring, 0).unwrap_err(), TrustError::Signature);
        assert_eq!(installation.ensure_with(&impostor.keyring(), 0).unwrap_err(), TrustError::Signature);
    }

    #[test]
    fn refuses_missing_files() {
        let installation = installation();
        fs::remove_file(installation.path("windows-host/app/dist/host.js")).unwrap();
        assert_eq!(installation.ensure(0).unwrap_err(), TrustError::Missing("windows-host/app/dist/host.js".into()));
        fs::remove_dir_all(installation.path("windows-host/app")).unwrap();
        assert_eq!(installation.ensure(0).unwrap_err(), TrustError::Missing("windows-host/app/dist/host.js".into()));
        fs::remove_file(installation.path(MANIFEST_FILE)).unwrap();
        assert_eq!(installation.ensure(0).unwrap_err(), TrustError::Missing(MANIFEST_FILE.into()));
        assert!(!installation.stamp.exists());
    }

    #[test]
    fn refuses_changed_files() {
        let installation = installation();
        fs::write(installation.path("windows-host/node/node.exe"), b"NODE").unwrap();
        assert_eq!(installation.ensure(0).unwrap_err(), TrustError::Mismatch("windows-host/node/node.exe".into()));
        fs::write(installation.path("windows-host/node/node.exe"), b"node!").unwrap();
        assert_eq!(installation.ensure(0).unwrap_err(), TrustError::Damaged("windows-host/node/node.exe".into()));
        fs::remove_file(installation.path("windows-host/node/node.exe")).unwrap();
        fs::create_dir(installation.path("windows-host/node/node.exe")).unwrap();
        assert_eq!(installation.ensure(0).unwrap_err(), TrustError::Damaged("windows-host/node/node.exe".into()));
        assert!(TrustError::Damaged(String::new()).is_integrity() && !TrustError::Signature.is_integrity());
    }

    #[cfg(windows)]
    #[test]
    fn refuses_links_and_replaced_folders() {
        let installation = installation();
        let app = installation.path("windows-host/app");
        let elsewhere = installation.dir.path().join("elsewhere");
        // The same files, in a folder a junction leads to.
        fs::rename(&app, &elsewhere).unwrap();
        if !junction(&app, &elsewhere) {
            eprintln!("mklink /J is unavailable; links were not tested");
            return;
        }
        assert_eq!(installation.ensure(0).unwrap_err(), TrustError::Replaced("windows-host/app/dist/host.js".into()));
        fs::remove_dir(&app).unwrap();
        fs::rename(&elsewhere, &app).unwrap();
        assert!(installation.ensure(0).is_ok());

        // A junction where a file is listed.
        fs::remove_file(installation.path("icon.ico")).unwrap();
        assert!(junction(&installation.path("icon.ico"), &app));
        fs::remove_file(&installation.stamp).unwrap();
        assert_eq!(installation.ensure(0).unwrap_err(), TrustError::Damaged("icon.ico".into()));
        fs::remove_dir(installation.path("icon.ico")).unwrap();

        // A file link, where Windows lets this user create one (Developer Mode or an administrator).
        let target = installation.dir.path().join("icon.ico");
        fs::write(&target, b"icon").unwrap();
        if std::os::windows::fs::symlink_file(&target, installation.path("icon.ico")).is_ok() {
            assert_eq!(installation.ensure(0).unwrap_err(), TrustError::Damaged("icon.ico".into()));
        }
    }

    #[test]
    fn reads_back_its_record_and_ignores_others() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join(STAMP_FILE);
        let stamp = Stamp {
            version: 3,
            manifest_sha256: "4841726608f7c561aec23981ffd42c7ccacd7741047e263a9773bcad094b42c4".into(),
            root: r"C:\Users\ROG\AppData\Local\Programs\RainyAgent\resources".into(),
            fingerprint: "02ee26a4".repeat(8),
            checked_at: 1_792_473_414_685,
        };
        write_stamp(&path, &stamp).unwrap();
        assert_eq!(read_stamp(&path), Some(stamp.clone()));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(r#"{"version":3,"manifestSha256":"4841"#) && text.ends_with("\"checkedAt\":1792473414685}\n"), "{text}");
        write_stamp(&path, &Stamp { checked_at: 1, ..stamp.clone() }).unwrap();
        assert_eq!(read_stamp(&path).unwrap().checked_at, 1);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "no temporary file is left");

        // 3.x's version 2, unknown fields, malformed digests: no record.
        let other = |change: &dyn Fn(&mut Value)| {
            let mut value = serde_json::to_value(&stamp).unwrap();
            change(&mut value);
            fs::write(&path, value.to_string()).unwrap();
            read_stamp(&path)
        };
        assert_eq!(other(&|_| {}), Some(stamp.clone()));
        assert_eq!(other(&|value| value["version"] = 2.into()), None);
        assert_eq!(other(&|value| value["extra"] = 1.into()), None);
        assert_eq!(other(&|value| value["fingerprint"] = "02EE26A4".repeat(8).into()), None);
        assert_eq!(other(&|value| value["root"] = "".into()), None);
        fs::write(&path, b"{").unwrap();
        assert_eq!(read_stamp(&path), None);

        // A folder in the record's place is left alone.
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(write_stamp(&path, &stamp).is_err());
        assert!(path.is_dir());
    }

    #[test]
    fn checks_everything_again_for_another_list_or_folder() {
        let installation = installation();
        assert_eq!(installation.ensure(0).unwrap().verified, Verified::Full);
        let stamp = read_stamp(&installation.stamp).unwrap();
        write_stamp(&installation.stamp, &Stamp { root: format!("{}x", stamp.root), ..stamp.clone() }).unwrap();
        assert_eq!(installation.ensure(0).unwrap().verified, Verified::Full);
        // An update signs a new list.
        installation.sign(&installation.files[1..]);
        assert_eq!(installation.ensure(0).unwrap().verified, Verified::Full);
        assert_ne!(read_stamp(&installation.stamp).unwrap().manifest_sha256, stamp.manifest_sha256);
        assert_eq!(installation.ensure(0).unwrap().verified, Verified::Stamp);
    }

    #[test]
    fn an_update_is_pruned_after_its_full_check_and_stays_accepted() {
        let runtime = br#"{"version":1,"files":[{"path":"node/node.exe"},{"path":"app/dist/host.js"}]}"#;
        let installation = Installation::new(&[
            ("windows-host/node/node.exe", b"node"),
            ("windows-host/app/dist/host.js", b"host"),
            ("windows-host/runtime.json", runtime),
        ]);
        // What the previous version shipped and this one dropped; the installer overwrote the rest.
        let stale = installation.path("windows-host/node_modules/a/node_modules/b/index.js");
        fs::create_dir_all(stale.parent().unwrap()).unwrap();
        fs::write(&stale, b"old").unwrap();
        let integrity = installation.ensure(0).unwrap();
        assert_eq!(integrity.verified, Verified::Full);
        assert_eq!(crate::maintenance::prune_windows_host(&installation.resources, false).unwrap(), 3);
        assert!(!installation.path("windows-host/node_modules").exists());
        // The record covers only listed files, so it still holds.
        assert_eq!(integrity.recheck(RECHECK_INTERVAL_MS, RECHECK_INTERVAL_MS), Ok(Recheck::Unchanged));
        assert_eq!(installation.ensure(RECHECK_INTERVAL_MS).unwrap().verified, Verified::Stamp);
    }

    #[test]
    fn rechecks_daily_and_forgets_the_record_on_damage() {
        let installation = installation();
        let day = RECHECK_INTERVAL_MS;
        let integrity = installation.ensure(0).unwrap();
        assert_eq!(integrity.recheck(day - 1, day), Ok(Recheck::Skipped));
        assert_eq!(integrity.recheck(day, day), Ok(Recheck::Unchanged));
        assert_eq!(read_stamp(&installation.stamp).unwrap().checked_at, day);
        assert_eq!(integrity.recheck(day + 1, day), Ok(Recheck::Skipped));
        // Touched but unchanged: hashed again, and accepted.
        let icon = File::options().write(true).open(installation.path("icon.ico")).unwrap();
        icon.set_modified(SystemTime::now() + Duration::from_secs(3600)).unwrap();
        drop(icon);
        assert_eq!(integrity.recheck(2 * day, day), Ok(Recheck::Reverified));
        assert_eq!(integrity.recheck(3 * day, day), Ok(Recheck::Unchanged));
        // Changed: the record goes, so the next start checks every file.
        fs::write(installation.path("icon.ico"), b"ICON").unwrap();
        assert_eq!(integrity.recheck(4 * day, day), Err(TrustError::Mismatch("icon.ico".into())));
        assert!(!installation.stamp.exists());
        assert_eq!(installation.ensure(5 * day).unwrap_err(), TrustError::Mismatch("icon.ico".into()));
    }

    #[cfg(windows)]
    #[test]
    fn fingerprints_the_files_metadata() {
        use std::os::windows::fs::MetadataExt as _;

        let installation = installation();
        let path = installation.path("icon.ico");
        let (modified, index) = identity(&path).unwrap();
        let written = fs::metadata(&path).unwrap().last_write_time();
        assert_eq!(modified, (i128::from(written) - UNIX_EPOCH_FILETIME) * 100);
        assert_ne!(index, 0);
        let walk = Walk::new(canonical(&installation.resources).unwrap());
        let file = ManifestFile { path: "icon.ico".into(), sha256: String::new(), bytes: 4 };
        assert_eq!(walk.locate(&file).unwrap().1, format!("icon.ico\x004\x00{modified}\x00{index}"));
    }

    /// A list made the way `scripts/sign-release.mjs` makes it verifies as it is: `windows-host` in sorted `readdir`
    /// order, then the named resources, the manifest as compact `JSON.stringify` in its field order, base64url without
    /// padding, Ed25519 over the domain and the payload's bytes, `JSON.stringify(signed) + '\n'`, and the keyring as
    /// `release-signing-key.mjs` writes it.
    #[test]
    fn verifies_a_list_signed_like_3x() {
        let dir = tempfile::tempdir().unwrap();
        let resources = dir.path().join("resources");
        let contents: [(&str, &[u8]); 15] = [
            ("windows-host/runtime.json", b"{\"version\":1}"),
            ("windows-host/node/node.exe", b"MZ node"),
            ("windows-host/app/dist/host.js", b"console.log('host')\n"),
            ("windows-host/app/LICENSE", b"MIT"),
            ("windows-host/app/Zed.txt", b""),
            ("windows-host/node_modules/a/index.js", "module.exports = '雨'\n".as_bytes()),
            ("release-public-keys.json", b"{}"),
            ("environment-component-catalog.json", b"{}"),
            ("install-environment-component.py", b"print()"),
            ("linux-runtime.json", b"{}"),
            ("linux-app.tar.gz", &[0x1f, 0x8b, 0x08]),
            ("install-runtime.py", b"print()"),
            ("optional-modules.json", b"{\"version\":1,\"modules\":[]}"),
            ("icon.ico", &[0, 0, 1, 0]),
            ("app-update.yml", b"owner: RainyMarks\n"),
        ];
        for (path, content) in contents {
            let full = resources.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, content).unwrap();
        }

        // `inventory()`: depth first, each folder's names sorted by UTF-16 code units.
        fn inventory(root: &Path, path: &Path, files: &mut Vec<Listed>) {
            if fs::symlink_metadata(path).unwrap().is_dir() {
                let mut names: Vec<String> = fs::read_dir(path).unwrap().map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect();
                names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
                for name in names {
                    inventory(root, &path.join(name), files);
                }
            } else {
                let relative = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                files.push(listed(&relative, &fs::read(path).unwrap()));
            }
        }
        let mut files = Vec::new();
        for name in [
            "windows-host",
            "release-public-keys.json",
            "environment-component-catalog.json",
            "install-environment-component.py",
            "linux-runtime.json",
            "linux-app.tar.gz",
            "install-runtime.py",
            "optional-modules.json",
            "icon.ico",
            "app-update.yml",
        ] {
            inventory(&resources, &resources.join(name), &mut files);
        }
        let order: Vec<&str> = files.iter().take(6).map(|(path, ..)| path.as_str()).collect();
        assert_eq!(
            order,
            [
                "windows-host/app/LICENSE",
                "windows-host/app/Zed.txt",
                "windows-host/app/dist/host.js",
                "windows-host/node/node.exe",
                "windows-host/node_modules/a/index.js",
                "windows-host/runtime.json"
            ]
        );

        // `releasePublicKeys()` and `prepareReleaseKey()`: SPKI PEM under the id, `JSON.stringify(keys, null, 2) + '\n'`.
        let signer = Signer::new();
        let mut der = vec![0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
        der.extend_from_slice(&signer.public());
        let pem = format!("-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n", base64::engine::general_purpose::STANDARD.encode(&der));
        let keys = format!("{{\n  \"version\": 1,\n  \"keys\": {{\n    \"{}\": {}\n  }}\n}}\n", signer.id, serde_json::to_string(&pem).unwrap());
        let keyring = Keyring::parse(&keys).unwrap();
        assert_eq!(keyring, signer.keyring());

        // `signReleaseResources()`.
        let bytes = payload("3.9.1", &signer.id, &files).into_bytes();
        let signed = format!(
            "{{\"version\":1,\"payload\":\"{}\",\"signature\":\"{}\"}}\n",
            URL_SAFE_NO_PAD.encode(&bytes),
            URL_SAFE_NO_PAD.encode(signer.pair.sign(&[b"RainyAgent/release-manifest/v1\0".as_slice(), &bytes].concat()).as_ref())
        );
        fs::write(resources.join(MANIFEST_FILE), &signed).unwrap();

        let stamp = dir.path().join(STAMP_FILE);
        let integrity = ensure(&Options { resources: &resources, keyring: &keyring, stamp: &stamp, progress: &|_, _| {} }, 7).unwrap();
        assert_eq!(integrity.verified, Verified::Full);
        assert_eq!(integrity.manifest.build_version, "3.9.1");
        assert_eq!(integrity.manifest.created_at, CREATED);
        assert_eq!(integrity.manifest.key_id, signer.id);
        let read: Vec<Listed> = integrity.manifest.files.iter().map(|file| (file.path.clone(), file.sha256.clone(), file.bytes)).collect();
        assert_eq!(read, files);
        // The record names the list by the SHA-256 of the payload text, as 3.x does.
        let payload_text = URL_SAFE_NO_PAD.encode(&bytes);
        assert_eq!(read_stamp(&stamp).unwrap().manifest_sha256, hex(&Sha256::digest(payload_text.as_bytes())));
        assert_eq!(URL_SAFE_NO_PAD.decode(&payload_text).unwrap(), bytes);
    }

    #[test]
    fn reads_lists_only_in_3x_shape() {
        let signer = Signer::new();
        let keyring = signer.keyring();
        let file = |path: &str| (path.to_owned(), "0".repeat(64), 0);
        let check = |files: &[Listed]| authenticate(&signer.envelope(payload("4.0.0", &signer.id, files).as_bytes(), DOMAIN), &keyring);
        assert!(check(&[file("windows-host/node/node.exe")]).is_ok());
        for bad in ["", "/a", "a/", "a//b", "./a", "a/./b", "a/../b", "..", "a\\b", "C:/a", "a\0b"] {
            assert!(matches!(check(&[file(bad)]), Err(TrustError::Invalid(_))), "{bad:?}");
        }
        assert!(matches!(check(&[file(&"a".repeat(4097))]), Err(TrustError::Invalid(_))));
        assert!(matches!(check(&[]), Err(TrustError::Invalid(_))));
        assert!(matches!(check(&[("a".into(), "A".repeat(64), 0)]), Err(TrustError::Invalid(_))));
        assert_eq!(check(&[file("windows-host/A.js"), file("Windows-Host/a.JS")]), Err(TrustError::DuplicatePaths));

        let raw = |json: &str| authenticate(&signer.envelope(json.as_bytes(), DOMAIN), &keyring);
        let manifest = payload("4.0.0", &signer.id, &[file("a")]);
        assert!(raw(&manifest).is_ok());
        for (from, to) in [
            ("\"version\":1", "\"version\":2"),
            ("RainyAgent", "Rainy"),
            ("\"buildVersion\":\"4.0.0\"", "\"buildVersion\":\"\""),
            (CREATED, "2026-10-21T23:55:45.824+08:00"),
            ("\"bytes\":0", "\"bytes\":-1"),
            ("\"bytes\":0", "\"bytes\":0,\"extra\":1"),
            ("\"files\"", "\"extra\":1,\"files\""),
        ] {
            assert!(matches!(raw(&manifest.replacen(from, to, 1)), Err(TrustError::Invalid(_))), "{to}");
        }
        let signed = signer.envelope(manifest.as_bytes(), DOMAIN);
        for (from, to) in [("{\"version\":1", "{\"version\":2"), ("}\n", ",\"extra\":1}\n")] {
            assert!(matches!(authenticate(&signed.replacen(from, to, 1), &keyring), Err(TrustError::Invalid(_))), "{to}");
        }
    }

    #[test]
    fn reads_iso_times_as_zod_does() {
        for good in ["2026-10-21T23:55:45.824Z", "2026-10-21T23:55:45Z", "2024-02-29T00:00:00Z", "2000-02-29T12:00:00.5Z", "0000-01-01T00:00:00Z"] {
            assert!(is_iso_datetime(good), "{good}");
        }
        for bad in [
            "2023-02-29T00:00:00Z",
            "1900-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-10-21T24:00:00Z",
            "2026-10-21T23:60:00Z",
            "2026-10-21T23:55Z",
            "2026-10-21T23:55:45",
            "2026-10-21T23:55:45.Z",
            "2026-10-21T23:55:45+08:00",
            "2026-10-21 23:55:45Z",
            "２026-10-21T23:55:45Z",
        ] {
            assert!(!is_iso_datetime(bad), "{bad}");
        }
    }

    /// A packaged 3.x resources folder, read only: `RAINY_TRUST_RESOURCES=<…\release\win-unpacked\resources>`, with the
    /// keyring from `RAINY_TRUST_KEYS` or the folder's own `release-public-keys.json`. The record goes to a temporary
    /// folder.
    #[test]
    #[ignore = "needs RAINY_TRUST_RESOURCES"]
    fn verifies_a_packaged_3x_folder() {
        let resources = PathBuf::from(std::env::var_os("RAINY_TRUST_RESOURCES").expect("RAINY_TRUST_RESOURCES"));
        let keys = std::env::var_os("RAINY_TRUST_KEYS").map_or_else(|| resources.join("release-public-keys.json"), PathBuf::from);
        let keyring = Keyring::parse(&fs::read_to_string(keys).unwrap()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let stamp = dir.path().join(STAMP_FILE);
        let options = Options { resources: &resources, keyring: &keyring, stamp: &stamp, progress: &|_, _| {} };
        let started = std::time::Instant::now();
        let integrity = ensure(&options, now_ms()).unwrap();
        eprintln!("{} files of {} verified ({}) in {:?}", integrity.manifest.files.len(), integrity.manifest.build_version, integrity.verified, started.elapsed());
        assert_eq!(integrity.verified, Verified::Full);
        let started = std::time::Instant::now();
        assert_eq!(ensure(&options, now_ms()).unwrap().verified, Verified::Stamp);
        assert_eq!(integrity.recheck(now_ms() + RECHECK_INTERVAL_MS, RECHECK_INTERVAL_MS), Ok(Recheck::Unchanged));
        eprintln!("record and recheck in {:?}", started.elapsed());
    }
}
