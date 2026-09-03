//! 权限文档的安全产品投影。
//!
//! 这些 DTO 只表达 Runtime 已支持的规则语义，不暴露权限文件物理路径，也不允许客户端
//! 构造通配 matcher。文件定位、解析、CAS 与 Registry 替换仍由 Runtime 持有。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{AgentVariant, PermissionDiagnostic, PermissionFileStatus, SessionId, WorkspaceId};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum PermissionDocumentScope {
    Global,
    Workspace { workspace_id: WorkspaceId },
    Session { session_id: SessionId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum PermissionDocumentRevision {
    Missing,
    Content { value: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PermissionDocumentDraft {
    pub schema_version: u32,
    pub rules: Vec<PermissionRuleDefinition>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PermissionDocumentSnapshot {
    pub scope: PermissionDocumentScope,
    pub revision: PermissionDocumentRevision,
    pub status: PermissionFileStatus,
    pub schema_version: u32,
    pub rules: Vec<PermissionRuleDefinition>,
    pub diagnostics: Vec<PermissionDiagnostic>,
    pub editable: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PermissionRuleDefinition {
    pub id: String,
    pub effect: PermissionRuleEffect,
    pub variants: Vec<AgentVariant>,
    pub matcher: PermissionRuleMatcher,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PermissionRuleEffect {
    Allow,
    Deny,
    Ask,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum PermissionRuleMatcher {
    General(PermissionGeneralMatcher),
    File(PermissionFileMatcher),
    Shell(PermissionShellMatcher),
    Mcp(crate::PermissionMcpMatcher),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PermissionGeneralMatcher {
    pub tool_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PermissionFileMatcher {
    pub operation: PermissionFileOperationDefinition,
    pub path: String,
    pub path_match: PermissionPathMatch,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PermissionFileOperationDefinition {
    Read,
    List,
    Find,
    Search,
    Write,
    Edit,
    Delete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PermissionPathMatch {
    Exact,
    Recursive,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PermissionShellMatcher {
    pub command: String,
    pub command_match: PermissionCommandMatch,
    pub working_directory: String,
    pub process_mode: PermissionProcessModeDefinition,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PermissionCommandMatch {
    Exact,
    Prefix,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PermissionProcessModeDefinition {
    Managed,
    Detached,
}
