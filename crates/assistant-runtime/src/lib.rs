//! Assistant 应用运行时。
//!
//! Runtime 持有业务权威状态，并负责协调多会话、Agent Run 和配置。正式产品由独立
//! Runtime Host 进程装配本 crate；本 crate 不依赖 Tauri 或具体进程通信方式。

mod agent_variant;
pub mod config;
mod context_compaction;
mod delegation;
mod environment;
mod error;
mod factory;
mod id;
mod journal;
mod observation;
mod permission;
mod run;
mod runtime;
mod session;
mod storage;
mod workspace;

pub use config::{
    ConfigCompilation, ConfigIssue, ConfigIssueCode, ConfigProjection, ConfigSourceFailure,
    ConfigSourceFailureKind, ConfigSourceFuture, ConfigSourceLoad, ConfigState, DelegationConfig,
    ModelConfigProjection, ModelProtocol, ResolvedConfig, ResolvedModelConfig, RuntimeConfig,
    RuntimeConfigSource, RuntimeModelTransportConfig, compile_runtime_config,
};
pub use delegation::DELEGATE_TASK_TOOL_NAME;
pub use environment::{
    PreparedSessionEnvironment, SessionEnvironmentFactory, SessionEnvironmentFactoryError,
    SessionEnvironmentFactoryRequest, SessionExecutionEnvironment, WorkspaceEnvironmentSource,
};
pub use error::{RuntimeError, RuntimeResult};
pub use factory::{
    ChildTaskWorkspaceError, ChildTaskWorkspaceFactory, ChildTaskWorkspaceFuture,
    ChildTaskWorkspaceLease, ModelCompatibilityProfile, ModelServiceFactory,
    ModelServiceFactoryError, ModelServiceFactoryRequest, RunToolBundle, RunToolFactory,
    RunToolFactoryError, RunToolFactoryErrorKind,
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
    ContextReplacementTarget, ConversationRewrite, ConversationWindowRequest,
    EmptySessionWorkspaceChange, ForkedAttachmentReference, MessageFeedbackChange, ModelChange,
    NewAttachmentUpload, NewStoredChildTask, NewStoredInput, NewStoredRunAttempt, NewStoredSession,
    NewWorkspaceRegistration, PendingChildToolExchange, PendingToolExchange, QueuePriorityChange,
    RecoveredRuntime, RewriteResult, RuntimeStore, SessionDeletion, SessionFork,
    SessionPinnedChange, SessionTitleChange, StoreError, StoreErrorKind, StoreFuture,
    StoredAttachment, StoredAttachmentState, StoredChildTask, StoredChildTaskSettlement,
    StoredConversationState, StoredConversationWindow, StoredInput, StoredInputState,
    StoredMessageFeedback, StoredRun, StoredRunSettlement, StoredSession, StoredSessionFork,
    StoredSessionLifecycle, StoredWorkspace, StoredWorkspaceLifecycle, ToolExecutionStart,
    UserMessageCommit, VariantChange, WorkspaceRemoval,
};
