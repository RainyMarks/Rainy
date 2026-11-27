//! Carrier-owned project identity shared by the Windows, WSL and SSH Hosts (3.x `shared/project-registry.ts`):
//! `<carrier state>/projects.json` binds each target's workspace to one public project.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::store::{read_json, write_json};
use crate::text::backup_damaged;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    #[serde(default)]
    pub target_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub path: String,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRecord {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub active_target_id: String,
    #[serde(default)]
    pub bindings: Vec<Binding>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn default_catalog_version() -> u8 {
    1
}

#[derive(Serialize, Deserialize, Default)]
struct Catalog {
    #[serde(default = "default_catalog_version")]
    version: u8,
    #[serde(default)]
    projects: Vec<ProjectRecord>,
    #[serde(default, flatten)]
    extra: BTreeMap<String, Value>,
}

fn valid_project_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|first| first.is_ascii_alphanumeric() || first == '_' || first == '-')
        && id.len() <= 160
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// One writer of the catalog per Host; the carrier writes only while no Host runs.
pub struct Registry {
    file: PathBuf,
    target: String,
    lock: Mutex<()>,
}

impl Registry {
    pub fn new(root: PathBuf, target: String) -> Self {
        Self { file: root.join("projects.json"), target, lock: Mutex::new(()) }
    }

    fn load(&self) -> HostResult<Catalog> {
        match read_json::<Catalog>(&self.file) {
            Ok(Some(catalog)) => Ok(catalog),
            Ok(None) => Ok(Catalog { version: 1, projects: Vec::new(), extra: BTreeMap::new() }),
            Err(error) => {
                if let Some(backup) = backup_damaged(&self.file) {
                    log::warn!("registry projects.json is unreadable ({error}); backed up to {}", backup.display());
                } else {
                    log::warn!("registry projects.json is unreadable ({error}); starting with an empty catalog");
                }
                Ok(Catalog { version: 1, projects: Vec::new(), extra: BTreeMap::new() })
            }
        }
    }

    /// The public identity of a workspace of this target, registering it when new.
    pub async fn get_or_register(&self, workspace_id: &str, path: &str, title: &str) -> HostResult<String> {
        let _guard = self.lock.lock().await;
        let mut catalog = self.load()?;
        if let Some(old) = catalog.projects.iter().find(|project| project.bindings.iter().any(|binding| binding.target_id == self.target && binding.workspace_id == workspace_id)) {
            return Ok(old.project_id.clone());
        }
        if !valid_project_id(workspace_id) {
            return Err(HostError::invalid("Invalid project identity."));
        }
        if catalog.projects.iter().any(|project| project.project_id == workspace_id) {
            return Err(HostError::other(l(
                "另一个执行环境中已有同一标识的项目（例如同一台服务器的两个 SSH 别名），项目记忆和运行环境选择在这里不可用。",
                "A project with this identity already exists on another execution target (such as two SSH aliases of one server), so project memory and interpreter choices are unavailable here.",
            )));
        }
        let binding = Binding { target_id: self.target.clone(), workspace_id: workspace_id.to_owned(), path: path.to_owned(), extra: BTreeMap::new() };
        catalog.projects.push(ProjectRecord {
            project_id: workspace_id.to_owned(),
            title: title.to_owned(),
            active_target_id: self.target.clone(),
            bindings: vec![binding],
            extra: BTreeMap::new(),
        });
        write_json(&self.file, &catalog)?;
        Ok(workspace_id.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preserves_unknown_fields_and_recovers_damaged_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("projects.json");
        std::fs::write(
            &file,
            r#"{"version":1,"futureTop":42,"projects":[{"projectId":"p1","title":"P1","activeTargetId":"win","futureProj":true,"bindings":[{"targetId":"win","workspaceId":"w1","path":"/a","futureBind":"keep"}]}]}"#,
        )
        .unwrap();
        let reg = Registry::new(dir.path().to_path_buf(), "win".into());
        assert_eq!(reg.get_or_register("w1", "/a", "P1").await.unwrap(), "p1");
        assert_eq!(reg.get_or_register("w2", "/b", "P2").await.unwrap(), "w2");
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved["futureTop"], 42);
        assert_eq!(saved["projects"][0]["futureProj"], true);
        assert_eq!(saved["projects"][0]["bindings"][0]["futureBind"], "keep");

        // Corrupted file is backed up instead of failing startup.
        std::fs::write(&file, "{broken").unwrap();
        assert_eq!(reg.get_or_register("w3", "/c", "P3").await.unwrap(), "w3");
        let backups = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("projects.json.damaged-"))
            .count();
        assert_eq!(backups, 1);
    }
}
