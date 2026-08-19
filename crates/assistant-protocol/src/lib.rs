//! Assistant 各层共享的请求、事件和标识类型。
//!
//! 该 crate 应保持轻量，避免依赖具体 UI、存储或模型实现。

mod command;
mod config;
mod error;
mod event;
mod host;
mod id;
mod memory;
mod permission;
mod product;
mod snapshot;

pub use command::{
    ArchiveSessionRequest, ArchiveSessionResult, CancelChildTaskRequest, CancelChildTaskResult,
    CancelQueuedInputRequest, CancelQueuedInputResult, CancelRunRequest, CancelRunResult,
    ConfigurationMutationResult, ConnectionValidationFailure, ConnectionValidationFailureKind,
    ConnectionValidationOutcome, CreateModelRequest, CreatePinnedMemoryRequest,
    CreateSessionRequest, CreateSessionResult, DecideApprovalRequest, DecideApprovalResult,
    DeleteModelRequest, DeletePinnedMemoryRequest, DeleteSessionImpact, DeleteSessionRequest,
    DeleteSessionResult, ForkSessionRequest, ForkSessionResult, GetAttachmentRequest,
    GetAttachmentResult, GetChildTaskRequest, GetChildTaskResult, GetConfigStatusRequest,
    GetConfigStatusResult, GetMemoryCapabilitiesRequest, GetMemoryCapabilitiesResult,
    GetModelRequest, GetModelResult, GetPermissionDocumentRequest, GetPermissionDocumentResult,
    GetPersonaRequest, GetPersonaResult, GetRunRequest, GetRunResult, GetSessionRequest,
    GetSessionResult, GetSystemContextRequest, GetSystemContextResult, GetWorkspaceRequest,
    GetWorkspaceResult, ListAttachmentsRequest, ListAttachmentsResult, ListChildTasksRequest,
    ListChildTasksResult, ListModelsRequest, ListModelsResult, ListPendingApprovalsRequest,
    ListPendingApprovalsResult, ListPinnedMemoriesRequest, ListPinnedMemoriesResult,
    ListRunsRequest, ListRunsResult, ListSessionsRequest, ListSessionsResult,
    ListWorkspacesRequest, ListWorkspacesResult, ModelCatalogEntrySnapshot, ModelCatalogSnapshot,
    ModelConfigurationInput, ModelConnectionTarget, ModelCredentialChange,
    PinnedMemoryMutationResult, PrepareDeleteSessionRequest, PrepareDeleteSessionResult,
    ReenterFromUserMessageRequest, ReenterFromUserMessageResult, RegisterWorkspaceRequest,
    RegisterWorkspaceResult, ReloadConfigRequest, ReloadConfigResult, ReloadPermissionsRequest,
    ReloadPermissionsResult, RemoveWorkspaceRequest, RemoveWorkspaceResult, RenameSessionRequest,
    RenameSessionResult, ReplacePermissionDocumentRequest, ReplacePermissionDocumentResult,
    RestoreSessionRequest, RestoreSessionResult, ResumeSessionRequest, ResumeSessionResult,
    RetryRunRequest, RetryRunResult, RuntimeCommand, RuntimeCommandResult, SecretValue,
    SetAuxiliaryVisionModelRequest, SetDefaultModelRequest, SetMessageFeedbackRequest,
    SetMessageFeedbackResult, SetPersonaRequest, SetPersonaResult, SetSessionApprovalModeRequest,
    SetSessionApprovalModeResult, SetSessionModelRequest, SetSessionModelResult,
    SetSessionPinnedRequest, SetSessionPinnedResult, SetSessionReasoningEffortRequest,
    SetSessionReasoningEffortResult, SetSessionVariantRequest, SetSessionVariantResult,
    ShutdownRuntimeRequest, ShutdownRuntimeResult, SubmitInputRequest, SubmitInputResult,
    UpdateModelRequest, UpdatePinnedMemoryRequest, UploadAttachmentResult,
    ValidateModelConnectionRequest, ValidateModelConnectionResult,
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
pub use memory::{
    MemoryAttributeValue, MemoryCapabilities, PersonaSnapshot, PinnedMemoryCollectionSnapshot,
    PinnedMemoryCreatedBy, PinnedMemorySnapshot, SystemContextSnapshot,
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
    ComposerCapabilitiesSnapshot, ContextUsageSnapshot, ConversationFileReference,
    ConversationHistoryHit, ConversationHistoryMatchKind, ConversationHistoryScope,
    ConversationItem, ConversationOwner, ConversationPage, GetApplicationSnapshotRequest,
    GetApplicationSnapshotResult, GetChildTaskViewRequest, GetChildTaskViewResult,
    GetConversationPageAroundMessageRequest, GetConversationPageAroundMessageResult,
    GetConversationPageAroundRunRequest, GetConversationPageAroundRunResult,
    GetConversationRecallWindowRequest, GetConversationRecallWindowResult, GetSessionViewRequest,
    GetSessionViewResult, GetToolDetailRequest, GetToolDetailResult, ImageHandlingMode,
    ImageInspectionDetailSnapshot, InterruptRunRequest, InterruptRunResult,
    ListConversationPageRequest, ListConversationPageResult, MessageFeedback, ObservedSnapshot,
    PrioritizeQueuedInputRequest, PrioritizeQueuedInputResult, QueueExecutionState, QueueSnapshot,
    QueuedInputSnapshot, ReasoningEffortOptionSnapshot, RecallNavigationTarget,
    RecallToolDetailFailure, RecallToolDetailItem, RecallToolDetailSnapshot,
    RejectApprovalAndStopRunRequest, RejectApprovalAndStopRunResult, ResumeQueuedInputRequest,
    ResumeQueuedInputResult, SearchConversationHistoryRequest, SearchConversationHistoryResult,
    SessionUsageSnapshot, SessionViewSnapshot, ToolDetailSnapshot, ToolEventSnapshot,
    ToolFileReference, ToolFileResourceOrigin, ToolFileResourceState, ToolInputSnapshot,
    UsageTotals, UserMessageSnapshot,
};
pub use snapshot::{
    AgentVariant, ApprovalDecision, ApprovalMode, ApprovalSnapshot, ApprovalStatus,
    AttachmentState, AttachmentSummary, ChildTaskSnapshot, ChildTaskStatus, GuardrailKind,
    GuardrailMode, PermissionDiagnostic, PermissionDiagnosticCode, PermissionFileStatus,
    PermissionFileSummary, PermissionScope, ReasoningEffortKey, RunSnapshot, RunStatus,
    RuntimeLifecycle, SessionLifecycle, SessionListFilter, SessionSummary, SessionTitleOrigin,
    TokenUsageSnapshot, ToolActivitySnapshot, ToolActivityStatus, ToolApprovalSubject,
    ToolOutputChannel, WorkspaceLifecycle, WorkspaceSummary,
};

/// 客户端与 Runtime Host 当前共同理解的应用协议版本。
///
/// 该常量通过 Host capabilities 投影，不定义 HTTP 或 SSE 的传输版本。
pub const PROTOCOL_VERSION: u32 = 1;
