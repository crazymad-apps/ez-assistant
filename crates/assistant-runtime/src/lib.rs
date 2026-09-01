//! Assistant 应用运行时。
//!
//! Runtime 持有业务权威状态，并负责协调多会话、Agent Run 和配置。正式产品由独立
//! Runtime Host 进程装配本 crate；本 crate 不依赖 Tauri 或具体进程通信方式。

mod agent_variant;
mod channel;
pub mod config;
mod context_compaction;
mod conversation_recall;
mod delegation;
mod device;
mod environment;
mod error;
mod factory;
mod goal;
mod id;
mod internal_boundary;
mod journal;
mod memory;
mod observation;
mod permission;
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
    ConfigSourceReplace, ConfigSourceReplaceFuture, ConfigState, DelegationConfig, ModelCatalog,
    ModelCatalogError, ModelConfigProjection, ModelProtocol, ReasoningEffortKey,
    ReasoningEffortWireValue, ResolvedConfig, ResolvedModelCapabilities, ResolvedModelConfig,
    ResolvedReasoningCapability, ResolvedReasoningEffort, RuntimeConfig, RuntimeConfigSource,
    RuntimeModelTransportConfig, compile_runtime_config, compile_runtime_config_with_catalog,
};
pub use conversation_recall::HmacRecallReferenceCodec;
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
pub use factory::{
    ChildTaskWorkspaceError, ChildTaskWorkspaceFactory, ChildTaskWorkspaceFuture,
    ChildTaskWorkspaceLease, ModelServiceFactory, ModelServiceFactoryError,
    ModelServiceFactoryRequest, RunToolBundle, RunToolFactory, RunToolFactoryError,
    RunToolFactoryErrorKind, RunToolFactoryRequest,
};
pub use memory::{
    MemoryContextSnapshot, PersonaMutation, PersonaSnapshot, PinnedMemoryCreatedBy,
    PinnedMemoryMutation, PinnedMemoryMutationResult, RuntimePinnedMemoryStore, StoredPinnedMemory,
    pinned_memory_limits,
};
pub use permission::{
    CommandMatch, FilePermissionMatcher, GeneralPermissionMatcher, PathMatch, PermissionDocument,
    PermissionDocumentError, PermissionEffect, PermissionFileLoad, PermissionFileOperation,
    PermissionFileRevision, PermissionFileScope, PermissionFileStore, PermissionMatcher,
    PermissionProcessMode, PermissionRule, PermissionSourceDiagnostic, PermissionStoreFuture,
    ShellPermissionMatcher,
};
pub use runtime::{AssistantRuntime, ResolvedToolFileResource, StagedAttachmentUpload};
pub use skill::{
    MAX_CATALOG_SKILLS, SessionSkillCatalog, SessionSkillDefinition, SkillActivationOwner,
    SkillActivationResolveError, SkillActivationTrigger, SkillCandidate, SkillCatalogStatus,
    SkillDiagnostic, SkillDiagnosticCode, SkillDiagnosticSeverity, SkillDiscovery,
    SkillDiscoveryStatus, SkillMetadata, SkillName, SkillNameError, SkillNameState,
    SkillNameStateChange, SkillPackageSource, SkillPackageSourceError, SkillScanFuture,
    SkillScanRequest, SkillScanResult, SkillSource, StoredSkillActivation, compile_skill_discovery,
    explicit_skill_states, sort_diagnostics,
};
pub use storage::{
    AcceptedInput, ApprovalModeChange, ArchiveChange, ChildTaskStart, ChildToolExecutionStart,
    CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementResult, ContextReplacementTarget, ConversationMessageLocationRequest,
    ConversationRawWindowRequest, ConversationRewrite, ConversationSearchHit,
    ConversationSearchPage, ConversationSearchRequest, ConversationSearchScope,
    ConversationWindowRequest, CrossSessionInputBinding, CrossSessionInputEnvelope,
    ForkedAttachmentReference, GoalClear, GoalHeldInputResume, GoalHeldInputResumeResult,
    GoalInputBinding, GoalStop, GoalStopResult, InputMessageValidationError, InputOrigin,
    MessageFeedbackChange, ModelChange, NewAttachmentUpload, NewStoredChildTask, NewStoredInput,
    NewStoredRunAttempt, NewStoredSession, NewWorkspaceRegistration, PendingChildToolExchange,
    PendingToolExchange, QueuePriorityChange, ReasoningEffortChange, RecoveredRuntime,
    RewriteGoalEffect, RewriteResult, RuntimeStore, SessionDeletion, SessionFork,
    SessionHistoryClear, SessionHistoryClearResult, SessionHistoryCompactionFinish,
    SessionHistoryCompactionFinishKind, SessionHistoryCompactionPreparation,
    SessionHistoryCompactionPreparationResult, SessionPinnedChange, SessionProxyChange,
    SessionProxyState, SessionRole, SessionTitleChange, StoreError, StoreErrorKind, StoreFuture,
    StoredAttachment, StoredAttachmentState, StoredChildTask, StoredChildTaskSettlement,
    StoredConversationMessageLocation, StoredConversationRawWindow, StoredConversationState,
    StoredConversationWindow, StoredGoal, StoredGoalBudget, StoredGoalObjective,
    StoredGoalObjectivePart, StoredGoalPauseReason, StoredGoalSettlementEffect, StoredGoalState,
    StoredInput, StoredInputState, StoredMessageFeedback, StoredRun, StoredRunContinuation,
    StoredRunContinuationResult, StoredRunSettlement, StoredRunSettlementResult, StoredSession,
    StoredSessionFork, StoredSessionLifecycle, StoredSessionUsage, StoredTodoItemStatus,
    StoredWorkPlan, StoredWorkPlanItem, StoredWorkspace, StoredWorkspaceLifecycle,
    ToolExecutionStart, UserMessageCommit, VariantChange, WorkPlanClear, WorkPlanMutation,
    WorkPlanMutationResult, WorkspaceRemoval, execution_context_from_product_history,
    merge_context_replacement_with_product_history, validate_input_message,
    validate_input_message_with_channel_source,
};
