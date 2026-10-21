//! The `ide` method's file, project and state operations (`src/shared/ide-files-protocol.ts`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One directory mounted into a project; the primary one is where chats run.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRoot {
    pub root_id: String,
    pub path: String,
    pub title: String,
    pub primary: bool,
}

/// A registered project.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IdeWorkspace {
    pub workspace_id: String,
    pub path: String,
    pub title: String,
    #[serde(default)]
    pub roots: Option<Vec<WorkspaceRoot>>,
}

impl IdeWorkspace {
    /// Its roots; projects saved before roots existed have only the primary directory.
    pub fn roots(&self) -> Vec<WorkspaceRoot> {
        match &self.roots {
            Some(roots) if !roots.is_empty() => roots.clone(),
            _ => vec![WorkspaceRoot { root_id: String::new(), path: self.path.clone(), title: self.title.clone(), primary: true }],
        }
    }
}

/// A directory entry; paths are slash-separated and relative to their root.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    /// `file`, `directory`, `symlink` or `other`.
    pub kind: String,
    #[serde(default)]
    pub target_kind: Option<String>,
    #[serde(default)]
    pub bytes: f64,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub outside_workspace: bool,
}

impl FileEntry {
    pub fn is_directory(&self) -> bool {
        self.kind == "directory" || (self.kind == "symlink" && self.target_kind.as_deref() == Some("directory"))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct IdeDirectory {
    pub path: String,
    pub entries: Vec<FileEntry>,
}

/// Bounded display bytes of a read-only file.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FilePreview {
    /// `utf8` or `hex`.
    pub kind: String,
    pub text: String,
    pub bytes_read: f64,
    pub truncated: bool,
}

/// A file as read: editable UTF-8 text, or why it is read-only.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IdeFileDocument {
    pub workspace_id: String,
    pub path: String,
    pub version: String,
    pub bytes: f64,
    pub content: Option<String>,
    pub bom: bool,
    /// `lf`, `crlf` or `mixed`.
    pub eol: String,
    /// `binary`, `too-large`, `unsupported-encoding`, `media`, or none.
    pub read_only_reason: Option<String>,
    #[serde(default)]
    pub preview: Option<FilePreview>,
}

/// An opened path observed again, to notice changes made outside the editor.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct IdeFileChange {
    pub path: String,
    pub version: Option<String>,
    /// An entry kind, or `missing`.
    pub kind: String,
}

/// One level of the in-app directory browser.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DirectoryListing {
    pub path: String,
    pub parent: Option<String>,
    pub roots: Vec<String>,
    pub entries: Vec<DirectoryListingEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DirectoryListingEntry {
    pub name: String,
    pub path: String,
    pub hidden: bool,
}

/// The saved selection of the current project.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSelection {
    pub workspace_id: Option<String>,
}

/// Per-project editor state with its durable revision. The data is kept as JSON so fields this interface does not
/// use (run configurations, dirty buffers of another client) are written back unchanged.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct IdeWorkspaceState {
    pub version: u32,
    pub revision: u64,
    pub data: Value,
}

/// Matching project-relative files.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct IdeFileSearch {
    pub paths: Vec<String>,
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_legacy_and_rooted_workspaces() {
        let legacy: IdeWorkspace = serde_json::from_value(json!({"workspaceId": "w", "path": "C:\\p", "title": "p"})).unwrap();
        assert_eq!(legacy.roots().len(), 1);
        assert!(legacy.roots()[0].primary);
        let rooted: IdeWorkspace = serde_json::from_value(json!({"workspaceId": "w", "path": "/p", "title": "p", "roots": [
            {"rootId": "r1", "path": "/p", "title": "p", "primary": true}, {"rootId": "r2", "path": "/q", "title": "q", "primary": false}]})).unwrap();
        assert_eq!(rooted.roots().len(), 2);
    }

    #[test]
    fn reads_documents_and_entries() {
        let document: IdeFileDocument = serde_json::from_value(json!({"workspaceId": "w", "path": "a.png", "version": "v", "bytes": 10,
            "content": null, "bom": false, "eol": "lf", "readOnlyReason": "media"})).unwrap();
        assert_eq!(document.read_only_reason.as_deref(), Some("media"));
        let link: FileEntry = serde_json::from_value(json!({"name": "l", "path": "l", "kind": "symlink", "targetKind": "directory",
            "bytes": 0, "version": "v", "outsideWorkspace": false})).unwrap();
        assert!(link.is_directory());
    }
}
