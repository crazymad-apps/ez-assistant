//! Runtime 持久化端口及跨基础设施边界的存储 DTO。
//!
//! 本模块使用业务操作表达 Runtime 对存储的需求，不暴露 SQL、路径、文件 offset 或
//! SQLite 实体。正式本地实现由 Runtime Host 装配；Runtime crate 不依赖具体数据库。

mod child_task;
mod contract;
mod conversation;
mod error;
mod execution;
mod goal;
mod session;
mod volatile;
mod work_plan;
mod workspace;

pub use child_task::{
    ChildTaskStart, ChildToolExecutionStart, CompletedChildToolExchange, NewStoredChildTask,
    PendingChildToolExchange, StoredChildTask, StoredChildTaskSettlement,
};
pub use contract::{RecoveredRuntime, RuntimeStore};
pub use conversation::{
    ContextReplacement, ContextReplacementResult, ContextReplacementTarget,
    ConversationMessageLocationRequest, ConversationRawWindowRequest, ConversationRewrite,
    ConversationSearchHit, ConversationSearchPage, ConversationSearchRequest,
    ConversationSearchScope, ConversationWindowRequest, RewriteGoalEffect, RewriteResult,
    StoredConversationMessageLocation, StoredConversationRawWindow, StoredConversationWindow,
    execution_context_from_product_history, merge_context_replacement_with_product_history,
};
pub use error::{StoreError, StoreErrorKind, StoreFuture};
pub use execution::{
    AcceptedInput, CompletedToolExchange, CrossSessionInputBinding, CrossSessionInputEnvelope,
    GoalInputBinding, InputMessageValidationError, InputOrigin, NewStoredInput,
    NewStoredRunAttempt, PendingToolExchange, QueuePriorityChange, StoredGoalSettlementEffect,
    StoredInput, StoredInputState, StoredRun, StoredRunContinuation, StoredRunContinuationResult,
    StoredRunSettlement, StoredRunSettlementResult, ToolExecutionStart, UserMessageCommit,
    validate_input_message, validate_input_message_with_channel_source,
};
pub use goal::{
    GoalClear, GoalHeldInputResume, GoalHeldInputResumeResult, GoalStop, GoalStopResult,
    StoredGoal, StoredGoalBudget, StoredGoalObjective, StoredGoalObjectivePart,
    StoredGoalPauseReason, StoredGoalState,
};
pub use session::{
    ApprovalModeChange, ArchiveChange, ForkedAttachmentReference, MessageFeedbackChange,
    ModelChange, NewStoredSession, ReasoningEffortChange, SessionDeletion, SessionFork,
    SessionHistoryClear, SessionHistoryClearResult, SessionHistoryCompactionFinish,
    SessionHistoryCompactionFinishKind, SessionHistoryCompactionPreparation,
    SessionHistoryCompactionPreparationResult, SessionPinnedChange, SessionProxyChange,
    SessionProxyState, SessionRole, SessionTitleChange, StoredConversationState,
    StoredMessageFeedback, StoredSession, StoredSessionFork, StoredSessionLifecycle,
    StoredSessionUsage, VariantChange,
};
pub(crate) use volatile::VolatileRuntimeStore;
pub use work_plan::{
    StoredTodoItemStatus, StoredWorkPlan, StoredWorkPlanItem, WorkPlanClear, WorkPlanMutation,
    WorkPlanMutationResult,
};
pub use workspace::{
    NewAttachmentUpload, NewWorkspaceRegistration, StoredAttachment, StoredAttachmentState,
    StoredWorkspace, StoredWorkspaceLifecycle, WorkspaceRemoval,
};
