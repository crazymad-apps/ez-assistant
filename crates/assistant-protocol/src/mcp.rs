//! MCP 管理、选择、控制指令和授权展示的轻量应用协议。
//!
//! 这里只承载跨进程所需的脱敏事实，不包含配置文件路径、动态 Tool Catalog、传输实现
//! 或可回显的既有 secret。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    AgentVariant, IdempotencyKey, InputId, McpServerKey, SecretValue, SessionId, WorkspaceId,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    Stdio,
    StreamableHttp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum McpServerRuntimeState {
    Disabled,
    Unavailable,
    Connected,
    ConnectedWithoutTools,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum McpDiagnosticCode {
    InvalidConfig,
    ConnectFailed,
    ProtocolFailed,
    CatalogFailed,
    SchemaInvalid,
    LimitExceeded,
    /// 工具说明较长的非阻断警告；不能据此把连接或测试标为失败。
    ToolDescriptionLong,
    ServerNotFound,
    UnknownField,
}

/// 用户可见的固定诊断；message 不得包含底层错误正文或 secret。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct McpDiagnosticSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub server_key: Option<McpServerKey>,
    pub code: McpDiagnosticCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub field_path: Option<String>,
    pub message: String,
}

/// 设置列表中的单个 Server；敏感字段只公开键名或目标摘要。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct McpServerSnapshot {
    pub server_key: McpServerKey,
    pub display_name: String,
    pub description: String,
    pub transport: McpTransportKind,
    pub enabled: bool,
    pub runtime_state: McpServerRuntimeState,
    pub tool_count: u32,
    pub needs_refresh: bool,
    pub target_summary: String,
    pub startup_timeout_ms: Option<u64>,
    pub tool_timeout_ms: Option<u64>,
    #[serde(default)]
    pub environment_keys: Vec<String>,
    #[serde(default)]
    pub header_keys: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct McpConfigurationSnapshot {
    pub revision: String,
    /// 包括已从配置删除、但活动连接尚未 retire 的服务。
    pub needs_refresh: bool,
    pub servers: Vec<McpServerSnapshot>,
    pub diagnostics: Vec<McpDiagnosticSnapshot>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetMcpConfigurationRequest {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetMcpConfigurationResult {
    pub snapshot: McpConfigurationSnapshot,
}

/// 不回显字段的三态修改；Keep 不会把 UI 占位文本写回配置。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "mode", content = "value", rename_all = "snake_case")]
pub enum McpSecretChange {
    Keep,
    Replace(#[ts(type = "string")] SecretValue),
    Remove,
}

/// 不回显的连接字段使用显式三态，避免打开编辑页就覆盖原参数或带凭据的 URL。
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "mode", content = "value", rename_all = "snake_case")]
pub enum McpFieldChange<T> {
    #[default]
    Keep,
    Replace(T),
    Remove,
}

impl<T> std::fmt::Debug for McpFieldChange<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Keep => "Keep",
            Self::Remove => "Remove",
            Self::Replace(_) => "Replace(<redacted>)",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum McpServerTransportDraft {
    Stdio {
        #[serde(default)]
        command: McpFieldChange<String>,
        #[serde(default)]
        args: McpFieldChange<Vec<String>>,
        #[serde(default)]
        cwd: McpFieldChange<String>,
        #[serde(default)]
        environment: BTreeMap<String, McpSecretChange>,
    },
    StreamableHttp {
        #[serde(default)]
        url: McpFieldChange<String>,
        #[serde(default)]
        headers: BTreeMap<String, McpSecretChange>,
    },
}

/// 设置页当前 Server 草稿；工具超时覆盖全局缺省值，连接超时仍受进程级上限约束。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct McpServerDraft {
    pub server_key: McpServerKey,
    pub display_name: String,
    pub description: String,
    pub enabled: bool,
    pub transport: McpServerTransportDraft,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub startup_timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PreviewMcpImportRequest {
    pub document: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct McpImportPreviewEntry {
    pub server_key: McpServerKey,
    pub display_name: String,
    pub transport: McpTransportKind,
    pub conflicts_with_existing: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PreviewMcpImportResult {
    pub entries: Vec<McpImportPreviewEntry>,
    pub diagnostics: Vec<McpDiagnosticSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum McpConfigurationMutation {
    Upsert {
        server: McpServerDraft,
    },
    SetEnabled {
        server_key: McpServerKey,
        enabled: bool,
    },
    Remove {
        server_key: McpServerKey,
    },
    Import {
        document: String,
        #[serde(default)]
        replace_server_keys: Vec<McpServerKey>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct MutateMcpConfigurationRequest {
    pub expected_revision: String,
    pub mutation: McpConfigurationMutation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct MutateMcpConfigurationResult {
    pub snapshot: McpConfigurationSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct TestMcpServerRequest {
    pub test_id: IdempotencyKey,
    pub server: McpServerDraft,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CancelMcpServerTestRequest {
    pub test_id: IdempotencyKey,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CancelMcpServerTestResult {}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionTestStage {
    Connect,
    Protocol,
    Catalog,
    Close,
    Complete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionTestOutcome {
    Success,
    Cancelled,
    Failure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct TestMcpServerResult {
    pub outcome: McpConnectionTestOutcome,
    pub stage: McpConnectionTestStage,
    pub elapsed_ms: u64,
    pub tool_count: u32,
    /// 成功时可携带非阻断警告；调用方应根据 outcome 而非该字段是否存在判断成功。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diagnostic: Option<McpDiagnosticSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum McpServerOptionsContext {
    Session {
        session_id: SessionId,
    },
    NewSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        workspace_id: Option<WorkspaceId>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListMcpServerOptionsRequest {
    pub context: McpServerOptionsContext,
    pub variant: AgentVariant,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct McpServerOptionSnapshot {
    pub server_key: McpServerKey,
    pub display_name: String,
    pub description: String,
    pub visible_tool_count: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListMcpServerOptionsResult {
    pub servers: Vec<McpServerOptionSnapshot>,
}

/// 与 Input/Message 一起冻结的 MCP 标签，不包含动态 Tool Catalog。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct McpSelectionTagSnapshot {
    pub server_key: McpServerKey,
    pub display_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum SessionCommand {
    McpRefresh {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        server: Option<McpServerKey>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SubmitSessionCommandRequest {
    pub session_id: SessionId,
    pub command: SessionCommand,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub idempotency_key: Option<IdempotencyKey>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct AcceptedSessionCommand {
    pub input_id: InputId,
    pub command: SessionCommand,
    pub is_duplicate: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SubmitSessionCommandResult {
    pub accepted: AcceptedSessionCommand,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SessionCommandQueueState {
    Queued,
    Executing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct QueuedSessionCommandSnapshot {
    pub input_id: InputId,
    pub command: SessionCommand,
    pub state: SessionCommandQueueState,
    pub submitted_at_ms: i64,
    pub position: u32,
    pub is_prioritized: bool,
}

/// 同一 Session FIFO 中互斥的消息与控制指令投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum QueuedSessionItemSnapshot {
    Message(crate::QueuedInputSnapshot),
    Command(QueuedSessionCommandSnapshot),
}

impl QueuedSessionItemSnapshot {
    pub fn input_id(&self) -> &InputId {
        match self {
            Self::Message(message) => &message.input_id,
            Self::Command(command) => &command.input_id,
        }
    }

    pub fn as_message(&self) -> Option<&crate::QueuedInputSnapshot> {
        match self {
            Self::Message(message) => Some(message),
            Self::Command(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum McpRefreshOutcome {
    Success,
    Partial,
    Failure,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum McpServerRefreshOutcome {
    Refreshed,
    ConnectedWithoutTools,
    RetainedAfterFailure,
    Removed,
    Disabled,
    NotFound,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct McpServerRefreshResultSnapshot {
    pub server_key: McpServerKey,
    pub outcome: McpServerRefreshOutcome,
    pub tool_count: u32,
    /// 成功时可携带非阻断警告；完整警告列表由 MCP 管理快照提供。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diagnostic: Option<McpDiagnosticSnapshot>,
}

/// Command 可靠结算后供 Desktop 和 Conversation 控制结果共享的脱敏投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct McpRefreshControlResultSnapshot {
    pub outcome: McpRefreshOutcome,
    pub servers: Vec<McpServerRefreshResultSnapshot>,
}

/// MCP 权限、审批和工具详情共同展示的实际远端 Tool 身份。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct McpToolIdentity {
    pub server_key: McpServerKey,
    pub server_display_name: String,
    pub tool_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum PermissionMcpServerMatch {
    Any,
    Exact { server_key: McpServerKey },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum PermissionMcpToolMatch {
    Any,
    Exact { tool_name: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PermissionMcpMatcher {
    pub server: PermissionMcpServerMatch,
    pub tool: PermissionMcpToolMatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_command_round_trips_as_structured_command() {
        let request = SubmitSessionCommandRequest {
            session_id: SessionId::new("session-1").expect("session"),
            command: SessionCommand::McpRefresh {
                server: Some(McpServerKey::new("github").expect("server")),
            },
            idempotency_key: Some(IdempotencyKey::new("request-1").expect("key")),
        };
        let json = serde_json::to_string(&request).expect("serialize");
        assert_eq!(
            serde_json::from_str::<SubmitSessionCommandRequest>(&json).expect("deserialize"),
            request
        );
        assert!(json.contains("mcp_refresh"));
        assert!(!json.contains("/mcp refresh"));
    }

    #[test]
    fn management_request_round_trips_through_runtime_command() {
        let command =
            crate::RuntimeCommand::GetMcpConfiguration(GetMcpConfigurationRequest::default());
        let json = serde_json::to_string(&command).expect("serialize");
        assert_eq!(
            serde_json::from_str::<crate::RuntimeCommand>(&json).expect("deserialize"),
            command
        );
        assert!(json.contains("get_mcp_configuration"));
    }

    #[test]
    fn server_draft_secret_debug_output_is_redacted() {
        let draft = McpServerDraft {
            server_key: McpServerKey::new("github").expect("server"),
            display_name: "GitHub".to_owned(),
            description: "Issues".to_owned(),
            enabled: true,
            transport: McpServerTransportDraft::Stdio {
                command: McpFieldChange::Replace("server".to_owned()),
                args: McpFieldChange::Replace(Vec::new()),
                cwd: McpFieldChange::Remove,
                environment: BTreeMap::from([(
                    "TOKEN".to_owned(),
                    McpSecretChange::Replace(SecretValue::new("do-not-log".to_owned())),
                )]),
            },
            startup_timeout_ms: None,
            tool_timeout_ms: None,
        };
        let debug = format!("{draft:?}");
        assert!(!debug.contains("do-not-log"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn configuration_snapshot_round_trip_contains_no_secret_values() {
        let snapshot = McpConfigurationSnapshot {
            revision: "sha256:test".to_owned(),
            needs_refresh: true,
            servers: vec![McpServerSnapshot {
                server_key: McpServerKey::new("github").expect("server"),
                display_name: "GitHub".to_owned(),
                description: "Issues".to_owned(),
                transport: McpTransportKind::Stdio,
                enabled: true,
                runtime_state: McpServerRuntimeState::Unavailable,
                tool_count: 0,
                needs_refresh: true,
                target_summary: "local process".to_owned(),
                startup_timeout_ms: None,
                tool_timeout_ms: None,
                environment_keys: vec!["TOKEN".to_owned()],
                header_keys: Vec::new(),
            }],
            diagnostics: Vec::new(),
        };
        let json = serde_json::to_string(&snapshot).expect("serialize");
        assert_eq!(
            serde_json::from_str::<McpConfigurationSnapshot>(&json).expect("deserialize"),
            snapshot
        );
        assert!(!json.contains("command"));
        assert!(!json.contains("environment_values"));
    }

    #[test]
    fn approval_subject_preserves_the_remote_tool_identity() {
        let subject = crate::ToolApprovalSubject::Mcp {
            identity: McpToolIdentity {
                server_key: McpServerKey::new("github").expect("server"),
                server_display_name: "GitHub".to_owned(),
                tool_name: "create_issue".to_owned(),
            },
            arguments_json: r#"{"owner":"example"}"#.to_owned(),
            untrusted_annotations_json: None,
        };

        let json = serde_json::to_string(&subject).expect("serialize approval subject");
        assert_eq!(
            serde_json::from_str::<crate::ToolApprovalSubject>(&json)
                .expect("deserialize approval subject"),
            subject
        );
        assert!(json.contains("\"server_key\":\"github\""));
        assert!(json.contains("\"tool_name\":\"create_issue\""));
    }
}
