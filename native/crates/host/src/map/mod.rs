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
/// Boxes described to the model: the top level and the level below its directories.
const EXPLAIN_MAX_NODES: usize = 48;
const EXPLAIN_TIMEOUT: Duration = Duration::from_secs(240);
const EXPLAIN_MAX_TOKENS: u64 = 6000;

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
                cache.lock().unwrap().insert(key.clone(), (map.clone(), Instant::now()));
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
