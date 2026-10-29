//! Project-scoped file operations of the editor with explicit version checks (3.x `ide/files.ts`,
//! `files-search.ts`, `directories.ts`). Every path is resolved again right before it is used, so a link swapped in
//! after a check cannot lead outside the project.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

use super::fs::{self as ifs, Info, Intent, Kind};
use super::preview;
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::paths::{contains, fully_qualified, join_relative, realpath, relative_path};
use crate::projects::Projects;
use crate::text::{now_ms, sha256_hex};

pub const MAX_TEXT_BYTES: u64 = 5 * 1024 * 1024;
const MAX_BINARY_BYTES: usize = 25 * 1024 * 1024;
const MAX_PREVIEW_BYTES: u64 = 64 * 1024;
const MAX_HEX_PREVIEW_BYTES: usize = 4 * 1024;
const SEARCH_RESULT_LIMIT: usize = 200;
const SEARCH_MAX_ENTRIES: usize = 50_000;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(3);
const SEARCH_EXCLUDED: [&str; 4] = [".git", "node_modules", ".venv", "__pycache__"];
const MAX_DIRECTORY_ENTRIES: usize = 10_000;
const MAX_DELETE_ENTRIES: usize = 20_000;
const MAX_DELETE_PREVIEWS: usize = 128;
const DELETE_PREVIEW_LIFETIME_MS: u64 = 120_000;
const MAX_CHANGE_PATHS: usize = 256;
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// One folder of a project, freshly resolved.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub workspace_id: String,
    pub root: PathBuf,
    pub root_id: String,
}

struct Observation {
    absolute: PathBuf,
    target: PathBuf,
    info: Info,
    target_info: Option<Info>,
    bytes: u64,
    version: String,
    outside: bool,
}

struct Deletion {
    workspace_id: String,
    root: PathBuf,
    path: String,
    expires: u64,
    fingerprint: String,
}

/// Decoded file text, or why there is none.
pub struct Decoded {
    pub content: Option<String>,
    pub bom: bool,
    pub eol: &'static str,
    pub reason: Option<&'static str>,
}

/// Decode editable text: UTF-8 with an optional BOM; UTF-16 is unsupported and a NUL byte means binary. `partial`
/// accepts a sequence cut at the end, for previews of large files.
pub fn decode(bytes: &[u8], partial: bool) -> Decoded {
    let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        return Decoded { content: None, bom: false, eol: "lf", reason: Some("unsupported-encoding") };
    }
    if bytes.contains(&0) {
        return Decoded { content: None, bom, eol: "lf", reason: Some("binary") };
    }
    let body = if bom { &bytes[3..] } else { bytes };
    let content = match std::str::from_utf8(body) {
        Ok(text) => text.to_owned(),
        Err(error) if partial && error.error_len().is_none() => String::from_utf8_lossy(&body[..error.valid_up_to()]).into_owned(),
        Err(_) => return Decoded { content: None, bom, eol: "lf", reason: Some("unsupported-encoding") },
    };
    let crlf = content.contains("\r\n");
    let lf = content.as_bytes().iter().enumerate().any(|(index, byte)| *byte == b'\n' && (index == 0 || content.as_bytes()[index - 1] != b'\r'));
    let eol = match (crlf, lf) {
        (true, true) => "mixed",
        (true, false) => "crlf",
        _ => "lf",
    };
    Decoded { content: Some(content), bom, eol, reason: None }
}

fn hex_preview(bytes: &[u8], total: u64) -> Value {
    let prefix = &bytes[..bytes.len().min(MAX_HEX_PREVIEW_BYTES)];
    let rows: Vec<String> = prefix
        .chunks(16)
        .enumerate()
        .map(|(row, chunk)| {
            let hex: Vec<String> = chunk.iter().map(|byte| format!("{byte:02x}")).collect();
            let ascii: String = chunk.iter().map(|byte| if (32..=126).contains(byte) { char::from(*byte) } else { '.' }).collect();
            format!("{:08x}  {:<47}  |{ascii}|", row * 16, hex.join(" "))
        })
        .collect();
    json!({ "kind": "hex", "text": rows.join("\n"), "bytesRead": prefix.len(), "truncated": (prefix.len() as u64) < total })
}

fn missing(error: &HostError) -> bool {
    error.code == "not-found"
}

fn gone() -> HostError {
    HostError::new("not-found", l("所选路径已不存在。", "The selected path no longer exists."))
}

fn not_absolute() -> HostError {
    HostError::new("invalid-path", l("请选择一个绝对路径的文件夹。", "Choose an absolute directory path."))
}

fn stale(version: Option<&str>) -> HostError {
    ifs::changed().with("currentVersion", json!(version))
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The editor's file service.
pub struct Files {
    projects: Arc<Projects>,
    deletions: Mutex<Vec<(String, Deletion)>>,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Directories of the files a change check observed, so they can be watched.
    on_changes_checked: Box<dyn Fn(&str, &str, Vec<PathBuf>) + Send + Sync>,
    /// PATH and other variables for Git in a directory (the Windows basic component's MinGit serves without Git).
    git_environment: Box<dyn Fn(&Path) -> BTreeMap<String, String> + Send + Sync>,
}

impl Files {
    pub fn new(
        projects: Arc<Projects>,
        on_changes_checked: impl Fn(&str, &str, Vec<PathBuf>) + Send + Sync + 'static,
        git_environment: impl Fn(&Path) -> BTreeMap<String, String> + Send + Sync + 'static,
    ) -> Self {
        Self { projects, deletions: Mutex::default(), locks: Mutex::default(), on_changes_checked: Box::new(on_changes_checked), git_environment: Box::new(git_environment) }
    }

    /// One folder of a registered project, checked right now.
    pub fn resolve_workspace(&self, workspace_id: &str, root_id: Option<&str>) -> HostResult<Workspace> {
        let root = self.projects.resolve_root(workspace_id, root_id)?;
        Ok(Workspace { workspace_id: workspace_id.to_owned(), root: PathBuf::from(root.path), root_id: root.root_id })
    }

    fn lock(&self, workspace_id: &str) -> Arc<Mutex<()>> {
        self.locks.lock().unwrap().entry(workspace_id.to_owned()).or_default().clone()
    }

    /// Run one request of the `ide` method whose op starts with `workspaces.`, `files.` or `directories.`.
    pub fn handle(&self, request: &Value) -> HostResult<Value> {
        let op = request["op"].as_str().unwrap_or_default();
        let text = |name: &str| request.get(name).and_then(Value::as_str);
        let required = |name: &str| text(name).ok_or_else(|| HostError::invalid("The IDE file request contains invalid fields."));
        match op {
            "directories.list" => return list_directories(text("path"), request["showHidden"].as_bool().unwrap_or(false)),
            "directories.create" => return create_directory(required("path")?),
            "workspaces.list" => return Ok(Value::Array(self.projects.list().iter().map(|project| project.view()).collect())),
            "workspaces.open" => return Ok(self.projects.open(required("path")?, None)?.view()),
            "workspaces.attach" => return Ok(self.projects.attach(required("workspaceId")?, required("path")?, None)?.view()),
            "workspaces.removeRoot" => return Ok(self.projects.detach(required("workspaceId")?, required("rootId")?)?.view()),
            "workspaces.rename" => return Ok(self.projects.rename(required("workspaceId")?, required("title")?)?.view()),
            "workspaces.remove" => {
                let id = required("workspaceId")?;
                if self.projects.get(id).is_none() {
                    return Err(crate::projects::missing());
                }
                self.projects.remove(id)?;
                return Ok(json!({ "workspaceId": id, "removed": true }));
            }
            _ => {}
        }
        let workspace = self.resolve_workspace(required("workspaceId")?, text("rootId"))?;
        let path = || required("path");
        let mutate = |action: &dyn Fn() -> HostResult<Value>| {
            let lock = self.lock(&workspace.workspace_id);
            let _guard = lock.lock().unwrap();
            action()
        };
        match op {
            "files.list" => self.list(&workspace, path()?),
            "files.read" => self.read(&workspace, path()?),
            "files.search" => Ok(search(&workspace.root, text("query").unwrap_or_default(), request["limit"].as_u64().map(|limit| limit as usize))),
            "files.diff" => self.diff(&workspace, path()?),
            "files.changes" => {
                let paths: Vec<String> = request["paths"].as_array().ok_or_else(|| HostError::invalid("paths must be a list"))?.iter().filter_map(|path| path.as_str().map(str::to_owned)).collect();
                self.changes(&workspace, &paths)
            }
            "files.deletePreview" => self.preview_delete(&workspace, path()?),
            "files.save" => mutate(&|| self.save(&workspace, path()?, required("content")?, required("expectedVersion")?)),
            "files.create" => mutate(&|| self.create(&workspace, path()?, required("content")?)),
            "files.mkdir" => mutate(&|| {
                let absolute = self.prepare_new(&workspace, path()?)?;
                std::fs::create_dir(&absolute)?;
                Ok(self.entry(path()?, &self.observe(&workspace, path()?, false, false)?))
            }),
            "files.createBinary" => mutate(&|| self.create_binary(&workspace, path()?, required("base64")?)),
            "files.rename" => mutate(&|| self.rename(&workspace, path()?, required("destination")?, required("expectedVersion")?)),
            "files.delete" => mutate(&|| self.delete(&workspace, path()?, required("token")?)),
            _ => Err(HostError::invalid("The IDE file request contains invalid fields.")),
        }
    }

    fn checked_path(&self, workspace: &Workspace, path: &str, allow_root: bool) -> HostResult<PathBuf> {
        relative_path(path, allow_root)?;
        let absolute = join_relative(&workspace.root, path);
        let parent = if path.is_empty() { workspace.root.clone() } else { realpath(absolute.parent().unwrap_or(&workspace.root))? };
        if !contains(&workspace.root, &parent) {
            return Err(HostError::new("outside-workspace", l("路径中有一段指向所选项目之外。", "A path component points outside the selected workspace.")));
        }
        Ok(absolute)
    }

    fn observe(&self, workspace: &Workspace, path: &str, allow_root: bool, allow_outside: bool) -> HostResult<Observation> {
        let absolute = self.checked_path(workspace, path, allow_root)?;
        let info = ifs::lstat(&absolute)?.ok_or_else(gone)?;
        let target = ifs::resolve(&absolute)?;
        let outside = !contains(&workspace.root, &target);
        if outside && !allow_outside {
            return Err(HostError::new("outside-workspace", l("所选链接指向项目之外。", "The selected link points outside the workspace.")));
        }
        let target_info = if outside { None } else { ifs::stat(&target)? };
        let version = sha256_hex(json!([absolute, info.version, target, target_info.as_ref().map(|info| &info.version)]).to_string());
        let bytes = target_info.as_ref().map_or(info.size, |info| info.size);
        Ok(Observation { absolute, target, info, target_info, bytes, version, outside })
    }

    fn entry(&self, path: &str, observation: &Observation) -> Value {
        let mut entry = json!({
            "name": file_name(path), "path": path, "kind": observation.info.kind.name(), "bytes": observation.bytes,
            "version": observation.version, "outsideWorkspace": observation.outside,
        });
        if let Some(target) = &observation.target_info {
            entry["targetKind"] = json!(if target.kind == Kind::Symlink { "other" } else { target.kind.name() });
        }
        entry
    }

    fn directory_names(&self, absolute: &Path, limit: usize) -> HostResult<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(absolute)? {
            if names.len() == limit {
                return Err(HostError::new("too-large", l("文件夹中的条目超过了上限。", "The directory contains more entries than the configured limit.")));
            }
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }

    fn list(&self, workspace: &Workspace, path: &str) -> HostResult<Value> {
        let directory = self.observe(workspace, path, true, false)?;
        if directory.target_info.as_ref().map(|info| info.kind) != Some(Kind::Directory) {
            return Err(HostError::new("not-directory", l("所选路径不是文件夹。", "The selected path is not a directory.")));
        }
        let mut names = self.directory_names(&directory.target, MAX_DIRECTORY_ENTRIES)?;
        names.sort();
        let mut entries = Vec::new();
        for name in names {
            let child = if path.is_empty() { name.clone() } else { format!("{path}/{name}") };
            match self.observe(workspace, &child, false, true) {
                Ok(observation) => entries.push((observation.info.kind == Kind::Directory, name.to_lowercase(), self.entry(&child, &observation))),
                Err(error) if missing(&error) || error.code == "invalid-path" => {}
                Err(error) => return Err(error),
            }
        }
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        Ok(json!({ "path": path, "entries": entries.into_iter().map(|(_, _, entry)| entry).collect::<Vec<_>>() }))
    }

    pub fn read(&self, workspace: &Workspace, path: &str) -> HostResult<Value> {
        let before = self.observe(workspace, path, false, false)?;
        if before.target_info.as_ref().map(|info| info.kind) != Some(Kind::File) {
            return Err(ifs::not_file());
        }
        let base = |bytes: u64| json!({ "workspaceId": workspace.workspace_id, "path": path, "version": before.version, "bytes": bytes });
        // Images, audio, video and PDF go to their viewers as bytes; decoding them as text would only be discarded.
        if preview::is_media(path) {
            let mut document = base(before.bytes);
            document.as_object_mut().unwrap().extend([("content".into(), Value::Null), ("bom".into(), json!(false)), ("eol".into(), json!("lf")), ("readOnlyReason".into(), json!("media"))]);
            return Ok(document);
        }
        let large = before.bytes > MAX_TEXT_BYTES;
        let bytes = if large { ifs::read_range(&before.target, 0, MAX_PREVIEW_BYTES)? } else { ifs::read_bytes(&before.target, MAX_TEXT_BYTES)? };
        let after = self.observe(workspace, path, false, false)?;
        if before.version != after.version {
            return Err(stale(Some(&after.version)));
        }
        let decoded = decode(&bytes, large && (bytes.len() as u64) < before.bytes);
        let mut document = base(if large { before.bytes } else { bytes.len() as u64 });
        let fields = document.as_object_mut().unwrap();
        fields.insert("bom".into(), json!(decoded.bom));
        fields.insert("eol".into(), json!(decoded.eol));
        if large {
            let shown = match &decoded.content {
                Some(text) => json!({ "kind": "utf8", "text": text, "bytesRead": bytes.len(), "truncated": (bytes.len() as u64) < before.bytes }),
                None => hex_preview(&bytes, before.bytes),
            };
            fields.insert("content".into(), Value::Null);
            fields.insert("readOnlyReason".into(), json!(decoded.reason.unwrap_or("too-large")));
            fields.insert("preview".into(), shown);
            return Ok(document);
        }
        fields.insert("readOnlyReason".into(), json!(decoded.reason));
        if decoded.content.is_none() {
            fields.insert("preview".into(), hex_preview(&bytes, bytes.len() as u64));
        }
        fields.insert("content".into(), json!(decoded.content));
        Ok(document)
    }

    /// A regular file of a project opened for streaming its bytes to a viewer, checked against its observation.
    pub fn open_bytes(&self, workspace_id: &str, root_id: Option<&str>, path: &str) -> HostResult<(std::fs::File, u64, String)> {
        let workspace = self.resolve_workspace(workspace_id, root_id)?;
        let observation = self.observe(&workspace, path, false, false)?;
        let Some(target) = observation.target_info.filter(|info| info.kind == Kind::File) else {
            return Err(ifs::not_file());
        };
        let (file, size) = ifs::open_stable(&observation.target, &target.version)?;
        Ok((file, size, observation.version))
    }

    fn check_text(&self, content: &str) -> HostResult<()> {
        if content.contains('\0') {
            return Err(HostError::new("read-only", l("编辑器只能保存有效的 UTF-8 文本。", "Only valid UTF-8 text can be saved by the editor.")));
        }
        if content.len() as u64 > MAX_TEXT_BYTES {
            return Err(HostError::new("too-large", l("编辑后的文本超过了文件大小上限。", "The edited text exceeds the configured file limit.")));
        }
        Ok(())
    }

    fn save(&self, workspace: &Workspace, path: &str, content: &str, expected: &str) -> HostResult<Value> {
        let current = match self.read(workspace, path) {
            Ok(current) => current,
            Err(error) if missing(&error) => return Err(stale(None)),
            Err(error) => return Err(error),
        };
        if current["version"] != expected {
            return Err(stale(current["version"].as_str()));
        }
        if !current["readOnlyReason"].is_null() {
            return Err(HostError::new("read-only", l("这个文件无法作为 UTF-8 文本编辑。", "This file cannot be edited as bounded UTF-8 text.")));
        }
        let content = if current["eol"] == "crlf" { content.replace("\r\n", "\n").replace('\n', "\r\n") } else { content.to_owned() };
        let raw = format!("{}{content}", if current["bom"] == true { "\u{feff}" } else { "" });
        self.check_text(&raw)?;
        let observed = self.observe(workspace, path, false, false)?;
        let Some(target) = observed.target_info.as_ref().filter(|_| observed.version == current["version"]) else {
            return Err(stale(Some(&observed.version)));
        };
        if let Err(error) = ifs::write(&observed.target, raw.as_bytes(), Intent::ReplaceIfVersion(&target.version)) {
            if error.code == "version-conflict" {
                return Err(stale(self.observe(workspace, path, false, false).ok().map(|observation| observation.version).as_deref()));
            }
            return Err(error);
        }
        self.read(workspace, path)
    }

    fn prepare_new(&self, workspace: &Workspace, path: &str) -> HostResult<PathBuf> {
        let absolute = self.checked_path(workspace, path, false)?;
        if ifs::lstat(&absolute)?.is_some() {
            return Err(ifs::exists());
        }
        Ok(absolute)
    }

    fn new_target(&self, workspace: &Workspace, path: &str) -> HostResult<PathBuf> {
        let absolute = self.prepare_new(workspace, path)?;
        let target = ifs::resolve(&absolute)?;
        if !contains(&workspace.root, &target) {
            return Err(HostError::new("outside-workspace", l("目标位置在所选项目之外。", "The destination points outside the selected workspace.")));
        }
        Ok(target)
    }

    fn create(&self, workspace: &Workspace, path: &str, content: &str) -> HostResult<Value> {
        self.check_text(content)?;
        let target = self.new_target(workspace, path)?;
        ifs::write(&target, content.as_bytes(), Intent::CreateIfAbsent)?;
        self.read(workspace, path)
    }

    fn create_binary(&self, workspace: &Workspace, path: &str, encoded: &str) -> HostResult<Value> {
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| HostError::invalid("The file content is not valid base64."))?;
        if bytes.len() > MAX_BINARY_BYTES {
            return Err(ifs::too_large());
        }
        let target = self.new_target(workspace, path)?;
        ifs::write(&target, &bytes, Intent::CreateIfAbsent)?;
        Ok(self.entry(path, &self.observe(workspace, path, false, false)?))
    }

    fn rename(&self, workspace: &Workspace, path: &str, destination: &str, expected: &str) -> HostResult<Value> {
        let source = self.observe(workspace, path, false, true)?;
        if source.version != expected {
            return Err(stale(Some(&source.version)));
        }
        if path == destination {
            return Ok(self.entry(path, &source));
        }
        let target = self.prepare_new(workspace, destination)?;
        if source.info.kind == Kind::Directory && contains(&source.absolute, &target) {
            return Err(HostError::new("invalid-path", l("不能把文件夹移动到它自己里面。", "A directory cannot be moved into itself.")));
        }
        let fresh = self.observe(workspace, path, false, true)?;
        if fresh.version != source.version {
            return Err(stale(Some(&fresh.version)));
        }
        ifs::rename_no_replace(&fresh.absolute, &target, fresh.info.kind)?;
        Ok(self.entry(destination, &self.observe(workspace, destination, false, true)?))
    }

    /// Every entry of the tree at `path`, root first, with identities that tell a replaced entry from the one previewed.
    fn deletion_snapshot(&self, workspace: &Workspace, path: &str) -> HostResult<Vec<(String, String, Kind, u64, String)>> {
        relative_path(path, false)?;
        let mut pending = vec![path.to_owned()];
        let mut entries = Vec::new();
        while let Some(next) = pending.pop() {
            let observed = self.observe(workspace, &next, false, true)?;
            let identity = observed.info.version.splitn(3, ':').take(2).collect::<Vec<_>>().join(":");
            let bytes = if observed.info.kind == Kind::File { observed.bytes } else { 0 };
            entries.push((next.clone(), observed.info.version.clone(), observed.info.kind, bytes, identity));
            if entries.len() > MAX_DELETE_ENTRIES {
                return Err(HostError::new("too-large", l("所选内容超过了一次可删除的上限。", "The selected tree exceeds the configured deletion limit.")));
            }
            if observed.info.kind == Kind::Directory {
                let mut names = self.directory_names(&observed.absolute, MAX_DELETE_ENTRIES.saturating_sub(entries.len() + pending.len()))?;
                names.sort();
                for name in names.into_iter().rev() {
                    pending.push(format!("{next}/{name}"));
                }
            }
        }
        Ok(entries)
    }

    fn fingerprint(entries: &[(String, String, Kind, u64, String)]) -> String {
        let text: String = entries.iter().map(|(path, version, kind, bytes, identity)| format!("{}\n", json!({ "path": path, "version": version, "kind": kind.name(), "bytes": bytes, "identity": identity }))).collect();
        sha256_hex(text)
    }

    fn preview_delete(&self, workspace: &Workspace, path: &str) -> HostResult<Value> {
        let entries = self.deletion_snapshot(workspace, path)?;
        let first = entries.first().ok_or_else(gone)?;
        let now = now_ms();
        let mut deletions = self.deletions.lock().unwrap();
        deletions.retain(|(_, deletion)| deletion.expires > now);
        if deletions.len() >= MAX_DELETE_PREVIEWS {
            deletions.remove(0);
        }
        let token = uuid::Uuid::new_v4().to_string();
        deletions.push((
            token.clone(),
            Deletion { workspace_id: workspace.workspace_id.clone(), root: workspace.root.clone(), path: path.to_owned(), expires: now + DELETE_PREVIEW_LIFETIME_MS, fingerprint: Self::fingerprint(&entries) },
        ));
        Ok(json!({ "path": path, "kind": first.2.name(), "entries": entries.len(), "bytes": entries.iter().map(|entry| entry.3).sum::<u64>(), "token": token }))
    }

    fn delete(&self, workspace: &Workspace, path: &str, token: &str) -> HostResult<Value> {
        let preview = {
            let mut deletions = self.deletions.lock().unwrap();
            let index = deletions.iter().position(|(known, _)| known == token);
            index.map(|index| deletions.remove(index).1)
        };
        let valid = preview.as_ref().is_some_and(|preview| preview.workspace_id == workspace.workspace_id && preview.root == workspace.root && preview.path == path && preview.expires > now_ms());
        let Some(preview) = preview.filter(|_| valid) else {
            return Err(HostError::new("confirmation-required", l("请先确认这个路径的最新预览，再删除。", "Confirm a fresh preview of this exact path before deleting it.")));
        };
        let entries = self.deletion_snapshot(workspace, path)?;
        if Self::fingerprint(&entries) != preview.fingerprint {
            return Err(HostError::new("version-conflict", l("所选内容在删除预览后发生了变化，请确认更新后的预览。", "The selected tree changed after the deletion preview. Confirm the updated preview.")));
        }
        // No recursive remover is used: links are unlinked and directories must be empty.
        for (entry_path, version, kind, _, identity) in entries.into_iter().rev() {
            let observed = self.observe(workspace, &entry_path, false, true)?;
            let now_identity = observed.info.version.splitn(3, ':').take(2).collect::<Vec<_>>().join(":");
            if now_identity != identity || observed.info.kind != kind || (kind != Kind::Directory && observed.info.version != version) {
                return Err(stale(Some(&observed.version)));
            }
            if kind == Kind::Directory {
                std::fs::remove_dir(&observed.absolute)?;
            } else if kind == Kind::Symlink && cfg!(windows) && std::fs::metadata(&observed.absolute).is_ok_and(|info| info.is_dir()) {
                // A directory link on Windows is removed as a directory, which leaves its target alone.
                std::fs::remove_dir(&observed.absolute)?;
            } else {
                std::fs::remove_file(&observed.absolute)?;
            }
        }
        Ok(json!({ "path": path, "deleted": true }))
    }

    fn changes(&self, workspace: &Workspace, paths: &[String]) -> HostResult<Value> {
        if paths.len() > MAX_CHANGE_PATHS {
            return Err(HostError::new("too-large", "Too many opened paths were requested in one change check."));
        }
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        let mut directories = Vec::new();
        for path in paths.iter().filter(|path| seen.insert(path.as_str())) {
            relative_path(path, true)?;
            match self.observe(workspace, path, true, true) {
                Ok(observation) => {
                    result.push(json!({ "path": path, "version": observation.version, "kind": observation.info.kind.name() }));
                    directories.push(if observation.info.kind == Kind::Directory { observation.target } else { observation.target.parent().map(Path::to_path_buf).unwrap_or(observation.target) });
                }
                Err(error) if missing(&error) => {
                    result.push(json!({ "path": path, "version": null, "kind": "missing" }));
                    // A file deleted to be written again, as by a clean build or a checkout, is still noticed through its folder.
                    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                    if parent.is_empty() {
                        directories.push(workspace.root.clone());
                    } else if let Ok(folder) = self.observe(workspace, parent, true, true) {
                        if folder.info.kind == Kind::Directory {
                            directories.push(folder.target);
                        }
                    }
                }
                Err(error) => return Err(error),
            }
        }
        (self.on_changes_checked)(&workspace.workspace_id, &workspace.root_id, directories);
        Ok(Value::Array(result))
    }

    fn git(&self, root: &Path, args: &[&str]) -> Result<Vec<u8>, Option<i32>> {
        let overlay = (self.git_environment)(root);
        let mut env = crate::process::child_env(&overlay);
        env.retain(|name, _| !name.to_string_lossy().to_uppercase().starts_with("GIT_"));
        env.insert("GIT_OPTIONAL_LOCKS".into(), "0".into());
        env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
        let mut full = vec!["-c", "core.fsmonitor=false", "-C"];
        let root_text = root.to_string_lossy().into_owned();
        full.push(&root_text);
        full.extend_from_slice(args);
        let output = tokio::runtime::Handle::current().block_on(crate::process::run("git", &full, root, &env, MAX_TEXT_BYTES as usize + 1, Some(GIT_TIMEOUT))).map_err(|_| None)?;
        if output.success { Ok(output.stdout) } else { Err(output.code) }
    }

    fn diff(&self, workspace: &Workspace, path: &str) -> HostResult<Value> {
        relative_path(path, false)?;
        let current = match self.read(workspace, path) {
            Ok(current) => Some(current),
            Err(error) if missing(&error) => None,
            Err(error) => return Err(error),
        };
        let mut result = json!({
            "path": path,
            "current": current.as_ref().map_or(Value::Null, |document| document["content"].clone()),
            "version": current.as_ref().map_or(Value::Null, |document| document["version"].clone()),
        });
        let set = |result: &mut Value, base: Value, status: &str, reason: Option<&str>| {
            result["base"] = base;
            result["status"] = json!(status);
            if let Some(reason) = reason {
                result["reason"] = json!(reason);
            }
        };
        if let Some(reason) = current.as_ref().and_then(|document| document["readOnlyReason"].as_str()) {
            set(&mut result, Value::Null, "unavailable", Some(if reason == "media" { "binary" } else { reason }));
            return Ok(result);
        }
        let repository = match self.git(&workspace.root, &["rev-parse", "--show-toplevel"]) {
            Ok(output) => String::from_utf8_lossy(&output).trim().to_owned(),
            Err(Some(128)) => {
                set(&mut result, Value::Null, "unavailable", Some("not-git"));
                return Ok(result);
            }
            Err(_) => return Err(HostError::new("io-error", l("无法运行 Git。", "Git could not be run."))),
        };
        let repository_path = crate::paths::relative_to(&realpath(Path::new(&repository)).unwrap_or_else(|_| PathBuf::from(&repository)), &join_relative(&workspace.root, path))
            .unwrap_or_else(|| path.to_owned());
        let added = |result: &mut Value| set(result, json!(""), if current.is_none() { "unchanged" } else { "added" }, None);
        let tree = match self.git(Path::new(&repository), &["ls-tree", "-z", "HEAD", "--", &repository_path]) {
            Ok(output) => String::from_utf8_lossy(&output).into_owned(),
            Err(Some(128)) => {
                added(&mut result);
                return Ok(result);
            }
            Err(_) => return Err(HostError::new("io-error", l("无法运行 Git。", "Git could not be run."))),
        };
        if tree.is_empty() {
            added(&mut result);
            return Ok(result);
        }
        let object = tree.split('\t').next().and_then(|head| {
            let mut parts = head.split(' ');
            let (_, kind, id) = (parts.next()?, parts.next()?, parts.next()?);
            (kind == "blob").then(|| id.to_owned())
        });
        let Some(object) = object else {
            set(&mut result, Value::Null, "unavailable", Some("binary"));
            return Ok(result);
        };
        let size: u64 = self.git(&workspace.root, &["cat-file", "-s", &object]).ok().and_then(|output| String::from_utf8_lossy(&output).trim().parse().ok()).unwrap_or(u64::MAX);
        if size > MAX_TEXT_BYTES {
            set(&mut result, Value::Null, "unavailable", Some("too-large"));
            return Ok(result);
        }
        let blob = self.git(&workspace.root, &["cat-file", "blob", &object]).map_err(|_| HostError::new("io-error", l("Git 无法读取已提交的文件。", "Git could not read the committed file.")))?;
        let decoded = decode(&blob, false);
        let Some(content) = decoded.content else {
            set(&mut result, Value::Null, "unavailable", Some(decoded.reason.unwrap_or("unsupported-encoding")));
            return Ok(result);
        };
        // Git stores LF; a CRLF checkout from core.autocrlf or eol attributes is not a content change.
        let crlf_checkout = current.as_ref().is_some_and(|document| document["eol"] == "crlf") && decoded.eol == "lf";
        let base = if crlf_checkout { content.replace('\n', "\r\n") } else { content };
        let status = match &current {
            None => "deleted",
            Some(document) if document["content"].as_str() == Some(base.as_str()) => "unchanged",
            Some(_) => "modified",
        };
        set(&mut result, json!(base), status, None);
        Ok(result)
    }
}

/// Quick Open: regular files whose relative path contains every query token, without following directory links.
pub fn search(root: &Path, query: &str, requested: Option<usize>) -> Value {
    let lower = query.trim().to_lowercase().replace('\\', "/");
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    let limit = requested.unwrap_or(SEARCH_RESULT_LIMIT).min(SEARCH_RESULT_LIMIT);
    let deadline = Instant::now() + SEARCH_TIMEOUT;
    let mut pending = vec![String::new()];
    let mut paths: Vec<String> = Vec::new();
    let mut scanned = 0;
    let mut truncated = false;
    let done = |mut paths: Vec<String>, truncated: bool| {
        paths.sort();
        json!({ "paths": paths, "truncated": truncated })
    };
    while let Some(path) = pending.pop() {
        if Instant::now() >= deadline {
            return done(paths, true);
        }
        let absolute = join_relative(root, &path);
        let Ok(entries) = std::fs::read_dir(&absolute) else {
            truncated = true;
            continue;
        };
        for entry in entries {
            if Instant::now() >= deadline || scanned == SEARCH_MAX_ENTRIES {
                return done(paths, true);
            }
            let Ok(entry) = entry else { continue };
            scanned += 1;
            let name = entry.file_name().to_string_lossy().into_owned();
            let child = if path.is_empty() { name.clone() } else { format!("{path}/{name}") };
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() && !kind.is_symlink() && !SEARCH_EXCLUDED.contains(&name.as_str()) {
                pending.push(child.clone());
            }
            if !kind.is_file() || !tokens.iter().all(|token| child.to_lowercase().contains(token)) {
                continue;
            }
            paths.push(child);
            if paths.len() == limit {
                return done(paths, true);
            }
        }
    }
    done(paths, truncated)
}

/// Drive letters on Windows that answer within a second; `/` and the home folder on Linux.
fn filesystem_roots() -> Vec<String> {
    if cfg!(not(windows)) {
        let home = crate::env::home_dir().display().to_string();
        return if home == "/" { vec!["/".into()] } else { vec!["/".into(), home] };
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    for letter in b'A'..=b'Z' {
        let drive = format!("{}:\\", char::from(letter));
        let sender = sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send((drive.clone(), std::fs::metadata(&drive).is_ok()));
        });
    }
    drop(sender);
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut answered: HashMap<String, bool> = HashMap::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match receiver.recv_timeout(left) {
            Ok((drive, present)) => {
                answered.insert(drive, present);
            }
            Err(_) => break,
        }
    }
    // A drive letter that does not answer in time is mapped but slow, so it is offered.
    (b'A'..=b'Z').map(|letter| format!("{}:\\", char::from(letter))).filter(|drive| answered.get(drive).copied().unwrap_or(true)).collect()
}

/// One level of the in-app folder browser: child folders (following links to folders), sorted by name.
pub fn list_directories(path: Option<&str>, show_hidden: bool) -> HostResult<Value> {
    let target = match path {
        None => crate::env::home_dir(),
        Some(path) if fully_qualified(path) => crate::paths::normalize(Path::new(path)),
        Some(_) => return Err(not_absolute()),
    };
    let failure = |error: std::io::Error| match error.kind() {
        std::io::ErrorKind::NotFound => HostError::new("not-found", l(&format!("文件夹不存在：{}", target.display()), &format!("The directory does not exist: {}", target.display()))),
        std::io::ErrorKind::NotADirectory => HostError::new("not-directory", l(&format!("这个路径不是文件夹：{}", target.display()), &format!("The path is not a directory: {}", target.display()))),
        std::io::ErrorKind::PermissionDenied => HostError::new("permission-denied", l(&format!("无法读取文件夹：{}", target.display()), &format!("The directory cannot be read: {}", target.display()))),
        _ => HostError::new("io-error", error.to_string()),
    };
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&target).map_err(failure)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() || (kind.is_symlink() && std::fs::metadata(entry.path()).is_ok_and(|info| info.is_dir())) {
            names.push(name);
        }
    }
    names.sort_by_key(|name| name.to_lowercase());
    names.truncate(MAX_DIRECTORY_ENTRIES);
    let entries: Vec<Value> = names.iter().map(|name| json!({ "name": name, "path": target.join(name), "hidden": name.starts_with('.') })).collect();
    let parent = target.parent().filter(|parent| *parent != target && !parent.as_os_str().is_empty()).map(Path::to_path_buf);
    Ok(json!({ "path": target, "parent": parent, "roots": filesystem_roots(), "entries": entries }))
}

/// Create one folder whose parent exists, then list it.
pub fn create_directory(path: &str) -> HostResult<Value> {
    if !fully_qualified(path) {
        return Err(not_absolute());
    }
    let target = crate::paths::normalize(Path::new(path));
    if target.file_name().is_none() {
        return Err(HostError::new("invalid-path", l("请在已有的文件夹中新建文件夹。", "Choose a new directory inside an existing directory.")));
    }
    match std::fs::create_dir(&target) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(HostError::new("already-exists", l(&format!("文件夹已存在：{}", target.display()), &format!("The directory already exists: {}", target.display()))));
        }
        Err(error) => return Err(error.into()),
    }
    list_directories(Some(&target.to_string_lossy()), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_like_3x() {
        let text = decode(b"\xef\xbb\xbfa\r\nb\r\n", false);
        assert_eq!((text.content.as_deref(), text.bom, text.eol), (Some("a\r\nb\r\n"), true, "crlf"));
        assert_eq!(decode(b"a\nb\r\n", false).eol, "mixed");
        assert_eq!(decode(b"a\0", false).reason, Some("binary"));
        assert_eq!(decode(b"\xff\xfea", false).reason, Some("unsupported-encoding"));
        assert_eq!(decode(b"\xe9\x9b", false).reason, Some("unsupported-encoding"));
        assert_eq!(decode(b"a\xe9\x9b", true).content.as_deref(), Some("a"));
    }

    #[test]
    fn previews_bytes_as_hex() {
        let preview = hex_preview(b"Rainy\x00\x01", 7);
        assert_eq!(preview["text"], "00000000  52 61 69 6e 79 00 01                             |Rainy..|");
        assert_eq!(preview["truncated"], false);
    }

    #[test]
    fn finds_files_by_tokens() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/ui")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/x")).unwrap();
        std::fs::write(dir.path().join("src/ui/chat.rs"), "").unwrap();
        std::fs::write(dir.path().join("node_modules/x/chat.js"), "").unwrap();
        let found = search(dir.path(), "UI chat", None);
        assert_eq!(found["paths"], json!(["src/ui/chat.rs"]));
    }
}
