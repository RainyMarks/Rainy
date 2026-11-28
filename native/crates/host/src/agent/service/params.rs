//! RPC parameter DTOs deserialized by [`super::Agent`] method handlers.

use rainy_protocol::chat::{ExternalAgentConfig, ExternalBackendRef, ImageContent};
use serde::Deserialize;

use crate::agent::hooks::HookRule;
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::settings::{McpServerConfig, ModelSelection};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionRef {
    pub(super) session_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TeamParams {
    pub(super) session_id: String,
    /// Only board posts newer than this.
    #[serde(default)]
    pub(super) after: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TeamMemberParams {
    pub(super) session_id: String,
    pub(super) member_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ItemParams {
    pub(super) session_id: String,
    pub(super) id: String,
}

impl ItemParams {
    pub(super) fn command_id(&self) -> HostResult<&str> {
        if self.id.trim().is_empty() {
            return Err(HostError::invalid(l("命令编号无效。", "The command id is invalid.")));
        }
        Ok(&self.id)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListParams {
    pub(super) workspace_id: Option<String>,
    pub(super) archived: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateParams {
    pub(super) workspace_id: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) permission: Option<String>,
    /// Work in a new Git worktree of the project instead of its checkout.
    #[serde(default)]
    pub(super) worktree: bool,
    #[serde(default)]
    pub(super) external_backend: Option<ExternalBackendRef>,
    #[serde(default)]
    pub(super) model: Option<ModelSelection>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExternalProbeParams {
    pub(super) kind: Option<String>,
    pub(super) binary: Option<String>,
    pub(super) args: Option<Vec<String>>,
    pub(super) cwd: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExternalConfigureParams {
    pub(super) config: ExternalAgentConfig,
    #[serde(default)]
    pub(super) remove: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SearchParams {
    pub(super) query: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BranchParams {
    pub(super) session_id: String,
    pub(super) name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GetParams {
    pub(super) session_id: String,
    pub(super) limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RenameParams {
    pub(super) session_id: String,
    pub(super) title: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ArchiveParams {
    pub(super) session_id: String,
    pub(super) archived: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PinParams {
    pub(super) session_id: String,
    pub(super) pinned: bool,
}

/// `sessions.bulk`: one action (`archive`, `unarchive`, `pin`, `unpin` or `delete`) on many chats.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BulkParams {
    pub(super) action: String,
    pub(super) session_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ForkParams {
    pub(super) session_id: String,
    pub(super) entry_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SendParams {
    pub(super) session_id: String,
    pub(super) text: String,
    pub(super) images: Option<Vec<ImageContent>>,
    pub(super) mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OrchestrateParams {
    pub(super) session_id: String,
    /// `null` turns orchestration off.
    pub(super) executor: Option<ModelSelection>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SetModelParams {
    pub(super) session_id: String,
    #[serde(flatten)]
    pub(super) selection: ModelSelection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ModeSessionParams {
    pub(super) session_id: String,
    pub(super) mode: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ApproveParams {
    pub(super) session_id: String,
    pub(super) tool_call_id: String,
    pub(super) decision: String,
    pub(super) reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PathParams {
    pub(super) session_id: String,
    pub(super) path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CallsParams {
    pub(super) session_id: String,
    pub(super) tool_call_ids: Vec<String>,
}

impl CallsParams {
    pub(super) fn checked(&self) -> HostResult<&[String]> {
        if self.tool_call_ids.is_empty() || self.tool_call_ids.len() > 1000 || self.tool_call_ids.iter().any(|id| id.is_empty() || id.len() > 512) {
            return Err(HostError::invalid("Invalid tool call ids."));
        }
        Ok(&self.tool_call_ids)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CompleteParams {
    pub(super) session_id: Option<String>,
    pub(super) workspace_id: Option<String>,
    pub(super) query: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkspaceRef {
    pub(super) workspace_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OptionalWorkspace {
    pub(super) workspace_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GitRequest {
    pub(super) workspace_id: String,
    #[serde(default)]
    pub(super) root_id: String,
    #[serde(default)]
    pub(super) paths: Vec<String>,
    #[serde(default)]
    pub(super) message: String,
    #[serde(default)]
    pub(super) stage_all: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RequestRef {
    pub(super) request_id: String,
}

#[derive(Deserialize)]
pub(super) struct ProxyParams {
    pub(super) mode: String,
    pub(super) url: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct ModeParams {
    pub(super) mode: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RuleParams {
    pub(super) workspace_id: String,
    pub(super) kind: String,
    pub(super) rule: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TrustParams {
    pub(super) workspace_id: Option<String>,
    pub(super) path: Option<String>,
    pub(super) trusted: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HooksParams {
    pub(super) hooks: Vec<HookRule>,
}

#[derive(Deserialize)]
pub(super) struct ExtensionsParams {
    pub(super) cwd: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SaveServerParams {
    pub(super) server: McpServerConfig,
    pub(super) previous_name: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct NameParams {
    pub(super) name: String,
}

#[derive(Deserialize)]
pub(super) struct ProviderParams {
    pub(super) provider: String,
}

#[derive(Deserialize)]
pub(super) struct TextParams {
    pub(super) text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MemorySwitches {
    pub(super) workspace_id: String,
    pub(super) enabled: Option<bool>,
    pub(super) generation_enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MemoryEdit {
    pub(super) workspace_id: String,
    pub(super) id: String,
    pub(super) text: String,
    pub(super) expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MemoryNote {
    pub(super) workspace_id: String,
    pub(super) id: String,
}
