//! Assistant 应用运行时。
//!
//! Runtime 持有业务权威状态，并负责协调多会话、Agent Run 和配置。正式产品由独立
//! Runtime Host 进程装配本 crate；本 crate 不依赖 Tauri 或具体进程通信方式。

mod agent_variant;
pub mod config;
mod context_compaction;
mod conversation_recall;
mod delegation;
mod environment;
mod error;
mod factory;
mod id;
mod journal;
mod memory;
mod observation;
mod permission;
mod run;
mod runtime;
mod session;
mod storage;
mod workspace;

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
pub use storage::{
    AcceptedInput, ApprovalModeChange, ArchiveChange, ChildTaskStart, ChildToolExecutionStart,
    CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementTarget, ConversationMessageLocationRequest, ConversationRawWindowRequest,
    ConversationRewrite, ConversationSearchHit, ConversationSearchPage, ConversationSearchRequest,
    ConversationSearchScope, ConversationWindowRequest, ForkedAttachmentReference,
    MessageFeedbackChange, ModelChange, NewAttachmentUpload, NewStoredChildTask, NewStoredInput,
    NewStoredRunAttempt, NewStoredSession, NewWorkspaceRegistration, PendingChildToolExchange,
    PendingToolExchange, QueuePriorityChange, ReasoningEffortChange, RecoveredRuntime,
    RewriteResult, RuntimeStore, SessionDeletion, SessionFork, SessionPinnedChange,
    SessionTitleChange, StoreError, StoreErrorKind, StoreFuture, StoredAttachment,
    StoredAttachmentState, StoredChildTask, StoredChildTaskSettlement,
    StoredConversationMessageLocation, StoredConversationRawWindow, StoredConversationState,
    StoredConversationWindow, StoredInput, StoredInputState, StoredMessageFeedback, StoredRun,
    StoredRunSettlement, StoredSession, StoredSessionFork, StoredSessionLifecycle,
    StoredSessionUsage, StoredWorkspace, StoredWorkspaceLifecycle, ToolExecutionStart,
    UserMessageCommit, VariantChange, WorkspaceRemoval,
};
