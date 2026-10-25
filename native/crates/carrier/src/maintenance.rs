//! What earlier versions left in the installation and Rainy removes a minute after start (`retired-resources.ts` in
//! 3.x), and the installed Windows runtime, which the uninstaller removes through Rainy (`--rainy-remove-runtime`).

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

/// Below `resources`: what earlier installers shipped and Rainy now downloads on demand or no longer uses.
pub const RETIRED_RESOURCES: [&str; 8] = [
    "strata-runtime",
    "php",
    "linux-runtime.tar.gz",
    "native-tools-metadata.json",
    "native-tools-download.json",
    "native-tools-catalog.json",
    "native-tools-channel.signed.json",
    "native-tools-public-keys.json",
];

/// Below the installation folder: the Electron files of Rainy 3.x, which 4.0 does not ship (named as the 3.x
/// uninstaller deletes them, `installer.nsh`). The 4.0 installer deletes them when it replaces 3.x; this removes what it
/// could not. `RainyAgent.exe` and `Uninstall RainyAgent.exe` keep their names in 4.0 and are not among them.
pub const ELECTRON_FILES: [&str; 19] = [
    "chrome_100_percent.pak",
    "chrome_200_percent.pak",
    "d3dcompiler_47.dll",
    "dxcompiler.dll",
    "dxil.dll",
    "ffmpeg.dll",
    "icudtl.dat",
    "LICENSE.electron.txt",
    "LICENSES.chromium.html",
    "resources.pak",
    "snapshot_blob.bin",
    "v8_context_snapshot.bin",
    "vk_swiftshader_icd.json",
    "vk_swiftshader.dll",
    "vulkan-1.dll",
    "release-integrity.json",
    "resources/app.asar",
    "resources/app-update.yml",
    "resources/elevate.exe",
];

/// Electron's language files, `locales\*.pak`, below the installation folder.
const LOCALES: &str = "locales";

/// `<install>\resources` when Rainy runs from an installation: `resources\windows-host` beside the executable, outside a
/// Cargo build folder (`native\target\<profile>`), where development resources may have been copied.
pub fn installed_resources(install_root: &Path) -> Option<PathBuf> {
    let resources = install_root.join("resources");
    (cargo_target(install_root).is_none() && resources.join("windows-host").is_dir()).then_some(resources)
}

/// The repository a development build runs from: the folder holding `native\target`, above `install_root`.
pub fn checkout(install_root: &Path) -> Option<&Path> {
    cargo_target(install_root)?.parent()?.parent()
}

/// The Cargo build folder `native\target` that holds `dir`, if any.
fn cargo_target(dir: &Path) -> Option<&Path> {
    let named = |dir: &Path, name: &str| dir.file_name().is_some_and(|own| own.eq_ignore_ascii_case(name));
    dir.ancestors().find(|dir| named(dir, "target") && dir.parent().is_some_and(|parent| named(parent, "native")))
}

/// What a clean-up removed, by name below the installation folder, and what it could not, with the reason.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub names: Vec<String>,
    pub failed: Vec<String>,
}

impl Removed {
    /// Remove a file or folder unless it is missing or a link.
    fn remove(&mut self, path: &Path, name: String) {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(error) => return self.failed.push(format!("{name}: {error}")),
        };
        if metadata.file_type().is_symlink() {
            return;
        }
        match retried(|| if metadata.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }) {
            Ok(()) => self.names.push(name),
            Err(error) => self.failed.push(format!("{name}: {error}")),
        }
    }
}

/// Delete [`RETIRED_RESOURCES`], [`ELECTRON_FILES`] and `locales\*.pak` (`removeRetiredResources`); missing entries and
/// links are skipped, and one failure does not stop the rest. `signed` holds the paths below `resources` that the
/// accepted signed list covers, lowercased; they and folders holding them are kept.
pub fn remove_retired(install_root: &Path, resources: &Path, signed: &HashSet<String>) -> Removed {
    let covered = |name: &str| {
        let name = name.to_lowercase();
        let folder = format!("{name}/");
        signed.contains(&name) || signed.iter().any(|path| path.starts_with(&folder))
    };
    let mut removed = Removed::default();
    for name in RETIRED_RESOURCES.into_iter().filter(|name| !covered(name)) {
        removed.remove(&resources.join(name), format!("resources/{name}"));
    }
    for name in ELECTRON_FILES.into_iter().filter(|name| !name.strip_prefix("resources/").is_some_and(covered)) {
        removed.remove(&name.split('/').fold(install_root.to_path_buf(), |path, segment| path.join(segment)), name.to_owned());
    }
    let locales = install_root.join(LOCALES);
    if fs::symlink_metadata(&locales).is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink()) {
        let packs: Vec<String> = match fs::read_dir(&locales) {
            Ok(entries) => entries
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| name.to_ascii_lowercase().ends_with(".pak"))
                .collect(),
            Err(error) => {
                removed.failed.push(format!("{LOCALES}: {error}"));
                Vec::new()
            }
        };
        for pack in packs {
            removed.remove(&locales.join(&pack), format!("{LOCALES}/{pack}"));
        }
        // Only an emptied folder goes.
        if fs::remove_dir(&locales).is_ok() {
            removed.names.push(LOCALES.to_owned());
        }
    }
    removed
}

/// Delete files below `windows-host` that its `runtime.json` does not list (`pruneWindowsHost`): an installer update
/// overwrites the files it ships but never removes those a newer version dropped. Links are left alone, and a folder is
/// removed when this emptied it or when it is empty in a folder that holds none of the listed files; an empty folder
/// beside listed files may be one the package ships and stays. `everything`, for the uninstaller, deletes every file but
/// links, then empty folders and `windows-host` itself. The bytes removed; 0 when the inventory is missing or unreadable.
pub fn prune_windows_host(resources: &Path, everything: bool) -> Result<u64> {
    let root = resources.join("windows-host");
    if !fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink()) {
        return Ok(0);
    }
    let Some(mut listed) = inventory(&root) else { return Ok(0) };
    if everything {
        listed.clear();
    }
    let mut removed = 0;
    if prune(&root, "", &listed, everything, &mut removed)?.may_go(everything) && everything {
        fs::remove_dir(&root).with_context(|| format!("Removing {}", root.display()))?;
    }
    Ok(removed)
}

/// `--rainy-remove-runtime`: remove the installed Windows runtime file by file ([`prune_windows_host`] with
/// `everything`). Refused when `resources` or `windows-host` is a link or another reparse point.
pub fn remove_runtime(resources: &Path) -> Result<u64> {
    for path in [resources.to_path_buf(), resources.join("windows-host")] {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if is_reparse_point(&metadata) => bail!("{} is a link; nothing was removed.", path.display()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error).with_context(|| format!("Reading {}", path.display())),
        }
    }
    prune_windows_host(resources, true)
}

/// The paths `runtime.json` lists, and itself, in the case-insensitive form Windows compares them in.
fn inventory(root: &Path) -> Option<HashSet<String>> {
    let value: Value = serde_json::from_str(&fs::read_to_string(root.join("runtime.json")).ok()?).ok()?;
    let mut listed = HashSet::from([key("runtime.json")]);
    for file in value.get("files")?.as_array()? {
        listed.insert(key(file.get("path")?.as_str()?));
    }
    Some(listed)
}

fn key(path: &str) -> String {
    if cfg!(windows) { path.to_lowercase() } else { path.to_owned() }
}

/// What [`prune`] left of a folder.
struct Pruned {
    /// Nothing is left in it.
    empty: bool,
    /// It holds a listed file, here or deeper: this version ships it.
    shipped: bool,
    /// This run removed something from it.
    changed: bool,
}

impl Pruned {
    /// Whether the folder may go: emptied here, empty while removing everything, or empty in a folder this version
    /// does not ship (the caller decides that last one).
    fn may_go(&self, everything: bool) -> bool {
        self.empty && (self.changed || everything)
    }
}

/// Prune one folder. A folder found empty stays only where this version ships its parent: packages ship empty
/// folders, and an earlier version's empty ones would otherwise keep their parents (4.0's `node` and `node_modules`).
fn prune(folder: &Path, relative: &str, listed: &HashSet<String>, everything: bool, removed: &mut u64) -> Result<Pruned> {
    let mut pruned = Pruned { empty: true, shipped: false, changed: false };
    let mut found_empty = Vec::new();
    for entry in fs::read_dir(folder).with_context(|| format!("Reading {}", folder.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if relative.is_empty() { name } else { format!("{relative}/{name}") };
        let full = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            pruned.empty = false;
        } else if kind.is_dir() {
            let inner = prune(&full, &path, listed, everything, removed)?;
            if inner.may_go(everything) {
                remove_folder(&full)?;
                pruned.changed = true;
            } else if inner.empty {
                found_empty.push(full);
            } else {
                pruned.empty = false;
                pruned.shipped |= inner.shipped;
            }
        } else if listed.contains(&key(&path)) {
            pruned.empty = false;
            pruned.shipped = true;
        } else {
            *removed += fs::symlink_metadata(&full)?.len();
            retried(|| fs::remove_file(&full)).with_context(|| format!("Removing {}", full.display()))?;
            pruned.changed = true;
        }
    }
    if pruned.shipped {
        pruned.empty &= found_empty.is_empty();
    } else {
        for full in found_empty {
            remove_folder(&full)?;
            pruned.changed = true;
        }
    }
    Ok(pruned)
}

/// Removes an empty folder, retried while a file just deleted inside is still held open (scanners, the indexer).
fn remove_folder(full: &Path) -> Result<()> {
    retried(|| fs::remove_dir(full)).with_context(|| format!("Removing {}", full.display()))
}

/// A removal tried up to four times, for a file another process holds for a moment.
fn retried(mut remove: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    for _ in 0..3 {
        if remove().is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    remove()
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    metadata.file_attributes() & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    fn write(root: &Path, path: &str, content: &str) {
        let full = path.split('/').fold(root.to_path_buf(), |full, segment| full.join(segment));
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, content).unwrap();
    }

    fn exists(root: &Path, path: &str) -> bool {
        fs::symlink_metadata(path.split('/').fold(root.to_path_buf(), |full, segment| full.join(segment))).is_ok()
    }

    /// A directory junction, which any user may create; false where `mklink` is unavailable.
    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(target).output().is_ok_and(|output| output.status.success())
    }

    /// `windows-host` as an update leaves it: this version's files, and some only an earlier version listed.
    fn windows_host() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let resources = dir.path().join("resources");
        let runtime = r#"{"version":1,"node":"22.22.1","files":[{"path":"node/node.exe","bytes":4,"sha256":"x"},{"path":"App/Dist/Host.js"},{"path":"app/empty/keep.txt"}]}"#;
        write(&resources, "windows-host/runtime.json", runtime);
        write(&resources, "windows-host/node/node.exe", "node");
        write(&resources, "windows-host/app/dist/host.js", "host");
        write(&resources, "windows-host/app/empty/keep.txt", "");
        write(&resources, "windows-host/app/dist/old.js", "old!");
        write(&resources, "windows-host/pwsh/pwsh.exe", "powershell");
        write(&resources, "windows-host/pwsh/modules/a.psm1", "a");
        dir
    }

    #[test]
    fn prunes_what_the_inventory_does_not_list() {
        let dir = windows_host();
        let resources = dir.path().join("resources");
        let host = resources.join("windows-host");
        // Empty folders an earlier version shipped (4.0's npm and ripgrep packages), and one this version ships.
        write(&resources, "windows-host/node/node_modules/npm/index.js", "npm");
        fs::create_dir_all(host.join("node").join("node_modules").join("npm").join("tap-snapshots")).unwrap();
        fs::create_dir_all(host.join("old").join("empty").join("deeper")).unwrap();
        fs::create_dir_all(host.join("app").join("dist").join("shipped-empty")).unwrap();
        assert_eq!(prune_windows_host(&resources, false).unwrap(), 18);
        for kept in ["runtime.json", "node/node.exe", "app/dist/host.js", "app/empty/keep.txt", "app/dist/shipped-empty"] {
            assert!(exists(&resources, &format!("windows-host/{kept}")), "{kept}");
        }
        for gone in ["app/dist/old.js", "pwsh", "node/node_modules", "old"] {
            assert!(!exists(&resources, &format!("windows-host/{gone}")), "{gone}");
        }
        assert_eq!(prune_windows_host(&resources, false).unwrap(), 0);
        assert!(exists(&resources, "windows-host/app/dist/shipped-empty"));

        // What an earlier prune left: empty folders only, which go now.
        fs::create_dir_all(host.join("node_modules").join("@vscode").join("ripgrep-win32-x64").join("bin")).unwrap();
        assert_eq!(prune_windows_host(&resources, false).unwrap(), 0);
        assert!(!exists(&resources, "windows-host/node_modules"));
    }

    #[test]
    fn keeps_everything_without_a_readable_inventory() {
        for runtime in ["", "{", "[]", r#"{"files":{}}"#, r#"{"files":[{"path":1}]}"#, r#"{"files":[null]}"#] {
            let dir = windows_host();
            let resources = dir.path().join("resources");
            write(&resources, "windows-host/runtime.json", runtime);
            assert_eq!(prune_windows_host(&resources, false).unwrap(), 0, "{runtime}");
            assert_eq!(remove_runtime(&resources).unwrap(), 0, "{runtime}");
            assert!(exists(&resources, "windows-host/pwsh/pwsh.exe"), "{runtime}");
        }
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(prune_windows_host(dir.path(), false).unwrap(), 0);
        assert_eq!(remove_runtime(&dir.path().join("resources")).unwrap(), 0);
    }

    #[test]
    fn removes_the_whole_runtime_for_the_uninstaller() {
        let dir = windows_host();
        let resources = dir.path().join("resources");
        write(&resources, "windows-host/app/dist/renderer/index.html", "<!doctype html>");
        fs::create_dir_all(resources.join("windows-host").join("app").join("shipped-empty")).unwrap();
        assert!(remove_runtime(&resources).unwrap() > 0);
        assert!(!exists(&resources, "windows-host"));
        assert!(resources.is_dir());
    }

    #[cfg(windows)]
    #[test]
    fn leaves_links_alone() {
        let dir = windows_host();
        let resources = dir.path().join("resources");
        let elsewhere = dir.path().join("user-files");
        write(&elsewhere, "keep.txt", "mine");
        let link = resources.join("windows-host").join("app").join("linked");
        if !junction(&link, &elsewhere) {
            eprintln!("mklink /J is unavailable; links were not tested");
            return;
        }
        prune_windows_host(&resources, false).unwrap();
        assert!(exists(&resources, "windows-host/app/linked") && exists(&elsewhere, "keep.txt"));
        remove_runtime(&resources).unwrap();
        assert!(exists(&resources, "windows-host/app/linked") && exists(&elsewhere, "keep.txt"));
        assert!(!exists(&resources, "windows-host/node/node.exe"));

        // A linked `windows-host` is refused, not followed.
        let host = resources.join("windows-host");
        fs::remove_dir(&link).unwrap();
        fs::remove_dir_all(&host).unwrap();
        let target = dir.path().join("elsewhere-host");
        write(&target, "runtime.json", r#"{"files":[]}"#);
        write(&target, "node.exe", "node");
        assert!(junction(&host, &target));
        assert!(remove_runtime(&resources).is_err());
        assert_eq!(prune_windows_host(&resources, false).unwrap(), 0);
        assert!(exists(&target, "node.exe"));
        fs::remove_dir(&host).unwrap();
    }

    #[test]
    fn removes_what_3x_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let resources = root.join("resources");
        let left_behind = || {
            for name in ELECTRON_FILES {
                write(root, name, "electron");
            }
            write(&resources, "php/php.exe", "php");
            write(&resources, "linux-runtime.tar.gz", "old");
            write(&resources, "native-tools-catalog.json", "{}");
            write(root, "locales/en-US.pak", "en");
            write(root, "locales/zh-CN.pak", "zh");
        };
        let kept = [
            "RainyAgent.exe",
            "Uninstall RainyAgent.exe",
            "WebView2Loader.dll",
            "LICENSE",
            "resources/icon.ico",
            "resources/release-manifest.signed.json",
            "resources/windows-host/runtime.json",
            "runtime/windows/keep.txt",
            "tools/keep.txt",
        ];
        for name in kept {
            write(root, name, "keep");
        }
        left_behind();
        let removed = remove_retired(root, &resources, &HashSet::new());
        assert!(removed.failed.is_empty(), "{:?}", removed.failed);
        assert_eq!(removed.names.len(), ELECTRON_FILES.len() + 3 + 3);
        assert!(removed.names.contains(&"resources/php".to_owned()) && removed.names.contains(&"locales".to_owned()));
        for name in ELECTRON_FILES.into_iter().chain(["resources/php", "resources/linux-runtime.tar.gz", "locales"]) {
            assert!(!exists(root, name), "{name}");
        }
        for name in kept {
            assert!(exists(root, name), "{name}");
        }
        assert_eq!(remove_retired(root, &resources, &HashSet::new()), Removed::default());

        // What the accepted list still covers stays.
        left_behind();
        let signed = HashSet::from(["app-update.yml".to_owned(), "php/php.exe".to_owned()]);
        let removed = remove_retired(root, &resources, &signed);
        assert_eq!(removed.names.len(), ELECTRON_FILES.len() - 1 + 2 + 3);
        assert!(exists(root, "resources/app-update.yml") && exists(root, "resources/php/php.exe") && !exists(root, "resources/app.asar"));

        // A folder of other files under `locales` stays.
        write(root, "locales/en-US.pak", "en");
        write(root, "locales/notes.txt", "mine");
        assert_eq!(remove_retired(root, &resources, &signed).names, ["locales/en-US.pak"]);
        assert!(exists(root, "locales/notes.txt"));
    }

    #[cfg(windows)]
    #[test]
    fn does_not_follow_a_retired_name_that_is_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let resources = dir.path().join("resources");
        let elsewhere = dir.path().join("user-php");
        write(&elsewhere, "php.exe", "mine");
        fs::create_dir_all(&resources).unwrap();
        if !junction(&resources.join("php"), &elsewhere) {
            eprintln!("mklink /J is unavailable; links were not tested");
            return;
        }
        assert_eq!(remove_retired(dir.path(), &resources, &HashSet::new()), Removed::default());
        assert!(exists(&elsewhere, "php.exe") && exists(&resources, "php"));
        fs::remove_dir(resources.join("php")).unwrap();
    }

    #[test]
    fn finds_installed_resources_only_in_an_installation() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Programs").join("RainyAgent");
        assert_eq!(installed_resources(&install), None);
        fs::create_dir_all(install.join("resources").join("windows-host")).unwrap();
        assert_eq!(installed_resources(&install), Some(install.join("resources")));
        let built = dir.path().join("Rainy").join("native").join("target").join("release");
        fs::create_dir_all(built.join("resources").join("windows-host")).unwrap();
        assert_eq!(installed_resources(&built), None);
    }

    #[test]
    fn finds_the_checkout_of_a_build_only() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("Rainy");
        assert_eq!(checkout(&repo.join("native").join("target").join("debug")), Some(repo.as_path()));
        assert_eq!(checkout(&repo.join("Native").join("Target").join("release").join("deps")), Some(repo.as_path()));
        assert_eq!(checkout(&dir.path().join("Programs").join("RainyAgent")), None);
        assert_eq!(checkout(&repo.join("target").join("debug")), None);
    }
}
