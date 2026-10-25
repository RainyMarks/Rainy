//! Execution targets and the desktop preferences file (`src/main/execution-targets.ts`).
//!
//! `desktop.json` in the user data folder is shared with Rainy 3.x: fields this version does not know are kept as they
//! are, and every change is written atomically.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::locale::localized;

/// One destination the Host runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ExecutionTarget {
    Windows { id: String, label: String },
    Wsl { id: String, label: String, distro: String },
    Ssh {
        id: String,
        label: String,
        destination: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
    },
}

impl ExecutionTarget {
    pub fn windows() -> Self {
        Self::Windows { id: "windows-local".into(), label: "Windows".into() }
    }

    /// A WSL distribution, by its registry id (`wsl:<guid>`) when known.
    pub fn wsl(id: Option<&str>, distro: &str) -> Self {
        let id = id.map_or_else(|| format!("wsl:{distro}"), |guid| format!("wsl:{}", guid.to_ascii_lowercase()));
        Self::Wsl { id, label: format!("WSL · {distro}"), distro: distro.to_owned() }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Windows { id, .. } | Self::Wsl { id, .. } | Self::Ssh { id, .. } => id,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Windows { label, .. } | Self::Wsl { label, .. } | Self::Ssh { label, .. } => label,
        }
    }

    fn validate(self) -> Result<Self> {
        match &self {
            Self::Windows { id, .. } if id != "windows-local" => bail!("Invalid Windows target"),
            Self::Wsl { distro, .. } if distro.is_empty() || distro.len() > 256 => bail!("Invalid WSL distribution"),
            Self::Ssh { id, destination, .. } if !id.starts_with("ssh:") || destination.is_empty() || destination.len() > 255 => {
                bail!("Invalid SSH target")
            }
            _ => Ok(self),
        }
    }
}

/// Read `desktop.json`; a missing file is empty.
pub fn read_preferences(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read(path) {
        // Notepad may save it with a byte order mark.
        Ok(bytes) => serde_json::from_slice(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes))
            .with_context(|| localized(&format!("{} 不是有效的 JSON 对象", path.display()), &format!("{} is not a JSON object", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(error).with_context(|| localized(&format!("无法读取 {}", path.display()), &format!("Reading {}", path.display()))),
    }
}

/// `desktop.json` for startup: a file that is not a JSON object (cut short by a crash or a full disk, or edited by
/// hand) is moved aside to `desktop.json.damaged` and Rainy starts with defaults, rather than refusing every start
/// until the user finds and deletes it. A file that cannot be read now (held by another program) is still an error.
pub fn recover_preferences(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(error).with_context(|| format!("Reading {}", path.display())),
    }
    let _guard = UPDATES.lock().unwrap();
    match read_preferences(path) {
        Ok(preferences) => Ok(preferences),
        Err(error) => {
            let mut aside = path.as_os_str().to_owned();
            aside.push(".damaged");
            log::warn!("{error:#}; it is moved aside and Rainy starts with default settings");
            std::fs::rename(path, &aside).with_context(|| format!("Moving {} aside", path.display()))?;
            Ok(Map::new())
        }
    }
}

/// The remembered target, or Windows for a new installation (`savedExecutionTarget`).
pub fn saved_target(preferences: &Map<String, Value>) -> Result<ExecutionTarget> {
    if let Some(value) = preferences.get("executionTarget") {
        return serde_json::from_value::<ExecutionTarget>(value.clone())?.validate();
    }
    if let Some(distro) = preferences.get("distro").and_then(Value::as_str).map(str::trim).filter(|d| !d.is_empty()) {
        return Ok(ExecutionTarget::wsl(None, distro));
    }
    Ok(ExecutionTarget::windows())
}

/// Updates of one file run one after another, so concurrent changes of different fields do not lose each other.
static UPDATES: Mutex<()> = Mutex::new(());

/// Read, change and atomically rewrite `desktop.json`; `change` returns the fields to set.
pub fn update_preferences(path: &Path, change: impl FnOnce(&Map<String, Value>) -> Map<String, Value>) -> Result<()> {
    edit_preferences(path, |preferences| {
        let fields = change(preferences);
        preferences.extend(fields);
    })
}

/// Read, change and atomically rewrite `desktop.json`; `change` may also remove fields.
pub fn edit_preferences(path: &Path, change: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
    let _guard = UPDATES.lock().unwrap();
    let mut preferences = read_preferences(path)?;
    change(&mut preferences);
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let mut text = serde_json::to_string_pretty(&Value::Object(preferences))?;
    text.push('\n');
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    // On disk before it takes the name, so a crash or power loss leaves the old file or the new one, never a torn one.
    let written = std::fs::File::create(&temporary)
        .and_then(|mut file| {
            use std::io::Write as _;
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written?;
    Ok(())
}

/// Remember the target for the next launch (`saveExecutionTarget`); a pending project binding is cleared.
pub fn save_target(path: &Path, target: &ExecutionTarget) -> Result<()> {
    let value = serde_json::to_value(target)?;
    update_preferences(path, |_| {
        let mut fields = Map::new();
        fields.insert("executionTarget".into(), value);
        if let ExecutionTarget::Wsl { distro, .. } = target {
            fields.insert("distro".into(), Value::String(distro.clone()));
        }
        fields.insert("pendingProject".into(), Value::Null);
        fields
    })
}

/// Remember the interface language, for messages shown before the Host answers.
pub fn save_locale(path: &Path, locale: &str) -> Result<()> {
    update_preferences(path, |_| Map::from_iter([("locale".to_owned(), Value::String(locale.to_owned()))]))
}

/// Registered WSL 2 distributions, without starting any (`listExecutionTargets`).
#[cfg(windows)]
pub fn wsl_distributions() -> Vec<ExecutionTarget> {
    use windows_sys::Win32::System::Registry::*;
    use crate::text::wide;
    let mut found = Vec::new();
    // SAFETY: plain registry reads into buffers sized by the calls themselves; every opened key is closed.
    unsafe {
        let mut root = std::ptr::null_mut();
        let path = wide(r"Software\Microsoft\Windows\CurrentVersion\Lxss");
        if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut root) != 0 {
            return found;
        }
        for index in 0.. {
            let mut name = [0u16; 256];
            let mut length = name.len() as u32;
            let status = RegEnumKeyExW(root, index, name.as_mut_ptr(), &mut length, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            if status != 0 {
                break;
            }
            let guid = String::from_utf16_lossy(&name[..length as usize]);
            let mut key = std::ptr::null_mut();
            if RegOpenKeyExW(root, name.as_ptr(), 0, KEY_READ, &mut key) != 0 {
                continue;
            }
            let read_dword = |value: &str| -> Option<u32> {
                let mut data = 0u32;
                let mut size = 4u32;
                let value = wide(value);
                (RegQueryValueExW(key, value.as_ptr(), std::ptr::null(), std::ptr::null_mut(), (&raw mut data).cast(), &mut size) == 0).then_some(data)
            };
            let read_string = |value: &str| -> Option<String> {
                let value = wide(value);
                let mut size = 0u32;
                if RegQueryValueExW(key, value.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), &mut size) != 0 {
                    return None;
                }
                let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
                if RegQueryValueExW(key, value.as_ptr(), std::ptr::null(), std::ptr::null_mut(), buffer.as_mut_ptr().cast(), &mut size) != 0 {
                    return None;
                }
                let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
                Some(String::from_utf16_lossy(&buffer[..end]))
            };
            let version = read_dword("Version");
            let distro = read_string("DistributionName");
            RegCloseKey(key);
            if let (Some(2), Some(distro)) = (version, distro)
                && !distro.is_empty()
                && distro != "docker-desktop"
                && distro != "docker-desktop-data"
            {
                found.push(ExecutionTarget::wsl(Some(&guid), &distro));
            }
        }
        RegCloseKey(root);
    }
    found
}

#[cfg(not(windows))]
pub fn wsl_distributions() -> Vec<ExecutionTarget> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_targets_written_by_3x() {
        let map = |value: Value| value.as_object().cloned().unwrap();
        assert_eq!(saved_target(&Map::new()).unwrap(), ExecutionTarget::windows());
        let wsl = saved_target(&map(json!({"executionTarget": {"id": "wsl:{abc}", "kind": "wsl", "label": "WSL · Ubuntu", "distro": "Ubuntu"}}))).unwrap();
        assert!(matches!(&wsl, ExecutionTarget::Wsl { distro, .. } if distro == "Ubuntu"));
        let legacy = saved_target(&map(json!({"distro": "Debian"}))).unwrap();
        assert_eq!(legacy.id(), "wsl:Debian");
        let ssh = saved_target(&map(json!({"executionTarget": {"id": "ssh:dev", "kind": "ssh", "label": "SSH · dev", "destination": "dev", "port": 2222}}))).unwrap();
        assert!(matches!(ssh, ExecutionTarget::Ssh { port: Some(2222), .. }));
        assert!(saved_target(&map(json!({"executionTarget": {"id": "x", "kind": "windows", "label": "W"}}))).is_err());
    }

    #[test]
    fn updates_keep_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop.json");
        std::fs::write(&path, r#"{"shellIntegration": {"openWith": true}, "sshHosts": []}"#).unwrap();
        save_target(&path, &ExecutionTarget::wsl(Some("{ABC}"), "Ubuntu")).unwrap();
        save_locale(&path, "en").unwrap();
        let saved = read_preferences(&path).unwrap();
        assert_eq!(saved["shellIntegration"]["openWith"], true);
        assert_eq!(saved["executionTarget"]["id"], "wsl:{abc}");
        assert_eq!(saved["distro"], "Ubuntu");
        assert_eq!(saved["locale"], "en");
        assert_eq!(saved["pendingProject"], Value::Null);
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|entry| entry.file_name()).collect();
        assert_eq!(names, ["desktop.json"]);
    }

    #[test]
    fn a_damaged_file_does_not_stop_every_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop.json");
        assert!(recover_preferences(&path).unwrap().is_empty());
        std::fs::write(&path, b"\xEF\xBB\xBF{\"locale\": \"en\"}").unwrap();
        assert_eq!(recover_preferences(&path).unwrap()["locale"], "en");
        // What power loss can leave after a rename that was not on disk yet.
        std::fs::write(&path, vec![0u8; 300]).unwrap();
        assert!(read_preferences(&path).is_err());
        assert!(recover_preferences(&path).unwrap().is_empty());
        assert!(!path.exists());
        assert_eq!(std::fs::read(dir.path().join("desktop.json.damaged")).unwrap(), vec![0u8; 300]);
        save_locale(&path, "zh").unwrap();
        assert_eq!(read_preferences(&path).unwrap()["locale"], "zh");
    }
}
