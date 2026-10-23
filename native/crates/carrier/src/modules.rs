//! Optional parts of Rainy that are downloaded on first use instead of shipping in the installer
//! (`src/main/optional-modules.ts`, `release-download.ts`). `optional-modules.json` in the resources pins each module's
//! archive and the pieces it is published in; a module is downloaded into `modules\.downloads`, unpacked into
//! `modules\.staging-<id>-<uuid>`, marked with `.rainy-module.json` and swapped into `modules\<id>`. The layout is
//! 3.x's, so a module 3.x installed is recognized without downloading it again.

use std::cell::Cell;
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::download::{self, Digest, DownloadError, Expected, Options};
use crate::tarball::{self, TarError};

/// The descriptor, in the resources folder.
pub const DESCRIPTOR_FILE: &str = "optional-modules.json";
/// Every module a descriptor may pin.
pub const IDS: [&str; 4] = ["strata", "php", "linux-runtime", "linux-dev-packages"];
const MARKER: &str = ".rainy-module.json";
const DOWNLOADS: &str = ".downloads";
/// JavaScript's `Number.MAX_SAFE_INTEGER`, the largest size the 3.x schema accepts.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const PUBLISH_INTERVAL: Duration = Duration::from_millis(100);

/// Modules the carrier fetches by itself; Settings neither lists them nor receives their progress.
pub fn internal(id: &str) -> bool {
    id == "linux-runtime" || id == "linux-dev-packages"
}

/// A directory module is unpacked; a file module stays an archive that another installer consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Directory,
    File,
}

/// One pinned module.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Module {
    pub id: String,
    pub kind: Kind,
    /// The archive's name, such as `php.tar.gz`.
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
    pub unpacked_bytes: u64,
    /// The release folder the pieces are published in.
    pub base_url: String,
    /// In order; their concatenation is the archive.
    pub pieces: Vec<Piece>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Piece {
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    version: u64,
    modules: Vec<Module>,
}

/// `.rainy-module.json` in an installed module: which archive it was unpacked from.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u64,
    id: String,
    sha256: String,
}

/// Why a module operation failed. The texts are 3.x's; the interface shows them in its language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleError {
    /// This build ships no descriptor, as development resources do not.
    Unavailable,
    /// The descriptor is not one a release build writes: why.
    Invalid(String),
    /// The descriptor pins no module with this id.
    NotOffered(String),
    Busy,
    Closing,
    RemoveBusy,
    /// The archive could not be unpacked: why.
    Archive(String),
    Download(DownloadError),
}

impl ModuleError {
    pub fn cancelled(&self) -> bool {
        *self == Self::Download(DownloadError::Cancelled)
    }
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("This version does not offer components for download."),
            Self::Invalid(detail) => write!(f, "The component list of this version is not valid: {detail}"),
            Self::NotOffered(id) => write!(f, "This version does not offer this component for download: {id}"),
            Self::Busy => f.write_str("Another component is downloading. Please wait."),
            Self::Closing => f.write_str("Rainy is closing."),
            Self::RemoveBusy => f.write_str("Wait for the component download to finish before removing it."),
            Self::Archive(detail) => write!(f, "The component archive could not be unpacked: {detail}"),
            Self::Download(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ModuleError {}

impl From<DownloadError> for ModuleError {
    fn from(error: DownloadError) -> Self {
        Self::Download(error)
    }
}

fn io_error(path: &Path, error: io::Error) -> ModuleError {
    ModuleError::Download(DownloadError::Io(format!("{}: {error}", path.display())))
}

/// Parse `optional-modules.json` as strictly as 3.x's schema: version 1, known and unique ids, the archive and piece
/// name patterns, positive sizes that the pieces add up to, lowercase SHA-256 digests, a `vX.Y.Z-resources` release
/// of RainyMarks on GitHub, a file module exactly for `linux-runtime`, and no other fields.
pub fn parse_descriptor(text: &str) -> Result<Vec<Module>, ModuleError> {
    let invalid = |detail: String| Err(ModuleError::Invalid(detail));
    let descriptor: Descriptor = serde_json::from_str(text).map_err(|error| ModuleError::Invalid(error.to_string()))?;
    if descriptor.version != 1 {
        return invalid("not version 1".into());
    }
    let mut seen = HashSet::new();
    for module in &descriptor.modules {
        let id = &module.id;
        if !IDS.contains(&id.as_str()) {
            return invalid(format!("Unknown module: {id}"));
        }
        if !seen.insert(id.as_str()) {
            return invalid("Duplicate module".into());
        }
        if !archive_name(&module.file) {
            return invalid(format!("Invalid archive name: {id}"));
        }
        if !(1..=MAX_SAFE_INTEGER).contains(&module.bytes) || module.unpacked_bytes > MAX_SAFE_INTEGER || !sha256_hex(&module.sha256) {
            return invalid(format!("Invalid size or digest: {id}"));
        }
        if !release_url(&module.base_url) {
            return invalid(format!("Invalid release address: {id}"));
        }
        if module.pieces.is_empty() {
            return invalid(format!("No pieces: {id}"));
        }
        if module.pieces.iter().any(|piece| !piece_name(&piece.file) || piece.bytes > MAX_SAFE_INTEGER || !sha256_hex(&piece.sha256)) {
            return invalid(format!("Invalid piece: {id}"));
        }
        if module.pieces.iter().map(|piece| u128::from(piece.bytes)).sum::<u128>() != u128::from(module.bytes) {
            return invalid(format!("Piece sizes differ: {id}"));
        }
        if (id == "linux-runtime") != (module.kind == Kind::File) {
            return invalid(format!("Unexpected module kind: {id}"));
        }
    }
    Ok(descriptor.modules)
}

fn lower_alnum(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// `^[a-z0-9][a-z0-9.-]*\.tar\.(?:gz|xz)$`
fn archive_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".tar.gz").or_else(|| name.strip_suffix(".tar.xz")) else { return false };
    let mut bytes = stem.bytes();
    bytes.next().is_some_and(lower_alnum) && bytes.all(|byte| lower_alnum(byte) || byte == b'.' || byte == b'-')
}

/// `^[a-f0-9]{64}$`
fn sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `^rainy-[a-f0-9]{20}\.\d{3}$`
fn piece_name(name: &str) -> bool {
    let Some((hash, index)) = name.strip_prefix("rainy-").and_then(|rest| rest.split_once('.')) else { return false };
    hash.len() == 20 && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) && index.len() == 3 && index.bytes().all(|byte| byte.is_ascii_digit())
}

/// `^https://github\.com/RainyMarks/(?:RainyAgent|Rainy)/releases/download/v\d+\.\d+\.\d+-resources/$`
fn release_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://github.com/RainyMarks/") else { return false };
    let Some(rest) = rest.strip_prefix("RainyAgent/").or_else(|| rest.strip_prefix("Rainy/")) else { return false };
    let Some(version) = rest.strip_prefix("releases/download/v").and_then(|rest| rest.strip_suffix("-resources/")) else { return false };
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3 && parts.iter().all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn expected(bytes: u64, sha256: &str) -> Result<Expected, ModuleError> {
    let digest = Digest::sha256_hex(sha256).ok_or_else(|| ModuleError::Invalid(format!("Invalid digest: {sha256}")))?;
    Ok(Expected { bytes, digest })
}

/// Where the latest operation stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    Downloading,
    Installing,
    Complete,
    Cancelled,
    Error,
}

/// Progress of the latest operation on a module Settings lists (`OptionalModulesState`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    pub phase: Phase,
    pub module: Option<String>,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub error: Option<ModuleError>,
}

/// A module Settings lists (`OptionalModuleStatus`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub id: String,
    pub installed: bool,
    /// The compressed download.
    pub download_bytes: u64,
    /// Disk space once installed.
    pub unpacked_bytes: u64,
}

struct Inner {
    state: State,
    /// The cancel flag of the operation that runs; installs and removals run one at a time.
    running: Option<Arc<AtomicBool>>,
    closed: bool,
    list: Option<Arc<Vec<Module>>>,
}

/// Downloads, verifies, unpacks and removes modules, one at a time (`OptionalModules`). Every call blocks; run them on
/// background threads.
pub struct Modules {
    descriptor: PathBuf,
    root: PathBuf,
    /// Tests serve the pieces from a local server instead of the release.
    mirror: Option<String>,
    publish: Box<dyn Fn(&State) + Send + Sync>,
    inner: Mutex<Inner>,
    idle: Condvar,
}

/// Holds the one operation slot; leaving frees it.
struct Slot<'a>(&'a Modules);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.inner.lock().unwrap().running = None;
        self.0.idle.notify_all();
    }
}

impl Modules {
    /// Modules pinned by `<resources>\optional-modules.json`, installed in `<userData>\modules`; `publish` receives the
    /// state of every operation on a module Settings lists.
    pub fn new(resources: &Path, user_data: &Path, publish: impl Fn(&State) + Send + Sync + 'static) -> Self {
        Self {
            descriptor: resources.join(DESCRIPTOR_FILE),
            root: user_data.join("modules"),
            mirror: None,
            publish: Box::new(publish),
            inner: Mutex::new(Inner { state: State::default(), running: None, closed: false, list: None }),
            idle: Condvar::new(),
        }
    }

    /// Serve every piece from `base` (`http://127.0.0.1:<port>/`), for tests.
    #[cfg(test)]
    fn mirrored(mut self, base: String) -> Self {
        self.mirror = Some(base);
        self
    }

    /// `<userData>\modules`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The pinned modules, read once.
    pub fn list(&self) -> Result<Arc<Vec<Module>>, ModuleError> {
        if let Some(list) = self.inner.lock().unwrap().list.clone() {
            return Ok(list);
        }
        let text = match fs::read_to_string(&self.descriptor) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(ModuleError::Unavailable),
            Err(error) => return Err(io_error(&self.descriptor, error)),
        };
        let list = Arc::new(parse_descriptor(&text)?);
        self.inner.lock().unwrap().list = Some(list.clone());
        Ok(list)
    }

    pub fn module(&self, id: &str) -> Result<Module, ModuleError> {
        self.list()?.iter().find(|module| module.id == id).cloned().ok_or_else(|| ModuleError::NotOffered(id.to_owned()))
    }

    /// Where a module is once installed: its folder, or the archive of a file module.
    pub fn path(&self, id: &str) -> Result<PathBuf, ModuleError> {
        let module = self.module(id)?;
        Ok(match module.kind {
            Kind::Directory => self.root.join(id),
            Kind::File => self.root.join(id).join(module.file),
        })
    }

    /// Download locations of a module's pieces, for a machine that fetches the archive itself (an SSH server); their
    /// concatenation is the archive. Empty when this build offers no download.
    pub fn pieces(&self, id: &str) -> Value {
        let Ok(module) = self.module(id) else { return json!([]) };
        Value::Array(module.pieces.iter().map(|piece| json!({ "url": format!("{}{}", module.base_url, piece.file), "bytes": piece.bytes, "sha256": piece.sha256 })).collect())
    }

    /// The archive of an installed file module.
    pub fn archive(&self, id: &str) -> Option<PathBuf> {
        let module = self.module(id).ok()?;
        (module.kind == Kind::File && self.is_installed(&module).ok()?).then(|| self.root.join(id).join(&module.file))
    }

    /// Whether the installed module is the archive this version pins.
    pub fn installed(&self, id: &str) -> Result<bool, ModuleError> {
        self.is_installed(&self.module(id)?)
    }

    /// The marker names this module's archive, and a file module's archive is there.
    fn is_installed(&self, module: &Module) -> Result<bool, ModuleError> {
        let folder = self.root.join(&module.id);
        let marker = folder.join(MARKER);
        download::check_path(&marker)?;
        let text = match fs::read(&marker) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(io_error(&marker, error)),
        };
        // A damaged marker is a module to install again.
        let Ok(marker) = serde_json::from_slice::<Marker>(&text) else { return Ok(false) };
        let named = marker.version == 1 && marker.id == module.id && marker.sha256 == module.sha256;
        Ok(named && (module.kind == Kind::Directory || fs::symlink_metadata(folder.join(&module.file)).is_ok_and(|metadata| metadata.is_file())))
    }

    /// The modules Settings lists, with their sizes and whether they are installed.
    pub fn status(&self) -> Result<Vec<Status>, ModuleError> {
        self.list()?
            .iter()
            .filter(|module| !internal(&module.id))
            .map(|module| {
                Ok(Status { id: module.id.clone(), installed: self.is_installed(module)?, download_bytes: module.bytes, unpacked_bytes: module.unpacked_bytes })
            })
            .collect()
    }

    /// Progress of the latest operation, for a page opened while it runs.
    pub fn state(&self) -> State {
        self.inner.lock().unwrap().state.clone()
    }

    fn publish(&self, state: State) {
        self.inner.lock().unwrap().state = state.clone();
        (self.publish)(&state);
    }

    fn slot(&self, busy: ModuleError, refuse_when_closed: bool) -> Result<(Slot<'_>, Arc<AtomicBool>), ModuleError> {
        let mut inner = self.inner.lock().unwrap();
        if refuse_when_closed && inner.closed {
            return Err(ModuleError::Closing);
        }
        if inner.running.is_some() {
            return Err(busy);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        inner.running = Some(cancel.clone());
        Ok((Slot(self), cancel))
    }

    fn options(&self, cancel: Arc<AtomicBool>) -> Options {
        Options { allow_loopback_http: self.mirror.is_some(), ..Options::new(cancel) }
    }

    fn url(&self, module: &Module, piece: &Piece) -> String {
        format!("{}{}", self.mirror.as_deref().unwrap_or(&module.base_url), piece.file)
    }

    /// Download, verify and install one module; an installed matching module needs no network. `progress` gets the
    /// bytes downloaded so far and the archive's size, also for the internal modules.
    pub fn install(&self, id: &str, progress: &dyn Fn(u64, u64)) -> Result<(), ModuleError> {
        let (_slot, cancel) = self.slot(ModuleError::Busy, true)?;
        let module = self.module(id)?;
        let published = Cell::new(None::<Instant>);
        let report = |phase: Phase, completed: u64| {
            // Download progress is published a few times a second; every other step at once.
            let due = phase != Phase::Downloading || completed == 0 || completed >= module.bytes || published.get().is_none_or(|at| at.elapsed() >= PUBLISH_INTERVAL);
            if !internal(id) && due {
                published.set(Some(Instant::now()));
                self.publish(State { phase, module: Some(id.to_owned()), completed_bytes: completed, total_bytes: module.bytes, error: None });
            }
            progress(completed, module.bytes);
        };
        let Err(error) = self.run(&module, &cancel, &report) else { return Ok(()) };
        let cancelled = cancel.load(Ordering::Relaxed);
        let error = if cancelled { ModuleError::Download(DownloadError::Cancelled) } else { error };
        if !internal(id) {
            let state = self.state();
            let phase = if cancelled { Phase::Cancelled } else { Phase::Error };
            self.publish(State { phase, module: Some(id.to_owned()), error: (!cancelled).then(|| error.clone()), ..state });
        }
        Err(error)
    }

    fn run(&self, module: &Module, cancel: &Arc<AtomicBool>, report: &dyn Fn(Phase, u64)) -> Result<(), ModuleError> {
        if self.is_installed(module)? {
            report(Phase::Complete, module.bytes);
            return Ok(());
        }
        let downloads = self.root.join(DOWNLOADS);
        download::check_path(&downloads)?;
        fs::create_dir_all(&downloads).map_err(|error| io_error(&downloads, error))?;
        report(Phase::Downloading, 0);
        let options = self.options(cancel.clone());
        let archive = self.download_archive(module, &downloads, &options, &|completed, _| report(Phase::Downloading, completed))?;
        cancelled(cancel)?;
        report(Phase::Installing, module.bytes);
        let staging = self.root.join(format!(".staging-{}-{}", module.id, uuid()));
        download::check_path(&staging)?;
        fs::create_dir_all(&staging).map_err(|error| io_error(&staging, error))?;
        let swapped = self.swap_in(module, &archive, &staging, cancel);
        if fs::symlink_metadata(&staging).is_ok() {
            let _ = remove_all(&staging);
        }
        swapped?;
        match fs::remove_file(&archive) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(io_error(&archive, error)),
            _ => {}
        }
        report(Phase::Complete, module.bytes);
        Ok(())
    }

    /// Unpack (or move) the archive into `staging`, mark it, and put it in place of the module's folder.
    fn swap_in(&self, module: &Module, archive: &Path, staging: &Path, cancel: &AtomicBool) -> Result<(), ModuleError> {
        match module.kind {
            Kind::Directory if module.file.ends_with(".tar.gz") => {
                tarball::unpack_gz(archive, staging, cancel).map_err(|error| match error {
                    TarError::Cancelled => ModuleError::Download(DownloadError::Cancelled),
                    TarError::Io(error) => io_error(staging, error),
                    error => ModuleError::Archive(error.to_string()),
                })?;
            }
            Kind::Directory => return Err(ModuleError::Archive(format!("{} is an xz archive, which Rainy unpacks only inside Linux", module.file))),
            Kind::File => download::rename(archive, &staging.join(&module.file)).map_err(|error| io_error(archive, error))?,
        }
        write_marker(&staging.join(MARKER), &Marker { version: 1, id: module.id.clone(), sha256: module.sha256.clone() })?;
        cancelled(cancel)?;
        let target = self.root.join(&module.id);
        let retired = self.root.join(format!(".retired-{}-{}", module.id, uuid()));
        if fs::symlink_metadata(&target).is_ok() {
            download::rename(&target, &retired).map_err(|error| io_error(&target, error))?;
        }
        download::rename(staging, &target).map_err(|error| io_error(staging, error))?;
        if fs::symlink_metadata(&retired).is_ok()
            && let Err(error) = remove_all(&retired)
        {
            log::warn!("An earlier module copy was not removed: {}: {error}", retired.display());
        }
        Ok(())
    }

    /// The verified archive in `downloads` (`downloadReleaseFile`): reused when it is there, downloaded straight when
    /// it is one piece, else joined from its pieces, which are reused when complete and removed once joined.
    fn download_archive(&self, module: &Module, downloads: &Path, options: &Options, progress: &dyn Fn(u64, u64)) -> Result<PathBuf, ModuleError> {
        let target = downloads.join(&module.file);
        let whole = expected(module.bytes, &module.sha256)?;
        if download::matches(&target, &whole, &options.cancel)? {
            progress(module.bytes, module.bytes);
            return Ok(target);
        }
        if let [single] = module.pieces.as_slice()
            && single.sha256 == module.sha256
        {
            download::download(&self.url(module, single), &target, &whole, options, progress)?;
            return Ok(target);
        }
        let (mut paths, mut offset) = (Vec::new(), 0);
        for piece in &module.pieces {
            let path = downloads.join(&piece.file);
            let before = offset;
            download::download(&self.url(module, piece), &path, &expected(piece.bytes, &piece.sha256)?, options, &|completed, _| progress(before + completed, module.bytes))?;
            paths.push(path);
            offset += piece.bytes;
        }
        let partial = download::partial_path(&target);
        download::check_path(&partial)?;
        join(&paths, &partial, &options.cancel)?;
        if !download::matches(&partial, &whole, &options.cancel)? {
            let _ = fs::remove_file(&partial);
            return Err(DownloadError::FileMismatch.into());
        }
        download::rename(&partial, &target).map_err(|error| io_error(&target, error))?;
        for path in paths {
            fs::remove_file(&path).map_err(|error| io_error(&path, error))?;
        }
        Ok(target)
    }

    /// Delete an installed module; the caller stops the programs that use it first.
    pub fn remove(&self, id: &str) -> Result<(), ModuleError> {
        let (_slot, _) = self.slot(ModuleError::RemoveBusy, false)?;
        self.module(id)?;
        let target = self.root.join(id);
        download::check_path(&target)?;
        let retired = self.root.join(format!(".retired-{id}-{}", uuid()));
        if fs::symlink_metadata(&target).is_ok() {
            download::rename(&target, &retired).map_err(|error| io_error(&target, error))?;
        }
        if fs::symlink_metadata(&retired).is_ok() {
            remove_all(&retired).map_err(|error| io_error(&retired, error))?;
        }
        if !internal(id) {
            self.publish(State { module: Some(id.to_owned()), ..State::default() });
        }
        Ok(())
    }

    /// Remove what interrupted installations left and downloads this version does not pin: `.staging-*` and
    /// `.retired-*` folders, and files in `.downloads` other than pinned archives, pieces and their partial files.
    /// Nothing is removed while an operation runs.
    pub fn clean(&self) -> Result<(), ModuleError> {
        let names: Vec<String> = match fs::read_dir(&self.root) {
            Ok(entries) => entries.flatten().filter_map(|entry| entry.file_name().into_string().ok()).collect(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_error(&self.root, error)),
        };
        let Ok((_slot, _)) = self.slot(ModuleError::Busy, false) else { return Ok(()) };
        let list = self.list()?;
        let pinned: HashSet<String> = list
            .iter()
            .flat_map(|module| [module.file.clone()].into_iter().chain(module.pieces.iter().map(|piece| piece.file.clone())))
            .flat_map(|name| [format!("{name}.partial"), name])
            .collect();
        for name in names.iter().filter(|name| name.starts_with(".staging-") || name.starts_with(".retired-")) {
            let path = self.root.join(name);
            remove_all(&path).map_err(|error| io_error(&path, error))?;
        }
        let downloads = self.root.join(DOWNLOADS);
        if !fs::symlink_metadata(&downloads).is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink()) {
            return Ok(());
        }
        for entry in fs::read_dir(&downloads).map_err(|error| io_error(&downloads, error))?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !pinned.contains(&name) && entry.file_type().is_ok_and(|kind| kind.is_file()) {
                fs::remove_file(entry.path()).map_err(|error| io_error(&entry.path(), error))?;
            }
        }
        Ok(())
    }

    /// Stop the running download; what was downloaded is kept for the next attempt.
    pub fn cancel(&self) {
        if let Some(cancel) = &self.inner.lock().unwrap().running {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Refuse new installations and stop the running one, as Rainy quits.
    pub fn close(&self) {
        self.inner.lock().unwrap().closed = true;
        self.cancel();
    }

    /// Wait up to `timeout` for the running operation to end; whether none runs.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let inner = self.inner.lock().unwrap();
        let (inner, _) = self.idle.wait_timeout_while(inner, timeout, |inner| inner.running.is_some()).unwrap();
        inner.running.is_none()
    }
}

fn cancelled(cancel: &AtomicBool) -> Result<(), ModuleError> {
    if cancel.load(Ordering::Relaxed) { Err(DownloadError::Cancelled.into()) } else { Ok(()) }
}

/// Concatenate the pieces into `partial`.
fn join(paths: &[PathBuf], partial: &Path, cancel: &AtomicBool) -> Result<(), ModuleError> {
    let mut output = File::create(partial).map_err(|error| io_error(partial, error))?;
    let mut buffer = vec![0u8; 1 << 20];
    for path in paths {
        let mut input = File::open(path).map_err(|error| io_error(path, error))?;
        loop {
            cancelled(cancel)?;
            let read = input.read(&mut buffer).map_err(|error| io_error(path, error))?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read]).map_err(|error| io_error(partial, error))?;
        }
    }
    output.sync_all().map_err(|error| io_error(partial, error))
}

/// Remove a folder and everything in it, or only the link when it is one.
fn remove_all(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        fs::remove_dir(path).or_else(|_| fs::remove_file(path))
    } else if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Write the marker in one step (`writeToolPackRecord`): pretty JSON with a trailing newline, through `<path>.<uuid>.tmp`.
fn write_marker(path: &Path, marker: &Marker) -> Result<(), ModuleError> {
    download::check_path(path)?;
    let mut text = serde_json::to_string_pretty(marker).map_err(|error| ModuleError::Invalid(error.to_string()))?;
    text.push('\n');
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.tmp", uuid()));
    let temporary = PathBuf::from(name);
    let written = fs::OpenOptions::new().write(true).create_new(true).open(&temporary).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        download::rename(&temporary, path)
    });
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written.map_err(|error| io_error(path, error))
}

/// A random version 4 UUID, as Node's `randomUUID` names staging folders.
fn uuid() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos());
        bytes = (nanos ^ u128::from(std::process::id()) << 96).to_le_bytes();
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// Download locations of a module's pieces, for a machine that fetches the archive itself (an SSH server); their
/// concatenation is the archive. Empty when this build offers no download.
pub fn pieces(resources: &Path, id: &str) -> Value {
    Modules::new(resources, Path::new(""), |_| {}).pieces(id)
}

/// The archive of a file module, when it is installed.
pub fn archive(resources: &Path, user_data: &Path, id: &str) -> Option<PathBuf> {
    Modules::new(resources, user_data, |_| {}).archive(id)
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;
    use crate::download::HashSink;
    use crate::tarball::build;

    const BASE: &str = "https://github.com/RainyMarks/Rainy/releases/download/v3.9.1-resources/";

    fn hex(bytes: &[u8]) -> String {
        let mut sink = HashSink::new(&Digest::Sha256([0; 32]));
        sink.update(bytes);
        let Digest::Sha256(digest) = sink.finish() else { unreachable!() };
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn php_archive() -> Vec<u8> {
        let mut tar = Vec::new();
        build::folder(&mut tar, "ext/");
        build::file(&mut tar, "php.exe", &[b'M'; 70_000]);
        build::file(&mut tar, "ext/php_curl.dll", &[3u8; 90_000]);
        build::gzip(&build::finish(tar))
    }

    /// A module entry whose archive is cut into `parts` pieces.
    fn entry(id: &str, file: &str, archive: &[u8], parts: usize) -> Value {
        let size = archive.len().div_ceil(parts);
        let pieces: Vec<Value> = archive
            .chunks(size)
            .enumerate()
            .map(|(index, chunk)| json!({ "file": format!("rainy-{}.{:03}", &hex(id.as_bytes())[..20], index + 1), "bytes": chunk.len(), "sha256": hex(chunk) }))
            .collect();
        let kind = if id == "linux-runtime" { "file" } else { "directory" };
        json!({ "id": id, "kind": kind, "file": file, "bytes": archive.len(), "sha256": hex(archive), "unpackedBytes": 160_000, "baseUrl": BASE, "pieces": pieces })
    }

    fn resources(modules: &[Value]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(DESCRIPTOR_FILE), json!({ "version": 1, "modules": modules }).to_string()).unwrap();
        dir
    }

    #[test]
    fn validates_the_descriptor_like_3x() {
        let php = entry("php", "php.tar.gz", &php_archive(), 2);
        let runtime = entry("linux-runtime", "linux-runtime.tar.xz", b"base", 1);
        let text = |modules: Vec<Value>| json!({ "version": 1, "modules": modules }).to_string();
        let parsed = parse_descriptor(&text(vec![php.clone(), runtime.clone()])).unwrap();
        assert_eq!(parsed[0].pieces.len(), 2);
        assert_eq!(parsed[1].kind, Kind::File);
        assert!(parse_descriptor(&json!({ "version": 1, "modules": [] }).to_string()).unwrap().is_empty());

        let changed = |module: &Value, key: &str, value: Value| {
            let mut module = module.clone();
            module[key] = value;
            module
        };
        let piece = |key: &str, value: Value| {
            let mut module = php.clone();
            module["pieces"][0][key] = value;
            module
        };
        let refused = [
            json!({ "version": 2, "modules": [] }).to_string(),
            json!({ "version": 1, "modules": [], "extra": true }).to_string(),
            json!({ "modules": [] }).to_string(),
            text(vec![php.clone(), php.clone()]),
            text(vec![changed(&php, "id", json!("node"))]),
            text(vec![changed(&php, "kind", json!("file"))]),
            text(vec![changed(&runtime, "kind", json!("directory"))]),
            text(vec![changed(&php, "kind", json!("folder"))]),
            text(vec![changed(&php, "file", json!("php.zip"))]),
            text(vec![changed(&php, "file", json!("PHP.tar.gz"))]),
            text(vec![changed(&php, "file", json!(".tar.gz"))]),
            text(vec![changed(&php, "file", json!("-php.tar.gz"))]),
            text(vec![changed(&php, "bytes", json!(0))]),
            text(vec![changed(&php, "bytes", json!(-1))]),
            text(vec![changed(&php, "bytes", json!(1.5))]),
            text(vec![changed(&php, "unpackedBytes", json!(9_007_199_254_740_992u64))]),
            text(vec![changed(&php, "sha256", json!("A".repeat(64)))]),
            text(vec![changed(&php, "baseUrl", json!("https://github.com/RainyMarks/Rainy/releases/download/v3.9.1-resources"))]),
            text(vec![changed(&php, "baseUrl", json!("https://github.com/Other/Rainy/releases/download/v3.9.1-resources/"))]),
            text(vec![changed(&php, "baseUrl", json!("https://github.com/RainyMarks/Rainy/releases/download/v3.9-resources/"))]),
            text(vec![changed(&php, "baseUrl", json!("http://github.com/RainyMarks/Rainy/releases/download/v3.9.1-resources/"))]),
            text(vec![changed(&php, "pieces", json!([]))]),
            text(vec![changed(&php, "extra", json!(1))]),
            text(vec![piece("file", json!("rainy-0123456789abcdef0123.1"))]),
            text(vec![piece("file", json!("rainy-0123456789ABCDEF0123.001"))]),
            text(vec![piece("bytes", json!(1))]),
            text(vec![piece("sha256", json!("0"))]),
            text(vec![piece("extra", json!(1))]),
        ];
        for text in refused {
            assert!(matches!(parse_descriptor(&text), Err(ModuleError::Invalid(_))), "{text}");
        }
        assert!(release_url("https://github.com/RainyMarks/RainyAgent/releases/download/v1.0.0-resources/"));
    }

    /// The modules folder as Rainy 3.x leaves it.
    fn installed_by_3x(user_data: &Path, id: &str, sha256: &str, file: Option<&str>) {
        let folder = user_data.join("modules").join(id);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(MARKER), format!("{{\n  \"version\": 1,\n  \"id\": \"{id}\",\n  \"sha256\": \"{sha256}\"\n}}\n")).unwrap();
        if let Some(file) = file {
            fs::write(folder.join(file), b"base").unwrap();
        }
    }

    #[test]
    fn recognizes_modules_3x_installed() {
        let php = entry("php", "php.tar.gz", &php_archive(), 1);
        let runtime = entry("linux-runtime", "linux-runtime.tar.xz", b"base", 1);
        let resources = resources(&[php.clone(), runtime.clone()]);
        let user_data = tempfile::tempdir().unwrap();
        let modules = Modules::new(resources.path(), user_data.path(), |_| {});
        assert_eq!(modules.installed("php"), Ok(false));
        installed_by_3x(user_data.path(), "php", php["sha256"].as_str().unwrap(), None);
        installed_by_3x(user_data.path(), "linux-runtime", runtime["sha256"].as_str().unwrap(), Some("linux-runtime.tar.xz"));
        assert_eq!(modules.installed("php"), Ok(true));
        assert_eq!(modules.status().unwrap(), [Status { id: "php".into(), installed: true, download_bytes: php["bytes"].as_u64().unwrap(), unpacked_bytes: 160_000 }]);
        let archive = user_data.path().join("modules").join("linux-runtime").join("linux-runtime.tar.xz");
        assert_eq!(modules.archive("linux-runtime"), Some(archive.clone()));
        assert_eq!(super::archive(resources.path(), user_data.path(), "linux-runtime"), Some(archive.clone()));
        assert_eq!(modules.path("linux-runtime"), Ok(archive.clone()));
        assert_eq!(modules.path("php"), Ok(user_data.path().join("modules").join("php")));
        // Installing what is installed needs nothing.
        let progress = Mutex::new(Vec::new());
        modules.install("php", &|done, total| progress.lock().unwrap().push((done, total))).unwrap();
        assert_eq!(*progress.lock().unwrap(), [(php["bytes"].as_u64().unwrap(), php["bytes"].as_u64().unwrap())]);
        // Another pinned archive, or a file module without its archive, is not installed.
        installed_by_3x(user_data.path(), "php", &"0".repeat(64), None);
        assert_eq!(modules.installed("php"), Ok(false));
        fs::remove_file(&archive).unwrap();
        assert_eq!(modules.archive("linux-runtime"), None);
        fs::write(user_data.path().join("modules").join("php").join(MARKER), "{damaged").unwrap();
        assert_eq!(modules.installed("php"), Ok(false));
    }

    #[test]
    fn pieces_name_the_release_assets() {
        let php = entry("php", "php.tar.gz", &php_archive(), 2);
        let resources = resources(std::slice::from_ref(&php));
        let pieces = pieces(resources.path(), "php");
        assert_eq!(pieces.as_array().unwrap().len(), 2);
        assert_eq!(pieces[0]["url"], format!("{BASE}{}", php["pieces"][0]["file"].as_str().unwrap()));
        assert_eq!(pieces[1]["bytes"], php["pieces"][1]["bytes"]);
        assert_eq!(super::pieces(resources.path(), "strata"), json!([]));
    }

    #[test]
    fn development_builds_have_no_modules() {
        let resources = tempfile::tempdir().unwrap();
        let user_data = tempfile::tempdir().unwrap();
        let modules = Modules::new(resources.path(), user_data.path(), |_| {});
        assert_eq!(modules.status(), Err(ModuleError::Unavailable));
        assert_eq!(modules.install("php", &|_, _| {}), Err(ModuleError::Unavailable));
        assert_eq!(pieces(resources.path(), "linux-runtime"), json!([]));
        assert_eq!(archive(resources.path(), user_data.path(), "linux-runtime"), None);
        fs::write(resources.path().join(DESCRIPTOR_FILE), "{\"version\":1}").unwrap();
        assert!(matches!(Modules::new(resources.path(), user_data.path(), |_| {}).status(), Err(ModuleError::Invalid(_))));
    }

    #[test]
    fn cleans_leftovers_and_unpinned_downloads() {
        let php = entry("php", "php.tar.gz", &php_archive(), 2);
        let resources = resources(std::slice::from_ref(&php));
        let user_data = tempfile::tempdir().unwrap();
        let root = user_data.path().join("modules");
        let downloads = root.join(DOWNLOADS);
        fs::create_dir_all(root.join(".staging-php-1").join("deep")).unwrap();
        fs::create_dir_all(root.join(".retired-php-2")).unwrap();
        fs::create_dir_all(root.join("php")).unwrap();
        fs::create_dir_all(&downloads).unwrap();
        let piece = php["pieces"][0]["file"].as_str().unwrap().to_owned();
        let kept = [piece.clone(), format!("{piece}.partial"), "php.tar.gz".into(), "php.tar.gz.partial".into()];
        for name in kept.iter().chain(&["strata-runtime.tar.gz".to_owned(), "rainy-00000000000000000000.001".to_owned()]) {
            fs::write(downloads.join(name), b"x").unwrap();
        }
        Modules::new(resources.path(), user_data.path(), |_| {}).clean().unwrap();
        let mut left: Vec<String> = fs::read_dir(&downloads).unwrap().map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        let mut kept = kept.to_vec();
        kept.sort();
        assert_eq!(left, kept);
        assert!(!root.join(".staging-php-1").exists());
        assert!(!root.join(".retired-php-2").exists());
        assert!(root.join("php").exists());
    }

    #[test]
    fn markers_are_written_as_3x_writes_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MARKER);
        write_marker(&path, &Marker { version: 1, id: "php".into(), sha256: "ab".repeat(32) }).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{{\n  \"version\": 1,\n  \"id\": \"php\",\n  \"sha256\": \"{}\"\n}}\n", "ab".repeat(32)));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        let id = uuid();
        assert_eq!((id.len(), &id[14..15]), (36, "4"));
    }

    #[cfg(windows)]
    mod network {
        use ::core::prelude::v1::test;
        use std::time::Instant;

        use super::super::*;
        use super::{entry, hex, installed_by_3x, php_archive, resources};
        use crate::test_server::{Reply, Server};

        /// Serves the pieces of `modules` by name.
        fn serve(modules: &[(Value, Vec<u8>)]) -> Server {
            let mut files = Vec::new();
            for (module, archive) in modules {
                let mut offset = 0;
                for piece in module["pieces"].as_array().unwrap() {
                    let bytes = piece["bytes"].as_u64().unwrap() as usize;
                    files.push((format!("/{}", piece["file"].as_str().unwrap()), archive[offset..offset + bytes].to_vec()));
                    offset += bytes;
                }
            }
            Server::start(move |seen| match files.iter().find(|(path, _)| *path == seen.path) {
                Some((_, bytes)) => Reply::ranged(seen, bytes),
                None => Reply::status(404),
            })
        }

        fn recorded() -> (Arc<Mutex<Vec<State>>>, impl Fn(&State) + Send + Sync + 'static) {
            let states = Arc::new(Mutex::new(Vec::new()));
            let sink = states.clone();
            (states, move |state: &State| sink.lock().unwrap().push(state.clone()))
        }

        #[test]
        fn installs_a_module_from_its_pieces() {
            let archive = php_archive();
            let php = entry("php", "php.tar.gz", &archive, 3);
            let resources = resources(std::slice::from_ref(&php));
            let server = serve(&[(php.clone(), archive.clone())]);
            let user_data = tempfile::tempdir().unwrap();
            let (states, publish) = recorded();
            let modules = Modules::new(resources.path(), user_data.path(), publish).mirrored(server.url("/"));
            // An older copy is replaced.
            installed_by_3x(user_data.path(), "php", &"0".repeat(64), None);
            fs::write(user_data.path().join("modules").join("php").join("old.dll"), b"old").unwrap();
            let progress = Mutex::new(Vec::new());
            modules.install("php", &|done, total| progress.lock().unwrap().push((done, total))).unwrap();
            let root = user_data.path().join("modules");
            assert_eq!(fs::read(root.join("php").join("php.exe")).unwrap(), vec![b'M'; 70_000]);
            assert_eq!(fs::read(root.join("php").join("ext").join("php_curl.dll")).unwrap(), vec![3u8; 90_000]);
            assert!(!root.join("php").join("old.dll").exists());
            assert_eq!(
                fs::read_to_string(root.join("php").join(MARKER)).unwrap(),
                format!("{{\n  \"version\": 1,\n  \"id\": \"php\",\n  \"sha256\": \"{}\"\n}}\n", hex(&archive))
            );
            assert_eq!(modules.installed("php"), Ok(true));
            // The pieces and the joined archive are gone, and nothing is left in staging.
            assert_eq!(fs::read_dir(root.join(DOWNLOADS)).unwrap().count(), 0);
            let names: Vec<String> = fs::read_dir(&root).unwrap().map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect();
            assert_eq!(names.len(), 2, "{names:?}");
            let progress = progress.into_inner().unwrap();
            assert!(progress.windows(2).all(|pair| pair[0].0 <= pair[1].0));
            assert_eq!(progress.last(), Some(&(archive.len() as u64, archive.len() as u64)));
            let phases: Vec<Phase> = states.lock().unwrap().iter().map(|state| state.phase).collect();
            assert_eq!(phases.first(), Some(&Phase::Downloading));
            assert_eq!(&phases[phases.len() - 2..], [Phase::Installing, Phase::Complete]);
            assert_eq!(modules.state().module.as_deref(), Some("php"));
            assert_eq!(server.seen().len(), 3);
            modules.remove("php").unwrap();
            assert!(!root.join("php").exists());
            assert_eq!(modules.state(), State { module: Some("php".into()), ..State::default() });
        }

        #[test]
        fn installs_the_linux_runtime_as_a_file() {
            let archive = vec![9u8; 200_000];
            let runtime = entry("linux-runtime", "linux-runtime.tar.xz", &archive, 1);
            let resources = resources(std::slice::from_ref(&runtime));
            let server = serve(&[(runtime, archive.clone())]);
            let user_data = tempfile::tempdir().unwrap();
            let (states, publish) = recorded();
            let modules = Modules::new(resources.path(), user_data.path(), publish).mirrored(server.url("/"));
            let last = Mutex::new((0, 0));
            modules.install("linux-runtime", &|done, total| *last.lock().unwrap() = (done, total)).unwrap();
            assert_eq!(*last.lock().unwrap(), (200_000, 200_000));
            let path = modules.archive("linux-runtime").unwrap();
            assert_eq!(fs::read(&path).unwrap(), archive);
            assert_eq!(modules.path("linux-runtime"), Ok(path));
            // Internal modules publish nothing.
            assert!(states.lock().unwrap().is_empty());
            modules.remove("linux-runtime").unwrap();
            assert_eq!(modules.archive("linux-runtime"), None);
        }

        #[test]
        fn reuses_a_verified_download_without_the_network() {
            let archive = php_archive();
            let php = entry("php", "php.tar.gz", &archive, 2);
            let resources = resources(&[php]);
            let server = Server::start(|_| Reply::status(500));
            let user_data = tempfile::tempdir().unwrap();
            let downloads = user_data.path().join("modules").join(DOWNLOADS);
            fs::create_dir_all(&downloads).unwrap();
            fs::write(downloads.join("php.tar.gz"), &archive).unwrap();
            let modules = Modules::new(resources.path(), user_data.path(), |_| {}).mirrored(server.url("/"));
            modules.install("php", &|_, _| {}).unwrap();
            assert!(server.seen().is_empty());
            assert_eq!(modules.installed("php"), Ok(true));
        }

        #[test]
        fn a_cancelled_download_resumes_on_retry() {
            let archive: Vec<u8> = (0..300_000u32).map(|index| (index % 253) as u8).collect();
            let runtime = entry("linux-runtime", "linux-runtime.tar.xz", &archive, 1);
            let resources = resources(std::slice::from_ref(&runtime));
            let served = archive.clone();
            let stalls = Arc::new(AtomicBool::new(true));
            let stalling = stalls.clone();
            let server = Server::start(move |seen| {
                let reply = Reply::ranged(seen, &served);
                if stalling.swap(false, Ordering::Relaxed) { reply.stall_after(120_000, Duration::from_secs(20)) } else { reply }
            });
            let user_data = tempfile::tempdir().unwrap();
            let modules = Arc::new(Modules::new(resources.path(), user_data.path(), |_| {}).mirrored(server.url("/")));
            let running = modules.clone();
            let install = std::thread::spawn(move || running.install("linux-runtime", &|_, _| {}));
            let partial = user_data.path().join("modules").join(DOWNLOADS).join("linux-runtime.tar.xz.partial");
            let started = Instant::now();
            while fs::metadata(&partial).map_or(0, |metadata| metadata.len()) < 120_000 {
                assert!(started.elapsed() < Duration::from_secs(10), "the download did not start");
                std::thread::sleep(Duration::from_millis(20));
            }
            // One operation at a time.
            assert_eq!(modules.install("php", &|_, _| {}), Err(ModuleError::Busy));
            assert_eq!(modules.remove("linux-runtime"), Err(ModuleError::RemoveBusy));
            modules.cancel();
            assert!(install.join().unwrap().unwrap_err().cancelled());
            assert!(modules.wait_idle(Duration::from_secs(1)));
            assert_eq!(fs::metadata(&partial).unwrap().len(), 120_000);
            modules.install("linux-runtime", &|_, _| {}).unwrap();
            assert_eq!(server.seen()[1].range.as_deref(), Some("bytes=120000-"));
            assert_eq!(fs::read(modules.archive("linux-runtime").unwrap()).unwrap(), archive);
            modules.close();
            assert_eq!(modules.install("linux-runtime", &|_, _| {}), Err(ModuleError::Closing));
        }

        #[test]
        fn a_damaged_piece_is_an_error() {
            let archive = php_archive();
            let php = entry("php", "php.tar.gz", &archive, 1);
            let resources = resources(std::slice::from_ref(&php));
            let mut damaged = archive.clone();
            damaged[100] ^= 0xff;
            let server = serve(&[(php, damaged)]);
            let user_data = tempfile::tempdir().unwrap();
            let (states, publish) = recorded();
            let modules = Modules::new(resources.path(), user_data.path(), publish).mirrored(server.url("/"));
            let error = ModuleError::Download(DownloadError::Mismatch);
            assert_eq!(modules.install("php", &|_, _| {}), Err(error.clone()));
            let last = states.lock().unwrap().last().cloned().unwrap();
            assert_eq!((last.phase, last.error), (Phase::Error, Some(error)));
            assert_eq!(modules.installed("php"), Ok(false));
        }
    }
}
