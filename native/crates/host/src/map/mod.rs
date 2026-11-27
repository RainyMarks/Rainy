//! Project maps (3.x `map/index.ts`): scanned on request, kept for a short while, and explained by the chat model on demand.

pub mod footprint;
pub mod graph;
pub mod imports;
pub mod languages;
pub mod scan;
pub mod stack;
pub mod tour;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::projects::Projects;

#[allow(unused_imports)]
pub use tour::{FileSymbols, SymbolEntry, symbol_outline};

/// Import dependencies of one file or directory prefix, exposed to the Agent's `module_deps` tool.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModuleDeps {
    pub target: String,
    pub found: bool,
    pub language: String,
    pub lines: u64,
    pub imports: Vec<String>,
    pub imported_by: Vec<String>,
    pub external_packages: Vec<String>,
}

/// One file of the project.
#[derive(Clone, Debug, Serialize)]
pub struct MapFile {
    /// Path under the project's primary directory, with `/` separators.
    pub path: String,
    /// Language id such as `typescript` or `python`; empty when unknown.
    pub language: String,
    pub kind: String,
    /// Lines of text; an estimate for large files and 0 for binary ones.
    pub lines: u64,
    pub bytes: u64,
}

/// An external package and the files (indexes) that import it.
#[derive(Clone, Debug, Serialize)]
pub struct MapPackage {
    pub name: String,
    pub files: Vec<usize>,
}

/// A language, framework, library or tool the project uses.
#[derive(Clone, Debug, Serialize)]
pub struct StackItem {
    pub name: String,
    pub kind: String,
}

/// What the AI wrote about the project's structure.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapExplanation {
    pub generated_at: f64,
    pub locale: String,
    pub summary: String,
    pub modules: Map<String, Value>,
    pub model: String,
    /// How to run it, where to start reading, how its main features work and where to make common changes; absent
    /// from explanations written before 4.8.
    #[serde(flatten)]
    pub tour: tour::Tour,
    /// Files of the project added and removed since it was written, when any were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift: Option<Drift>,
}

/// Files added and removed since an explanation was written.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Drift {
    pub added: usize,
    pub removed: usize,
}

/// Everything the map page draws.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMap {
    pub workspace_id: String,
    pub title: String,
    pub generated_at: u64,
    pub truncated: bool,
    pub files: Vec<MapFile>,
    /// Imports between project files as index pairs into `files`: `[from, to, from, to, …]`.
    pub imports: Vec<usize>,
    pub packages: Vec<MapPackage>,
    pub stack: Vec<StackItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<MapExplanation>,
}

/// A map younger than this is served again instead of scanning the project anew.
const CACHE: Duration = Duration::from_secs(20);
/// A map this young serves the file relations, which follow the editor from file to file.
const RELATIONS_CACHE: Duration = Duration::from_secs(180);
/// Maximum number of scanned project maps kept in memory at once.
const MAX_CACHED_MAPS: usize = 8;
/// Boxes described to the model: the top level and the level below its directories.
const EXPLAIN_MAX_NODES: usize = 48;
const EXPLAIN_TIMEOUT: Duration = Duration::from_secs(240);
const EXPLAIN_MAX_TOKENS: u64 = 6000;

fn insert_cached_map(cache: &Mutex<HashMap<String, (Arc<ProjectMap>, Instant)>>, key: String, map: Arc<ProjectMap>) {
    let mut guard = cache.lock().unwrap();
    if !guard.contains_key(&key) && guard.len() >= MAX_CACHED_MAPS {
        if let Some(oldest) = guard.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| k.clone()) {
            guard.remove(&oldest);
        }
    }
    guard.insert(key, (map, Instant::now()));
}

fn describe_node(map: &ProjectMap, view: &graph::MapView, node: &graph::MapNode) -> String {
    let base = match node.node_type {
        graph::NodeType::Directory => format!("{}/", node.id),
        graph::NodeType::Loose => node.id[..node.id.len() - 1].to_owned(),
        graph::NodeType::File => String::new(),
    };
    let mut main = node.files.clone();
    main.sort_by(|left, right| map.files[*right].lines.cmp(&map.files[*left].lines));
    let main: Vec<&str> = main.iter().take(5).map(|index| map.files[*index].path.get(base.len()..).unwrap_or(&map.files[*index].path)).collect();
    let uses: Vec<&str> = view.edges.iter().filter(|edge| edge.from == node.id).map(|edge| edge.to.as_str()).collect();
    let used_by: Vec<&str> = view.edges.iter().filter(|edge| edge.to == node.id).map(|edge| edge.from.as_str()).collect();
    let languages = node.languages.iter().take(3).map(|(language, _)| language.as_str()).collect::<Vec<_>>().join(", ");
    let kind = match node.node_type {
        graph::NodeType::Directory => "directory",
        graph::NodeType::Loose => "loose",
        graph::NodeType::File => "file",
    };
    let mut line = format!("- {} ({kind}, {}, {} files, {} lines{})", node.id, node.kind, node.files.len(), node.lines, if languages.is_empty() { String::new() } else { format!(", {languages}") });
    if node.node_type != graph::NodeType::File {
        line.push_str(&format!("; main files: {}", main.join(", ")));
    }
    if !uses.is_empty() {
        line.push_str(&format!("; imports: {}", uses[..uses.len().min(6)].join(", ")));
    }
    if !used_by.is_empty() {
        line.push_str(&format!("; imported by: {}", used_by[..used_by.len().min(6)].join(", ")));
    }
    line
}

/// The parts of a project described to the model: the top-level boxes, then the boxes inside its larger directories.
pub fn explain_outline(map: &ProjectMap) -> (Vec<String>, Vec<String>) {
    let top = graph::map_view(map, "");
    let mut ids: Vec<String> = Vec::new();
    let mut lines = vec!["Top level:".to_owned()];
    for node in top.nodes.iter().filter(|node| !node.outside) {
        ids.push(node.id.clone());
        lines.push(describe_node(map, &top, node));
    }
    let mut inner: Vec<&graph::MapNode> = top.nodes.iter().filter(|node| !node.outside && node.openable && (node.kind == "code" || node.kind == "test")).collect();
    inner.sort_by(|left, right| right.lines.cmp(&left.lines));
    for parent in inner {
        if ids.len() >= EXPLAIN_MAX_NODES {
            break;
        }
        let view = graph::map_view(map, &parent.id);
        let children: Vec<&graph::MapNode> = view.nodes.iter().filter(|node| !node.outside && !ids.contains(&node.id)).collect();
        if children.is_empty() {
            continue;
        }
        lines.push(format!("Inside {}:", parent.id));
        for node in children {
            if ids.len() >= EXPLAIN_MAX_NODES {
                break;
            }
            ids.push(node.id.clone());
            lines.push(describe_node(map, &view, node));
        }
    }
    (ids, lines)
}

type Scan = Shared<BoxFuture<'static, Result<Arc<ProjectMap>, HostError>>>;

/// Scans projects and keeps their explanations.
pub struct ProjectMaps {
    projects: Arc<Projects>,
    directory: PathBuf,
    cache: Arc<Mutex<HashMap<String, (Arc<ProjectMap>, Instant)>>>,
    scanning: Arc<Mutex<HashMap<String, Scan>>>,
    pub(crate) explaining: tokio::sync::Mutex<()>,
    cancelled: Arc<AtomicBool>,
}

impl ProjectMaps {
    pub fn new(projects: Arc<Projects>, directory: PathBuf) -> Self {
        Self { projects, directory, cache: Arc::default(), scanning: Arc::default(), explaining: tokio::sync::Mutex::new(()), cancelled: Arc::default() }
    }

    pub(crate) fn project(&self, workspace_id: &str) -> HostResult<crate::projects::Project> {
        self.projects.get(workspace_id).ok_or_else(|| HostError::new("workspace-not-found", l("工作区不存在。", "This workspace does not exist.")))
    }

    /// The map of a project with its stored explanation; `refresh` scans again even when a recent map exists.
    pub async fn get(&self, workspace_id: &str, refresh: bool) -> HostResult<ProjectMap> {
        let cached = self.cache.lock().unwrap().get(workspace_id).filter(|(_, at)| at.elapsed() < CACHE).map(|(map, _)| map.clone());
        let map = match cached {
            Some(map) if !refresh => map,
            _ => self.scan(workspace_id)?.await?,
        };
        let mut map = (*map).clone();
        map.explanation = self.stored(workspace_id).map(|(mut explanation, paths)| {
            if let Some(paths) = paths {
                let (added, removed) = tour::drift(&paths, &map);
                explanation.drift = (added + removed > 0).then_some(Drift { added, removed });
            }
            explanation
        });
        Ok(map)
    }

    /// What one file imports and is imported by, the external packages it uses, the AI's line on its part of the
    /// project and what the tour says about it. A map scanned in the last minutes is reused, as this follows the file
    /// open in the editor.
    pub async fn relations(&self, workspace_id: &str, path: &str) -> HostResult<Value> {
        let cached = self.cache.lock().unwrap().get(workspace_id).filter(|(_, at)| at.elapsed() < RELATIONS_CACHE).map(|(map, _)| map.clone());
        let map = match cached {
            Some(map) => map,
            None => self.scan(workspace_id)?.await?,
        };
        let Some(index) = map.files.iter().position(|file| file.path == path) else {
            return Ok(json!({ "path": path, "known": false }));
        };
        let file = |index: usize| json!({ "path": map.files[index].path, "lines": map.files[index].lines, "kind": map.files[index].kind });
        let mut imports: Vec<usize> = map.imports.chunks_exact(2).filter(|pair| pair[0] == index).map(|pair| pair[1]).collect();
        let mut imported_by: Vec<usize> = map.imports.chunks_exact(2).filter(|pair| pair[1] == index).map(|pair| pair[0]).collect();
        for list in [&mut imports, &mut imported_by] {
            list.sort_by(|left, right| map.files[*left].path.cmp(&map.files[*right].path));
            list.dedup();
        }
        let packages: Vec<&str> = map.packages.iter().filter(|package| package.files.contains(&index)).map(|package| package.name.as_str()).collect();
        let explanation = self.stored(workspace_id).map(|(explanation, _)| explanation);
        // The line on the closest part that holds the file: the file itself, else its nearest directory described.
        let area = explanation.as_ref().and_then(|explanation| {
            let mut candidate = path.to_owned();
            loop {
                if let Some(text) = explanation.modules.get(&candidate).and_then(Value::as_str) {
                    return Some(json!({ "path": candidate, "text": text }));
                }
                candidate = candidate.rsplit_once('/')?.0.to_owned();
            }
        });
        let mut mentions = Vec::new();
        if let Some(explanation) = &explanation {
            let tour = &explanation.tour;
            let places = tour.start.iter().chain(tour.flows.iter().flat_map(|flow| &flow.steps)).chain(tour.tasks.iter().flat_map(|task| &task.places));
            for place in places.filter(|place| place.path.as_deref() == Some(path)) {
                if !mentions.iter().any(|seen: &Value| seen["text"] == place.text) {
                    mentions.push(json!({ "text": place.text, "line": place.line }));
                }
            }
        }
        Ok(json!({
            "path": path,
            "known": true,
            "lines": map.files[index].lines,
            "language": map.files[index].language,
            "imports": imports.into_iter().map(file).collect::<Vec<_>>(),
            "importedBy": imported_by.into_iter().map(file).collect::<Vec<_>>(),
            "packages": packages,
            "area": area,
            "mentions": mentions,
        }))
    }

    /// Query structured import dependencies for a file or directory prefix inside a registered workspace (reusing the
    /// cached ProjectMap scan).
    pub async fn module_deps(&self, workspace_id: &str, path: &str) -> HostResult<ModuleDeps> {
        let cached = self.cache.lock().unwrap().get(workspace_id).filter(|(_, at)| at.elapsed() < RELATIONS_CACHE).map(|(map, _)| map.clone());
        let map = match cached {
            Some(map) => map,
            None => self.scan(workspace_id)?.await?,
        };
        Ok(extract_module_deps(&map, path))
    }

    /// Query import dependencies for a file or directory under an arbitrary project directory `root` (reuses the
    /// workspace cache when `workspace_id` matches `root`, otherwise caches a bounded scan keyed by canonical root).
    pub async fn module_deps_in_dir(&self, root: &std::path::Path, path: &str, workspace_id: Option<&str>) -> HostResult<ModuleDeps> {
        let canonical_root = crate::paths::realpath(root)?;
        if let Some(id) = workspace_id
            && let Ok(project) = self.project(id)
            && crate::paths::realpath(std::path::Path::new(&project.path)).ok().as_deref() == Some(canonical_root.as_path())
        {
            return self.module_deps(id, path).await;
        }
        let cache_key = format!("dir:{}", canonical_root.to_string_lossy());
        let cached = self.cache.lock().unwrap().get(&cache_key).filter(|(_, at)| at.elapsed() < RELATIONS_CACHE).map(|(map, _)| map.clone());
        let map = match cached {
            Some(map) => map,
            None => self.scan_dir(cache_key, canonical_root).await?,
        };
        Ok(extract_module_deps(&map, path))
    }

    fn scan_dir(&self, cache_key: String, scan_root: PathBuf) -> Scan {
        let mut scanning = self.scanning.lock().unwrap();
        if let Some(pending) = scanning.get(&cache_key) {
            return pending.clone();
        }
        let (key, cancelled, cache, all) = (cache_key.clone(), self.cancelled.clone(), self.cache.clone(), self.scanning.clone());
        let pending: Scan = async move {
            let task = tokio::task::spawn_blocking(move || {
                let result = scan::scan_project(&scan_root, scan::ScanLimits::default(), &cancelled);
                ProjectMap {
                    workspace_id: String::new(),
                    title: scan_root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    generated_at: crate::text::now_ms(),
                    truncated: result.truncated,
                    files: result.files,
                    imports: result.imports,
                    packages: result.packages,
                    stack: result.stack,
                    explanation: None,
                }
            });
            let result = task.await.map(Arc::new).map_err(|error| HostError::other(error.to_string()));
            if let Ok(map) = &result {
                insert_cached_map(&cache, key.clone(), map.clone());
            }
            all.lock().unwrap().remove(&key);
            result
        }
        .boxed()
        .shared();
        scanning.insert(cache_key, pending.clone());
        pending
    }

    /// Ask the default model to explain the project, and keep the explanation; one request per project at a time.
    pub async fn explain(&self, workspace_id: &str, models: &crate::agent::models::Models, locale: &str) -> HostResult<MapExplanation> {
        let _one = self.explaining.lock().await;
        let project = self.project(workspace_id)?;
        let resolved = models.resolve(None).ok_or_else(|| HostError::new("no-model", l("还没有可用的模型。请先在“设置 → 模型”中添加一个模型。", "No model is set up yet. Add one in Settings → Models.")))?;
        let map = self.get(workspace_id, false).await?;
        let (ids, lines) = explain_outline(&map);
        let code: Vec<&MapFile> = map.files.iter().filter(|file| file.kind == "code").collect();
        let stack = map.stack.iter().map(|item| item.name.as_str()).collect::<Vec<_>>().join(", ");
        let language = if locale == "zh" { "Simplified Chinese" } else { "English" };
        let root = std::path::PathBuf::from(&project.path);
        let material = {
            let map = map.clone();
            let locale = locale.to_owned();
            tokio::task::spawn_blocking(move || tour::material(&map, &root, &locale)).await.map_err(|error| HostError::other(error.to_string()))?
        };
        let prompt = [
            format!("Project: {}", project.title),
            format!("Stack: {}", if stack.is_empty() { "unknown" } else { &stack }),
            format!("Size: {} files, {} code files, {} lines of code", map.files.len(), code.len(), code.iter().map(|file| file.lines).sum::<u64>()),
            String::new(),
            lines.join("
"),
            String::new(),
            material,
            String::new(),
            format!("Write every text in {language}; keep identifiers, paths and commands as they are."),
            format!("Part ids: {}", serde_json::to_string(&ids).unwrap_or_default()),
        ]
        .join("
");
        let options = crate::agent::models::Completion { system: Some(tour::TOUR_SYSTEM.into()), max_tokens: Some(EXPLAIN_MAX_TOKENS), thinking: Some(crate::agent::llm::Level::Off), cancel: None };
        let reply = models.complete(&resolved, &prompt, &[], EXPLAIN_TIMEOUT, options).await;
        if reply.stop_reason == "error" || reply.stop_reason == "aborted" {
            return Err(HostError::new("explain-failed", reply.error_message.unwrap_or_else(|| l("讲解请求失败。", "The explanation request failed."))));
        }
        let text = crate::agent::llm::reply_text(&reply);
        let unreadable = || HostError::new("explain-failed", l("模型的回复无法解析，请重试。", "The model's reply could not be read. Try again."));
        let json_text = match (text.find('{'), text.rfind('}')) {
            (Some(start), Some(end)) if start < end => &text[start..=end],
            _ => return Err(unreadable()),
        };
        let parsed: Value = serde_json::from_str(json_text).map_err(|_| unreadable())?;
        let (Some(summary), Some(modules)) = (parsed["summary"].as_str(), parsed["modules"].as_object()) else { return Err(unreadable()) };
        let modules: Map<String, Value> = modules
            .iter()
            .filter_map(|(id, line)| {
                let line = line.as_str()?.trim();
                (ids.contains(id) && !line.is_empty()).then(|| (id.clone(), Value::String(line.chars().take(160).collect())))
            })
            .collect();
        let explanation = MapExplanation {
            generated_at: crate::text::now_ms() as f64,
            locale: locale.to_owned(),
            summary: summary.trim().chars().take(1200).collect(),
            modules,
            model: format!("{}/{}", resolved.model.provider, resolved.model.id),
            tour: tour::Checker::new(&map).tour(&parsed),
            drift: None,
        };
        self.store_explanation(workspace_id, &explanation, &tour::tracked_paths(&map))?;
        Ok(explanation)
    }

    /// Stop scans, for shutdown.
    pub fn close(&self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn scan(&self, workspace_id: &str) -> HostResult<Scan> {
        let mut scanning = self.scanning.lock().unwrap();
        if let Some(pending) = scanning.get(workspace_id) {
            return Ok(pending.clone());
        }
        let project = self.project(workspace_id)?;
        let (id, cancelled, cache, all) = (workspace_id.to_owned(), self.cancelled.clone(), self.cache.clone(), self.scanning.clone());
        let pending: Scan = async move {
            let key = id.clone();
            let task = tokio::task::spawn_blocking(move || {
                let result = scan::scan_project(std::path::Path::new(&project.path), scan::ScanLimits::default(), &cancelled);
                ProjectMap {
                    workspace_id: id,
                    title: project.title,
                    generated_at: crate::text::now_ms(),
                    truncated: result.truncated,
                    files: result.files,
                    imports: result.imports,
                    packages: result.packages,
                    stack: result.stack,
                    explanation: None,
                }
            });
            let result = task.await.map(Arc::new).map_err(|error| HostError::other(error.to_string()));
            if let Ok(map) = &result {
                insert_cached_map(&cache, key.clone(), map.clone());
            }
            all.lock().unwrap().remove(&key);
            result
        }
        .boxed()
        .shared();
        scanning.insert(workspace_id.to_owned(), pending.clone());
        Ok(pending)
    }

    fn path(&self, workspace_id: &str) -> PathBuf {
        self.directory.join(format!("{}.json", crate::text::encode_uri_component(workspace_id)))
    }

    /// The kept explanation, and the files it was written for (absent before 4.8).
    fn stored(&self, workspace_id: &str) -> Option<(MapExplanation, Option<Vec<String>>)> {
        let value: Value = crate::store::read_json(&self.path(workspace_id)).ok()??;
        if value["version"] != 1 {
            return None;
        }
        let explanation: MapExplanation = serde_json::from_value(value["explanation"].clone()).ok()?;
        let paths = serde_json::from_value(value["paths"].clone()).ok();
        (explanation.locale == "zh" || explanation.locale == "en").then_some((explanation, paths))
    }

    /// Keep an explanation written by the model, with the files it was written for.
    pub(crate) fn store_explanation(&self, workspace_id: &str, explanation: &MapExplanation, paths: &[String]) -> HostResult<()> {
        crate::store::write_json(&self.path(workspace_id), &json!({ "version": 1, "explanation": explanation, "paths": paths }))?;
        Ok(())
    }
}

fn extract_module_deps(map: &ProjectMap, raw_path: &str) -> ModuleDeps {
    let target = raw_path.trim().trim_start_matches("./").trim_end_matches('/').replace('\\', "/");
    if target.is_empty() {
        return ModuleDeps { target, found: false, language: String::new(), lines: 0, imports: Vec::new(), imported_by: Vec::new(), external_packages: Vec::new() };
    }
    // Exact file match first, then unique suffix match, then directory prefix aggregation.
    let exact = map.files.iter().position(|file| file.path == target).or_else(|| {
        let suffix = format!("/{target}");
        let mut matches = map.files.iter().enumerate().filter(|(_, file)| file.path.ends_with(&suffix)).map(|(idx, _)| idx);
        match (matches.next(), matches.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    });
    if let Some(index) = exact {
        let mut imports: Vec<String> = map.imports.chunks_exact(2).filter(|pair| pair[0] == index).map(|pair| map.files[pair[1]].path.clone()).collect();
        let mut imported_by: Vec<String> = map.imports.chunks_exact(2).filter(|pair| pair[1] == index).map(|pair| map.files[pair[0]].path.clone()).collect();
        let mut external_packages: Vec<String> = map.packages.iter().filter(|pkg| pkg.files.contains(&index)).map(|pkg| pkg.name.clone()).collect();
        for list in [&mut imports, &mut imported_by, &mut external_packages] {
            list.sort();
            list.dedup();
        }
        return ModuleDeps {
            target: map.files[index].path.clone(),
            found: true,
            language: map.files[index].language.clone(),
            lines: map.files[index].lines,
            imports,
            imported_by,
            external_packages,
        };
    }

    let prefix = format!("{target}/");
    let members: std::collections::HashSet<usize> = map.files.iter().enumerate().filter(|(_, file)| file.path.starts_with(&prefix)).map(|(idx, _)| idx).collect();
    if members.is_empty() {
        return ModuleDeps { target, found: false, language: String::new(), lines: 0, imports: Vec::new(), imported_by: Vec::new(), external_packages: Vec::new() };
    }
    let lines: u64 = members.iter().map(|&idx| map.files[idx].lines).sum();
    let mut imports = Vec::new();
    let mut imported_by = Vec::new();
    for pair in map.imports.chunks_exact(2) {
        let from_in = members.contains(&pair[0]);
        let to_in = members.contains(&pair[1]);
        if from_in && !to_in {
            imports.push(map.files[pair[1]].path.clone());
        } else if !from_in && to_in {
            imported_by.push(map.files[pair[0]].path.clone());
        }
    }
    let mut external_packages: Vec<String> = map.packages.iter().filter(|pkg| pkg.files.iter().any(|idx| members.contains(idx))).map(|pkg| pkg.name.clone()).collect();
    for list in [&mut imports, &mut imported_by, &mut external_packages] {
        list.sort();
        list.dedup();
    }
    ModuleDeps { target, found: true, language: "directory".into(), lines, imports, imported_by, external_packages }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_symbol_outline_and_module_deps() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub struct Engine {}\npub async fn start() {\n}\nimpl Engine {\n    pub fn run(&self) {}\n}\n",
        )
        .unwrap();
        let outline = symbol_outline(dir.path(), std::path::Path::new("src/lib.rs"), 50).unwrap();
        assert_eq!(outline.path, "src/lib.rs");
        assert_eq!(outline.language, "rust");
        assert!(!outline.truncated);
        assert_eq!(outline.symbols.len(), 4);
        assert_eq!((outline.symbols[0].line, outline.symbols[0].kind.as_str()), (1, "struct"));
        assert_eq!((outline.symbols[1].line, outline.symbols[1].kind.as_str()), (2, "fn"));
        assert_eq!((outline.symbols[2].line, outline.symbols[2].kind.as_str()), (4, "impl"));
        assert_eq!((outline.symbols[3].line, outline.symbols[3].kind.as_str()), (5, "fn"));

        let map = ProjectMap {
            workspace_id: "w".into(),
            title: "t".into(),
            generated_at: 0,
            truncated: false,
            files: vec![
                MapFile { path: "src/a.ts".into(), language: "typescript".into(), kind: "code".into(), lines: 20, bytes: 200 },
                MapFile { path: "src/b.ts".into(), language: "typescript".into(), kind: "code".into(), lines: 10, bytes: 100 },
                MapFile { path: "app/main.ts".into(), language: "typescript".into(), kind: "code".into(), lines: 30, bytes: 300 },
            ],
            imports: vec![2, 0, 0, 1],
            packages: vec![MapPackage { name: "react".into(), files: vec![0, 2] }],
            stack: vec![],
            explanation: None,
        };
        let file_deps = extract_module_deps(&map, "src/a.ts");
        assert!(file_deps.found);
        assert_eq!(file_deps.imports, vec!["src/b.ts"]);
        assert_eq!(file_deps.imported_by, vec!["app/main.ts"]);
        assert_eq!(file_deps.external_packages, vec!["react"]);

        let dir_deps = extract_module_deps(&map, "src");
        assert!(dir_deps.found);
        assert!(dir_deps.imports.is_empty());
        assert_eq!(dir_deps.imported_by, vec!["app/main.ts"]);
        assert_eq!(dir_deps.external_packages, vec!["react"]);
    }
}
