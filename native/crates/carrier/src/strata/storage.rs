//! Storage, disk-space, model discovery, and O(1) chunked-download accounting for Strata.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::Value;

use crate::fs_util;
use crate::locale::localized;

pub(super) fn write_record(path: &Path, text: &str) -> Result<()> {
    fs_util::write_atomic(path, text.as_bytes(), true)?;
    Ok(())
}

pub(super) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// The size of a folder's regular files without following symlinks or junctions.
pub(super) fn folder_bytes(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    let Ok(entries) = fs::read_dir(path) else { return 0 };
    entries
        .flatten()
        .map(|entry| {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else { return 0 };
            if fs_util::is_reparse_point(&meta) {
                0
            } else if meta.is_dir() {
                folder_bytes(&entry.path())
            } else if meta.is_file() {
                meta.len()
            } else {
                0
            }
        })
        .fold(0u64, u64::saturating_add)
}

/// The data folder Strata's own settings remember (`data_dir` of `<APPDATA>\Strata\settings.json` of its processes):
/// where its setup put the model files of the last download, when that folder is still there.
pub(super) fn remembered_data_dir(roaming: &Path) -> Option<PathBuf> {
    let settings: Value = read_json(&roaming.join("Strata").join("settings.json"))?;
    let folder = PathBuf::from(settings.get("data_dir")?.as_str().filter(|folder| !folder.is_empty())?);
    folder.is_dir().then_some(folder)
}

pub(super) fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension().is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

/// The model folders of a data folder (`<data>\models\<tag>`) with GGUF files or downloads in progress: the tag in
/// lower case (the catalog's id), whether every GGUF has its finish mark (`<file>.done`) and no part is left, and the
/// bytes on disk.
pub(super) fn models_in(data: &Path) -> Vec<(String, bool, u64)> {
    let Ok(entries) = fs::read_dir(data.join("models")) else { return Vec::new() };
    let mut found: Vec<(String, bool, u64)> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let files: Vec<PathBuf> = fs::read_dir(entry.path()).ok()?.flatten().map(|file| file.path()).collect();
            let ggufs: Vec<&PathBuf> = files.iter().filter(|file| has_extension(file, "gguf")).collect();
            let parts = files.iter().any(|file| has_extension(file, "part"));
            if ggufs.is_empty() && !parts {
                return None;
            }
            let marked = |file: &PathBuf| {
                let mut mark = file.as_os_str().to_owned();
                mark.push(".done");
                Path::new(&mark).is_file()
            };
            let complete = !parts && ggufs.iter().all(|file| marked(file));
            let bytes = files
                .iter()
                .filter(|file| has_extension(file, "gguf") || has_extension(file, "part"))
                .map(|file| downloaded_bytes(file))
                .fold(0u64, u64::saturating_add);
            Some((entry.file_name().to_string_lossy().to_lowercase(), complete, bytes))
        })
        .collect();
    found.sort();
    found
}

/// The bytes a model file holds. Strata downloads a file in 32 MB pieces over several connections into `<name>.part`,
/// which reaches as far as the furthest piece written, gaps included; `<name>.part.chunks` lists the finished pieces
/// (`{"v": 1, "total", "chunk", "done": [[first, end), ...]}`), and only those count, as Strata's own `_load_pieces`
/// counts them. Computed via merged O(1)-per-interval `u128` arithmetic so malformed huge ranges cannot stall or overflow.
pub(super) fn downloaded_bytes(file: &Path) -> u64 {
    let length = fs::metadata(file).map(|info| info.len()).unwrap_or(0);
    if !has_extension(file, "part") {
        return length;
    }
    let mut list = file.as_os_str().to_owned();
    list.push(".chunks");
    let Some(record) = fs::read(&list).ok().and_then(|text| serde_json::from_slice::<Value>(&text).ok()) else {
        return length;
    };
    let (Some(1), Some(total), Some(chunk), Some(done)) = (
        record["v"].as_u64(),
        record["total"].as_u64(),
        record["chunk"].as_u64().filter(|chunk| *chunk > 0),
        record["done"].as_array(),
    ) else {
        return length;
    };
    if total == 0 || length == 0 {
        return 0;
    }
    let pieces = total.div_ceil(chunk);
    // A piece `p` has end byte `min((p + 1) * chunk, total)`. It satisfies `end <= length` iff either `length >= total`
    // (all pieces fit) or `p < length / chunk` (since the final piece ends at `total > length`).
    let valid_pieces = if length >= total { pieces } else { (length / chunk).min(pieces) };
    if valid_pieces == 0 {
        return 0;
    }
    let mut intervals: Vec<(u64, u64)> = done
        .iter()
        .filter_map(|range| {
            let first = range.get(0)?.as_u64()?;
            let end = range.get(1)?.as_u64()?.min(valid_pieces);
            (first < end).then_some((first, end))
        })
        .collect();
    if intervals.is_empty() {
        return 0;
    }
    intervals.sort_unstable();
    let total_u128 = u128::from(total);
    let chunk_u128 = u128::from(chunk);
    let mut merged_start = intervals[0].0;
    let mut merged_end = intervals[0].1;
    let mut bytes = 0u128;
    for &(first, end) in &intervals[1..] {
        if first <= merged_end {
            merged_end = merged_end.max(end);
        } else {
            let s = u128::from(merged_start) * chunk_u128;
            let e = (u128::from(merged_end) * chunk_u128).min(total_u128);
            bytes = bytes.saturating_add(e.saturating_sub(s));
            merged_start = first;
            merged_end = end;
        }
    }
    let s = u128::from(merged_start) * chunk_u128;
    let e = (u128::from(merged_end) * chunk_u128).min(total_u128);
    bytes = bytes.saturating_add(e.saturating_sub(s));
    u64::try_from(bytes).unwrap_or(u64::MAX).min(length)
}

/// The same folder, spelled either way.
pub(super) fn same_folder(a: &Path, b: &Path) -> bool {
    let key = |path: &Path| path.to_string_lossy().trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase();
    key(a) == key(b)
}

pub(super) fn free_gb(path: &Path) -> Option<f64> {
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent()?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = existing.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut available = 0u64;
        // SAFETY: a NUL-terminated path and one plain integer out.
        (unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) } != 0)
            .then(|| available as f64 / 1e9)
    }
    #[cfg(not(windows))]
    {
        let _ = existing;
        None
    }
}

/// Remove a folder, retrying while a process that just ended still holds a file.
pub(super) fn remove_folder(path: &Path) -> Result<()> {
    for attempt in 0..10 {
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if attempt == 9 => {
                return Err(anyhow!(localized(
                    &format!("无法删除 {}：{error}", path.display()),
                    &format!("{} could not be removed: {error}", path.display())
                )));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }
    Ok(())
}
