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
mod skill;
mod snapshot;

pub use command::{
    ArchiveSessionRequest, ArchiveSessionResult, CancelChildTaskRequest, CancelChildTaskResult,
    CancelQueuedInputRequest, CancelQueuedInputResult, CancelRunRequest, CancelRunResult,
    CancelSessionCompactionRequest, CancelSessionCompactionResult, ClearGoalRequest,
    ClearGoalResult, ClearSessionRequest, ClearSessionResult, ClearWorkPlanRequest,
    ClearWorkPlanResult, CompactSessionOutcome, CompactSessionRequest, CompactSessionResult,
    ConfigurationMutationResult, ConnectionValidationFailure, ConnectionValidationFailureKind,
    ConnectionValidationOutcome, CreateModelRequest, CreatePinnedMemoryRequest,
    CreateSessionRequest, CreateSessionResult, DecideApprovalRequest, DecideApprovalResult,
    DeleteModelRequest, DeletePinnedMemoryRequest, DeleteSessionImpact, DeleteSessionRequest,
    DeleteSessionResult, ForkSessionRequest, ForkSessionResult, GetAttachmentRequest,
    GetAttachmentResult, GetChildTaskRequest, GetChildTaskResult, GetConfigStatusRequest,
    GetConfigStatusResult, GetMemoryCapabilitiesRequest, GetMemoryCapabilitiesResult,
    GetModelRequest, GetModelResult, GetPermissionDocumentRequest, GetPermissionDocumentResult,
    GetPersonaRequest, GetPersonaResult, GetRunRequest, GetRunResult, GetSessionRequest,
    GetSessionResult, GetSkillDetailRequest, GetSkillDetailResult, GetSystemContextRequest,
    GetSystemContextResult, GetWorkspaceRequest, GetWorkspaceResult, ListAttachmentsRequest,
    ListAttachmentsResult, ListChildTasksRequest, ListChildTasksResult, ListModelsRequest,
    ListModelsResult, ListPendingApprovalsRequest, ListPendingApprovalsResult,
    ListPinnedMemoriesRequest, ListPinnedMemoriesResult, ListRunsRequest, ListRunsResult,
    ListSessionsRequest, ListSessionsResult, ListSkillsRequest, ListSkillsResult,
    ListWorkspacesRequest, ListWorkspacesResult, ModelCatalogEntrySnapshot, ModelCatalogSnapshot,
    ModelConfigurationInput, ModelConnectionTarget, ModelCredentialChange,
    PinnedMemoryMutationResult, PrepareDeleteSessionRequest, PrepareDeleteSessionResult,
    ReenterFromUserMessageRequest, ReenterFromUserMessageResult, RegisterWorkspaceRequest,
    RegisterWorkspaceResult, ReloadConfigRequest, ReloadConfigResult, ReloadPermissionsRequest,
    ReloadPermissionsResult, RemoveWorkspaceRequest, RemoveWorkspaceResult, RenameSessionRequest,
    RenameSessionResult, ReplacePermissionDocumentRequest, ReplacePermissionDocumentResult,
    RestoreSessionRequest, RestoreSessionResult, ResumeGoalRequest, ResumeGoalResult,
    ResumeSessionRequest, ResumeSessionResult, RetryRunRequest, RetryRunResult, RuntimeCommand,
    RuntimeCommandResult, SecretValue, SessionHistoryCleanupStatus, SetAuxiliaryVisionModelRequest,
    SetDefaultModelRequest, SetMessageFeedbackRequest, SetMessageFeedbackResult, SetPersonaRequest,
    SetPersonaResult, SetSessionApprovalModeRequest, SetSessionApprovalModeResult,
    SetSessionModelRequest, SetSessionModelResult, SetSessionPinnedRequest, SetSessionPinnedResult,
    SetSessionProxyRequest, SetSessionProxyResult, SetSessionReasoningEffortRequest,
    SetSessionReasoningEffortResult, SetSessionVariantRequest, SetSessionVariantResult,
    SetSkillEnabledRequest, SetSkillEnabledResult, ShutdownRuntimeRequest, ShutdownRuntimeResult,
    StopGoalRequest, StopGoalResult, SubmitInputMode, SubmitInputRequest, SubmitInputResult,
    UpdateModelRequest, UpdatePinnedMemoryRequest, UploadAttachmentResult,
    ValidateModelConnectionRequest, ValidateModelConnectionResult,
};
pub use config::{
    ConfigurationIssue, ConfigurationIssueCode, ConfigurationState, ConfigurationStatus,
    ModelConfiguration, ModelConfigurationOrigin,
};
pub use error::{ModelFailureKind, RuntimeErrorCode, RuntimeErrorInfo};
pub use event::{
    ChildTaskEvent, RuntimeEvent, RuntimeEventEnvelope, SessionCompactionFinishedOutcome,
};
pub use host::{
    RuntimeHostCapabilities, RuntimeHostFeature, RuntimeHostHealth, RuntimeHostHealthStatus,
};
pub use id::{
    ApprovalId, AttachmentId, ChildTaskId, DeleteConfirmationToken, GoalId, IdempotencyKey,
    IdentifierError, InputId, MessageId, ModelKey, ModelKeyError, PartId, ResourceRefId, RunId,
    SessionId, TodoItemId, ToolCallId, WorkspaceId,
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
    ConversationInputSourceSnapshot, ConversationItem, ConversationOwner, ConversationPage,
    GetApplicationSnapshotRequest, GetApplicationSnapshotResult, GetChildTaskViewRequest,
    GetChildTaskViewResult, GetConversationPageAroundMessageRequest,
    GetConversationPageAroundMessageResult, GetConversationPageAroundRunRequest,
    GetConversationPageAroundRunResult, GetConversationRecallWindowRequest,
    GetConversationRecallWindowResult, GetSessionViewRequest, GetSessionViewResult,
    GetToolDetailRequest, GetToolDetailResult, GoalBudgetSnapshot, GoalPauseReasonSnapshot,
    GoalSnapshot, GoalStateSnapshot, ImageHandlingMode, ImageInspectionDetailSnapshot,
    InterruptRunRequest, InterruptRunResult, ListConversationPageRequest,
    ListConversationPageResult, MessageFeedback, ObservedSnapshot, PrioritizeQueuedInputRequest,
    PrioritizeQueuedInputResult, QueueExecutionState, QueueSnapshot, QueuedInputSnapshot,
    ReasoningEffortOptionSnapshot, RecallNavigationTarget, RecallToolDetailFailure,
    RecallToolDetailItem, RecallToolDetailSnapshot, RejectApprovalAndStopRunRequest,
    RejectApprovalAndStopRunResult, ResumeQueuedInputRequest, ResumeQueuedInputResult,
    SearchConversationHistoryRequest, SearchConversationHistoryResult, SessionUsageSnapshot,
    SessionViewSnapshot, TodoItemStatusSnapshot, ToolDetailSnapshot, ToolEventSnapshot,
    ToolFileReference, ToolFileResourceOrigin, ToolFileResourceState, ToolInputSnapshot,
    UsageTotals, UserMessageSnapshot, WorkPlanItemSnapshot, WorkPlanSnapshot,
};
pub use skill::{
    ActiveSkillSnapshot, SessionSkillCatalogSnapshot, SessionSkillCatalogStatusSnapshot,
    SkillActivationTagSnapshot, SkillActivationTriggerSnapshot, SkillDetailSnapshot,
    SkillDiagnosticSeveritySnapshot, SkillDiagnosticSnapshot, SkillHealthSnapshot,
    SkillManagementSnapshot, SkillSourceSnapshot, SkillSummarySnapshot,
};
pub use snapshot::{
    AgentVariant, ApprovalDecision, ApprovalMode, ApprovalSnapshot, ApprovalStatus,
    AttachmentState, AttachmentSummary, ChildTaskSnapshot, ChildTaskStatus,
    ControllerAvailabilitySnapshot, GuardrailKind, GuardrailMode, PermissionDiagnostic,
    PermissionDiagnosticCode, PermissionFileStatus, PermissionFileSummary, PermissionScope,
    ReasoningEffortKey, RunSnapshot, RunStatus, RuntimeLifecycle, SessionCompactionReasonSnapshot,
    SessionCompactionSnapshot, SessionCompactionTriggerSnapshot, SessionLifecycle,
    SessionListFilter, SessionProxySnapshot, SessionRoleSnapshot, SessionSummary,
    SessionTitleOrigin, TokenUsageSnapshot, ToolActivitySnapshot, ToolActivityStatus,
    ToolApprovalSubject, ToolOutputChannel, WorkspaceLifecycle, WorkspaceSummary,
};

/// 客户端与 Runtime Host 当前共同理解的应用协议版本。
///
/// 该常量通过 Host capabilities 投影，不定义 HTTP 或 SSE 的传输版本。
pub const PROTOCOL_VERSION: u32 = 1;
