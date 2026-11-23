//! Execution targets and the desktop preferences file (`src/main/execution-targets.ts`).
//!
//! `desktop.json` in the user data folder is shared with Rainy 3.x: fields this version does not know are kept as they
//! are, and every change is written atomically.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::locale::localized;

/// Default OpenSSH `ServerAliveInterval` in seconds (10s × 3 = 30s drop detection).
pub const DEFAULT_KEEPALIVE_INTERVAL: u16 = 10;
/// Default OpenSSH `ServerAliveCountMax`.
pub const DEFAULT_KEEPALIVE_COUNT_MAX: u8 = 3;
const MAX_REMOTE_PATH_LEN: usize = 512;
const MAX_NOTES_CHARS: usize = 2000;

fn default_remote_host() -> String {
    "127.0.0.1".to_owned()
}

/// Whether `host` is safe to interpolate into OpenSSH `-L 127.0.0.1:<local>:<host>:<remote>`.
pub(crate) fn valid_remote_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 255 || host.starts_with('-') {
        return false;
    }
    if let Some(inner) = host.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
        return !inner.is_empty() && inner.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.');
    }
    let mut chars = host.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric()) && chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// One local SSH port forwarding rule (`-L 127.0.0.1:<local_port>:<remote_host>:<remote_port>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortForwardRule {
    /// Stable identifier for UI tracking (auto-filled when omitted).
    #[serde(default)]
    pub id: String,
    pub local_port: u16,
    pub remote_port: u16,
    #[serde(default = "default_remote_host")]
    pub remote_host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub auto_start: bool,
}

impl PortForwardRule {
    pub fn new(local_port: u16, remote_port: u16) -> Self {
        let mut rule = Self {
            id: String::new(),
            local_port,
            remote_port,
            remote_host: default_remote_host(),
            label: None,
            auto_start: false,
        };
        rule.id = rule.default_id();
        rule
    }

    pub fn default_id(&self) -> String {
        let host = if self.remote_host.trim().is_empty() { "127.0.0.1" } else { self.remote_host.trim() };
        format!("{}->{host}:{}", self.local_port, self.remote_port)
    }

    /// Loopback URL to open this forwarded service in the local browser.
    pub fn local_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.local_port)
    }

    pub fn validate(&self) -> Result<()> {
        if self.local_port == 0 || self.remote_port == 0 {
            bail!(localized("转发端口必须是 1 到 65535 之间的整数。", "Forwarded ports must be whole numbers from 1 to 65535."));
        }
        if !valid_remote_host(&self.remote_host) {
            bail!(localized("远程转发主机地址无效。", "The remote forwarding host address is not valid."));
        }
        if self.label.as_deref().is_some_and(|label| label.chars().any(|c| c.is_control())) {
            bail!(localized("端口转发名称不能包含控制字符。", "A port forward label cannot contain control characters."));
        }
        Ok(())
    }

    pub fn normalized(&self) -> Result<Self> {
        let remote_host = {
            let trimmed = self.remote_host.trim();
            if trimmed.is_empty() { default_remote_host() } else { trimmed.to_owned() }
        };
        let label = self.label.as_deref().map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned);
        let id = self.id.trim().to_owned();
        let mut normalized = Self {
            id,
            local_port: self.local_port,
            remote_port: self.remote_port,
            remote_host,
            label,
            auto_start: self.auto_start,
        };
        if normalized.id.is_empty() {
            normalized.id = normalized.default_id();
        }
        normalized.validate()?;
        Ok(normalized)
    }
}

/// Per-host SSH research workbench profile persisted under `desktop.json`'s `sshHostProfiles`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshHostProfile {
    /// Custom remote Rainy home directory (e.g. `/root/kube-user/.rainy-agent`) forwarded via `RAINY_HOME`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rainy_home: Option<String>,
    /// Default remote workspace folder opened for this host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_workspace: Option<String>,
    /// Bookmarked remote folders shown in the folder picker.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<String>,
    /// Extra remote environment variables (e.g. `HF_ENDPOINT`, `PIP_INDEX_URL`) encoded into `RAINY_SSH_EXTRA_ENV`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Override OpenSSH `ServerAliveInterval` in seconds (default 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keepalive_interval: Option<u16>,
    /// Override OpenSSH `ServerAliveCountMax` (default 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keepalive_count_max: Option<u8>,
    /// Saved `-L` port forward presets for TensorBoard / Jupyter / Web services.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwards: Vec<PortForwardRule>,
    /// Short host notes (e.g. GPU sharing or cgroup limits) shown in UI and forwarded via `RAINY_SSH_NOTES`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

fn normalize_remote_path(raw: &str, forbid_parent_dir: bool, forbid_root: bool, zh: &str, en: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.starts_with('/') || trimmed.len() > MAX_REMOTE_PATH_LEN || trimmed.chars().any(|c| matches!(c, '\0' | '\r' | '\n')) {
        bail!(localized(&format!("{zh} 必须是以 / 开头的有效 Linux 绝对路径。"), &format!("{en} must be a valid absolute Linux path starting with '/'.")));
    }
    if forbid_parent_dir && trimmed.split('/').any(|part| part == "..") {
        bail!(localized(&format!("{zh} 不能包含 \"..\" 路径段。"), &format!("{en} cannot contain '..' path segments.")));
    }
    let clean = if trimmed.len() > 1 { trimmed.trim_end_matches('/') } else { trimmed };
    let clean = if clean.is_empty() { "/" } else { clean };
    if forbid_root && clean == "/" {
        bail!(localized(&format!("{zh} 不能为根目录 \"/\"。"), &format!("{en} cannot be the root directory '/'.")));
    }
    Ok(clean.to_owned())
}

fn valid_extra_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let ok = (1..=128).contains(&name.len())
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok && !name.to_ascii_uppercase().starts_with("RAINY_")
}

fn sanitize_notes(raw: &str) -> Option<String> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let cleaned: String = normalized
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_NOTES_CHARS).collect())
}

impl SshHostProfile {
    pub fn is_empty(&self) -> bool {
        self.rainy_home.as_deref().map(str::trim).unwrap_or_default().is_empty()
            && self.default_workspace.as_deref().map(str::trim).unwrap_or_default().is_empty()
            && self.bookmarks.is_empty()
            && self.env.is_empty()
            && self.keepalive_interval.is_none()
            && self.keepalive_count_max.is_none()
            && self.forwards.is_empty()
            && self.notes.as_deref().map(str::trim).unwrap_or_default().is_empty()
    }

    /// Effective `(ServerAliveInterval, ServerAliveCountMax)` clamped to safe bounds.
    pub fn keepalive(&self) -> (u16, u8) {
        let interval = self.keepalive_interval.unwrap_or(DEFAULT_KEEPALIVE_INTERVAL).clamp(1, 600);
        let count = self.keepalive_count_max.unwrap_or(DEFAULT_KEEPALIVE_COUNT_MAX).clamp(1, 60);
        (interval, count)
    }

    pub fn validate(&self) -> Result<()> {
        self.normalized().map(|_| ())
    }

    pub fn normalized(&self) -> Result<Self> {
        let rainy_home = match self.rainy_home.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
            Some(path) => Some(normalize_remote_path(path, true, true, "持久化目录 (RAINY_HOME)", "Persistent Rainy home (RAINY_HOME)")?),
            None => None,
        };
        let default_workspace = match self.default_workspace.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
            Some(path) => Some(normalize_remote_path(path, false, false, "默认工作目录", "Default workspace")?),
            None => None,
        };
        let mut bookmarks = Vec::new();
        for raw in &self.bookmarks {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let path = normalize_remote_path(trimmed, false, false, "收藏目录", "Bookmarked path")?;
            if !bookmarks.iter().any(|existing| existing == &path) {
                bookmarks.push(path);
            }
        }
        let mut env = BTreeMap::new();
        for (key, value) in &self.env {
            let key = key.trim();
            if !valid_extra_env_name(key) {
                bail!(localized(
                    &format!("环境变量名称“{key}”无效（仅允许字母、数字与下划线，且不能以 RAINY_ 开头）。"),
                    &format!("Environment variable name \"{key}\" is not valid (use letters, digits and underscores, and do not start with RAINY_)."),
                ));
            }
            if value.contains('\0') {
                bail!(localized("环境变量值不能包含空字节。", "Environment variable values cannot contain NUL bytes."));
            }
            env.insert(key.to_owned(), value.clone());
        }
        if let Some(interval) = self.keepalive_interval
            && !(1..=600).contains(&interval)
        {
            bail!(localized("SSH 保活间隔必须在 1 到 600 秒之间。", "SSH keepalive interval must be between 1 and 600 seconds."));
        }
        if let Some(count) = self.keepalive_count_max
            && !(1..=60).contains(&count)
        {
            bail!(localized("SSH 保活重试次数必须在 1 到 60 次之间。", "SSH keepalive max count must be between 1 and 60."));
        }
        let mut forwards = Vec::with_capacity(self.forwards.len());
        for rule in &self.forwards {
            forwards.push(rule.normalized()?);
        }
        let notes = self.notes.as_deref().and_then(sanitize_notes);
        Ok(Self {
            rainy_home,
            default_workspace,
            bookmarks,
            env,
            keepalive_interval: self.keepalive_interval,
            keepalive_count_max: self.keepalive_count_max,
            forwards,
            notes,
        })
    }

    /// Environment variables forwarded to the remote `rainy-host` process.
    pub fn host_env_vars(&self) -> Vec<(String, String)> {
        let mut vars = Vec::new();
        if let Some(home) = self.rainy_home.as_deref().map(str::trim).filter(|text| !text.is_empty())
            && let Ok(clean) = normalize_remote_path(home, true, true, "RAINY_HOME", "RAINY_HOME")
        {
            vars.push(("RAINY_HOME".to_owned(), clean));
        }
        if let Some(notes) = self.notes.as_deref().and_then(sanitize_notes) {
            vars.push(("RAINY_SSH_NOTES".to_owned(), notes));
        }
        let safe_env: BTreeMap<&str, &str> = self
            .env
            .iter()
            .filter(|(k, v)| valid_extra_env_name(k.trim()) && !v.contains('\0'))
            .map(|(k, v)| (k.trim(), v.as_str()))
            .collect();
        if !safe_env.is_empty()
            && let Ok(encoded) = serde_json::to_string(&safe_env)
        {
            vars.push(("RAINY_SSH_EXTRA_ENV".to_owned(), encoded));
        }
        vars
    }
}

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
            Self::Windows { id, .. } if id != "windows-local" => bail!(localized("保存的 Windows 执行环境无效。", "The saved Windows execution environment is not valid.")),
            Self::Wsl { distro, .. } if distro.is_empty() || distro.len() > 256 => {
                bail!(localized("保存的 WSL 发行版名称无效。", "The saved WSL distribution name is not valid."))
            }
            Self::Ssh { id, destination, .. } if !id.starts_with("ssh:") || destination.is_empty() || destination.len() > 255 => {
                bail!(localized("保存的 SSH 主机无效。", "The saved SSH host is not valid."))
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
        Err(error) => return Err(error).with_context(|| localized(&format!("无法读取 {}", path.display()), &format!("Reading {}", path.display()))),
    }
    let _guard = UPDATES.lock().unwrap();
    match read_preferences(path) {
        Ok(preferences) => Ok(preferences),
        Err(error) => {
            let mut aside = path.as_os_str().to_owned();
            aside.push(".damaged");
            log::warn!("{error:#}; it is moved aside and Rainy starts with default settings");
            std::fs::rename(path, &aside)
                .with_context(|| localized(&format!("无法把损坏的 {} 移到一旁", path.display()), &format!("Moving the damaged {} aside", path.display())))?;
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

/// Read all saved per-host SSH research profiles from `desktop.json`; malformed entries are skipped.
pub fn ssh_host_profiles(preferences: &Map<String, Value>) -> BTreeMap<String, SshHostProfile> {
    let mut profiles = BTreeMap::new();
    let Some(entries) = preferences.get("sshHostProfiles").and_then(Value::as_object) else {
        return profiles;
    };
    for (target_id, raw) in entries {
        if !target_id.starts_with("ssh:") || target_id.len() <= 4 {
            continue;
        }
        if let Ok(profile) = serde_json::from_value::<SshHostProfile>(raw.clone())
            && let Ok(normalized) = profile.normalized()
            && !normalized.is_empty()
        {
            profiles.insert(target_id.clone(), normalized);
        }
    }
    profiles
}

/// Read one SSH host's profile, or defaults when none is stored.
pub fn ssh_host_profile(preferences: &Map<String, Value>, target_id: &str) -> SshHostProfile {
    ssh_host_profiles(preferences).remove(target_id).unwrap_or_default()
}

/// Save or update one SSH host's research profile in `desktop.json`. An empty profile removes the entry.
pub fn save_ssh_host_profile(path: &Path, target_id: &str, profile: &SshHostProfile) -> Result<()> {
    if !target_id.starts_with("ssh:") || target_id.len() <= 4 {
        bail!(localized("保存的 SSH 主机标识无效。", "The SSH host target id is not valid."));
    }
    let normalized = profile.normalized()?;
    let encoded = (!normalized.is_empty()).then(|| serde_json::to_value(&normalized)).transpose()?;
    edit_preferences(path, |preferences| {
        let mut map = preferences.get("sshHostProfiles").and_then(Value::as_object).cloned().unwrap_or_default();
        match encoded {
            Some(value) => {
                map.insert(target_id.to_owned(), value);
            }
            None => {
                map.remove(target_id);
            }
        }
        if map.is_empty() {
            preferences.remove("sshHostProfiles");
        } else {
            preferences.insert("sshHostProfiles".into(), Value::Object(map));
        }
    })
}

/// Remove one SSH host's research profile from `desktop.json`.
pub fn remove_ssh_host_profile(path: &Path, target_id: &str) -> Result<()> {
    edit_preferences(path, |preferences| {
        if let Some(map) = preferences.get_mut("sshHostProfiles").and_then(Value::as_object_mut) {
            map.remove(target_id);
            if map.is_empty() {
                preferences.remove("sshHostProfiles");
            }
        }
    })
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

    #[test]
    fn ssh_host_profiles_round_trip_and_stay_compatible_with_3x() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop.json");
        // Legacy 3.x desktop.json has no sshHostProfiles field at all.
        std::fs::write(&path, r#"{"sshHosts": [{"destination": "leaper"}], "locale": "zh"}"#).unwrap();
        let legacy = read_preferences(&path).unwrap();
        assert!(ssh_host_profiles(&legacy).is_empty());
        assert_eq!(ssh_host_profile(&legacy, "ssh:leaper"), SshHostProfile::default());
        assert_eq!(SshHostProfile::default().keepalive(), (10, 3));

        let profile = SshHostProfile {
            rainy_home: Some("/root/kube-user/.rainy-agent/".into()),
            default_workspace: Some("/root/kube-user/work/".into()),
            bookmarks: vec!["/root/kube-user".into(), "/root/kube-user/".into(), "/data".into()],
            env: BTreeMap::from([("HF_ENDPOINT".into(), "https://hf-mirror.com".into())]),
            keepalive_interval: Some(15),
            keepalive_count_max: Some(4),
            forwards: vec![PortForwardRule {
                id: String::new(),
                local_port: 6006,
                remote_port: 6006,
                remote_host: "127.0.0.1".into(),
                label: Some("TensorBoard".into()),
                auto_start: true,
            }],
            notes: Some("2号卡常被占\r\n限36核192G\x07".into()),
        };
        save_ssh_host_profile(&path, "ssh:leaper", &profile).unwrap();
        let saved = read_preferences(&path).unwrap();
        // Legacy sshHosts and locale remain untouched.
        assert_eq!(saved["sshHosts"][0]["destination"], "leaper");
        assert_eq!(saved["locale"], "zh");
        let loaded = ssh_host_profile(&saved, "ssh:leaper");
        assert_eq!(loaded.rainy_home.as_deref(), Some("/root/kube-user/.rainy-agent"));
        assert_eq!(loaded.default_workspace.as_deref(), Some("/root/kube-user/work"));
        assert_eq!(loaded.bookmarks, vec!["/root/kube-user", "/data"]);
        assert_eq!(loaded.keepalive(), (15, 4));
        assert_eq!(loaded.forwards.len(), 1);
        assert_eq!(loaded.forwards[0].id, "6006->127.0.0.1:6006");
        assert_eq!(loaded.forwards[0].local_url(), "http://127.0.0.1:6006");
        assert_eq!(loaded.notes.as_deref(), Some("2号卡常被占\n限36核192G"));

        let env_vars: BTreeMap<String, String> = loaded.host_env_vars().into_iter().collect();
        assert_eq!(env_vars.get("RAINY_HOME").map(String::as_str), Some("/root/kube-user/.rainy-agent"));
        assert_eq!(env_vars.get("RAINY_SSH_NOTES").map(String::as_str), Some("2号卡常被占\n限36核192G"));
        let decoded_env: BTreeMap<String, String> = serde_json::from_str(&env_vars["RAINY_SSH_EXTRA_ENV"]).unwrap();
        assert_eq!(decoded_env.get("HF_ENDPOINT").map(String::as_str), Some("https://hf-mirror.com"));

        // Saving an empty profile removes the entry and cleans up the empty map.
        save_ssh_host_profile(&path, "ssh:leaper", &SshHostProfile::default()).unwrap();
        let cleaned = read_preferences(&path).unwrap();
        assert!(cleaned.get("sshHostProfiles").is_none());
    }

    #[test]
    fn rejects_invalid_profile_and_forward_rules() {
        assert!(SshHostProfile { rainy_home: Some("relative/home".into()), ..Default::default() }.validate().is_err());
        assert!(SshHostProfile { rainy_home: Some("/root/../etc".into()), ..Default::default() }.validate().is_err());
        assert!(SshHostProfile { rainy_home: Some("/".into()), ..Default::default() }.validate().is_err());
        assert!(SshHostProfile { rainy_home: Some("///".into()), ..Default::default() }.validate().is_err());
        assert!(SshHostProfile { rainy_home: Some("/".into()), ..Default::default() }.host_env_vars().is_empty());
        assert!(SshHostProfile { env: BTreeMap::from([("RAINY_PORT".into(), "1".into())]), ..Default::default() }.validate().is_err());
        assert!(SshHostProfile { env: BTreeMap::from([("1BAD".into(), "x".into())]), ..Default::default() }.validate().is_err());
        assert!(SshHostProfile { keepalive_interval: Some(0), ..Default::default() }.validate().is_err());
        assert!(PortForwardRule::new(0, 6006).validate().is_err());
        assert!(PortForwardRule::new(6006, 0).validate().is_err());
        assert!(PortForwardRule { remote_host: "-oProxyCommand=x".into(), ..PortForwardRule::new(6006, 6006) }.validate().is_err());
        assert!(PortForwardRule { remote_host: "127.0.0.1:9999".into(), ..PortForwardRule::new(6006, 6006) }.validate().is_err());
        assert!(PortForwardRule { remote_host: "[::1]".into(), ..PortForwardRule::new(6006, 6006) }.validate().is_ok());
    }
}
