//! Assistant 各层共享的请求、事件和标识类型。
//!
//! 该 crate 应保持轻量，避免依赖具体 UI、存储或模型实现。

mod command;
mod config;
mod error;
mod event;
mod host;
mod id;
mod permission;
mod product;
mod snapshot;

pub use command::{
    ArchiveSessionRequest, ArchiveSessionResult, CancelChildTaskRequest, CancelChildTaskResult,
    CancelQueuedInputRequest, CancelQueuedInputResult, CancelRunRequest, CancelRunResult,
    ConfigurationMutationResult, ConnectionValidationFailure, ConnectionValidationFailureKind,
    ConnectionValidationOutcome, CreateModelRequest, CreateSessionRequest, CreateSessionResult,
    DecideApprovalRequest, DecideApprovalResult, DeleteModelRequest, DeleteSessionImpact,
    DeleteSessionRequest, DeleteSessionResult, ForkSessionRequest, ForkSessionResult,
    GetAttachmentRequest, GetAttachmentResult, GetChildTaskRequest, GetChildTaskResult,
    GetConfigStatusRequest, GetConfigStatusResult, GetModelRequest, GetModelResult,
    GetPermissionDocumentRequest, GetPermissionDocumentResult, GetRunRequest, GetRunResult,
    GetSessionRequest, GetSessionResult, GetWorkspaceRequest, GetWorkspaceResult,
    ListAttachmentsRequest, ListAttachmentsResult, ListChildTasksRequest, ListChildTasksResult,
    ListModelsRequest, ListModelsResult, ListPendingApprovalsRequest, ListPendingApprovalsResult,
    ListRunsRequest, ListRunsResult, ListSessionsRequest, ListSessionsResult,
    ListWorkspacesRequest, ListWorkspacesResult, ModelConfigurationInput, ModelConnectionTarget,
    ModelCredentialChange, PrepareDeleteSessionRequest, PrepareDeleteSessionResult,
    ReenterFromUserMessageRequest, ReenterFromUserMessageResult, RegisterWorkspaceRequest,
    RegisterWorkspaceResult, ReloadConfigRequest, ReloadConfigResult, ReloadPermissionsRequest,
    ReloadPermissionsResult, RemoveWorkspaceRequest, RemoveWorkspaceResult, RenameSessionRequest,
    RenameSessionResult, ReplacePermissionDocumentRequest, ReplacePermissionDocumentResult,
    RestoreSessionRequest, RestoreSessionResult, ResumeSessionRequest, ResumeSessionResult,
    RetryRunRequest, RetryRunResult, RuntimeCommand, RuntimeCommandResult, SecretValue,
    SetDefaultModelRequest, SetEmptySessionWorkspaceRequest, SetEmptySessionWorkspaceResult,
    SetMessageFeedbackRequest, SetMessageFeedbackResult, SetSessionApprovalModeRequest,
    SetSessionApprovalModeResult, SetSessionModelRequest, SetSessionModelResult,
    SetSessionPinnedRequest, SetSessionPinnedResult, SetSessionVariantRequest,
    SetSessionVariantResult, ShutdownRuntimeRequest, ShutdownRuntimeResult, SubmitInputRequest,
    SubmitInputResult, UpdateModelRequest, UploadAttachmentResult, ValidateModelConnectionRequest,
    ValidateModelConnectionResult,
};
pub use config::{
    ConfigurationIssue, ConfigurationIssueCode, ConfigurationState, ConfigurationStatus,
    ModelConfiguration, ModelConfigurationOrigin,
};
pub use error::{ModelFailureKind, RuntimeErrorCode, RuntimeErrorInfo};
pub use event::{ChildTaskEvent, RuntimeEvent, RuntimeEventEnvelope};
pub use host::{
    RuntimeHostCapabilities, RuntimeHostFeature, RuntimeHostHealth, RuntimeHostHealthStatus,
};
pub use id::{
    ApprovalId, AttachmentId, ChildTaskId, DeleteConfirmationToken, IdempotencyKey,
    IdentifierError, InputId, MessageId, ModelKey, ModelKeyError, PartId, ResourceRefId, RunId,
    SessionId, ToolCallId, WorkspaceId,
};
pub use permission::{
    PermissionCommandMatch, PermissionDocumentDraft, PermissionDocumentRevision,
    PermissionDocumentScope, PermissionDocumentSnapshot, PermissionFileMatcher,
    PermissionFileOperationDefinition, PermissionGeneralMatcher, PermissionPathMatch,
    PermissionProcessModeDefinition, PermissionRuleDefinition, PermissionRuleEffect,
    PermissionRuleMatcher, PermissionShellMatcher,
};
pub use product::{
    ApplicationCapabilities, ApplicationSnapshot, ApprovalQueueSnapshot, AssistantMessageSnapshot,
    AssistantSegment, ChildTaskTreeItemSnapshot, ChildTaskUsageSnapshot, ChildTaskViewSnapshot,
    ContextUsageSnapshot, ConversationFileReference, ConversationItem, ConversationOwner,
    ConversationPage, GetApplicationSnapshotRequest, GetApplicationSnapshotResult,
    GetChildTaskViewRequest, GetChildTaskViewResult, GetConversationPageAroundRunRequest,
    GetConversationPageAroundRunResult, GetSessionViewRequest, GetSessionViewResult,
    GetToolDetailRequest, GetToolDetailResult, InterruptRunRequest, InterruptRunResult,
    ListConversationPageRequest, ListConversationPageResult, MessageFeedback, ObservedSnapshot,
    PrioritizeQueuedInputRequest, PrioritizeQueuedInputResult, QueueExecutionState, QueueSnapshot,
    QueuedInputSnapshot, RejectApprovalAndStopRunRequest, RejectApprovalAndStopRunResult,
    ResumeQueuedInputRequest, ResumeQueuedInputResult, SessionUsageSnapshot, SessionViewSnapshot,
    ToolDetailSnapshot, ToolEventSnapshot, ToolFileReference, ToolFileResourceOrigin,
    ToolFileResourceState, ToolInputSnapshot, UsageTotals, UserMessageSnapshot,
};
pub use snapshot::{
    AgentVariant, ApprovalDecision, ApprovalMode, ApprovalSnapshot, ApprovalStatus,
    AttachmentState, AttachmentSummary, ChildTaskSnapshot, ChildTaskStatus, GuardrailKind,
    GuardrailMode, PermissionDiagnostic, PermissionDiagnosticCode, PermissionFileStatus,
    PermissionFileSummary, PermissionScope, RunSnapshot, RunStatus, RuntimeLifecycle,
    SessionLifecycle, SessionListFilter, SessionSummary, SessionTitleOrigin, TokenUsageSnapshot,
    ToolActivitySnapshot, ToolActivityStatus, ToolApprovalSubject, ToolOutputChannel,
    WorkspaceLifecycle, WorkspaceSummary,
};

/// 客户端与 Runtime Host 当前共同理解的应用协议版本。
///
/// 该常量通过 Host capabilities 投影，不定义 HTTP 或 SSE 的传输版本。
pub const PROTOCOL_VERSION: u32 = 1;
