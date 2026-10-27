//! Assistant 应用运行时。
//!
//! Runtime 持有业务权威状态，并负责协调多会话、Agent Run 和配置。正式产品由独立
//! Runtime Host 进程装配本 crate；本 crate 不依赖 Tauri 或具体进程通信方式。

mod agent_variant;
mod artifact;
pub use artifact::ArtifactFiles;
mod channel;
pub mod config;
mod context_compaction;
mod conversation_recall;
mod custom_agent;
mod delegation;
mod device;
mod environment;
mod error;
mod factory;
mod feedback;
pub use feedback::FeedbackMutation;
mod execution_window;
mod goal;
mod id;
mod internal_boundary;
mod journal;
pub use execution_window::ExecutionWindow;
mod mcp;
mod memory;
mod observation;
mod permission;
mod plugin;
pub mod request_diagnostics;
mod run;
mod runtime;
mod session;
mod skill;
mod storage;
mod work_plan;
mod workspace;

pub use channel::{
    ChannelOutput, ChannelOutputDispatchError, ChannelOutputDispatcher, ChannelOutputFuture,
    ChannelSpeechRequirementFuture, ChannelSpeechSegment, DesktopInputSource,
    DeviceDeliveryPreference, DeviceInputSource, InputChannelSource, InputModality,
    OutputCycleState, OutputPreference, ReplyRoute, ResolvedChannelDelivery,
    SubmitSessionInputRequest,
};
pub use config::{
    ConfigCompilation, ConfigDocument, ConfigIssue, ConfigIssueCode, ConfigProjection,
    ConfigSourceFailure, ConfigSourceFailureKind, ConfigSourceFuture, ConfigSourceLoad,
    ConfigSourceReplace, ConfigSourceReplaceFuture, ConfigState, DelegationConfig,
    ExternalModelConfiguration, ModelProtocol, ModelSource, ReasoningEffortKey,
    ReasoningEffortWireValue, ResolvedConfig, ResolvedModelCapabilities, ResolvedModelConfig,
    ResolvedReasoningCapability, ResolvedReasoningEffort, RuntimeConfig, RuntimeConfigSource,
    RuntimeModelTransportConfig, compile_runtime_config,
};
pub use conversation_recall::HmacRecallReferenceCodec;
pub use custom_agent::{CenterCustomAgentDirectory, CustomAgentDirectorySource};
pub use delegation::DELEGATE_TASK_TOOL_NAME;
pub use device::{
    DeviceLifecycle, DeviceNameChange, DevicePublicKey, DeviceRevocation, DeviceRevocationResult,
    NewPairedDevice, PairedDevice, PcOutputHosting, PcOutputHostingChange,
};
pub use environment::{
    ForkSessionEnvironmentFactoryRequest, PreparedSessionEnvironment, SessionEnvironmentFactory,
    SessionEnvironmentFactoryError, SessionEnvironmentFactoryRequest, SessionExecutionEnvironment,
    WorkspaceEnvironmentSource,
};
pub use error::{RuntimeError, RuntimeResult};
pub use plugin::{
    CommittedPluginChange, PluginAudit, PluginConfigLease, PluginFileError, PluginFilePort,
    PluginMcpDiscovery, PluginUseLease, PreparedPluginPackage,
};
mod shell;
pub use shell::FrozenShellEnvironment;

pub use factory::{
    ChildTaskWorkspaceError, ChildTaskWorkspaceFactory, ChildTaskWorkspaceFuture,
    ChildTaskWorkspaceLease, DiscoveredModel, ModelDiscoveryError, ModelDiscoveryErrorKind,
    ModelDiscoveryFormat, ModelDiscoveryFuture, ModelDiscoveryRequest, ModelFeatureSupport,
    ModelParameterError, ModelParameterSource, ModelParameters, ModelReasoningMode,
    ModelServiceFactory, ModelServiceFactoryError, ModelServiceFactoryRequest, ModelTokenLimit,
    ResolvedModelParameters, RunToolBundle, RunToolFactory, RunToolFactoryError,
    RunToolFactoryErrorKind, RunToolFactoryRequest, resolve_model_parameters,
    validate_fixed_model_parameters,
};
pub use mcp::{
    ManagedMcpServerDefinition, McpConfigSource, McpConnection, McpConnectionError,
    McpConnectionFactory, McpConnectionFailureKind, McpConnectionFuture, McpConnectionOptions,
    McpImageMaterializationFailure, McpImageMaterializationFuture, McpImageMaterializer,
    McpRawCallResult, McpRawContent, McpSecret, McpServerConfig, McpServerTransportConfig,
    McpToolDefinition, McpToolPage,
};
pub use memory::{
    MemoryContextSnapshot, PersonaMutation, PersonaSnapshot, PinnedMemoryCreatedBy,
    PinnedMemoryMutation, PinnedMemoryMutationResult, RuntimePinnedMemoryStore, StoredPinnedMemory,
    pinned_memory_limits,
};
pub use permission::{
    CommandMatch, FilePermissionMatcher, GeneralPermissionMatcher, McpPermissionMatcher,
    McpPermissionServerMatch, McpPermissionToolMatch, PathMatch, PermissionDocument,
    PermissionDocumentError, PermissionEffect, PermissionFileLoad, PermissionFileOperation,
    PermissionFileRevision, PermissionFileScope, PermissionFileStore, PermissionMatcher,
    PermissionProcessMode, PermissionRule, PermissionRuleSource, PermissionSourceDiagnostic,
    PermissionStoreFuture, ShellPermissionMatcher, file_matcher_matches, resolve_permission_path,
};
pub use runtime::{
    AssistantRuntime, ExternalModelPublication, ResolvedToolFileResource, StagedAttachmentUpload,
    StagedSessionAttachment,
};
pub use skill::{
    BUILTIN_GUIDE_REFERENCES, BUILTIN_GUIDE_SKILL_NAME, MAX_CATALOG_SKILLS,
    MAX_SKILL_REFERENCE_BYTES, PluginSkillPackage, SkillActivationOwner,
    SkillActivationResolveError, SkillActivationTrigger, SkillCandidate, SkillCatalog,
    SkillCatalogStatus, SkillDefinition, SkillDiagnostic, SkillDiagnosticCode,
    SkillDiagnosticSeverity, SkillDiscovery, SkillDiscoveryStatus, SkillMetadata, SkillName,
    SkillNameError, SkillNameState, SkillNameStateChange, SkillPackageSource,
    SkillPackageSourceError, SkillScanFuture, SkillScanRequest, SkillScanResult, SkillSource,
    StoredSkillActivation, compile_skill_discovery, explicit_skill_states, sort_diagnostics,
};
pub use skill::{SkillReferenceFuture, SkillReferenceRequest};
pub use storage::StoredConversationUsage;
pub use storage::{
    AcceptedInput, AcceptedStoredSessionCommand, ApprovalModeChange, ArchiveChange, ChildTaskStart,
    ChildToolExecutionStart, CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementResult, ContextReplacementTarget, ConversationMessageLocationRequest,
    ConversationRawWindowRequest, ConversationRewrite, ConversationSearchHit,
    ConversationSearchPage, ConversationSearchRequest, ConversationSearchScope,
    ConversationWindowRequest, CrossSessionInputBinding, CrossSessionInputEnvelope,
    CustomAgentMutation, ForkedAttachmentReference, ForkedSessionCommand, GoalClear,
    GoalHeldInputResume, GoalHeldInputResumeResult, GoalInputBinding, GoalStop, GoalStopResult,
    InputMessageValidationError, InputOrigin, LoadedSession, MessageFeedbackChange, ModelChange,
    NewAttachmentUpload, NewStoredChildTask, NewStoredInput, NewStoredRunAttempt, NewStoredSession,
    NewStoredSessionCommand, NewStoredSessionMaterialization, NewWorkspaceRegistration,
    PendingChildToolExchange, PendingToolExchange, PluginOperationKind, PluginOperationPhase,
    PluginOperationUpdate, PluginRecordChange, PluginSettingsChange, PluginSource,
    ProviderModelCatalogReplacement, QueuePriorityChange, ReasoningEffortChange, RecoveredRuntime,
    RewriteGoalEffect, RewriteResult, RuntimeStore, SessionCommandCommit, SessionDeletion,
    SessionFork, SessionHistoryClear, SessionHistoryClearResult, SessionHistoryCompactionFinish,
    SessionHistoryCompactionFinishKind, SessionHistoryCompactionPreparation,
    SessionHistoryCompactionPreparationResult, SessionPinnedChange, SessionProxyChange,
    SessionProxyState, SessionRole, SessionSummaryQuery, SessionTitleChange,
    SessionTitleGenerationCommit, SessionTitleGenerationCommitResult, StoreError, StoreErrorKind,
    StoreFuture, StoredAttachment, StoredAttachmentState, StoredChildTask, StoredChildTaskHistory,
    StoredChildTaskSettlement, StoredConversationHistory, StoredConversationMessageLocation,
    StoredConversationRawWindow, StoredConversationState, StoredConversationWindow,
    StoredCustomAgent, StoredGoal, StoredGoalBudget, StoredGoalObjective, StoredGoalObjectivePart,
    StoredGoalPauseReason, StoredGoalSettlementEffect, StoredGoalState, StoredHistoryAttachment,
    StoredHistoryFacts, StoredHistoryFeedback, StoredInput, StoredInputHistory, StoredInputState,
    StoredMcpSelection, StoredMessageFeedback, StoredMessageOrigin, StoredModelCatalog,
    StoredPlugin, StoredPluginOperation, StoredProvider, StoredQueueItem, StoredRun,
    StoredRunContinuation, StoredRunContinuationResult, StoredRunHistory, StoredRunSettlement,
    StoredRunSettlementResult, StoredSession, StoredSessionCommand, StoredSessionCommandResult,
    StoredSessionCommandState, StoredSessionFork, StoredSessionLifecycle,
    StoredSessionMaterialization, StoredSessionUsage, StoredTerminalRunInputReconciliation,
    StoredTodoItemStatus, StoredWorkPlan, StoredWorkPlanItem, StoredWorkspace,
    StoredWorkspaceLifecycle, ToolExecutionStart, UserMessageCommit, VariantChange, WorkPlanClear,
    WorkPlanMutation, WorkPlanMutationResult, WorkspaceRemoval, WorkspaceRestoration,
    WorkspaceUpdate, attachment_stable_view_path, custom_agent_revision,
    execution_context_from_product_history, merge_context_replacement_with_product_history,
    normalize_context_usage_adjustments, stored_model_catalog_is_valid, valid_custom_agent_config,
    valid_custom_agent_name, validate_input_message, validate_input_message_with_channel_source,
};
