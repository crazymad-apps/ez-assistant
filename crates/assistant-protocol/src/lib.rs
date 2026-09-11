//! Assistant 各层共享的请求、事件和标识类型。
//!
//! 该 crate 应保持轻量，避免依赖具体 UI、存储或模型实现。

mod command;
mod compatibility;
mod config;
mod device_gateway;
mod error;
mod event;
mod host;
mod host_access;
mod id;
mod materialization;
mod mcp;
mod memory;
mod model_management;
mod permission;
mod product;
mod resource;
mod skill;
mod snapshot;
mod software_version;

pub use compatibility::{
    CLIENT_VERSION_HEADER, ClientCompatibility, MIN_COMPATIBLE_VERSION_HEADER,
    RuntimeCompatibilityError, RuntimeCompatibilityErrorCode, check_compatibility,
};

pub use software_version::parse_software_version;

/// 当前软件发布版本；与 Host、Desktop 和随包 Web 同源。
pub const SOFTWARE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 当前发布要求的应用协议最低兼容软件版本，不表达数据库格式。
pub const MIN_COMPATIBLE_VERSION: &str = env!("EZ_ASSISTANT_MIN_COMPATIBLE_VERSION");

pub use host_access::{
    HostAccessCommand, HostAccessConfiguration, HostAccessScheme, HostAccessStatus,
    HostListenerState, HostLoginRequest, HostLoginResult,
};

pub use command::{
    ArchiveSessionRequest, ArchiveSessionResult, CancelChildTaskRequest, CancelChildTaskResult,
    CancelQueuedInputRequest, CancelQueuedInputResult, CancelRunRequest, CancelRunResult,
    CancelSessionCompactionRequest, CancelSessionCompactionResult, ClearGoalRequest,
    ClearGoalResult, ClearSessionRequest, ClearSessionResult, ClearWorkPlanRequest,
    ClearWorkPlanResult, CompactSessionOutcome, CompactSessionRequest, CompactSessionResult,
    ConnectionValidationFailure, ConnectionValidationFailureKind, ConnectionValidationOutcome,
    CreatePinnedMemoryRequest, CreateSessionRequest, CreateSessionResult, DecideApprovalRequest,
    DecideApprovalResult, DeletePinnedMemoryRequest, DeleteSessionImpact, DeleteSessionRequest,
    DeleteSessionResult, ForkSessionRequest, ForkSessionResult, GenerateSessionTitleRequest,
    GenerateSessionTitleResult, GetAttachmentRequest, GetAttachmentResult, GetChildTaskRequest,
    GetChildTaskResult, GetConfigStatusRequest, GetConfigStatusResult,
    GetMemoryCapabilitiesRequest, GetMemoryCapabilitiesResult, GetPermissionDocumentRequest,
    GetPermissionDocumentResult, GetPersonaRequest, GetPersonaResult, GetRunRequest, GetRunResult,
    GetSessionRequest, GetSessionResult, GetSkillDetailRequest, GetSkillDetailResult,
    GetSystemContextRequest, GetSystemContextResult, GetWorkspaceRequest, GetWorkspaceResult,
    ListAttachmentsRequest, ListAttachmentsResult, ListChildTasksRequest, ListChildTasksResult,
    ListPendingApprovalsRequest, ListPendingApprovalsResult, ListPinnedMemoriesRequest,
    ListPinnedMemoriesResult, ListRunsRequest, ListRunsResult, ListSessionsRequest,
    ListSessionsResult, ListSkillsRequest, ListSkillsResult, ListWorkspacesRequest,
    ListWorkspacesResult, PinnedMemoryMutationResult, PrepareDeleteSessionRequest,
    PrepareDeleteSessionResult, ReenterFromUserMessageRequest, ReenterFromUserMessageResult,
    RegisterWorkspaceRequest, RegisterWorkspaceResult, ReloadConfigRequest, ReloadConfigResult,
    ReloadPermissionsRequest, ReloadPermissionsResult, RemoveWorkspaceRequest,
    RemoveWorkspaceResult, RenameSessionRequest, RenameSessionResult,
    ReplacePermissionDocumentRequest, ReplacePermissionDocumentResult, RestoreSessionRequest,
    RestoreSessionResult, ResumeGoalRequest, ResumeGoalResult, ResumeSessionRequest,
    ResumeSessionResult, RetryRunRequest, RetryRunResult, RuntimeCommand, RuntimeCommandResult,
    SecretValue, SessionHistoryCleanupStatus, SetAuxiliaryVisionModelRequest,
    SetCurrentControllerOutputHostingRequest, SetCurrentControllerOutputHostingResult,
    SetDefaultModelRequest, SetMessageFeedbackRequest, SetMessageFeedbackResult, SetPersonaRequest,
    SetPersonaResult, SetSessionApprovalModeRequest, SetSessionApprovalModeResult,
    SetSessionModelRequest, SetSessionModelResult, SetSessionPinnedRequest, SetSessionPinnedResult,
    SetSessionProxyRequest, SetSessionProxyResult, SetSessionReasoningEffortRequest,
    SetSessionReasoningEffortResult, SetSessionVariantRequest, SetSessionVariantResult,
    SetSkillEnabledRequest, SetSkillEnabledResult, ShutdownRuntimeRequest, ShutdownRuntimeResult,
    StopGoalRequest, StopGoalResult, SubmitInputMode, SubmitInputRequest, SubmitInputResult,
    UpdatePinnedMemoryRequest, UpdateWorkspaceRequest, UpdateWorkspaceResult,
    UploadAttachmentResult, ValidateModelConnectionRequest, ValidateModelConnectionResult,
};
pub use config::{
    ConfigurationIssue, ConfigurationIssueCode, ConfigurationState, ConfigurationStatus,
};
pub use device_gateway::{
    CloseDevicePairingWindowRequest, ConfirmDevicePairingRequest, DeviceCapabilitiesSnapshot,
    DeviceConnectionSnapshot, DeviceGatewayCommand, DeviceGatewayCommandResult, DeviceGatewayEvent,
    DeviceGatewayMutationResult, DeviceGatewaySnapshot, DeviceLifecycleSnapshot,
    DevicePairingWindowSnapshot, DeviceSpeechServicesSnapshot, DeviceSummarySnapshot,
    GetDeviceGatewaySnapshotRequest, OpenDevicePairingWindowRequest, PendingDevicePairingSnapshot,
    RenameDeviceRequest, RevokeDeviceRequest, SetDeviceAccessEnabledRequest,
    SpeechServiceStatusSnapshot,
};
pub use error::{ModelFailureKind, RuntimeErrorCode, RuntimeErrorInfo};
pub use event::{
    ChildTaskEvent, RuntimeEvent, RuntimeEventEnvelope, SessionCompactionFinishedOutcome,
    SessionTitleGenerationFinishedOutcome,
};
pub use host::{
    RuntimeHostCapabilities, RuntimeHostFeature, RuntimeHostHealth, RuntimeHostHealthStatus,
    RuntimeHostStartupError, RuntimeHostStartupStage,
};
pub use id::{
    ApprovalId, AttachmentId, ChildTaskId, DeleteConfirmationToken, DeviceId, GoalId,
    IdempotencyKey, IdentifierError, InputId, McpServerKey, McpServerKeyError, MessageId, PartId,
    ProviderInstanceId, ResourceRefId, RunId, SessionId, TodoItemId, ToolCallId, WorkspaceId,
};
pub use materialization::{
    SessionMaterializationAttachment, SessionMaterializationManifest, SessionMaterializationResult,
};
pub use mcp::{
    AcceptedSessionCommand, CancelMcpServerTestRequest, CancelMcpServerTestResult,
    GetMcpConfigurationRequest, GetMcpConfigurationResult, ListMcpServerOptionsRequest,
    ListMcpServerOptionsResult, McpConfigurationMutation, McpConfigurationSnapshot,
    McpConnectionTestOutcome, McpConnectionTestStage, McpDiagnosticCode, McpDiagnosticSnapshot,
    McpFieldChange, McpImportPreviewEntry, McpRefreshControlResultSnapshot, McpRefreshOutcome,
    McpSecretChange, McpSelectionTagSnapshot, McpServerDraft, McpServerOptionSnapshot,
    McpServerOptionsContext, McpServerRefreshOutcome, McpServerRefreshResultSnapshot,
    McpServerRuntimeState, McpServerSnapshot, McpServerTransportDraft, McpToolIdentity,
    McpTransportKind, MutateMcpConfigurationRequest, MutateMcpConfigurationResult,
    PermissionMcpMatcher, PermissionMcpServerMatch, PermissionMcpToolMatch,
    PreviewMcpImportRequest, PreviewMcpImportResult, QueuedSessionCommandSnapshot,
    QueuedSessionItemSnapshot, SessionCommand, SessionCommandQueueState,
    SubmitSessionCommandRequest, SubmitSessionCommandResult, TestMcpServerRequest,
    TestMcpServerResult,
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
    AssistantSegment, AuxiliaryUsageSnapshot, ChildTaskTreeItemSnapshot, ChildTaskUsageSnapshot,
    ChildTaskViewSnapshot, ComposerCapabilitiesSnapshot, ContextUsageSnapshot,
    ConversationFileReference, ConversationHistoryHit, ConversationHistoryMatchKind,
    ConversationHistoryScope, ConversationInputSourceSnapshot, ConversationItem, ConversationOwner,
    ConversationPage, GetApplicationSnapshotRequest, GetApplicationSnapshotResult,
    GetChildTaskViewRequest, GetChildTaskViewResult, GetConversationPageAroundMessageRequest,
    GetConversationPageAroundMessageResult, GetConversationPageAroundRunRequest,
    GetConversationPageAroundRunResult, GetConversationRecallWindowRequest,
    GetConversationRecallWindowResult, GetSessionViewRequest, GetSessionViewResult,
    GetToolDetailRequest, GetToolDetailResult, GoalBudgetSnapshot, GoalPauseReasonSnapshot,
    GoalSnapshot, GoalStateSnapshot, ImageHandlingMode, ImageInspectionDetailSnapshot,
    InputModalitySnapshot, InterruptRunRequest, InterruptRunResult, ListConversationPageRequest,
    ListConversationPageResult, MessageFeedback, ObservedSnapshot, OutputPreferenceSnapshot,
    PrioritizeQueuedInputRequest, PrioritizeQueuedInputResult, QueueExecutionState, QueueSnapshot,
    QueuedInputSnapshot, QuotedTextSnapshot, QuotedTextSourceRoleSnapshot,
    ReasoningEffortOptionSnapshot, RecallNavigationTarget, RecallToolDetailFailure,
    RecallToolDetailItem, RecallToolDetailSnapshot, RejectApprovalAndStopRunRequest,
    RejectApprovalAndStopRunResult, ResumeQueuedInputRequest, ResumeQueuedInputResult,
    SearchConversationHistoryRequest, SearchConversationHistoryResult, SessionUsageSnapshot,
    SessionViewSnapshot, SessionWorkspaceSnapshot, TodoItemStatusSnapshot, ToolDetailSnapshot,
    ToolEventSnapshot, ToolFileReference, ToolFileResourceOrigin, ToolFileResourceState,
    ToolInputSnapshot, UsageTotals, UserMessageSnapshot, WorkPlanItemSnapshot, WorkPlanSnapshot,
};
pub use resource::{
    HostFileEntry, HostFileRequest, ListHostFilesRequest, ListHostFilesResult,
    ListSessionResourceFilesRequest, ListSessionResourceFilesResult,
    PreviewSessionResourceFileRequest, PreviewSessionResourceFileResult, SessionResourceEntry,
    SessionResourceEntryKind, SessionResourceEntryState, SessionResourceLocator,
    SessionResourcePreviewKind, SessionResourceRoot,
};
pub use skill::{
    ActiveSkillSnapshot, SkillActivationTagSnapshot, SkillActivationTriggerSnapshot,
    SkillDetailSnapshot, SkillDiagnosticSeveritySnapshot, SkillDiagnosticSnapshot,
    SkillHealthSnapshot, SkillManagementSnapshot, SkillSourceSnapshot, SkillSummarySnapshot,
};
pub use snapshot::{
    AgentVariant, ApprovalDecision, ApprovalMode, ApprovalSnapshot, ApprovalStatus,
    AttachmentState, AttachmentSummary, ChildTaskSnapshot, ChildTaskStatus,
    ControllerAvailabilitySnapshot, GuardrailKind, GuardrailMode, PcOutputHostingSnapshot,
    PermissionDiagnostic, PermissionDiagnosticCode, PermissionFileStatus, PermissionFileSummary,
    PermissionScope, ReasoningEffortKey, RunSnapshot, RunStatus, RuntimeLifecycle,
    SessionCompactionReasonSnapshot, SessionCompactionSnapshot, SessionCompactionTriggerSnapshot,
    SessionLifecycle, SessionListFilter, SessionProxySnapshot, SessionRoleSnapshot, SessionSummary,
    SessionTitleGenerationSnapshot, SessionTitleGenerationTriggerSnapshot, SessionTitleOrigin,
    TokenUsageSnapshot, ToolActivitySnapshot, ToolActivityStatus, ToolApprovalSubject,
    ToolOutputChannel, WorkspaceLifecycle, WorkspaceSummary,
};

mod user_terminal;
pub use user_terminal::{
    UserTerminalControl, UserTerminalNotice, UserTerminalSize, UserTerminalSource,
};

pub use model_management::{
    CreateProviderRequest, DiscoveredModel, GetModelConfigurationRequest, GetModelSettingsRequest,
    ListFixedModelConfigsRequest, ListProvidersRequest, ModelConfigOrigin,
    ModelConfigurationDetail, ModelConfigurationSource, ModelConfigurationSummary,
    ModelDiscoveryFormat, ModelFeatureSupport, ModelFixedConfig, ModelParameters,
    ModelReasoningMode, ModelSelection, ModelSettings, ModelTokenLimit, ModelToolChoiceSupport,
    ModelToolImageProjection, ProviderConnection, ProviderCredentialChange,
    ProviderProtocolPreference, ProviderRequest, ProviderSessionUsage, ProviderSummary,
    ProviderType, ProviderUsage, SaveModelFixedConfigRequest, UpdateProviderRequest,
};
