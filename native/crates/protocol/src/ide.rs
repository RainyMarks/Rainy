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
    #[serde(default)]
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
    #[serde(default)]
    pub pinned: bool,
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
    #[serde(default)]
    pub entries: Vec<FileEntry>,
}

/// Bounded display bytes of a read-only file.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FilePreview {
    /// `utf8` or `hex`.
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub bytes_read: f64,
    #[serde(default)]
    pub truncated: bool,
}

/// A file as read: editable UTF-8 text, or why it is read-only.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IdeFileDocument {
    pub workspace_id: String,
    pub path: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub bytes: f64,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub bom: bool,
    /// `lf`, `crlf` or `mixed`.
    #[serde(default)]
    pub eol: String,
    /// `binary`, `too-large`, `unsupported-encoding`, `media`, or none.
    #[serde(default)]
    pub read_only_reason: Option<String>,
    #[serde(default)]
    pub preview: Option<FilePreview>,
}

/// An opened path observed again, to notice changes made outside the editor.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct IdeFileChange {
    pub path: String,
    #[serde(default)]
    pub version: Option<String>,
    /// An entry kind, or `missing`.
    pub kind: String,
}

/// One level of the in-app directory browser.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DirectoryListing {
    pub path: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub roots: Vec<String>,
    #[serde(default)]
    pub entries: Vec<DirectoryListingEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DirectoryListingEntry {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub hidden: bool,
}

/// The saved selection of the current project.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSelection {
    #[serde(default)]
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
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub truncated: bool,
}

/// Simple acknowledgement for `ide` operations without extra payload.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdeOk {
    pub ok: bool,
}

/// How an execution process exited.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProcessExit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(default)]
    pub stopped: bool,
}

/// One terminal as the Host reports it (`terminal.start`, `execution.status`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub cwd: String,
    /// `starting`, `running`, `stopping`, `exited` or `failed`.
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ProcessExit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TerminalInfo {
    pub fn running(&self) -> bool {
        self.phase == "starting" || self.phase == "running"
    }
}

/// Resolved command specification of a run (`run.resolve`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunSpec {
    #[serde(default)]
    pub terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// One run of a program (`run.start`, `execution.status`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub name: String,
    /// `starting`, `building`, `running`, `stopping`, `exited` or `failed`.
    pub phase: String,
    pub spec: RunSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ProcessExit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RunInfo {
    pub fn running(&self) -> bool {
        !matches!(self.phase.as_str(), "exited" | "failed")
    }
}

/// What a debug adapter supports.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DebugCapabilities {
    #[serde(default)]
    pub evaluate: bool,
    #[serde(default)]
    pub pause: bool,
    #[serde(default)]
    pub step_in: bool,
    #[serde(default)]
    pub step_out: bool,
    #[serde(default)]
    pub next: bool,
}

/// One breakpoint reported by a debug adapter (`debug.setBreakpoints`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DebugBreakpoint {
    pub path: String,
    pub requested_line: u64,
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One debug session (`debug.start`, `execution.status`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DebugInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_id: Option<String>,
    /// `starting`, `building`, `initializing`, `running`, `paused`, `stopping`, `terminated` or `failed`.
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<DebugCapabilities>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub breakpoints: Vec<DebugBreakpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ProcessExit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DebugInfo {
    pub fn running(&self) -> bool {
        !matches!(self.phase.as_str(), "terminated" | "failed")
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DebugThread {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DebugFrame {
    pub id: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub line: i64,
    pub column: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DebugScope {
    pub name: String,
    pub variables_reference: i64,
    #[serde(default)]
    pub expensive: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DebugVariable {
    pub name: String,
    pub value: String,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub variables_reference: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DebugEvaluation {
    pub result: String,
    pub variables_reference: i64,
}

/// Snapshot of running and recent operations of one project (`execution.status`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionStatus {
    #[serde(default)]
    pub runs: Vec<RunInfo>,
    #[serde(default)]
    pub terminals: Vec<TerminalInfo>,
    #[serde(default)]
    pub debug_sessions: Vec<DebugInfo>,
}

/// One ordered event returned by `execution.poll`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEvent {
    #[serde(default)]
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// `output`, `run`, `terminal` or `debug`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug: Option<Value>,
}

/// Response of `execution.poll`: ordered events after the requested cursor plus whether older output was dropped.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionPoll {
    #[serde(default)]
    pub events: Vec<ExecutionEvent>,
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub status: ExecutionStatus,
}

/// Formatted document text (`format`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct FormatResult {
    pub text: String,
}

/// Availability of one local IDE tool (`tools.status`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ToolStatusEntry {
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub ready: bool,
}

/// Response of `tools.status`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolsStatus {
    pub platform: String,
    #[serde(default)]
    pub ubuntu_version: Option<String>,
    #[serde(default)]
    pub offline_install_supported: bool,
    #[serde(default)]
    pub tools: Vec<ToolStatusEntry>,
}

/// One-shot tokenized URL for a converted Office PDF (`ide.registerOfficePdf` / `officePdf.register` / `POST /rainy/ide/office-pdf`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OfficePdfRegistration {
    pub token: String,
    pub url: String,
    pub expires_in_secs: u64,
}

/// Incremental tail slice of a workspace file (`ide` op `files.tail`), used for live-following large training logs.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileTailResult {
    pub workspace_id: String,
    pub path: String,
    pub text: String,
    pub offset: u64,
    pub next_offset: u64,
    pub total_bytes: u64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub reset: bool,
}

/// Response of `POST /rainy/ide/upload`: confirms the uploaded workspace file's path, size and version.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileUploadResult {
    pub workspace_id: String,
    pub path: String,
    pub bytes: u64,
    pub version: String,
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

    #[test]
    fn reads_execution_poll_with_truncated_and_office_pdf() {
        let poll: ExecutionPoll = serde_json::from_value(json!({
            "events": [{ "sequence": 2, "workspaceId": "w", "kind": "output", "operationId": "t1", "stream": "terminal", "text": "ok" }],
            "cursor": 2,
            "truncated": true,
            "status": { "runs": [], "terminals": [{ "id": "t1", "cwd": "/p", "phase": "running" }], "debugSessions": [] }
        }))
        .unwrap();
        assert!(poll.truncated);
        assert_eq!(poll.cursor, 2);
        assert!(poll.status.terminals[0].running());
        let reg: OfficePdfRegistration = serde_json::from_value(json!({
            "token": "abc",
            "url": "/rainy/ide/office-pdf?token=abc",
            "expiresInSecs": 120
        }))
        .unwrap();
        assert_eq!(reg.expires_in_secs, 120);
    }
}
