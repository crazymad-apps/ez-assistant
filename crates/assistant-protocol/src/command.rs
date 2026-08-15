//! Runtime 客户端意图及其成功结果。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    AgentVariant, ApprovalDecision, ApprovalId, ApprovalMode, ApprovalSnapshot, AttachmentId,
    AttachmentSummary, ChildTaskId, ChildTaskSnapshot, ConfigurationStatus,
    DeleteConfirmationToken, GetApplicationSnapshotRequest, GetApplicationSnapshotResult,
    GetChildTaskViewRequest, GetChildTaskViewResult, GetConversationPageAroundRunRequest,
    GetConversationPageAroundRunResult, GetSessionViewRequest, GetSessionViewResult,
    GetToolDetailRequest, GetToolDetailResult, IdempotencyKey, InputId, InterruptRunRequest,
    InterruptRunResult, ListConversationPageRequest, ListConversationPageResult, MessageFeedback,
    MessageId, ModelConfiguration, ModelKey, PermissionDiagnostic, PermissionDocumentDraft,
    PermissionDocumentRevision, PermissionDocumentScope, PermissionDocumentSnapshot,
    PermissionFileSummary, PrioritizeQueuedInputRequest, PrioritizeQueuedInputResult,
    RejectApprovalAndStopRunRequest, RejectApprovalAndStopRunResult, ResumeQueuedInputRequest,
    ResumeQueuedInputResult, RunId, RunSnapshot, RuntimeLifecycle, SessionId, SessionListFilter,
    SessionSummary, WorkspaceId, WorkspaceSummary,
};

/// 查询当前配置总体状态。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetConfigStatusRequest {}

/// 当前配置总体状态查询结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetConfigStatusResult {
    /// 当前配置总体状态。
    pub status: ConfigurationStatus,
}

/// 按确定性顺序查询全部脱敏模型投影。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListModelsRequest {}

/// 全部模型的脱敏投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListModelsResult {
    /// 按配置 key 确定性排序的模型投影。
    pub models: Vec<ModelConfiguration>,
}

/// 查询一个合法 model key 的脱敏投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetModelRequest {
    /// 要查询的用户 model key。
    pub model_key: ModelKey,
}

/// 单个模型的脱敏投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetModelResult {
    /// 指定模型的脱敏投影。
    pub model: ModelConfiguration,
}

/// 显式重新读取唯一配置源。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReloadConfigRequest {}

/// reload 后立即可见的配置总体状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReloadConfigResult {
    /// 本次 reload 原子交换出的配置总体状态。
    pub status: ConfigurationStatus,
}

/// 只允许在命令请求体中出现的敏感字符串。
///
/// JSON 仍使用普通字符串传输，但 Rust 调试输出始终脱敏。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// 编辑模型时对既有凭据采取的显式动作。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "mode", content = "value", rename_all = "snake_case")]
pub enum ModelCredentialChange {
    Unchanged,
    Replace(#[ts(type = "string")] SecretValue),
    Clear,
}

/// 设置表单提交给 Runtime 的完整模型 candidate。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ModelConfigurationInput {
    pub model_key: ModelKey,
    pub display_name: String,
    pub protocol: String,
    pub provider: String,
    pub endpoint: String,
    pub model: String,
    pub context_window_tokens: u64,
    pub max_output_tokens: u32,
    pub credential: ModelCredentialChange,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ConfigurationMutationResult {
    pub status: ConfigurationStatus,
    pub models: Vec<ModelConfiguration>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CreateModelRequest {
    pub model: ModelConfigurationInput,
    pub expected_revision: Option<String>,
    pub set_default: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UpdateModelRequest {
    pub model: ModelConfigurationInput,
    pub expected_revision: String,
    pub set_default: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeleteModelRequest {
    pub model_key: ModelKey,
    pub expected_revision: String,
    pub replacement_default: Option<ModelKey>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetDefaultModelRequest {
    pub model_key: ModelKey,
    pub expected_revision: String,
}

/// 以 Session 为入口显式重载 Global、可选 Workspace 和 Session 权限文件。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReloadPermissionsRequest {
    pub session_id: SessionId,
}

/// 权限 cohort 的重载结果；只有 `applied` 为 true 时才替换内存快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReloadPermissionsResult {
    pub session_id: SessionId,
    pub applied: bool,
    pub files: Vec<PermissionFileSummary>,
    pub diagnostics: Vec<PermissionDiagnostic>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetPermissionDocumentRequest {
    pub scope: PermissionDocumentScope,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetPermissionDocumentResult {
    pub document: PermissionDocumentSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReplacePermissionDocumentRequest {
    pub scope: PermissionDocumentScope,
    pub expected_revision: PermissionDocumentRevision,
    pub document: PermissionDocumentDraft,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReplacePermissionDocumentResult {
    pub document: PermissionDocumentSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListPendingApprovalsRequest {
    /// 仅查询这个 Session 当前仍可决策的审批。
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListPendingApprovalsResult {
    /// 按创建时间稳定排序的内存审批快照。
    pub approvals: Vec<ApprovalSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DecideApprovalRequest {
    /// 审批所属 Session；防止跨 Session 猜测标识。
    pub session_id: SessionId,
    /// 要原子消费的待处理审批。
    pub approval_id: ApprovalId,
    /// 用户从 `available_decisions` 中选择的决定。
    pub decision: ApprovalDecision,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DecideApprovalResult {
    /// 已消费的审批标识。
    pub approval_id: ApprovalId,
    /// 已经应用到等待 Tool Call 的决定。
    pub decision: ApprovalDecision,
}

/// 显式验证一个已配置模型的基本连接与协议响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ValidateModelConnectionRequest {
    /// 已保存模型或尚未写入配置的表单 candidate。
    pub target: ModelConnectionTarget,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ModelConnectionTarget {
    Configured { model_key: ModelKey },
    Candidate(ModelConfigurationInput),
}

/// 连接验证失败的稳定分类。
///
/// 这些值是应用层契约，不直接序列化 Provider SDK、HTTP 客户端或
/// `ModelError` 的内部类型。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ConnectionValidationFailureKind {
    /// 模型服务或固定验证请求无法从当前配置构造。
    Configuration,
    /// DNS、TLS、拒绝连接或建流后中断。
    Connection,
    /// 连接或整体请求超时。
    Timeout,
    /// Provider 拒绝当前 credential。
    Authentication,
    /// Provider 无法使用当前模型或固定最小请求。
    ModelUnavailable,
    /// Provider 限流。
    RateLimited,
    /// Provider 服务暂时不可用。
    ServiceUnavailable,
    /// Provider 以其他可识别状态拒绝请求。
    ProviderRejected,
    /// 响应编码、事件顺序或流终态不符合契约。
    Protocol,
}

/// 一次连接验证的脱敏失败。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ConnectionValidationFailure {
    /// 客户端可稳定分支的失败分类。
    pub kind: ConnectionValidationFailureKind,
    /// 不包含 credential、prompt、Provider 原始正文或底层错误链的展示消息。
    pub message: String,
}

/// 连接验证的业务结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "status", content = "failure", rename_all = "snake_case")]
pub enum ConnectionValidationOutcome {
    /// 模型流产生了唯一且合法的 `TurnFinished` 终态。
    Succeeded,
    /// 配置、传输、Provider 或协议验证失败。
    Failed(ConnectionValidationFailure),
}

/// 指定模型的连接验证结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ValidateModelConnectionResult {
    /// 本次使用的用户 model key。
    pub model_key: ModelKey,
    /// 成功或结构化失败。
    pub outcome: ConnectionValidationOutcome,
}

/// 登记或恢复一个本机 Workspace。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RegisterWorkspaceRequest {
    /// 用户选择的本机绝对 UTF-8 目录路径。
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RegisterWorkspaceResult {
    pub workspace: WorkspaceSummary,
}

/// 查询一个 Workspace；已移除 Workspace 仍可查询。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetWorkspaceRequest {
    pub workspace_id: WorkspaceId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetWorkspaceResult {
    pub workspace: WorkspaceSummary,
}

/// 按确定性顺序列出当前活动 Workspace。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListWorkspacesRequest {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListWorkspacesResult {
    pub workspaces: Vec<WorkspaceSummary>,
}

/// 从新 Session 的正常可选列表中假删一个 Workspace。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RemoveWorkspaceRequest {
    pub workspace_id: WorkspaceId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RemoveWorkspaceResult {
    pub workspace: WorkspaceSummary,
}

/// 查询 Session 中的一个 Attachment。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetAttachmentRequest {
    pub session_id: SessionId,
    pub attachment_id: AttachmentId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetAttachmentResult {
    pub attachment: AttachmentSummary,
}

/// 按创建顺序列出 Session 的全部 Attachment。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListAttachmentsRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListAttachmentsResult {
    pub attachments: Vec<AttachmentSummary>,
}

/// HTTP 流式上传完成后的稳定业务结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UploadAttachmentResult {
    pub attachment: AttachmentSummary,
}

/// 创建一个空 Session。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CreateSessionRequest {
    /// 可选展示标题；`None` 表示由 Runtime 选择默认标题。
    pub title: Option<String>,
    /// 显式模型 key；`None` 表示使用创建时配置快照中的默认模型。
    pub model_key: Option<ModelKey>,
    /// 可选的 Workspace 冻结绑定；创建后不能直接换绑。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

/// 创建 Session 的成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CreateSessionResult {
    /// 新创建的 Session 摘要。
    pub session: SessionSummary,
}

/// 从一条已可靠提交的 Assistant Message 创建独立 Session。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ForkSessionRequest {
    pub session_id: SessionId,
    pub fork_point: MessageId,
    /// 客户端取得 fork point 时观察到的正文 generation。
    pub expected_generation: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ForkSessionResult {
    pub session: SessionSummary,
}

/// 永久删除将移除的 Runtime 私有事实摘要；不包含 Workspace 用户文件。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeleteSessionImpact {
    pub message_count: u64,
    pub run_count: u64,
    pub child_task_count: u64,
    pub attachment_count: u64,
}

/// 请求 Runtime 重新核对永久删除影响并签发短期确认 token。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PrepareDeleteSessionRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PrepareDeleteSessionResult {
    pub session: SessionSummary,
    pub impact: DeleteSessionImpact,
    pub confirmation_token: DeleteConfirmationToken,
    pub expires_at_ms: i64,
}

/// 使用预检签发的单次 token 永久删除 Session 私有事实。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeleteSessionRequest {
    pub session_id: SessionId,
    pub confirmation_token: DeleteConfirmationToken,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeleteSessionResult {
    pub session_id: SessionId,
}

/// 按生命周期列出 Session；缺省只返回活动 Session。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListSessionsRequest {
    #[serde(default)]
    pub filter: SessionListFilter,
}

/// 列出 Session 的成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListSessionsResult {
    /// 按 Runtime 确定性顺序返回的 Session 摘要。
    pub sessions: Vec<SessionSummary>,
}

/// 查询指定 Session。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetSessionRequest {
    /// 要查询的 Session。
    pub session_id: SessionId,
}

/// 查询 Session 的成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetSessionResult {
    /// 当前 Session 摘要。
    pub session: SessionSummary,
}

/// 可靠提交一条用户输入；同 Session 内可按 key 幂等重试。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SubmitInputRequest {
    /// 目标 Session。
    pub session_id: SessionId,
    /// 原样进入规范 UserMessage 的文本；Runtime 负责非空白校验。
    pub message: String,
    /// 本次输入实际使用的 Agent 变体；不能由 Session 展示状态代替。
    pub variant: AgentVariant,
    /// 按用户选择顺序引用的 Session Attachment。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<AttachmentId>,
    /// 可选的不透明请求身份；重复 key 直接返回首次结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<IdempotencyKey>,
}

/// 输入已持久化接受的结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SubmitInputResult {
    pub input_id: InputId,
    pub run: RunSnapshot,
}

/// 取消尚未进入 Conversation 的排队输入。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CancelQueuedInputRequest {
    pub session_id: SessionId,
    pub input_id: InputId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CancelQueuedInputResult {
    pub input_id: InputId,
}

/// 显式恢复重启后暂停的 Session 队列。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ResumeSessionRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ResumeSessionResult {
    pub session: SessionSummary,
}

/// 为可重试的失败或中断 Run 创建新 attempt。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RetryRunRequest {
    pub session_id: SessionId,
    pub run_id: RunId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RetryRunResult {
    pub run: RunSnapshot,
}

/// 查询指定 Session 中的 Run。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetRunRequest {
    /// Run 所属 Session。
    pub session_id: SessionId,
    /// 要查询的 Run。
    pub run_id: RunId,
}

/// 查询 Run 的成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetRunResult {
    /// 当前 Run 快照。
    pub run: RunSnapshot,
}

/// 查询指定 Session 的全部 Run。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListRunsRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListRunsResult {
    pub runs: Vec<RunSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListChildTasksRequest {
    pub session_id: SessionId,
    pub parent_run_id: RunId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListChildTasksResult {
    pub tasks: Vec<ChildTaskSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetChildTaskRequest {
    pub session_id: SessionId,
    pub child_task_id: ChildTaskId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetChildTaskResult {
    pub task: ChildTaskSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CancelChildTaskRequest {
    pub session_id: SessionId,
    pub child_task_id: ChildTaskId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CancelChildTaskResult {
    pub task: ChildTaskSnapshot,
}

/// 把完全空闲的活动 Session 转为只读归档状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ArchiveSessionRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ArchiveSessionResult {
    pub session: SessionSummary,
}

/// 恢复一个归档 Session；不会自动启动任何 Run。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RestoreSessionRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RestoreSessionResult {
    pub session: SessionSummary,
}

/// 修改 Session 的用户可见标题。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RenameSessionRequest {
    pub session_id: SessionId,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RenameSessionResult {
    pub session: SessionSummary,
}

/// 显式设置 Session 的固定状态；重复提交相同目标值保持幂等。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetSessionPinnedRequest {
    pub session_id: SessionId,
    pub is_pinned: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetSessionPinnedResult {
    pub session: SessionSummary,
}

/// 为完全空的 Session 重新冻结可选 Workspace。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetEmptySessionWorkspaceRequest {
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetEmptySessionWorkspaceResult {
    pub session: SessionSummary,
}

/// 保存或清除一条 Assistant Message 的本地反馈。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetMessageFeedbackRequest {
    pub session_id: SessionId,
    pub message_id: crate::MessageId,
    pub feedback: Option<MessageFeedback>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetMessageFeedbackResult {
    pub message_id: crate::MessageId,
    pub feedback: Option<MessageFeedback>,
}

/// 切换 Session 后续 Run 使用的模型 key。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetSessionModelRequest {
    pub session_id: SessionId,
    pub model_key: ModelKey,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetSessionModelResult {
    pub session: SessionSummary,
}

/// 只更新 Session 当前展示和下次提交默认使用的 Agent 变体。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetSessionVariantRequest {
    pub session_id: SessionId,
    pub variant: AgentVariant,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetSessionVariantResult {
    pub session: SessionSummary,
}

/// 更新 Session 后续 Run 捕获的审批模式。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetSessionApprovalModeRequest {
    pub session_id: SessionId,
    pub approval_mode: ApprovalMode,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetSessionApprovalModeResult {
    pub session: SessionSummary,
}

/// 从历史 User Message 位置提交一条全新输入并销毁原目标及尾段。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReenterFromUserMessageRequest {
    pub session_id: SessionId,
    pub message_id: crate::MessageId,
    pub message: String,
    /// 本次重新输入实际使用的 Agent 变体。
    pub variant: AgentVariant,
    /// 替换消息按用户选择顺序引用的 Session Attachment。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<AttachmentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<IdempotencyKey>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReenterFromUserMessageResult {
    pub input_id: InputId,
    pub run: RunSnapshot,
}

/// 请求取消指定 Session 中的活动 Run。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CancelRunRequest {
    /// Run 所属 Session。
    pub session_id: SessionId,
    /// 要取消的 Run。
    pub run_id: RunId,
}

/// 取消请求被 Runtime 接受后的结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CancelRunResult {
    /// 已反映取消请求的当前 Run 快照。
    pub run: RunSnapshot,
}

/// 请求受控关闭 Runtime。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ShutdownRuntimeRequest {}

/// 受控关闭请求被接受后的结果。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ShutdownRuntimeResult {
    /// 接受请求后的 Runtime 生命周期。
    pub lifecycle: RuntimeLifecycle,
}

/// Runtime 支持的最小客户端意图。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum RuntimeCommand {
    /// 查询 Desktop 首屏所需的权威组合投影。
    GetApplicationSnapshot(GetApplicationSnapshotRequest),
    /// 查询一个主 Session 页面所需的权威组合投影。
    GetSessionView(GetSessionViewRequest),
    /// 查询一个子 Agent 二级消息列表所需的权威组合投影。
    GetChildTaskView(GetChildTaskViewRequest),
    /// 分页查询主或子 Conversation 的可靠历史。
    ListConversationPage(ListConversationPageRequest),
    /// 查询包含目标 Run 的主 Conversation 页。
    GetConversationPageAroundRun(GetConversationPageAroundRunRequest),
    /// 按需查询一条工具调用的安全详情。
    GetToolDetail(GetToolDetailRequest),
    /// 把一条排队输入提升为下一条执行项。
    PrioritizeQueuedInput(PrioritizeQueuedInputRequest),
    /// 中断当前 Run 并暂停剩余队列。
    InterruptRun(InterruptRunRequest),
    /// 从暂停队列中指定恢复一条输入。
    ResumeQueuedInput(ResumeQueuedInputRequest),
    /// 拒绝队首审批并停止其所属 Run。
    RejectApprovalAndStopRun(RejectApprovalAndStopRunRequest),
    /// 查询配置总体状态。
    GetConfigStatus(GetConfigStatusRequest),
    /// 列出全部模型脱敏投影。
    ListModels(ListModelsRequest),
    /// 查询一个模型脱敏投影。
    GetModel(GetModelRequest),
    /// 显式重新加载配置。
    ReloadConfig(ReloadConfigRequest),
    /// 在唯一配置源中新建模型。
    CreateModel(CreateModelRequest),
    /// 更新唯一配置源中的模型。
    UpdateModel(UpdateModelRequest),
    /// 从唯一配置源删除模型。
    DeleteModel(DeleteModelRequest),
    /// 设置后续新会话使用的默认模型。
    SetDefaultModel(SetDefaultModelRequest),
    /// 显式重新加载目标 Session 的权限 cohort。
    ReloadPermissions(ReloadPermissionsRequest),
    /// 查询一份权限文档的安全产品投影。
    GetPermissionDocument(GetPermissionDocumentRequest),
    /// 以 revision CAS 替换 Session 或 Workspace 权限文档。
    ReplacePermissionDocument(ReplacePermissionDocumentRequest),
    ListPendingApprovals(ListPendingApprovalsRequest),
    DecideApproval(DecideApprovalRequest),
    /// 显式验证指定模型连接。
    ValidateModelConnection(ValidateModelConnectionRequest),
    /// 登记或恢复 Workspace。
    RegisterWorkspace(RegisterWorkspaceRequest),
    /// 查询 Workspace。
    GetWorkspace(GetWorkspaceRequest),
    /// 列出活动 Workspace。
    ListWorkspaces(ListWorkspacesRequest),
    /// 假删 Workspace。
    RemoveWorkspace(RemoveWorkspaceRequest),
    /// 查询 Attachment。
    GetAttachment(GetAttachmentRequest),
    /// 列出 Session Attachment。
    ListAttachments(ListAttachmentsRequest),
    /// 创建 Session。
    CreateSession(CreateSessionRequest),
    /// 从可靠 Assistant Message 创建独立 Session。
    ForkSession(ForkSessionRequest),
    /// 预检永久删除影响并签发短期 token。
    PrepareDeleteSession(PrepareDeleteSessionRequest),
    /// 使用单次 token 永久删除 Session。
    DeleteSession(DeleteSessionRequest),
    /// 列出 Session。
    ListSessions(ListSessionsRequest),
    /// 查询 Session。
    GetSession(GetSessionRequest),
    /// 提交持久化输入。
    SubmitInput(SubmitInputRequest),
    /// 取消排队输入。
    CancelQueuedInput(CancelQueuedInputRequest),
    /// 恢复重启后暂停的队列。
    ResumeSession(ResumeSessionRequest),
    /// 重试失败或中断 Run。
    RetryRun(RetryRunRequest),
    /// 查询 Run。
    GetRun(GetRunRequest),
    /// 列出 Session 的全部 Run。
    ListRuns(ListRunsRequest),
    ListChildTasks(ListChildTasksRequest),
    GetChildTask(GetChildTaskRequest),
    CancelChildTask(CancelChildTaskRequest),
    /// 归档 Session。
    ArchiveSession(ArchiveSessionRequest),
    /// 恢复归档 Session。
    RestoreSession(RestoreSessionRequest),
    /// 修改 Session 标题。
    RenameSession(RenameSessionRequest),
    /// 设置 Session 固定状态。
    SetSessionPinned(SetSessionPinnedRequest),
    /// 为空 Session 重新选择 Workspace。
    SetEmptySessionWorkspace(SetEmptySessionWorkspaceRequest),
    /// 保存 Assistant Message 反馈。
    SetMessageFeedback(SetMessageFeedbackRequest),
    /// 切换 Session 模型。
    SetSessionModel(SetSessionModelRequest),
    /// 切换 Session 当前 Agent 变体。
    SetSessionVariant(SetSessionVariantRequest),
    /// 切换 Session 当前审批模式。
    SetSessionApprovalMode(SetSessionApprovalModeRequest),
    /// 从历史 User Message 重新输入。
    ReenterFromUserMessage(ReenterFromUserMessageRequest),
    /// 取消 Run。
    CancelRun(CancelRunRequest),
    /// 关闭 Runtime。
    ShutdownRuntime(ShutdownRuntimeRequest),
}

/// Runtime 命令的成功结果；失败统一由 Host 发送 [`crate::RuntimeErrorInfo`]。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum RuntimeCommandResult {
    GetApplicationSnapshot(GetApplicationSnapshotResult),
    GetSessionView(Box<GetSessionViewResult>),
    GetChildTaskView(Box<GetChildTaskViewResult>),
    ListConversationPage(ListConversationPageResult),
    GetConversationPageAroundRun(GetConversationPageAroundRunResult),
    GetToolDetail(GetToolDetailResult),
    PrioritizeQueuedInput(PrioritizeQueuedInputResult),
    InterruptRun(InterruptRunResult),
    ResumeQueuedInput(ResumeQueuedInputResult),
    RejectApprovalAndStopRun(RejectApprovalAndStopRunResult),
    /// 配置总体状态已返回。
    GetConfigStatus(GetConfigStatusResult),
    /// 模型列表已返回。
    ListModels(ListModelsResult),
    /// 单模型投影已返回。
    GetModel(GetModelResult),
    /// 配置已重新加载并返回新状态。
    ReloadConfig(ReloadConfigResult),
    CreateModel(ConfigurationMutationResult),
    UpdateModel(ConfigurationMutationResult),
    DeleteModel(ConfigurationMutationResult),
    SetDefaultModel(ConfigurationMutationResult),
    /// 权限 cohort 重载已完成。
    ReloadPermissions(ReloadPermissionsResult),
    GetPermissionDocument(GetPermissionDocumentResult),
    ReplacePermissionDocument(ReplacePermissionDocumentResult),
    ListPendingApprovals(ListPendingApprovalsResult),
    DecideApproval(DecideApprovalResult),
    /// 模型连接验证已完成。
    ValidateModelConnection(ValidateModelConnectionResult),
    /// Workspace 已登记或恢复。
    RegisterWorkspace(RegisterWorkspaceResult),
    /// Workspace 查询已返回。
    GetWorkspace(GetWorkspaceResult),
    /// Workspace 列表已返回。
    ListWorkspaces(ListWorkspacesResult),
    /// Workspace 已假删。
    RemoveWorkspace(RemoveWorkspaceResult),
    /// Attachment 查询已返回。
    GetAttachment(GetAttachmentResult),
    /// Attachment 列表已返回。
    ListAttachments(ListAttachmentsResult),
    /// Session 已创建。
    CreateSession(CreateSessionResult),
    /// Fork Session 已创建。
    ForkSession(ForkSessionResult),
    /// 永久删除预检已完成。
    PrepareDeleteSession(PrepareDeleteSessionResult),
    /// Session 已永久删除。
    DeleteSession(DeleteSessionResult),
    /// Session 列表已返回。
    ListSessions(ListSessionsResult),
    /// Session 查询已返回。
    GetSession(GetSessionResult),
    /// 输入已接受。
    SubmitInput(SubmitInputResult),
    /// 排队输入已取消。
    CancelQueuedInput(CancelQueuedInputResult),
    /// Session 队列已恢复。
    ResumeSession(ResumeSessionResult),
    /// 新 Run attempt 已创建。
    RetryRun(RetryRunResult),
    /// Run 查询已返回。
    GetRun(GetRunResult),
    /// Run 列表已返回。
    ListRuns(ListRunsResult),
    ListChildTasks(ListChildTasksResult),
    GetChildTask(GetChildTaskResult),
    CancelChildTask(CancelChildTaskResult),
    /// Session 已归档。
    ArchiveSession(ArchiveSessionResult),
    /// Session 已恢复。
    RestoreSession(RestoreSessionResult),
    /// Session 标题已修改。
    RenameSession(RenameSessionResult),
    /// Session 固定状态已设置。
    SetSessionPinned(SetSessionPinnedResult),
    /// 空 Session 的 Workspace 已重新冻结。
    SetEmptySessionWorkspace(SetEmptySessionWorkspaceResult),
    /// Assistant Message 反馈已保存。
    SetMessageFeedback(SetMessageFeedbackResult),
    /// Session 模型已切换。
    SetSessionModel(SetSessionModelResult),
    /// Session 当前 Agent 变体已切换。
    SetSessionVariant(SetSessionVariantResult),
    /// Session 当前审批模式已切换。
    SetSessionApprovalMode(SetSessionApprovalModeResult),
    /// 历史重新输入已接受。
    ReenterFromUserMessage(ReenterFromUserMessageResult),
    /// 取消请求已接受。
    CancelRun(CancelRunResult),
    /// Runtime 关闭请求已接受。
    ShutdownRuntime(ShutdownRuntimeResult),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn session_summary() -> SessionSummary {
        SessionSummary {
            session_id: SessionId::new("session-1").expect("session id"),
            title: "Session 1".to_owned(),
            model_key: ModelKey::new("model-1").expect("model key"),
            lifecycle: crate::SessionLifecycle::Active,
            current_variant: AgentVariant::Build,
            approval_mode: ApprovalMode::Ask,
            workspace_id: None,
            active_run_id: None,
            message_count: 0,
            queued_input_count: 0,
            resume_required: false,
            created_at_ms: None,
            updated_at_ms: None,
            archived_at_ms: None,
            is_pinned: false,
            title_origin: Default::default(),
            pending_approval_count: 0,
            active_child_count: 0,
            active_run_status: None,
        }
    }

    fn run_snapshot() -> RunSnapshot {
        RunSnapshot {
            run_id: RunId::new("run-1").expect("run id"),
            session_id: SessionId::new("session-1").expect("session id"),
            input_id: InputId::new("input-1").expect("input id"),
            attempt: 1,
            created_at_ms: Some(1),
            finished_at_ms: None,
            status: crate::RunStatus::Accepted,
            variant: AgentVariant::Build,
            approval_mode: ApprovalMode::Ask,
            cancel_requested: false,
            reasoning: String::new(),
            text: String::new(),
            tools: Vec::new(),
            error: None,
        }
    }

    fn child_task_snapshot() -> ChildTaskSnapshot {
        ChildTaskSnapshot {
            child_task_id: ChildTaskId::new("child-1").expect("child id"),
            session_id: SessionId::new("session-1").expect("session id"),
            parent_run_id: RunId::new("run-1").expect("run id"),
            parent_tool_call_id: crate::ToolCallId::new("call-1").expect("call id"),
            title: "Inspect".to_owned(),
            status: crate::ChildTaskStatus::Completed,
            variant: AgentVariant::Build,
            cancel_requested: false,
            final_text: "done".to_owned(),
            error: None,
            created_at_ms: 1,
            started_at_ms: Some(2),
            finished_at_ms: Some(3),
        }
    }

    fn workspace_summary() -> WorkspaceSummary {
        WorkspaceSummary {
            workspace_id: WorkspaceId::new("workspace-1").expect("workspace id"),
            user_directory: "/workspace".to_owned(),
            agent_directory: "/runtime/workspaces/workspace-1/agent".to_owned(),
            lifecycle: crate::WorkspaceLifecycle::Active,
            created_at_ms: 1,
            updated_at_ms: 1,
            removed_at_ms: None,
        }
    }

    fn configuration_status() -> ConfigurationStatus {
        ConfigurationStatus {
            config_path: Some("/private/runtime/config.toml".to_owned()),
            revision: Some("revision-1".to_owned()),
            state: crate::ConfigurationState::Ready,
            schema_version: Some(1),
            default_model: Some(ModelKey::new("model-1").expect("model key")),
            issues: Vec::new(),
        }
    }

    fn model_configuration() -> ModelConfiguration {
        ModelConfiguration {
            model_key: Some(ModelKey::new("model-1").expect("model key")),
            display_name: "Model 1".to_owned(),
            protocol: Some("chat_completions".to_owned()),
            provider: Some("fixture".to_owned()),
            endpoint: Some("https://api.example.test/v1".to_owned()),
            model: Some("fixture-model".to_owned()),
            context_window_tokens: Some(8_192),
            max_output_tokens: Some(4_096),
            agent_max_output_tokens: None,
            effective_max_output_tokens: Some(4_096),
            api_key_configured: true,
            origin: crate::ModelConfigurationOrigin::ConfigurationFile,
            editable: true,
            deletable: true,
            is_default: true,
            is_valid: true,
            issues: Vec::new(),
        }
    }

    fn approval_snapshot() -> ApprovalSnapshot {
        ApprovalSnapshot {
            approval_id: ApprovalId::new("approval-1").expect("approval id"),
            session_id: SessionId::new("session-1").expect("session id"),
            run_id: RunId::new("run-1").expect("run id"),
            child_task_id: None,
            call_id: crate::ToolCallId::new("call-1").expect("call id"),
            variant: AgentVariant::Build,
            approval_mode: ApprovalMode::Ask,
            subject: crate::ToolApprovalSubject::General {
                tool_name: "echo_text".to_owned(),
            },
            available_decisions: vec![ApprovalDecision::AllowOnce, ApprovalDecision::Deny],
            exact_rule_preview: crate::ToolApprovalSubject::General {
                tool_name: "echo_text".to_owned(),
            },
            status: crate::ApprovalStatus::Pending,
            created_at_ms: 1,
        }
    }

    #[test]
    fn command_uses_explicit_type_and_payload_tags() {
        let command = RuntimeCommand::SubmitInput(SubmitInputRequest {
            session_id: SessionId::new("session-1").expect("session id"),
            message: "hello".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            idempotency_key: None,
        });
        let value = serde_json::to_value(&command).expect("serialize command");

        assert_eq!(
            value,
            json!({
                "type": "submit_input",
                "payload": {
                    "session_id": "session-1",
                    "message": "hello",
                    "variant": "build"
                }
            })
        );
        assert_eq!(
            serde_json::from_value::<RuntimeCommand>(value).expect("deserialize command"),
            command
        );
    }

    #[test]
    fn input_variant_is_required_and_attachment_ids_default_to_empty() {
        let missing_variant = serde_json::from_value::<SubmitInputRequest>(json!({
            "session_id": "session-1",
            "message": "hello"
        }));
        assert!(missing_variant.is_err());

        let minimal = serde_json::from_value::<SubmitInputRequest>(json!({
            "session_id": "session-1",
            "message": "hello",
            "variant": "plan"
        }))
        .expect("minimal input request");
        assert!(minimal.attachment_ids.is_empty());

        let request = SubmitInputRequest {
            session_id: SessionId::new("session-1").expect("session id"),
            message: "compare".to_owned(),
            variant: AgentVariant::Plan,
            attachment_ids: vec![
                AttachmentId::new("attachment-2").expect("attachment id"),
                AttachmentId::new("attachment-1").expect("attachment id"),
            ],
            idempotency_key: None,
        };
        assert_eq!(
            serde_json::to_value(request).expect("serialize input request")["attachment_ids"],
            json!(["attachment-2", "attachment-1"])
        );
    }

    #[test]
    fn command_result_uses_matching_explicit_tag() {
        let result = RuntimeCommandResult::ShutdownRuntime(ShutdownRuntimeResult {
            lifecycle: RuntimeLifecycle::ShuttingDown,
        });
        let value = serde_json::to_value(&result).expect("serialize result");

        assert_eq!(
            value,
            json!({
                "type": "shutdown_runtime",
                "payload": { "lifecycle": "shutting_down" }
            })
        );
        assert_eq!(
            serde_json::from_value::<RuntimeCommandResult>(value).expect("deserialize result"),
            result
        );
    }

    #[test]
    fn every_command_and_result_variant_round_trips() {
        let session_id = SessionId::new("session-1").expect("session id");
        let run_id = RunId::new("run-1").expect("run id");
        let commands = vec![
            (
                RuntimeCommand::GetConfigStatus(GetConfigStatusRequest::default()),
                "get_config_status",
            ),
            (
                RuntimeCommand::ListModels(ListModelsRequest::default()),
                "list_models",
            ),
            (
                RuntimeCommand::GetModel(GetModelRequest {
                    model_key: ModelKey::new("model-1").expect("model key"),
                }),
                "get_model",
            ),
            (
                RuntimeCommand::ReloadConfig(ReloadConfigRequest::default()),
                "reload_config",
            ),
            (
                RuntimeCommand::ReloadPermissions(ReloadPermissionsRequest {
                    session_id: session_id.clone(),
                }),
                "reload_permissions",
            ),
            (
                RuntimeCommand::ListPendingApprovals(ListPendingApprovalsRequest {
                    session_id: session_id.clone(),
                }),
                "list_pending_approvals",
            ),
            (
                RuntimeCommand::DecideApproval(DecideApprovalRequest {
                    session_id: session_id.clone(),
                    approval_id: ApprovalId::new("approval-1").expect("approval id"),
                    decision: ApprovalDecision::AllowOnce,
                }),
                "decide_approval",
            ),
            (
                RuntimeCommand::ValidateModelConnection(ValidateModelConnectionRequest {
                    target: ModelConnectionTarget::Configured {
                        model_key: ModelKey::new("model-1").expect("model key"),
                    },
                }),
                "validate_model_connection",
            ),
            (
                RuntimeCommand::RegisterWorkspace(RegisterWorkspaceRequest {
                    path: "/workspace".to_owned(),
                }),
                "register_workspace",
            ),
            (
                RuntimeCommand::GetWorkspace(GetWorkspaceRequest {
                    workspace_id: WorkspaceId::new("workspace-1").expect("workspace id"),
                }),
                "get_workspace",
            ),
            (
                RuntimeCommand::ListWorkspaces(ListWorkspacesRequest::default()),
                "list_workspaces",
            ),
            (
                RuntimeCommand::RemoveWorkspace(RemoveWorkspaceRequest {
                    workspace_id: WorkspaceId::new("workspace-1").expect("workspace id"),
                }),
                "remove_workspace",
            ),
            (
                RuntimeCommand::GetAttachment(GetAttachmentRequest {
                    session_id: session_id.clone(),
                    attachment_id: AttachmentId::new("attachment-1").expect("attachment id"),
                }),
                "get_attachment",
            ),
            (
                RuntimeCommand::ListAttachments(ListAttachmentsRequest {
                    session_id: session_id.clone(),
                }),
                "list_attachments",
            ),
            (
                RuntimeCommand::CreateSession(CreateSessionRequest::default()),
                "create_session",
            ),
            (
                RuntimeCommand::ForkSession(ForkSessionRequest {
                    session_id: session_id.clone(),
                    fork_point: MessageId::new("message-1").expect("message id"),
                    expected_generation: 3,
                }),
                "fork_session",
            ),
            (
                RuntimeCommand::PrepareDeleteSession(PrepareDeleteSessionRequest {
                    session_id: session_id.clone(),
                }),
                "prepare_delete_session",
            ),
            (
                RuntimeCommand::DeleteSession(DeleteSessionRequest {
                    session_id: session_id.clone(),
                    confirmation_token: DeleteConfirmationToken::new("delete-confirm-1")
                        .expect("delete confirmation"),
                }),
                "delete_session",
            ),
            (
                RuntimeCommand::ListSessions(ListSessionsRequest::default()),
                "list_sessions",
            ),
            (
                RuntimeCommand::GetSession(GetSessionRequest {
                    session_id: session_id.clone(),
                }),
                "get_session",
            ),
            (
                RuntimeCommand::SubmitInput(SubmitInputRequest {
                    session_id: session_id.clone(),
                    message: "hello".to_owned(),
                    variant: AgentVariant::Build,
                    attachment_ids: Vec::new(),
                    idempotency_key: None,
                }),
                "submit_input",
            ),
            (
                RuntimeCommand::CancelQueuedInput(CancelQueuedInputRequest {
                    session_id: session_id.clone(),
                    input_id: InputId::new("input-1").expect("input id"),
                }),
                "cancel_queued_input",
            ),
            (
                RuntimeCommand::ResumeSession(ResumeSessionRequest {
                    session_id: session_id.clone(),
                }),
                "resume_session",
            ),
            (
                RuntimeCommand::RetryRun(RetryRunRequest {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                }),
                "retry_run",
            ),
            (
                RuntimeCommand::GetRun(GetRunRequest {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                }),
                "get_run",
            ),
            (
                RuntimeCommand::ListRuns(ListRunsRequest {
                    session_id: session_id.clone(),
                }),
                "list_runs",
            ),
            (
                RuntimeCommand::ListChildTasks(ListChildTasksRequest {
                    session_id: session_id.clone(),
                    parent_run_id: run_id.clone(),
                }),
                "list_child_tasks",
            ),
            (
                RuntimeCommand::GetChildTask(GetChildTaskRequest {
                    session_id: session_id.clone(),
                    child_task_id: ChildTaskId::new("child-1").expect("child id"),
                }),
                "get_child_task",
            ),
            (
                RuntimeCommand::CancelChildTask(CancelChildTaskRequest {
                    session_id: session_id.clone(),
                    child_task_id: ChildTaskId::new("child-1").expect("child id"),
                }),
                "cancel_child_task",
            ),
            (
                RuntimeCommand::ArchiveSession(ArchiveSessionRequest {
                    session_id: session_id.clone(),
                }),
                "archive_session",
            ),
            (
                RuntimeCommand::RestoreSession(RestoreSessionRequest {
                    session_id: session_id.clone(),
                }),
                "restore_session",
            ),
            (
                RuntimeCommand::SetSessionModel(SetSessionModelRequest {
                    session_id: session_id.clone(),
                    model_key: ModelKey::new("model-2").expect("model key"),
                }),
                "set_session_model",
            ),
            (
                RuntimeCommand::SetSessionVariant(SetSessionVariantRequest {
                    session_id: session_id.clone(),
                    variant: AgentVariant::Plan,
                }),
                "set_session_variant",
            ),
            (
                RuntimeCommand::SetSessionApprovalMode(SetSessionApprovalModeRequest {
                    session_id: session_id.clone(),
                    approval_mode: ApprovalMode::Auto,
                }),
                "set_session_approval_mode",
            ),
            (
                RuntimeCommand::ReenterFromUserMessage(ReenterFromUserMessageRequest {
                    session_id: session_id.clone(),
                    message_id: crate::MessageId::new("message-1").expect("message id"),
                    message: "replacement".to_owned(),
                    variant: AgentVariant::Plan,
                    attachment_ids: Vec::new(),
                    idempotency_key: Some(
                        IdempotencyKey::new("replace-1").expect("idempotency key"),
                    ),
                }),
                "reenter_from_user_message",
            ),
            (
                RuntimeCommand::CancelRun(CancelRunRequest {
                    session_id: session_id.clone(),
                    run_id,
                }),
                "cancel_run",
            ),
            (
                RuntimeCommand::ShutdownRuntime(ShutdownRuntimeRequest::default()),
                "shutdown_runtime",
            ),
        ];
        for (command, tag) in commands {
            let value = serde_json::to_value(&command).expect("serialize command");
            assert_eq!(value["type"], tag);
            assert_eq!(
                serde_json::from_value::<RuntimeCommand>(value).expect("deserialize command"),
                command
            );
        }

        let results = vec![
            (
                RuntimeCommandResult::GetConfigStatus(GetConfigStatusResult {
                    status: configuration_status(),
                }),
                "get_config_status",
            ),
            (
                RuntimeCommandResult::ListModels(ListModelsResult {
                    models: vec![model_configuration()],
                }),
                "list_models",
            ),
            (
                RuntimeCommandResult::GetModel(GetModelResult {
                    model: model_configuration(),
                }),
                "get_model",
            ),
            (
                RuntimeCommandResult::ReloadConfig(ReloadConfigResult {
                    status: configuration_status(),
                }),
                "reload_config",
            ),
            (
                RuntimeCommandResult::ReloadPermissions(ReloadPermissionsResult {
                    session_id: session_id.clone(),
                    applied: true,
                    files: vec![crate::PermissionFileSummary {
                        scope: crate::PermissionScope::Global,
                        status: crate::PermissionFileStatus::Empty,
                    }],
                    diagnostics: Vec::new(),
                }),
                "reload_permissions",
            ),
            (
                RuntimeCommandResult::ListPendingApprovals(ListPendingApprovalsResult {
                    approvals: vec![approval_snapshot()],
                }),
                "list_pending_approvals",
            ),
            (
                RuntimeCommandResult::DecideApproval(DecideApprovalResult {
                    approval_id: ApprovalId::new("approval-1").expect("approval id"),
                    decision: ApprovalDecision::AllowSession,
                }),
                "decide_approval",
            ),
            (
                RuntimeCommandResult::ValidateModelConnection(ValidateModelConnectionResult {
                    model_key: ModelKey::new("model-1").expect("model key"),
                    outcome: ConnectionValidationOutcome::Failed(ConnectionValidationFailure {
                        kind: ConnectionValidationFailureKind::Authentication,
                        message: "model authentication failed".to_owned(),
                    }),
                }),
                "validate_model_connection",
            ),
            (
                RuntimeCommandResult::RegisterWorkspace(RegisterWorkspaceResult {
                    workspace: workspace_summary(),
                }),
                "register_workspace",
            ),
            (
                RuntimeCommandResult::GetWorkspace(GetWorkspaceResult {
                    workspace: workspace_summary(),
                }),
                "get_workspace",
            ),
            (
                RuntimeCommandResult::ListWorkspaces(ListWorkspacesResult {
                    workspaces: vec![workspace_summary()],
                }),
                "list_workspaces",
            ),
            (
                RuntimeCommandResult::RemoveWorkspace(RemoveWorkspaceResult {
                    workspace: workspace_summary(),
                }),
                "remove_workspace",
            ),
            (
                RuntimeCommandResult::GetAttachment(GetAttachmentResult {
                    attachment: AttachmentSummary {
                        attachment_id: AttachmentId::new("attachment-1").expect("attachment id"),
                        session_id: session_id.clone(),
                        original_name: "reference.txt".to_owned(),
                        size_bytes: 9,
                        agent_readable_path: "/session/attachments/reference.txt".to_owned(),
                        state: crate::AttachmentState::Ready,
                        created_at_ms: 1,
                    },
                }),
                "get_attachment",
            ),
            (
                RuntimeCommandResult::ListAttachments(ListAttachmentsResult {
                    attachments: Vec::new(),
                }),
                "list_attachments",
            ),
            (
                RuntimeCommandResult::CreateSession(CreateSessionResult {
                    session: session_summary(),
                }),
                "create_session",
            ),
            (
                RuntimeCommandResult::ForkSession(ForkSessionResult {
                    session: session_summary(),
                }),
                "fork_session",
            ),
            (
                RuntimeCommandResult::PrepareDeleteSession(PrepareDeleteSessionResult {
                    session: session_summary(),
                    impact: DeleteSessionImpact {
                        message_count: 4,
                        run_count: 2,
                        child_task_count: 1,
                        attachment_count: 3,
                    },
                    confirmation_token: DeleteConfirmationToken::new("delete-confirm-1")
                        .expect("delete confirmation"),
                    expires_at_ms: 10_000,
                }),
                "prepare_delete_session",
            ),
            (
                RuntimeCommandResult::DeleteSession(DeleteSessionResult {
                    session_id: session_id.clone(),
                }),
                "delete_session",
            ),
            (
                RuntimeCommandResult::ListSessions(ListSessionsResult {
                    sessions: vec![session_summary()],
                }),
                "list_sessions",
            ),
            (
                RuntimeCommandResult::GetSession(GetSessionResult {
                    session: session_summary(),
                }),
                "get_session",
            ),
            (
                RuntimeCommandResult::SubmitInput(SubmitInputResult {
                    input_id: InputId::new("input-1").expect("input id"),
                    run: run_snapshot(),
                }),
                "submit_input",
            ),
            (
                RuntimeCommandResult::CancelQueuedInput(CancelQueuedInputResult {
                    input_id: InputId::new("input-1").expect("input id"),
                }),
                "cancel_queued_input",
            ),
            (
                RuntimeCommandResult::ResumeSession(ResumeSessionResult {
                    session: session_summary(),
                }),
                "resume_session",
            ),
            (
                RuntimeCommandResult::RetryRun(RetryRunResult {
                    run: run_snapshot(),
                }),
                "retry_run",
            ),
            (
                RuntimeCommandResult::GetRun(GetRunResult {
                    run: run_snapshot(),
                }),
                "get_run",
            ),
            (
                RuntimeCommandResult::ListRuns(ListRunsResult {
                    runs: vec![run_snapshot()],
                }),
                "list_runs",
            ),
            (
                RuntimeCommandResult::ListChildTasks(ListChildTasksResult {
                    tasks: vec![child_task_snapshot()],
                }),
                "list_child_tasks",
            ),
            (
                RuntimeCommandResult::GetChildTask(GetChildTaskResult {
                    task: child_task_snapshot(),
                }),
                "get_child_task",
            ),
            (
                RuntimeCommandResult::CancelChildTask(CancelChildTaskResult {
                    task: child_task_snapshot(),
                }),
                "cancel_child_task",
            ),
            (
                RuntimeCommandResult::ArchiveSession(ArchiveSessionResult {
                    session: session_summary(),
                }),
                "archive_session",
            ),
            (
                RuntimeCommandResult::RestoreSession(RestoreSessionResult {
                    session: session_summary(),
                }),
                "restore_session",
            ),
            (
                RuntimeCommandResult::SetSessionModel(SetSessionModelResult {
                    session: session_summary(),
                }),
                "set_session_model",
            ),
            (
                RuntimeCommandResult::SetSessionVariant(SetSessionVariantResult {
                    session: session_summary(),
                }),
                "set_session_variant",
            ),
            (
                RuntimeCommandResult::SetSessionApprovalMode(SetSessionApprovalModeResult {
                    session: session_summary(),
                }),
                "set_session_approval_mode",
            ),
            (
                RuntimeCommandResult::ReenterFromUserMessage(ReenterFromUserMessageResult {
                    input_id: InputId::new("input-1").expect("input id"),
                    run: run_snapshot(),
                }),
                "reenter_from_user_message",
            ),
            (
                RuntimeCommandResult::CancelRun(CancelRunResult {
                    run: run_snapshot(),
                }),
                "cancel_run",
            ),
            (
                RuntimeCommandResult::ShutdownRuntime(ShutdownRuntimeResult {
                    lifecycle: RuntimeLifecycle::ShuttingDown,
                }),
                "shutdown_runtime",
            ),
        ];
        for (result, tag) in results {
            let value = serde_json::to_value(&result).expect("serialize result");
            assert_eq!(value["type"], tag);
            assert_eq!(
                serde_json::from_value::<RuntimeCommandResult>(value).expect("deserialize result"),
                result
            );
        }
    }

    #[test]
    fn every_connection_failure_kind_has_a_stable_wire_value() {
        let cases = [
            (
                ConnectionValidationFailureKind::Configuration,
                "configuration",
            ),
            (ConnectionValidationFailureKind::Connection, "connection"),
            (ConnectionValidationFailureKind::Timeout, "timeout"),
            (
                ConnectionValidationFailureKind::Authentication,
                "authentication",
            ),
            (
                ConnectionValidationFailureKind::ModelUnavailable,
                "model_unavailable",
            ),
            (ConnectionValidationFailureKind::RateLimited, "rate_limited"),
            (
                ConnectionValidationFailureKind::ServiceUnavailable,
                "service_unavailable",
            ),
            (
                ConnectionValidationFailureKind::ProviderRejected,
                "provider_rejected",
            ),
            (ConnectionValidationFailureKind::Protocol, "protocol"),
        ];

        for (kind, wire) in cases {
            assert_eq!(
                serde_json::to_string(&kind).expect("serialize validation failure kind"),
                format!("\"{wire}\"")
            );
        }
    }
}
