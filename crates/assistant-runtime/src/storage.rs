//! Runtime 持久化端口及跨基础设施边界的存储 DTO。
//!
//! 本模块使用业务操作表达 Runtime 对存储的需求，不暴露 SQL、路径、文件 offset 或
//! SQLite 实体。正式本地实现由 Runtime Host 装配；Runtime crate 不依赖具体数据库。

mod child_task;
mod contract;
mod conversation;
mod error;
mod execution;
mod session;
mod volatile;
mod workspace;

pub use child_task::{
    ChildTaskStart, ChildToolExecutionStart, CompletedChildToolExchange, NewStoredChildTask,
    PendingChildToolExchange, StoredChildTask, StoredChildTaskSettlement,
};
pub use contract::{RecoveredRuntime, RuntimeStore};
pub use conversation::{
    ContextReplacement, ContextReplacementTarget, ConversationMessageLocationRequest,
    ConversationRawWindowRequest, ConversationRewrite, ConversationSearchHit,
    ConversationSearchPage, ConversationSearchRequest, ConversationSearchScope,
    ConversationWindowRequest, RewriteResult, StoredConversationMessageLocation,
    StoredConversationRawWindow, StoredConversationWindow,
};
pub use error::{StoreError, StoreErrorKind, StoreFuture};
pub use execution::{
    AcceptedInput, CompletedToolExchange, NewStoredInput, NewStoredRunAttempt, PendingToolExchange,
    QueuePriorityChange, StoredInput, StoredInputState, StoredRun, StoredRunSettlement,
    ToolExecutionStart, UserMessageCommit,
};
pub use session::{
    ApprovalModeChange, ArchiveChange, ForkedAttachmentReference, MessageFeedbackChange,
    ModelChange, NewStoredSession, SessionDeletion, SessionFork, SessionPinnedChange,
    SessionTitleChange, StoredConversationState, StoredMessageFeedback, StoredSession,
    StoredSessionFork, StoredSessionLifecycle, VariantChange,
};
pub(crate) use volatile::VolatileRuntimeStore;
pub use workspace::{
    NewAttachmentUpload, NewWorkspaceRegistration, StoredAttachment, StoredAttachmentState,
    StoredWorkspace, StoredWorkspaceLifecycle, WorkspaceRemoval,
};
