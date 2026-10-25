//! "Export diagnostics" (3.x `src/main/diagnostics.ts`): one zip with versions, Rainy's and the Host's logs and the newest
//! WebView2 crash dumps, for the user to attach to a report. Nothing is uploaded. Logs are cut to their end and scrubbed
//! of keys, tokens, passwords and the Windows user name; chats and API keys are never included.

use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Datelike as _, Local, NaiveDateTime, Timelike as _, Utc};
use flate2::Compression;
use flate2::write::DeflateEncoder;
use regex::Regex;

use crate::locale::localized;
use crate::logging;

/// The context of a file that could not be read, as the export failure shows it.
fn reading(path: &Path) -> String {
    localized(&format!("无法读取 {}", path.display()), &format!("Reading {}", path.display()))
}

/// Rainy's own log and the one before it, and the Host's log, are cut to their last 4 MiB.
const LOG_BYTES: u64 = 4 * 1024 * 1024;
const SETTINGS_BYTES: u64 = 64 * 1024;
const DUMP_BYTES: u64 = 64 * 1024 * 1024;
/// The newest crash dumps packed.
const DUMPS: usize = 5;

/// One file to include.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticFile {
    /// Path inside the zip, with `/` separators.
    pub name: String,
    pub path: PathBuf,
    /// Only the last bytes of a longer file are kept.
    pub max_bytes: u64,
    /// Text to scrub of secrets; binary crash dumps are copied as they are.
    pub text: bool,
}

/// The account whose name and home folder logs must not show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub home: String,
}

impl Account {
    /// The signed-in user (Node's `userInfo().username` and `homedir()`).
    pub fn current() -> Self {
        let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default();
        Self { name: platform::user_name().unwrap_or_else(|| std::env::var("USERNAME").unwrap_or_default()), home }
    }
}

fn secrets() -> &'static [(Regex, &'static str)] {
    static SECRETS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    SECRETS.get_or_init(|| {
        // JavaScript's `\w` and `\b` are ASCII-only; so are these.
        [
            (r#"(?i)([?&](?:token|key|api[_-]?key|access_token|auth)=)[^&\s"']+"#, "${1}***"),
            (r"(?-u:\b)(Bearer|Basic)\s+[0-9A-Za-z_.~+/=-]{8,}", "${1} ***"),
            (r"(?-u:\b)(sk-(?:ant-)?)[0-9A-Za-z_-]{8,}", "${1}***"),
            (
                r#"(?i)("?(?:api[_-]?key|apikey|authorization|password|passwd|secret|token|x-api-key)"?\s*[:=]\s*)("[^"]*"|'[^']*'|[^\s,;}]+)"#,
                "${1}***",
            ),
            (r"([0-9A-Za-z_]+://[^\s:/@]+:)[^\s@/]+@", "${1}***@"),
        ]
        .into_iter()
        .map(|(pattern, replacement)| (Regex::new(pattern).expect("valid secret pattern"), replacement))
        .collect()
    })
}

/// Remove secrets, the home folder (shown as `~`) and the account name between path separators (shown as `<user>`)
/// from log text.
pub fn scrub(text: &str, account: &Account) -> String {
    let mut result = text.to_owned();
    for (pattern, replacement) in secrets() {
        if let std::borrow::Cow::Owned(replaced) = pattern.replace_all(&result, *replacement) {
            result = replaced;
        }
    }
    if account.home.chars().count() > 3
        && let Ok(home) = Regex::new(&format!("(?i){}", regex::escape(&account.home)))
    {
        result = home.replace_all(&result, regex::NoExpand("~")).into_owned();
    }
    if account.name.chars().count() > 1 {
        result = hide_user(&result, &account.name);
    }
    result
}

/// `name` wherever a `\` or `/` comes right before and right after it. The name cannot hold a separator, so a match
/// that is not between separators never overlaps one that is.
fn hide_user(text: &str, name: &str) -> String {
    let Ok(pattern) = Regex::new(&format!("(?i){}", regex::escape(name))) else { return text.to_owned() };
    let separator = |found: Option<char>| matches!(found, Some('\\' | '/'));
    let mut result = String::with_capacity(text.len());
    let mut last = 0;
    for found in pattern.find_iter(text) {
        if separator(text[..found.start()].chars().next_back()) && separator(text[found.end()..].chars().next()) {
            result.push_str(&text[last..found.start()]);
            result.push_str("<user>");
            last = found.end();
        }
    }
    result.push_str(&text[last..]);
    result
}

/// The newest `count` files whose names end with `suffix` (any case) in `directories`, newest first across all of them.
/// A missing folder has no files; links are not followed.
pub fn newest_files(directories: &[PathBuf], suffix: &str, count: usize) -> io::Result<Vec<PathBuf>> {
    let suffix = suffix.to_lowercase();
    let mut found = Vec::new();
    for directory in directories {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_name().to_string_lossy().to_lowercase().ends_with(&suffix) {
                continue;
            }
            if let Ok(info) = entry.metadata()
                && info.is_file()
            {
                found.push((info.modified().unwrap_or(SystemTime::UNIX_EPOCH), entry.path()));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(found.into_iter().take(count).map(|(_, path)| path).collect())
}

/// The last `max_bytes` of a file, or `None` when it does not exist.
fn tail(path: &Path, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let size = file.metadata()?.len();
    let length = size.min(max_bytes);
    file.seek(SeekFrom::Start(size - length))?;
    let mut data = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
    file.take(length).read_to_end(&mut data)?;
    Ok(Some(data))
}

/// What Rainy packs from its user-data folder: its own log and the one before it, the Host's log, `desktop.json`, and
/// the newest crash dumps of WebView2. Missing files are skipped when the zip is written.
pub fn standard_files(user_data: &Path) -> Result<Vec<DiagnosticFile>> {
    let file = |name: &str, path: PathBuf, max_bytes: u64, text: bool| DiagnosticFile { name: name.to_owned(), path, max_bytes, text };
    let rainy = user_data.join(logging::FILE_NAME);
    let mut older = rainy.clone().into_os_string();
    older.push(".1");
    let mut files = vec![
        file("logs/rainy.log", rainy, LOG_BYTES, true),
        file("logs/rainy.log.1", older.into(), LOG_BYTES, true),
        file("logs/host.log", user_data.join("host.log"), LOG_BYTES, true),
        file("desktop.json", user_data.join("desktop.json"), SETTINGS_BYTES, true),
    ];
    let reports = user_data.join("WebView2").join("EBWebView").join("Crashpad").join("reports");
    let dumps = newest_files(std::slice::from_ref(&reports), ".dmp", DUMPS).with_context(|| reading(&reports))?;
    for dump in dumps {
        let name = format!("crashes/{}", dump.file_name().unwrap_or_default().to_string_lossy());
        files.push(file(&name, dump, DUMP_BYTES, false));
    }
    Ok(files)
}

/// The file name the save dialog suggests: `Rainy-diagnostics-<UTC yyyyMMddHHmm>.zip`.
pub fn default_name(now: DateTime<Utc>) -> String {
    format!("Rainy-diagnostics-{}.zip", now.format("%Y%m%d%H%M"))
}

/// Write the diagnostics zip: `about.txt` with one `Key: value` line per entry, then the files that exist. The zip is
/// written beside `target` and moved over it when complete.
/// Returns the names packed.
pub fn export(target: &Path, about: &[(String, String)], files: &[DiagnosticFile], account: &Account) -> Result<Vec<String>> {
    let mut temporary = target.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let written = write(&temporary, about, files, account).and_then(|names| Ok(std::fs::rename(&temporary, target).map(|()| names)?));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written.with_context(|| localized(&format!("无法保存 {}", target.display()), &format!("Saving {}", target.display())))
}

fn write(path: &Path, about: &[(String, String)], files: &[DiagnosticFile], account: &Account) -> Result<Vec<String>> {
    let out = File::create(path)?;
    let mut zip = Zip::new(io::BufWriter::new(out), Local::now().naive_local());
    let mut names = vec!["about.txt".to_owned()];
    let lines: String = about.iter().map(|(key, value)| format!("{key}: {}\n", scrub(value, account))).collect();
    zip.add("about.txt", lines.as_bytes())?;
    for file in files {
        let Some(data) = tail(&file.path, file.max_bytes).with_context(|| reading(&file.path))? else { continue };
        if file.text {
            zip.add(&file.name, scrub(&String::from_utf8_lossy(&data), account).as_bytes())?;
        } else {
            zip.add(&file.name, &data)?;
        }
        names.push(file.name.clone());
    }
    let out = zip.finish()?;
    out.into_inner().map_err(io::IntoInnerError::into_error)?.sync_all()?;
    Ok(names)
}

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const END_OF_DIRECTORY: u32 = 0x0605_4b50;
/// Zip 2.0: deflate.
const VERSION: u16 = 20;
/// General purpose bit 11: names are UTF-8.
const UTF8_NAMES: u16 = 0x0800;
const DEFLATE: u16 = 8;

/// A zip written entry by entry: deflate, UTF-8 names, one modification time for every entry, no ZIP64 (the export stays
/// far below 4 GiB) and no folder entries.
pub struct Zip<W: Write> {
    out: W,
    central: Vec<u8>,
    offset: u64,
    count: u16,
    time: u16,
    day: u16,
}

/// MS-DOS time and date of a local time; years outside 1980–2107 are clamped.
pub fn dos_time(time: NaiveDateTime) -> (u16, u16) {
    let clock = (time.hour() << 11) | (time.minute() << 5) | (time.second() / 2);
    let year = time.year().clamp(1980, 2107) as u32 - 1980;
    let day = (year << 9) | (time.month() << 5) | time.day();
    (clock as u16, day as u16)
}

fn too_large() -> io::Error {
    io::Error::other("The diagnostics are too large for a zip file.")
}

/// A size or offset that fits the 32-bit fields; all ones would mean ZIP64.
fn field(value: u64) -> io::Result<u32> {
    u32::try_from(value).ok().filter(|value| *value != u32::MAX).ok_or_else(too_large)
}

impl<W: Write> Zip<W> {
    pub fn new(out: W, modified: NaiveDateTime) -> Self {
        let (time, day) = dos_time(modified);
        Self { out, central: Vec::new(), offset: 0, count: 0, time, day }
    }

    pub fn add(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data)?;
        let packed = encoder.finish()?;
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let (size, packed_size, offset) = (field(data.len() as u64)?, field(packed.len() as u64)?, field(self.offset)?);
        let name_length = u16::try_from(name.len()).map_err(|_| too_large())?;
        let count = self.count.checked_add(1).filter(|count| *count != u16::MAX).ok_or_else(too_large)?;
        // The local and the central header share the fields from "version needed" to the extra field's length.
        let mut shared = Vec::with_capacity(26);
        [VERSION, UTF8_NAMES, DEFLATE, self.time, self.day].into_iter().for_each(|value| put16(&mut shared, value));
        [crc.sum(), packed_size, size].into_iter().for_each(|value| put32(&mut shared, value));
        put16(&mut shared, name_length);
        put16(&mut shared, 0);
        let mut local = Vec::with_capacity(30 + name.len());
        put32(&mut local, LOCAL_HEADER);
        local.extend_from_slice(&shared);
        local.extend_from_slice(name.as_bytes());
        self.out.write_all(&local)?;
        self.out.write_all(&packed)?;
        let central = &mut self.central;
        put32(central, CENTRAL_HEADER);
        // Made by: MS-DOS, zip 2.0.
        put16(central, VERSION);
        central.extend_from_slice(&shared);
        // No comment, disk 0, no internal or external attributes, then where the local header is.
        [0, 0, 0].into_iter().for_each(|value| put16(central, value));
        put32(central, 0);
        put32(central, offset);
        central.extend_from_slice(name.as_bytes());
        self.offset += (local.len() + packed.len()) as u64;
        self.count = count;
        Ok(())
    }

    /// Write the central directory and its end; returns the writer.
    pub fn finish(mut self) -> io::Result<W> {
        let (size, offset) = (field(self.central.len() as u64)?, field(self.offset)?);
        let mut end = Vec::with_capacity(22);
        put32(&mut end, END_OF_DIRECTORY);
        // Disk 0, which holds the whole directory.
        [0, 0, self.count, self.count].into_iter().for_each(|value| put16(&mut end, value));
        put32(&mut end, size);
        put32(&mut end, offset);
        // No comment.
        put16(&mut end, 0);
        self.out.write_all(&self.central)?;
        self.out.write_all(&end)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

fn put16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Windows as `<product> <release> (<major>.<minor>.<build>.<revision>, <architecture>)`, such as
/// `Windows 11 Pro 25H2 (10.0.26200.6725, x64)`.
pub fn windows_version() -> String {
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    match platform::windows_version() {
        Some(version) => {
            let mut product = version.product.unwrap_or_else(|| "Windows".to_owned());
            // The registry still says "Windows 10" on Windows 11.
            if version.build >= 22000
                && let Some(edition) = product.strip_prefix("Windows 10")
            {
                product = format!("Windows 11{edition}");
            }
            let release = version.release.map(|release| format!(" {release}")).unwrap_or_default();
            let revision = version.revision.map(|revision| format!(".{revision}")).unwrap_or_default();
            format!("{product}{release} ({}.{}.{}{revision}, {architecture})", version.major, version.minor, version.build)
        }
        None => format!("{} ({architecture})", std::env::consts::OS),
    }
}

/// Installed and free memory, as `<N> GiB, <M> GiB free`.
pub fn memory() -> String {
    let gib = |bytes: u64| (bytes as f64 / f64::from(1u32 << 30)).round();
    match platform::memory() {
        Some((total, free)) => format!("{} GiB, {} GiB free", gib(total), gib(free)),
        None => "unknown".to_owned(),
    }
}

/// The user's Downloads folder, where the zip is suggested.
pub fn downloads_folder() -> PathBuf {
    platform::downloads_folder().unwrap_or_else(|| PathBuf::from(Account::current().home).join("Downloads"))
}

/// Show `path` selected in an Explorer window (3.x `shell.showItemInFolder`).
pub fn reveal(path: &Path) -> io::Result<()> {
    platform::reveal(path)
}

/// What Windows reports about itself.
#[derive(Debug, Default)]
struct WindowsVersion {
    product: Option<String>,
    release: Option<String>,
    major: u32,
    minor: u32,
    build: u32,
    revision: Option<u32>,
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::{Path, PathBuf};

    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX, OSVERSIONINFOW};
    use windows_sys::Win32::System::WindowsProgramming::GetUserNameW;
    use windows_sys::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::WindowsVersion;

    const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

    use crate::text::wide;

    fn until_nul(units: &[u16]) -> String {
        let end = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
        String::from_utf16_lossy(&units[..end])
    }

    fn registry_text(name: &str) -> Option<String> {
        let (key, value) = (wide(CURRENT_VERSION), wide(name));
        let mut buffer = vec![0u16; 256];
        let mut size = u32::try_from(buffer.len() * 2).ok()?;
        // SAFETY: both names end with NUL, and `size` is the buffer's length in bytes.
        let status = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buffer.as_mut_ptr().cast(), &mut size) };
        let text = until_nul(&buffer[..(size as usize / 2).min(buffer.len())]);
        (status == ERROR_SUCCESS && !text.trim().is_empty()).then(|| text.trim().to_owned())
    }

    fn registry_number(name: &str) -> Option<u32> {
        let (key, value) = (wide(CURRENT_VERSION), wide(name));
        let mut number = 0u32;
        let mut size = 4u32;
        // SAFETY: both names end with NUL; a DWORD is read into `number`.
        let status = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&raw mut number).cast(), &mut size) };
        (status == ERROR_SUCCESS).then_some(number)
    }

    pub fn windows_version() -> Option<WindowsVersion> {
        // SAFETY: OSVERSIONINFOW is plain data; its size is set before the call.
        let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
        info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
        // SAFETY: `info` is a valid, sized OSVERSIONINFOW.
        if unsafe { RtlGetVersion(&mut info) } != 0 {
            return None;
        }
        Some(WindowsVersion {
            product: registry_text("ProductName"),
            release: registry_text("DisplayVersion").or_else(|| registry_text("ReleaseId")),
            major: info.dwMajorVersion,
            minor: info.dwMinorVersion,
            build: info.dwBuildNumber,
            revision: registry_number("UBR"),
        })
    }

    pub fn memory() -> Option<(u64, u64)> {
        // SAFETY: MEMORYSTATUSEX is plain data; its size is set before the call.
        let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        // SAFETY: `status` is a valid, sized MEMORYSTATUSEX.
        (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some((status.ullTotalPhys, status.ullAvailPhys))
    }

    pub fn downloads_folder() -> Option<PathBuf> {
        let mut path = std::ptr::null_mut();
        // SAFETY: the known folder's path is allocated by the shell and freed below.
        let result = unsafe { SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut path) };
        let folder = (result >= 0 && !path.is_null()).then(|| {
            // SAFETY: on success `path` is a NUL-terminated wide string.
            let length = (0..).take_while(|&index| unsafe { *path.add(index) } != 0).count();
            // SAFETY: `length` units were just read.
            PathBuf::from(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(path, length) }))
        });
        // SAFETY: allocated by SHGetKnownFolderPath (even on failure), freed once.
        unsafe { CoTaskMemFree(path.cast()) };
        folder
    }

    pub fn user_name() -> Option<String> {
        let mut buffer = [0u16; 257];
        let mut size = buffer.len() as u32;
        // SAFETY: `size` is the buffer's length in characters.
        let name = (unsafe { GetUserNameW(buffer.as_mut_ptr(), &mut size) } != 0).then(|| until_nul(&buffer));
        name.filter(|name| !name.is_empty())
    }

    /// An item list from `SHParseDisplayName`, freed when dropped.
    struct ItemList(*mut ITEMIDLIST);

    impl ItemList {
        fn parse(path: &Path) -> io::Result<Self> {
            let name = wide_path(path);
            let mut list = std::ptr::null_mut();
            // SAFETY: `name` ends with NUL; the list is owned by the result.
            let result = unsafe { SHParseDisplayName(name.as_ptr(), std::ptr::null_mut(), &mut list, 0, std::ptr::null_mut()) };
            if result < 0 {
                return Err(io::Error::from_raw_os_error(result));
            }
            Ok(Self(list))
        }
    }

    impl Drop for ItemList {
        fn drop(&mut self) {
            // SAFETY: allocated by SHParseDisplayName, freed once.
            unsafe { CoTaskMemFree(self.0.cast_const().cast()) };
        }
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain([0]).collect()
    }

    /// Electron's `ShowItemInFolder`: an Explorer window already showing the folder is reused. The shell is called from a
    /// thread of its own, in a single-threaded COM apartment.
    pub fn reveal(path: &Path) -> io::Result<()> {
        let path = path.to_owned();
        std::thread::Builder::new()
            .name("reveal".to_owned())
            .spawn(move || {
                // SAFETY: undone below when it succeeded; every shell object is released before.
                let initialized = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
                if initialized < 0 {
                    return Err(io::Error::from_raw_os_error(initialized));
                }
                let shown = show_in_folder(&path);
                // SAFETY: COM was initialized on this thread above.
                unsafe { CoUninitialize() };
                shown
            })?
            .join()
            .map_err(|_| io::Error::other("Explorer could not be asked to show the file"))?
    }

    fn show_in_folder(path: &Path) -> io::Result<()> {
        let folder = path.parent().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let (folder_item, file_item) = (ItemList::parse(folder)?, ItemList::parse(path)?);
        let selected = [file_item.0.cast_const()];
        // SAFETY: both item lists live until the end of this function.
        let result = unsafe { SHOpenFolderAndSelectItems(folder_item.0, 1, selected.as_ptr(), 0) };
        if result >= 0 {
            return Ok(());
        }
        // Electron and GPUI: this fails with "file not found" on some systems although the file is there; the folder is
        // then opened without selecting it.
        if result == ERROR_FILE_NOT_FOUND as i32 || result == (0x8007_0000 | ERROR_FILE_NOT_FOUND) as i32 {
            let (open, folder) = (wide("open"), wide_path(folder));
            // SAFETY: both strings end with NUL.
            let opened = unsafe { ShellExecuteW(std::ptr::null_mut(), open.as_ptr(), folder.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
            // Success is a value above 32.
            return if opened as usize > 32 { Ok(()) } else { Err(io::Error::last_os_error()) };
        }
        Err(io::Error::from_raw_os_error(result))
    }
}

#[cfg(not(windows))]
mod platform {
    use std::io;
    use std::path::{Path, PathBuf};

    use super::WindowsVersion;

    pub fn windows_version() -> Option<WindowsVersion> {
        None
    }

    pub fn memory() -> Option<(u64, u64)> {
        None
    }

    pub fn downloads_folder() -> Option<PathBuf> {
        None
    }

    pub fn user_name() -> Option<String> {
        None
    }

    pub fn reveal(_: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;
    use std::collections::BTreeMap;
    use std::time::Duration;

    use chrono::{NaiveDate, TimeZone as _};
    use flate2::read::DeflateDecoder;

    use super::*;

    /// An entry read back through the central directory.
    #[derive(Debug)]
    struct Entry {
        name: String,
        data: Vec<u8>,
        time: u16,
        day: u16,
    }

    fn u16_at(archive: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([archive[at], archive[at + 1]])
    }

    fn u32_at(archive: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([archive[at], archive[at + 1], archive[at + 2], archive[at + 3]])
    }

    /// Read every entry back through the central directory, checking that each local header agrees with it and that
    /// each CRC and size is right.
    fn unzip(archive: &[u8]) -> Vec<Entry> {
        let end = archive.len() - 22;
        assert_eq!(u32_at(archive, end), END_OF_DIRECTORY);
        assert_eq!((u16_at(archive, end + 4), u16_at(archive, end + 6), u16_at(archive, end + 20)), (0, 0, 0));
        let count = usize::from(u16_at(archive, end + 10));
        assert_eq!(usize::from(u16_at(archive, end + 8)), count);
        let mut at = u32_at(archive, end + 16) as usize;
        assert_eq!(at + u32_at(archive, end + 12) as usize, end);
        let mut entries = Vec::new();
        for _ in 0..count {
            assert_eq!(u32_at(archive, at), CENTRAL_HEADER);
            assert_eq!((u16_at(archive, at + 4), u16_at(archive, at + 6)), (20, 20));
            let (flags, method, time, day) = (u16_at(archive, at + 8), u16_at(archive, at + 10), u16_at(archive, at + 12), u16_at(archive, at + 14));
            assert_eq!((flags, method), (0x0800, 8));
            let (crc, packed, size) = (u32_at(archive, at + 16), u32_at(archive, at + 20) as usize, u32_at(archive, at + 24) as usize);
            let (name_length, extra, comment) = (usize::from(u16_at(archive, at + 28)), usize::from(u16_at(archive, at + 30)), usize::from(u16_at(archive, at + 32)));
            let local = u32_at(archive, at + 42) as usize;
            let name = &archive[at + 46..at + 46 + name_length];
            assert_eq!(u32_at(archive, local), LOCAL_HEADER);
            assert_eq!(archive[local + 4..local + 26], archive[at + 6..at + 28], "the local header repeats the central one");
            assert_eq!(usize::from(u16_at(archive, local + 26)), name_length);
            assert_eq!(&archive[local + 30..local + 30 + name_length], name);
            let start = local + 30 + name_length + usize::from(u16_at(archive, local + 28));
            let mut data = Vec::new();
            DeflateDecoder::new(&archive[start..start + packed]).read_to_end(&mut data).unwrap();
            assert_eq!(data.len(), size);
            let mut check = flate2::Crc::new();
            check.update(&data);
            assert_eq!(check.sum(), crc);
            entries.push(Entry { name: String::from_utf8(name.to_vec()).unwrap(), data, time, day });
            at += 46 + name_length + extra + comment;
        }
        assert_eq!(at, end);
        entries
    }

    fn texts(archive: &[u8]) -> BTreeMap<String, String> {
        unzip(archive).into_iter().map(|entry| (entry.name, String::from_utf8(entry.data).unwrap())).collect()
    }

    fn alice() -> Account {
        Account { name: "alice".to_owned(), home: r"C:\Users\alice".to_owned() }
    }

    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = Zip::new(Vec::new(), NaiveDate::from_ymd_opt(2026, 10, 22).unwrap().and_hms_opt(9, 30, 15).unwrap());
        for (name, data) in entries {
            zip.add(name, data).unwrap();
        }
        zip.finish().unwrap()
    }

    fn age(path: &Path, seconds: u64) {
        File::options().write(true).open(path).unwrap().set_modified(SystemTime::now() - Duration::from_secs(seconds)).unwrap();
    }

    #[test]
    fn scrubs_keys_tokens_passwords_and_the_account_name_from_logs() {
        let text = [
            "GET http://127.0.0.1:4000/?token=abcdef123456 200",
            "Authorization: Bearer abcdefghijklmnop",
            "key sk-ant-api03-SECRETSECRET and sk-proj-12345678abc",
            r#"{"apiKey": "plain-secret", "password":"hunter22"}"#,
            "proxy http://me:pw@corp:3128 failed",
            r"opened C:\Users\alice\ctf\solve.py and /home/alice/x",
        ]
        .join("\n");
        let cleaned = scrub(&text, &alice());
        for secret in ["abcdef123456", "abcdefghijklmnop", "SECRETSECRET", "12345678abc", "plain-secret", "hunter22", ":pw@", "alice"] {
            assert!(!cleaned.contains(secret), "{secret} in {cleaned}");
        }
        assert!(cleaned.contains(r"~\ctf\solve.py"));
        assert!(cleaned.contains("/home/<user>/x"));
        assert!(cleaned.contains("?token=***"));
        assert!(cleaned.contains("sk-ant-***"));
        assert!(cleaned.contains("http://me:***@corp"));
    }

    #[test]
    fn hides_the_home_folder_in_any_case_and_the_name_only_between_separators() {
        let cleaned = scrub(r"c:\USERS\Alice\a /alice/alice/ alice\x malice/ \alicex\ \ALICE", &alice());
        assert_eq!(cleaned, r"~\a /<user>/<user>/ alice\x malice/ \alicex\ \ALICE");
        // Too short to replace safely.
        let short = Account { name: "a".to_owned(), home: "C:".to_owned() };
        assert_eq!(scrub(r"C:\a\b", &short), r"C:\a\b");
        // Words are ASCII words, as in JavaScript.
        assert_eq!(scrub("éBearer abcdefghij", &alice()), "éBearer ***");
        assert_eq!(scrub("xBearer abcdefghij", &alice()), "xBearer abcdefghij");
    }

    #[test]
    fn picks_the_newest_crash_dumps() {
        let folder = tempfile::tempdir().unwrap();
        for (name, seconds) in [("old.dmp", 300), ("mid.DMP", 200), ("new.dmp", 100), ("notes.txt", 0)] {
            let path = folder.path().join(name);
            std::fs::write(&path, name).unwrap();
            age(&path, seconds);
        }
        std::fs::create_dir(folder.path().join("folder.dmp")).unwrap();
        let missing = folder.path().join("missing");
        let found = newest_files(&[folder.path().to_owned(), missing.clone()], ".dmp", 2).unwrap();
        assert_eq!(found, [folder.path().join("new.dmp"), folder.path().join("mid.DMP")]);
        assert!(newest_files(&[missing], ".dmp", 2).unwrap().is_empty());
    }

    #[test]
    fn packs_the_tail_of_each_log_and_skips_missing_files() {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path();
        std::fs::write(source.join("host.log"), format!("{}\nlast line token=abcdef123456\n", "x".repeat(100))).unwrap();
        std::fs::write(source.join("new.dmp"), [0u8, 159, 146, 150, b'a']).unwrap();
        // An earlier export with the same name is replaced.
        let target = source.join("out.zip");
        std::fs::write(&target, "old").unwrap();
        let about = [("Rainy".to_owned(), "4.0.0".to_owned()), ("Execution target".to_owned(), r"Windows C:\Users\alice".to_owned())];
        let before = dos_time(Local::now().naive_local());
        let names = export(&target, &about, &[
            DiagnosticFile { name: "logs/host.log".to_owned(), path: source.join("host.log"), max_bytes: 40, text: true },
            DiagnosticFile { name: "crashes/new.dmp".to_owned(), path: source.join("new.dmp"), max_bytes: 1024, text: false },
            DiagnosticFile { name: "logs/absent.log".to_owned(), path: source.join("absent.log"), max_bytes: 1024, text: true },
        ], &alice())
        .unwrap();
        assert_eq!(names, ["about.txt", "logs/host.log", "crashes/new.dmp"]);
        let entries = unzip(&std::fs::read(&target).unwrap());
        assert_eq!(entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(), names);
        assert_eq!(entries[0].data, b"Rainy: 4.0.0\nExecution target: Windows ~\n");
        // The last 40 bytes: the end of the long line, then the scrubbed last line.
        assert_eq!(String::from_utf8_lossy(&entries[1].data), format!("{}\nlast line token=***\n", "x".repeat(10)));
        // Binary files are copied as they are.
        assert_eq!(entries[2].data, [0u8, 159, 146, 150, b'a']);
        let after = dos_time(Local::now().naive_local());
        assert!(entries.iter().all(|entry| [before, after].contains(&(entry.time, entry.day))));
        assert!(entries.iter().all(|entry| (entry.time, entry.day) == (entries[0].time, entries[0].day)));
        let left: Vec<_> = std::fs::read_dir(source).unwrap().map(|entry| entry.unwrap().file_name()).filter(|name| name.to_string_lossy().ends_with(".tmp")).collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn a_failed_export_leaves_nothing_behind() {
        let folder = tempfile::tempdir().unwrap();
        let unreadable = folder.path().join("a folder.log");
        std::fs::create_dir(&unreadable).unwrap();
        let target = folder.path().join("out.zip");
        let file = DiagnosticFile { name: "logs/host.log".to_owned(), path: unreadable, max_bytes: 10, text: true };
        let error = format!("{:#}", export(&target, &[], &[file], &alice()).unwrap_err());
        // The zip first, then the file, in either interface language.
        let at = |text: &str| error.find(text).unwrap_or_else(|| panic!("{text} in {error}"));
        assert!(at(&target.display().to_string()) < at("a folder.log"), "{error}");
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
    }

    #[test]
    fn keeps_names_in_utf8_with_one_time_for_all() {
        let archive = zip(&[("日志/说明.txt", "内容".as_bytes()), ("empty", b""), ("big", "rainy ".repeat(100_000).as_bytes())]);
        let read = texts(&archive);
        assert_eq!(read["日志/说明.txt"], "内容");
        assert_eq!(read["empty"], "");
        assert_eq!(read["big"].len(), 600_000);
        assert!(archive.len() < 10_000, "deflated: {}", archive.len());
        let entries = unzip(&archive);
        assert!(entries.iter().all(|entry| (entry.time, entry.day) == ((9 << 11) | (30 << 5) | 7, (46 << 9) | (10 << 5) | 22)));
        assert_eq!(unzip(&zip(&[])).len(), 0);
    }

    #[test]
    fn dates_and_names_follow_3x() {
        let date = |year, month, day| NaiveDate::from_ymd_opt(year, month, day).unwrap().and_hms_opt(23, 59, 59).unwrap();
        assert_eq!(dos_time(date(1980, 1, 1)), ((23 << 11) | (59 << 5) | 29, (1 << 5) | 1));
        assert_eq!(dos_time(date(1970, 1, 1)).1, (1 << 5) | 1);
        assert_eq!(dos_time(date(2200, 12, 31)).1, (127 << 9) | (12 << 5) | 31);
        assert_eq!(default_name(Utc.with_ymd_and_hms(2026, 10, 2, 8, 5, 59).unwrap()), "Rainy-diagnostics-202610020805.zip");
    }

    #[test]
    fn packs_rainys_logs_the_host_log_settings_and_the_newest_webview_dumps() {
        let folder = tempfile::tempdir().unwrap();
        let user_data = folder.path();
        let reports = user_data.join("WebView2").join("EBWebView").join("Crashpad").join("reports");
        std::fs::create_dir_all(&reports).unwrap();
        for index in 0..7 {
            let path = reports.join(format!("{index}.dmp"));
            std::fs::write(&path, "dump").unwrap();
            age(&path, 100 * index);
        }
        let files = standard_files(user_data).unwrap();
        let listed: Vec<_> = files.iter().map(|file| (file.name.as_str(), file.max_bytes, file.text)).collect();
        assert_eq!(listed, [
            ("logs/rainy.log", LOG_BYTES, true),
            ("logs/rainy.log.1", LOG_BYTES, true),
            ("logs/host.log", LOG_BYTES, true),
            ("desktop.json", SETTINGS_BYTES, true),
            ("crashes/0.dmp", DUMP_BYTES, false),
            ("crashes/1.dmp", DUMP_BYTES, false),
            ("crashes/2.dmp", DUMP_BYTES, false),
            ("crashes/3.dmp", DUMP_BYTES, false),
            ("crashes/4.dmp", DUMP_BYTES, false),
        ]);
        assert_eq!(files[1].path, user_data.join("rainy.log.1"));
        assert_eq!(standard_files(&user_data.join("missing")).unwrap().len(), 4);
    }

    #[test]
    fn says_which_crash_folder_could_not_be_read() {
        let folder = tempfile::tempdir().unwrap();
        let crashpad = folder.path().join("WebView2").join("EBWebView").join("Crashpad");
        std::fs::create_dir_all(&crashpad).unwrap();
        std::fs::write(crashpad.join("reports"), "not a folder").unwrap();
        let error = format!("{:#}", standard_files(folder.path()).unwrap_err());
        assert!(error.contains(&format!("{}: ", crashpad.join("reports").display())), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn reveals_nothing_for_a_missing_file() {
        let folder = tempfile::tempdir().unwrap();
        let error = reveal(&folder.path().join("missing.zip")).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(0x8007_0002_u32 as i32), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn describes_this_computer() {
        let windows = windows_version();
        assert!(windows.starts_with("Windows 1") && windows.contains("(10.0.") && windows.ends_with(", x64)"), "{windows}");
        let memory = memory();
        let numbers: Vec<_> = memory.split([' ', ',']).filter_map(|word| word.parse::<u64>().ok()).collect();
        assert!(memory.ends_with(" GiB free") && numbers.len() == 2 && numbers[0] >= numbers[1] && numbers[0] > 0, "{memory}");
        assert!(downloads_folder().is_absolute());
        let account = Account::current();
        assert!(!account.name.is_empty() && Path::new(&account.home).is_absolute(), "{account:?}");
    }

    /// Not part of the normal run: writes a sample export to the path in `RAINY_DIAGNOSTICS_SAMPLE` for other zip readers
    /// (`Expand-Archive`, Explorer) to open.
    #[test]
    #[ignore]
    fn writes_a_sample() {
        let target = PathBuf::from(std::env::var_os("RAINY_DIAGNOSTICS_SAMPLE").expect("RAINY_DIAGNOSTICS_SAMPLE is set"));
        let folder = tempfile::tempdir().unwrap();
        let log = folder.path().join("rainy.log");
        std::fs::write(&log, "2026-10-22T09:30:15.000+08:00 INFO  Rainy: Authorization: Bearer abcdefghijklmnop\n").unwrap();
        let dump = folder.path().join("crash.dmp");
        std::fs::write(&dump, (0..=255u8).cycle().take(100_000).collect::<Vec<_>>()).unwrap();
        let about = [("Rainy".to_owned(), env!("CARGO_PKG_VERSION").to_owned()), ("Windows".to_owned(), windows_version())];
        let names = export(&target, &about, &[
            DiagnosticFile { name: "logs/rainy.log".to_owned(), path: log, max_bytes: LOG_BYTES, text: true },
            DiagnosticFile { name: "日志/说明.log".to_owned(), path: folder.path().join("rainy.log"), max_bytes: 30, text: true },
            DiagnosticFile { name: "crashes/crash.dmp".to_owned(), path: dump, max_bytes: DUMP_BYTES, text: false },
        ], &Account::current())
        .unwrap();
        println!("{} entries: {names:?}", names.len());
    }
}
