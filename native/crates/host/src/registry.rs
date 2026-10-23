//! Carrier-owned project identity shared by the Windows, WSL and SSH Hosts (3.x `shared/project-registry.ts`):
//! `<carrier state>/projects.json` binds each target's workspace to one public project.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::{HostError, HostResult};
use crate::store::{read_json, write_json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Binding {
    pub target_id: String,
    pub workspace_id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectRecord {
    pub project_id: String,
    pub title: String,
    pub active_target_id: String,
    pub bindings: Vec<Binding>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Catalog {
    version: u8,
    projects: Vec<ProjectRecord>,
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
        Ok(read_json::<Catalog>(&self.file)?.unwrap_or(Catalog { version: 1, projects: Vec::new() }))
    }

    pub async fn list(&self) -> HostResult<Vec<ProjectRecord>> {
        let _guard = self.lock.lock().await;
        Ok(self.load()?.projects)
    }

    pub async fn project_for(&self, workspace_id: &str) -> HostResult<Option<String>> {
        let _guard = self.lock.lock().await;
        Ok(self
            .load()?
            .projects
            .into_iter()
            .find(|project| project.bindings.iter().any(|binding| binding.target_id == self.target && binding.workspace_id == workspace_id))
            .map(|project| project.project_id))
    }

    /// The public identity of a workspace of this target, registering it when new; `project_id` joins an existing one.
    pub async fn get_or_register(&self, workspace_id: &str, path: &str, title: &str, project_id: Option<&str>) -> HostResult<String> {
        let _guard = self.lock.lock().await;
        let mut catalog = self.load()?;
        if let Some(old) = catalog.projects.iter().find(|project| project.bindings.iter().any(|binding| binding.target_id == self.target && binding.workspace_id == workspace_id)) {
            if project_id.is_some_and(|id| id != old.project_id) {
                return Err(HostError::other("The workspace is already bound to another project."));
            }
            return Ok(old.project_id.clone());
        }
        let index = match project_id {
            Some(id) => catalog.projects.iter().position(|project| project.project_id == id).ok_or_else(|| HostError::other("The selected public project does not exist."))?,
            None => {
                if !valid_project_id(workspace_id) {
                    return Err(HostError::invalid("Invalid project identity."));
                }
                if catalog.projects.iter().any(|project| project.project_id == workspace_id) {
                    return Err(HostError::other("A project with this identity already exists on another target; select it explicitly."));
                }
                catalog.projects.push(ProjectRecord { project_id: workspace_id.to_owned(), title: title.to_owned(), active_target_id: self.target.clone(), bindings: Vec::new() });
                catalog.projects.len() - 1
            }
        };
        let project = &mut catalog.projects[index];
        project.bindings.push(Binding { target_id: self.target.clone(), workspace_id: workspace_id.to_owned(), path: path.to_owned() });
        project.active_target_id = self.target.clone();
        let id = project.project_id.clone();
        write_json(&self.file, &catalog)?;
        Ok(id)
    }

    pub async fn select_target(&self, project_id: &str) -> HostResult<()> {
        let _guard = self.lock.lock().await;
        let mut catalog = self.load()?;
        let project = catalog.projects.iter_mut().find(|project| project.project_id == project_id).ok_or_else(|| HostError::other("The selected public project does not exist."))?;
        project.active_target_id = self.target.clone();
        write_json(&self.file, &catalog)?;
        Ok(())
    }
}
