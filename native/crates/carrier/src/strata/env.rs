//! The environment of every Strata process Rainy starts. Strata's per-user folders are inside Rainy's own, so that
//! removing the tool removes all of it and an installation of Strata's own app is left alone; the temporary folder is
//! one the engine can create files in by a path any code page can spell (AMD's runtime compiles its first kernels
//! through temporary files and crashes in `amdhip64` when it cannot, Strata #654: "tmp" errors on a PC whose TEMP
//! names a folder that is gone, is read-only, or holds a name the engine's code page cannot spell); ROCm variables
//! naming folders that are gone are left out (the same crash); nothing of the user's own Python or Strata setup, and
//! nothing that looks like a credential, reaches it.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The folders of Rainy's Strata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folders {
    /// `<user data>\tools\strata`: everything below is the tool's.
    pub root: PathBuf,
    /// The Terminal versions Rainy downloaded: `control\<version>`.
    pub control: PathBuf,
    /// `LOCALAPPDATA` of Strata's processes: components, runtime, launch and tuning state.
    pub local: PathBuf,
    /// `APPDATA` of Strata's processes: its settings and profiles.
    pub roaming: PathBuf,
    /// `STRATA_INSTALL_DIR`: the run configs of the installed models and the management server's state.
    pub install: PathBuf,
    /// `STRATA_COMPONENTS_DIR`: engines and the Control versions `strata update` installed.
    pub components: PathBuf,
    /// Rainy's own temporary folder, used when the system's cannot be.
    pub temp: PathBuf,
    pub logs: PathBuf,
}

impl Folders {
    pub fn new(user_data: &Path) -> Self {
        let root = user_data.join("tools").join("strata");
        let local = root.join("AppData").join("Local");
        Self {
            control: root.join("control"),
            roaming: root.join("AppData").join("Roaming"),
            install: local.join("Strata").join("install"),
            components: local.join("Strata").join("components"),
            local,
            temp: root.join("temp"),
            logs: root.join("logs"),
            root,
        }
    }
}

/// Where downloads go when the person chose no folder: `<largest fixed drive>\Strata-data` when that drive is not the
/// system's, else inside the tool's folder.
pub fn default_data_dir(folders: &Folders) -> PathBuf {
    #[cfg(windows)]
    if let Some(drive) = roomiest_other_drive() {
        return PathBuf::from(format!("{drive}:\\Strata-data"));
    }
    folders.root.join("Strata-data")
}

/// The fixed drive other than the system's with the most free space, when it has more than the system drive.
#[cfg(windows)]
fn roomiest_other_drive() -> Option<char> {
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
    const DRIVE_FIXED: u32 = 3;
    let system = std::env::var("SystemDrive").ok().and_then(|drive| drive.chars().next()).unwrap_or('C').to_ascii_uppercase();
    let free = |letter: char| -> Option<u64> {
        let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain(Some(0)).collect();
        // SAFETY: a NUL-terminated root path; the output is one plain integer.
        unsafe {
            if GetDriveTypeW(root.as_ptr()) != DRIVE_FIXED {
                return None;
            }
            let mut available = 0u64;
            (GetDiskFreeSpaceExW(root.as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) != 0).then_some(available)
        }
    };
    // SAFETY: no arguments; a bit mask of the drives.
    let mask = unsafe { GetLogicalDrives() };
    let system_free = free(system).unwrap_or(0);
    (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| char::from(b'A' + bit))
        .filter(|letter| *letter != system)
        .filter_map(|letter| free(letter).map(|bytes| (letter, bytes)))
        .filter(|(_, bytes)| *bytes > system_free)
        .max_by_key(|(_, bytes)| *bytes)
        .map(|(letter, _)| letter)
}

fn ascii(path: &Path) -> bool {
    path.as_os_str().to_str().is_some_and(str::is_ascii)
}

/// The 8.3 name of an existing path, which is ASCII where the volume keeps short names.
#[cfg(windows)]
fn short_name(path: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buffer = vec![0u16; 1024];
    // SAFETY: a NUL-terminated input and an output buffer of the length given.
    let length = unsafe { GetShortPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    (length > 0 && length < buffer.len()).then(|| PathBuf::from(OsString::from_wide(&buffer[..length])))
}

#[cfg(not(windows))]
fn short_name(_path: &Path) -> Option<PathBuf> {
    None
}

/// Whether a file can be created, written and removed in `folder`.
fn writable(folder: &Path) -> bool {
    let mut bytes = [0u8; 6];
    let _ = getrandom::fill(&mut bytes);
    let probe = folder.join(format!("rainy-probe-{}-{}.tmp", std::process::id(), bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>()));
    let written = fs::OpenOptions::new().write(true).create_new(true).open(&probe).and_then(|mut file| file.write_all(b"x"));
    let removed = fs::remove_file(&probe).is_ok();
    written.is_ok() && removed
}

/// `folder` spelled so that the engine can use it, when it can: created if missing, ASCII (through its short name if
/// need be) and writable.
fn usable(folder: &Path) -> Option<PathBuf> {
    if !folder.is_absolute() || fs::create_dir_all(folder).is_err() {
        return None;
    }
    let spelled = if ascii(folder) { folder.to_path_buf() } else { short_name(folder).filter(|short| ascii(short))? };
    writable(&spelled).then_some(spelled)
}

/// The temporary folder of Strata's processes: the system's when the engine can use it, else Rainy's own, else one
/// beside the system's program data or at the root of the system drive (Strata's own advice is `C:\Temp`).
pub fn temporary_folder(own: &Path) -> PathBuf {
    let system_drive = std::env::var_os("SystemDrive").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:"));
    let candidates = [
        std::env::var_os("TEMP").map(PathBuf::from),
        std::env::var_os("TMP").map(PathBuf::from),
        Some(own.to_path_buf()),
        std::env::var_os("ProgramData").map(|data| PathBuf::from(data).join("RainyAgent").join("Temp")),
        Some(system_drive.join("\\").join("RainyTemp")),
    ];
    for candidate in candidates.into_iter().flatten() {
        if let Some(folder) = usable(&candidate) {
            return folder;
        }
    }
    let _ = fs::create_dir_all(own);
    own.to_path_buf()
}

/// A variable of the user's environment that Strata's processes do not get.
fn withheld(name: &str, value: &OsStr) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "SECRET", "TOKEN", "PASSWORD", "CREDENTIAL"].iter().any(|word| upper.contains(word))
        || ["PYTHONHOME", "PYTHONPATH", "PYTHONSTARTUP", "PYTHONUSERBASE", "PYTHONEXECUTABLE", "VIRTUAL_ENV", "NODE_OPTIONS", "NODE_PATH", "TMPDIR"].contains(&upper.as_str())
        || upper.starts_with("STRATA_")
        // ROCm folders that are gone crash AMD's runtime before the engine prints a line.
        || (["HIP_PATH", "HIP_DEVICE_LIB_PATH", "LLVM_PATH"].contains(&upper.as_str()) && !Path::new(value).exists())
}

/// Where Strata downloads models from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Hugging Face, Strata's default.
    #[default]
    Huggingface,
    /// ModelScope, the same files for mainland China.
    Modelscope,
}

/// The environment of a Strata the person installed themselves: their own `LOCALAPPDATA` and `APPDATA`, so it finds its
/// engine, settings and models where it keeps them; the rest as for Rainy's own.
pub fn own_environment(temp: &Path, source: Source) -> Vec<(OsString, OsString)> {
    let mut environment = common(temp, source);
    environment.retain(|(name, _)| !name.to_str().is_some_and(|name| name.eq_ignore_ascii_case("STRATA_INSTALL_DIR") || name.eq_ignore_ascii_case("STRATA_COMPONENTS_DIR")));
    environment
}

/// The environment of a process of Rainy's own Strata: its per-user folders inside Rainy's.
pub fn environment(folders: &Folders, temp: &Path, source: Source) -> Vec<(OsString, OsString)> {
    let mut environment = common(temp, source);
    let set = |environment: &mut Vec<(OsString, OsString)>, name: &str, value: &OsStr| {
        environment.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        environment.push((name.into(), value.to_owned()));
    };
    for (name, value) in [("LOCALAPPDATA", &folders.local), ("APPDATA", &folders.roaming), ("STRATA_INSTALL_DIR", &folders.install), ("STRATA_COMPONENTS_DIR", &folders.components)] {
        set(&mut environment, name, value.as_os_str());
    }
    environment
}

/// What every Strata process gets: this process's environment without credentials or foreign Python, plain output,
/// no questions, a usable temporary folder and the download source.
fn common(temp: &Path, source: Source) -> Vec<(OsString, OsString)> {
    let mut environment: Vec<(OsString, OsString)> = std::env::vars_os().filter(|(name, value)| !name.to_str().is_some_and(|name| withheld(name, value))).collect();
    let set = |environment: &mut Vec<(OsString, OsString)>, name: &str, value: &OsStr| {
        environment.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        environment.push((name.into(), value.to_owned()));
    };
    for (name, value) in [("PYTHONUTF8", "1"), ("PYTHONNOUSERSITE", "1"), ("PYTHONIOENCODING", "utf-8"), ("PYTHONUNBUFFERED", "1"), ("NO_COLOR", "1"), ("STRATA_NO_PROMPT", "1")] {
        set(&mut environment, name, OsStr::new(value));
    }
    for (name, value) in [("TEMP", &temp.to_path_buf()), ("TMP", &temp.to_path_buf())] {
        set(&mut environment, name, value.as_os_str());
    }
    if source == Source::Modelscope {
        set(&mut environment, "STRATA_SOURCE", OsStr::new("modelscope"));
    }
    environment
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn withholds_credentials_foreign_python_and_missing_rocm() {
        assert!(withheld("OPENAI_API_KEY", OsStr::new("x")));
        assert!(withheld("HF_TOKEN", OsStr::new("x")));
        assert!(withheld("PYTHONPATH", OsStr::new("C:\\x")));
        assert!(withheld("STRATA_UPDATE_FEED", OsStr::new("x")));
        assert!(withheld("HIP_PATH", OsStr::new("C:\\no\\such\\rocm")));
        assert!(!withheld("HIP_PATH", std::env::temp_dir().as_os_str()));
        assert!(!withheld("PATH", OsStr::new("C:\\Windows")));
        assert!(!withheld("HTTPS_PROXY", OsStr::new("http://127.0.0.1:7890")));
    }

    #[test]
    fn strata_gets_its_own_folders_and_a_usable_temporary_folder() {
        let dir = tempfile::tempdir().unwrap();
        let folders = Folders::new(dir.path());
        assert_eq!(folders.install, dir.path().join("tools\\strata\\AppData\\Local\\Strata\\install"));
        let temp = temporary_folder(&folders.temp);
        assert!(temp.is_dir() && writable(&temp));
        assert!(ascii(&temp) || !cfg!(windows) || short_name(&temp).is_none());
        let environment = environment(&folders, &temp, Source::Modelscope);
        let get = |name: &str| environment.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| PathBuf::from(value));
        assert_eq!(get("LOCALAPPDATA"), Some(folders.local.clone()));
        assert_eq!(get("STRATA_COMPONENTS_DIR"), Some(folders.components.clone()));
        assert_eq!(get("TEMP"), Some(temp.clone()));
        assert_eq!(get("STRATA_SOURCE"), Some(PathBuf::from("modelscope")));
        assert_eq!(environment.iter().filter(|(key, _)| key.eq_ignore_ascii_case("TEMP")).count(), 1);
    }

    #[test]
    fn a_temporary_folder_that_is_gone_or_unusable_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        // A name that is a file cannot be a folder.
        let file = dir.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert_eq!(usable(&file), None);
        assert_eq!(usable(Path::new("relative")), None);
        let missing = dir.path().join("gone").join("Temp");
        assert_eq!(usable(&missing).map(|folder| folder.is_dir()), Some(true));
    }

    #[cfg(windows)]
    #[test]
    fn a_folder_with_chinese_letters_is_spelled_by_its_short_name() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("临时文件");
        fs::create_dir_all(&folder).unwrap();
        // Volumes without short names (a ReFS or a setting) cannot spell it: then it is not used.
        match usable(&folder) {
            Some(spelled) => assert!(ascii(&spelled) && writable(&spelled)),
            None => assert!(short_name(&folder).is_none_or(|short| !ascii(&short))),
        }
    }
}
