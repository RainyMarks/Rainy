//! Project maps (`map.get`, `map.explain`, `map.relations`, `map.footprint`; `src/shared/map-protocol.ts`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One file of the project map.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct MapFile {
    pub path: String,
    pub language: String,
    pub kind: String,
    pub lines: u64,
    pub bytes: u64,
}

/// An external package and the file indexes that import it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct MapPackage {
    pub name: String,
    pub files: Vec<usize>,
}

/// One stack item detected in the project.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct StackItem {
    pub name: String,
    pub kind: String,
}

/// Files added and removed since an explanation was written.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct MapDrift {
    pub added: usize,
    pub removed: usize,
}

/// A file or directory pointed to by a project tour.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MapPlace {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

/// One project flow in a tour.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MapFlow {
    pub title: String,
    pub steps: Vec<MapPlace>,
}

/// One common task in a tour.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MapTask {
    pub task: String,
    pub places: Vec<MapPlace>,
}

/// Tour sections stored inside a `MapExplanation`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MapTour {
    #[serde(default)]
    pub run: Vec<String>,
    #[serde(default)]
    pub start: Vec<MapPlace>,
    #[serde(default)]
    pub flows: Vec<MapFlow>,
    #[serde(default)]
    pub tasks: Vec<MapTask>,
}

/// AI-written explanation of a project's architecture (`map.explain`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MapExplanation {
    pub generated_at: f64,
    pub locale: String,
    pub summary: String,
    pub modules: Map<String, Value>,
    pub model: String,
    #[serde(flatten)]
    pub tour: MapTour,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift: Option<MapDrift>,
}

/// Full project map snapshot (`map.get`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMap {
    pub workspace_id: String,
    pub title: String,
    pub generated_at: u64,
    pub truncated: bool,
    pub files: Vec<MapFile>,
    pub imports: Vec<usize>,
    pub packages: Vec<MapPackage>,
    pub stack: Vec<StackItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<MapExplanation>,
}

/// One related file entry returned by `map.relations`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct MapRelationFile {
    pub path: String,
    pub lines: u64,
    pub kind: String,
}

/// Closest described area for a file in `map.relations`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct MapRelationArea {
    pub path: String,
    pub text: String,
}

/// One tour mention of a file in `map.relations`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct MapRelationMention {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

/// Response of `map.relations`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MapRelations {
    pub path: String,
    pub known: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default)]
    pub imports: Vec<MapRelationFile>,
    #[serde(default)]
    pub imported_by: Vec<MapRelationFile>,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<MapRelationArea>,
    #[serde(default)]
    pub mentions: Vec<MapRelationMention>,
}

/// One thing the agent did in a turn (`map.footprint`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FootprintStep {
    /// `read`, `create`, `edit`, `delete`, `search`, `command`, `web`, `agent` or `other`.
    pub action: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
}

/// One turn's file footprint (`map.footprint`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FootprintTurn {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub ts: f64,
    #[serde(default)]
    pub steps: Vec<FootprintStep>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn round_trips_map_relations_and_execution_poll() {
        let relations: MapRelations = serde_json::from_value(json!({
            "path": "src/main.rs",
            "known": true,
            "lines": 42,
            "language": "rust",
            "imports": [{ "path": "src/lib.rs", "lines": 100, "kind": "code" }],
            "importedBy": [],
            "packages": ["serde"],
            "area": { "path": "src", "text": "core" },
            "mentions": [{ "text": "entry", "line": 1 }]
        }))
        .unwrap();
        assert!(relations.known);
        assert_eq!(relations.imports.len(), 1);
    }
}
