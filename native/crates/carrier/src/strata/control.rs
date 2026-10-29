//! The Strata Terminal ("Control") of the newest release: what its release folder's `install-manifest.txt` names for
//! Windows x64, downloaded and checked against that manifest and against the digest GitHub keeps for the file, unpacked,
//! and kept only when it accepts the release's signed update feed (what Strata's own `install.ps1` does, without its
//! PATH change). Later versions arrive through `strata update`, which puts them among the components and points
//! `control\active.json` at them; [`active`] follows that pointer as the Terminal's own hand-over does.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::Value;

use super::env::Folders;
use super::process::{Failure, Terminal};
use crate::download::{self, Digest, Expected, Options};
use crate::locale::localized;

/// Strata's release folder: the newest release's files.
pub const RELEASES: &str = "https://github.com/Niko1221/Strata/releases/latest/download";
/// The newest release, for the digest GitHub computed of each file.
const LATEST_API: &str = "https://api.github.com/repos/Niko1221/Strata/releases/latest";
const PLATFORM: &str = "windows-x86_64";
/// GitHub keeps release files under 2 GiB; the Terminal is about 70 MB.
const MAX_ARCHIVE: u64 = 1024 * 1024 * 1024;

/// The Terminal a release offers for this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub version: String,
    pub file: String,
    pub sha256: String,
    pub bytes: u64,
}

/// A Control folder that can run: its version and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    pub version: String,
    pub root: PathBuf,
}

impl Control {
    pub fn python(&self) -> PathBuf {
        self.root.join("python").join("python.exe")
    }
}

/// The manifest line for this computer: `<platform> <version> <file> <sha256> <bytes>`.
pub fn parse_manifest(text: &str) -> Result<Offer> {
    let line = text.lines().map(str::trim).find(|line| line.split_whitespace().next() == Some(PLATFORM)).ok_or_else(|| {
        anyhow!(localized("Strata 的最新版本没有提供 Windows x64 的终端组件。", "The newest Strata release has no Terminal for Windows x64."))
    })?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    let invalid = || anyhow!(localized("Strata 安装清单格式不正确。", "The Strata install manifest is not valid."));
    if fields.len() < 5 {
        return Err(invalid());
    }
    let (version, file, sha256) = (fields[1], fields[2], fields[3].to_ascii_lowercase());
    let bytes: u64 = fields[4].parse().map_err(|_| invalid())?;
    let safe_name = !file.is_empty() && file.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) && file.ends_with(".zip");
    if version_key(version).is_none() || !safe_name || Digest::sha256_hex(&sha256).is_none() || bytes == 0 || bytes > MAX_ARCHIVE {
        return Err(invalid());
    }
    Ok(Offer { version: version.to_owned(), file: file.to_owned(), sha256, bytes })
}

/// `0.1.53`, `0.2.2-rc1`: the numbers, and whether it is a final version (it sorts after its pre-releases).
pub fn version_key(text: &str) -> Option<(Vec<u64>, bool, String)> {
    let text = text.trim();
    let (base, pre) = match text.split_once('-') {
        Some((base, pre)) if !pre.is_empty() => (base, Some(pre)),
        Some(_) => return None,
        None => (text, None),
    };
    let numbers: Vec<u64> = base.split('.').map(|part| part.parse().ok()).collect::<Option<_>>()?;
    (2..=4).contains(&numbers.len()).then(|| {
        let mut numbers = numbers;
        numbers.resize(4, 0);
        (numbers, pre.is_none(), pre.unwrap_or_default().to_owned())
    })
}

/// Whether `a` is newer than `b`; unreadable versions are never newer.
pub fn newer(a: &str, b: &str) -> bool {
    match (version_key(a), version_key(b)) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true,
        _ => false,
    }
}

/// What the newest release offers, read from its manifest.
pub fn latest(cancel: Arc<AtomicBool>) -> Result<Offer> {
    let options = Options::new(cancel);
    let text = download::fetch(&format!("{RELEASES}/install-manifest.txt"), 64 * 1024, &options).map_err(|error| {
        anyhow!(localized(&format!("无法读取 Strata 的最新版本信息：{error}"), &format!("The newest Strata release could not be read: {error}")))
    })?;
    parse_manifest(&String::from_utf8_lossy(&text))
}

/// The digest GitHub reports for `file` in the newest release, when it can be read: a second opinion on the manifest.
fn github_digest(file: &str, cancel: Arc<AtomicBool>) -> Option<String> {
    let body = download::fetch(LATEST_API, 2 * 1024 * 1024, &Options::new(cancel)).ok()?;
    let release: Value = serde_json::from_slice(&body).ok()?;
    let asset = release.get("assets")?.as_array()?.iter().find(|asset| asset.get("name").and_then(Value::as_str) == Some(file))?;
    asset.get("digest")?.as_str()?.strip_prefix("sha256:").map(str::to_ascii_lowercase)
}

/// The Control folders that can run, newest first: the ones Rainy downloaded and the one `strata update` points to.
pub fn installed(folders: &Folders) -> Vec<Control> {
    let mut found = Vec::new();
    if let Ok(entries) = fs::read_dir(&folders.control) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if version_key(&name).is_some() && whole(&entry.path()) {
                found.push(Control { version: name, root: entry.path() });
            }
        }
    }
    if let Some(updated) = pointed(&folders.components) {
        found.push(updated);
    }
    found.sort_by(|a, b| version_key(&b.version).cmp(&version_key(&a.version)));
    found.dedup_by(|a, b| a.root == b.root);
    found
}

/// The Control to run: the newest that can.
pub fn active(folders: &Folders) -> Option<Control> {
    installed(folders).into_iter().next()
}

/// A Control folder that has its interpreter and entry point.
fn whole(root: &Path) -> bool {
    root.join("python").join("python.exe").is_file() && root.join("strata.py").is_file() && root.join("strata_control").is_dir() && root.join("tools").join("strata_mcp.py").is_file()
}

/// The Control `strata update` installed: `<components>\control\active.json` names a folder beside it.
fn pointed(components: &Path) -> Option<Control> {
    let base = components.join("control");
    let text = fs::read_to_string(base.join("active.json")).ok()?;
    let pointer: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let name = pointer.get("folder")?.as_str()?;
    if name.is_empty() || name == "." || name == ".." || !name.chars().all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c)) {
        return None;
    }
    let root = base.join(name);
    if !whole(&root) {
        return None;
    }
    let version = fs::read_to_string(root.join("VERSION"))
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|version| version_key(version).is_some())
        .or_else(|| pointer.get("version").and_then(Value::as_str).map(str::to_owned))?;
    Some(Control { version, root })
}

/// Download, check and unpack `offer` into `control\<version>`; `progress` gets the bytes downloaded and the total, then
/// `(0, 0)` while it unpacks and checks the release's signature. `terminal` makes the commands of the unpacked copy.
pub fn install(folders: &Folders, offer: &Offer, cancel: Arc<AtomicBool>, progress: &dyn Fn(u64, u64), terminal: &dyn Fn(Control) -> Terminal) -> Result<Control> {
    let destination = folders.control.join(&offer.version);
    if whole(&destination) {
        return Ok(Control { version: offer.version.clone(), root: destination });
    }
    if let Some(digest) = github_digest(&offer.file, cancel.clone())
        && digest != offer.sha256
    {
        bail!(localized(
            "Strata 发布页上的文件与它的安装清单不一致，已停止安装。请稍后再试。",
            "A file on the Strata release page does not match its install manifest, so nothing was installed. Try again later."
        ));
    }
    let downloads = folders.root.join("downloads");
    fs::create_dir_all(&downloads)?;
    let archive = downloads.join(&offer.file);
    let expected = Expected { bytes: offer.bytes, digest: Digest::sha256_hex(&offer.sha256).context("digest")? };
    download::download(&format!("{RELEASES}/{}", offer.file), &archive, &expected, &Options::new(cancel.clone()), progress).map_err(|error| match error {
        download::DownloadError::Cancelled => anyhow!(Failure::Cancelled),
        error => anyhow!(localized(&format!("下载 Strata 失败：{error}"), &format!("Downloading Strata failed: {error}"))),
    })?;
    progress(0, 0);
    let incoming = folders.control.join(format!("{}.incoming", offer.version));
    let _ = fs::remove_dir_all(&incoming);
    crate::unzip::unpack(&archive, &incoming, &cancel, &|_, _| {}).map_err(|error| {
        let _ = fs::remove_dir_all(&incoming);
        if cancel.load(std::sync::atomic::Ordering::SeqCst) { anyhow!(Failure::Cancelled) } else { error }
    })?;
    // The archive is rooted at the bundle, or at one folder holding it.
    let root = if whole(&incoming) {
        incoming.clone()
    } else {
        fs::read_dir(&incoming)?.flatten().map(|entry| entry.path()).find(|path| whole(path)).ok_or_else(|| {
            let _ = fs::remove_dir_all(&incoming);
            anyhow!(localized("下载的文件不是 Strata 终端组件。", "The download is not a Strata Terminal."))
        })?
    };
    // Its own check of the release's signed feed decides, as in Strata's installer.
    let candidate = Control { version: offer.version.clone(), root: root.clone() };
    let check = terminal(candidate).json(&["update", "--check", "--no-engine", "--output", "json"], Duration::from_secs(600), &cancel);
    let verified = match &check {
        Ok(answer) => answer.get("ok").and_then(Value::as_bool) == Some(true),
        Err(_) => false,
    };
    if !verified {
        let _ = fs::remove_dir_all(&incoming);
        if matches!(check, Err(Failure::Cancelled)) {
            return Err(anyhow!(Failure::Cancelled));
        }
        let detail = match check {
            Ok(answer) => super::process::reported_error(&answer).unwrap_or_default(),
            Err(error) => error.to_string(),
        };
        bail!(localized(
            &format!("Strata 的签名更新源未通过验证，已停止安装。{detail}"),
            &format!("The signed Strata update feed could not be verified, so nothing was installed. {detail}")
        ));
    }
    let _ = fs::remove_dir_all(&destination);
    fs::rename(&root, &destination).with_context(|| format!("Moving {} to {}", root.display(), destination.display()))?;
    let _ = fs::remove_dir_all(&incoming);
    let _ = fs::remove_file(&archive);
    Ok(Control { version: offer.version.clone(), root: destination })
}

/// Remove Control versions older than `keep` that Rainy downloaded.
pub fn prune(folders: &Folders, keep: &Control) {
    let Ok(entries) = fs::read_dir(&folders.control) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let stale = name.ends_with(".incoming") || (version_key(&name).is_some() && newer(&keep.version, &name));
        if stale && path != keep.root {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    const MANIFEST: &str = "linux-x86_64 0.1.53 strata-control-0.1.53-linux-x86_64.tar.gz 6f746e48d3abfd2c660fe460336ece5c66caf75610771f819e279de6e7adfe65 99446914
windows-x86_64 0.1.53 strata-control-0.1.53-windows-x86_64.zip 72CB29F2BB0FC534D4A2D7EA7D76AFCE0F7E2D41C72CB54A88FE2080EB2CF5AC 73064190
";

    #[test]
    fn reads_the_windows_line_of_the_manifest() {
        let offer = parse_manifest(MANIFEST).unwrap();
        assert_eq!(offer.version, "0.1.53");
        assert_eq!(offer.file, "strata-control-0.1.53-windows-x86_64.zip");
        assert_eq!(offer.sha256, "72cb29f2bb0fc534d4a2d7ea7d76afce0f7e2d41c72cb54a88fe2080eb2cf5ac");
        assert_eq!(offer.bytes, 73_064_190);
        assert!(parse_manifest("linux-x86_64 0.1.53 a.tar.gz 00 1").is_err());
        assert!(parse_manifest("windows-x86_64 0.1.53 ../x.zip 72cb29f2bb0fc534d4a2d7ea7d76afce0f7e2d41c72cb54a88fe2080eb2cf5ac 5").is_err());
        assert!(parse_manifest("windows-x86_64 x.y a.zip 72cb29f2bb0fc534d4a2d7ea7d76afce0f7e2d41c72cb54a88fe2080eb2cf5ac 5").is_err());
    }

    #[test]
    fn compares_versions_as_the_terminal_does() {
        assert!(newer("0.1.54", "0.1.53"));
        assert!(newer("0.2.0", "0.1.99"));
        assert!(newer("0.2.2", "0.2.2-rc1"));
        assert!(!newer("0.2.2-rc1", "0.2.2"));
        assert!(!newer("0.1.53", "0.1.53"));
        assert!(newer("0.1.45.1", "0.1.45"));
        assert!(!newer("garbage", "0.1.0"));
    }

    #[test]
    fn follows_the_update_pointer_to_a_newer_whole_control() {
        let dir = tempfile::tempdir().unwrap();
        let folders = Folders::new(dir.path());
        let make = |root: &Path, version: &str| {
            fs::create_dir_all(root.join("python")).unwrap();
            fs::create_dir_all(root.join("strata_control")).unwrap();
            fs::create_dir_all(root.join("tools")).unwrap();
            fs::write(root.join("python").join("python.exe"), b"").unwrap();
            fs::write(root.join("strata.py"), b"").unwrap();
            fs::write(root.join("tools").join("strata_mcp.py"), b"").unwrap();
            fs::write(root.join("VERSION"), version).unwrap();
        };
        assert_eq!(active(&folders), None);
        make(&folders.control.join("0.1.53"), "0.1.53");
        assert_eq!(active(&folders).unwrap().version, "0.1.53");
        let updated = folders.components.join("control").join("0.1.54");
        make(&updated, "0.1.54");
        fs::write(folders.components.join("control").join("active.json"), r#"{"folder": "0.1.54", "version": "0.1.54"}"#).unwrap();
        assert_eq!(active(&folders).unwrap().root, updated);
        // A pointer that leaves the folder, or to a folder missing its interpreter, is not followed.
        fs::write(folders.components.join("control").join("active.json"), r#"{"folder": "..\\..\\x"}"#).unwrap();
        assert_eq!(active(&folders).unwrap().version, "0.1.53");
        fs::remove_file(updated.join("python").join("python.exe")).unwrap();
        fs::write(folders.components.join("control").join("active.json"), r#"{"folder": "0.1.54"}"#).unwrap();
        assert_eq!(active(&folders).unwrap().version, "0.1.53");
        // Older downloads go once a newer one is kept.
        make(&folders.control.join("0.1.52"), "0.1.52");
        fs::create_dir_all(folders.control.join("0.1.55.incoming")).unwrap();
        prune(&folders, &active(&folders).unwrap());
        assert!(!folders.control.join("0.1.52").exists() && !folders.control.join("0.1.55.incoming").exists());
        assert!(folders.control.join("0.1.53").exists());
    }
}
