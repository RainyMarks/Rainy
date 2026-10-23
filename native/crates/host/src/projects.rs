//! Projects registered in this Host and the folders attached to each (3.x `projects.ts`), in `<home>/projects.json`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{HostError, HostResult};
use crate::paths::{contains, realpath, same_path};
use crate::store::{read_json, write_json};
use crate::text::now_ms;

/// The root id of each project's primary folder.
pub const PRIMARY: &str = "primary";
/// Folders per project, the primary one included.
pub const MAX_ROOTS: usize = 16;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attached {
    pub root_id: String,
    pub path: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    /// The canonical primary folder; chats started in the project use it as their working folder.
    pub path: String,
    pub title: String,
    pub attached: Vec<Attached>,
    pub created_at: f64,
    pub opened_at: f64,
}

/// One folder of a project, as the page sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub root_id: String,
    pub path: String,
    pub title: String,
    pub primary: bool,
}

impl Project {
    /// The primary folder, then the attached ones.
    pub fn roots(&self) -> Vec<Root> {
        let mut roots = vec![Root { root_id: PRIMARY.into(), path: self.path.clone(), title: self.title.clone(), primary: true }];
        roots.extend(self.attached.iter().map(|root| Root { root_id: root.root_id.clone(), path: root.path.clone(), title: root.title.clone(), primary: false }));
        roots
    }

    /// The `workspaces.*` shape.
    pub fn view(&self) -> serde_json::Value {
        serde_json::json!({ "workspaceId": self.id, "path": self.path, "title": self.title, "roots": self.roots() })
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u8,
    projects: Vec<Project>,
}

fn folder_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).filter(|name| !name.is_empty()).unwrap_or_else(|| path.display().to_string())
}

/// The canonical form of a folder the user chose.
pub fn canonical_directory(path: &str) -> HostResult<PathBuf> {
    if !crate::paths::fully_qualified(path) {
        return Err(HostError::new("invalid-path", "Choose an absolute directory path."));
    }
    let canonical = realpath(Path::new(path)).map_err(|_| HostError::new("not-directory", "Choose an existing directory."))?;
    if !canonical.is_dir() {
        return Err(HostError::new("not-directory", "Choose an existing directory."));
    }
    Ok(canonical)
}

pub struct Projects {
    file: PathBuf,
    projects: Mutex<Vec<Project>>,
    listeners: Mutex<Vec<Box<dyn Fn(&Project) + Send + Sync>>>,
}

impl Projects {
    pub fn load(file: PathBuf) -> std::io::Result<Self> {
        let projects = read_json::<File>(&file)?.map(|file| file.projects).unwrap_or_default();
        Ok(Self { file, projects: Mutex::new(projects), listeners: Mutex::default() })
    }

    /// Called with each project after it changed.
    pub fn on_change(&self, listener: impl Fn(&Project) + Send + Sync + 'static) {
        self.listeners.lock().unwrap().push(Box::new(listener));
    }

    /// Projects, most recently opened first.
    pub fn list(&self) -> Vec<Project> {
        let mut list = self.projects.lock().unwrap().clone();
        list.sort_by(|a, b| b.opened_at.total_cmp(&a.opened_at));
        list
    }

    pub fn get(&self, id: &str) -> Option<Project> {
        self.projects.lock().unwrap().iter().find(|project| project.id == id).cloned()
    }

    /// The project whose primary folder is `cwd`.
    pub fn by_path(&self, cwd: &Path) -> Option<Project> {
        self.projects.lock().unwrap().iter().find(|project| same_path(Path::new(&project.path), cwd)).cloned()
    }

    /// The innermost project with `path` in one of its folders.
    pub fn containing(&self, path: &Path) -> Option<Project> {
        let projects = self.projects.lock().unwrap();
        let mut found: Vec<&Project> = projects.iter().filter(|project| project.roots().iter().any(|root| contains(Path::new(&root.path), path))).collect();
        found.sort_by_key(|project| std::cmp::Reverse(project.path.len()));
        found.first().map(|project| (*project).clone())
    }

    fn save(&self, projects: &[Project], changed: &Project) -> HostResult<()> {
        write_json(&self.file, &File { version: 1, projects: projects.to_vec() })?;
        for listener in self.listeners.lock().unwrap().iter() {
            listener(changed);
        }
        Ok(())
    }

    /// Register a folder as a project, or mark the project that has it as primary folder as opened now.
    pub fn open(&self, path: &str, id: Option<String>) -> HostResult<Project> {
        let canonical = canonical_directory(path)?;
        let mut projects = self.projects.lock().unwrap();
        let now = now_ms() as f64;
        let index = match projects.iter().position(|project| same_path(Path::new(&project.path), &canonical)) {
            Some(index) => {
                projects[index].opened_at = now;
                index
            }
            None => {
                projects.push(Project {
                    id: id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    path: canonical.display().to_string(),
                    title: folder_name(&canonical),
                    attached: Vec::new(),
                    created_at: now,
                    opened_at: now,
                });
                projects.len() - 1
            }
        };
        let project = projects[index].clone();
        self.save(&projects, &project)?;
        Ok(project)
    }

    /// Attach another folder; one already attached keeps its root. `imported` keeps a root's identity across targets.
    pub fn attach(&self, id: &str, path: &str, imported: Option<(String, String)>) -> HostResult<Project> {
        let canonical = canonical_directory(path)?;
        let mut projects = self.projects.lock().unwrap();
        let index = projects.iter().position(|project| project.id == id).ok_or_else(missing)?;
        let roots = projects[index].roots();
        if imported.as_ref().is_some_and(|(root, _)| root == PRIMARY) {
            return Err(HostError::invalid("The primary directory cannot be imported as an attachment."));
        }
        if let Some(existing) = roots.iter().find(|root| same_path(Path::new(&root.path), &canonical)) {
            if imported.as_ref().is_some_and(|(root, _)| *root != existing.root_id) {
                return Err(HostError::invalid("The imported directory already has another root identity."));
            }
            return Ok(projects[index].clone());
        }
        if roots.iter().any(|root| contains(Path::new(&root.path), &canonical) || contains(&canonical, Path::new(&root.path))) {
            return Err(HostError::new("root-overlap", "The directory overlaps an existing project folder."));
        }
        if roots.len() >= MAX_ROOTS {
            return Err(HostError::new("too-large", "The project has reached its directory limit."));
        }
        let (root_id, title) = imported.unwrap_or_else(|| (uuid::Uuid::new_v4().to_string(), folder_name(&canonical)));
        projects[index].attached.push(Attached { root_id, path: canonical.display().to_string(), title });
        let project = projects[index].clone();
        self.save(&projects, &project)?;
        Ok(project)
    }

    /// Take an attached folder out of a project; its files and chats are untouched.
    pub fn detach(&self, id: &str, root_id: &str) -> HostResult<Project> {
        if root_id == PRIMARY {
            return Err(HostError::invalid("The primary project directory cannot be removed."));
        }
        let mut projects = self.projects.lock().unwrap();
        let index = projects.iter().position(|project| project.id == id).ok_or_else(missing)?;
        let before = projects[index].attached.len();
        projects[index].attached.retain(|root| root.root_id != root_id);
        let project = projects[index].clone();
        if project.attached.len() != before {
            self.save(&projects, &project)?;
        }
        Ok(project)
    }

    pub fn rename(&self, id: &str, title: &str) -> HostResult<Project> {
        let trimmed = title.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 200 {
            return Err(HostError::invalid("Project titles must be 1–200 characters."));
        }
        let mut projects = self.projects.lock().unwrap();
        let index = projects.iter().position(|project| project.id == id).ok_or_else(missing)?;
        projects[index].title = trimmed.to_owned();
        let project = projects[index].clone();
        self.save(&projects, &project)?;
        Ok(project)
    }

    /// Remove a project from the list; its folders and chats stay on disk.
    pub fn remove(&self, id: &str) -> HostResult<()> {
        let mut projects = self.projects.lock().unwrap();
        projects.retain(|project| project.id != id);
        write_json(&self.file, &File { version: 1, projects: projects.clone() })?;
        Ok(())
    }

    /// One folder of a project, checked to still resolve to itself right before a file or process operation.
    pub fn resolve_root(&self, id: &str, root_id: Option<&str>) -> HostResult<Root> {
        let project = self.get(id).ok_or_else(missing)?;
        let root_id = root_id.unwrap_or(PRIMARY);
        let root = project
            .roots()
            .into_iter()
            .find(|root| root.root_id == root_id)
            .ok_or_else(|| HostError::new("workspace-not-found", "This directory is no longer attached to the project."))?;
        match realpath(Path::new(&root.path)) {
            Ok(canonical) if same_path(&canonical, Path::new(&root.path)) && canonical.is_dir() => Ok(root),
            Ok(_) => Err(HostError::new("workspace-unavailable", "The project directory is unavailable or now resolves to another location.")),
            Err(_) => Err(HostError::new("workspace-unavailable", "The project directory is unavailable.")),
        }
    }
}

fn missing() -> HostError {
    HostError::new("workspace-not-found", "The selected project is no longer registered.")
}
