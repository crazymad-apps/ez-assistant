//! Runtime 异常收敛测试使用的易失 Store 包装；不接触真实 Runtime Home。

use std::sync::atomic::{AtomicBool, Ordering};

use agent_types::ConversationSnapshot;
use assistant_protocol::{ChildTaskId, InputId, SessionId};

use crate::{
    AcceptedInput, ApprovalModeChange, ArchiveChange, ChildTaskStart, ChildToolExecutionStart,
    CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ConversationMessageLocationRequest, ConversationRawWindowRequest, ConversationRewrite,
    ConversationSearchPage, ConversationSearchRequest, ConversationWindowRequest,
    MemoryContextSnapshot, MessageFeedbackChange, ModelChange, NewAttachmentUpload,
    NewStoredChildTask, NewStoredInput, NewStoredRunAttempt, NewStoredSession,
    NewWorkspaceRegistration, PendingChildToolExchange, PendingToolExchange, PersonaMutation,
    PersonaSnapshot, PinnedMemoryMutation, PinnedMemoryMutationResult, QueuePriorityChange,
    RecoveredRuntime, RewriteResult, RuntimeStore, SessionDeletion, SessionFork,
    SessionPinnedChange, SessionTitleChange, StoreError, StoreErrorKind, StoreFuture,
    StoredAttachment, StoredChildTask, StoredChildTaskSettlement,
    StoredConversationMessageLocation, StoredConversationRawWindow, StoredConversationWindow,
    StoredMessageFeedback, StoredPinnedMemory, StoredRun, StoredRunSettlement, StoredSession,
    StoredSessionFork, StoredSessionUsage, StoredWorkPlan, StoredWorkspace, UserMessageCommit,
    VariantChange, WorkPlanClear, WorkPlanMutation, WorkPlanMutationResult, WorkspaceRemoval,
    storage::{ToolExecutionStart, VolatileRuntimeStore},
};

pub(super) struct FaultInjectingStore {
    inner: VolatileRuntimeStore,
    panic_next_settlement: AtomicBool,
    fail_settlement: bool,
    hang_shutdown: bool,
    shutdown_called: AtomicBool,
}

impl FaultInjectingStore {
    pub(super) fn panic_once_on_settlement() -> Self {
        Self {
            inner: VolatileRuntimeStore::default(),
            panic_next_settlement: AtomicBool::new(true),
            fail_settlement: false,
            hang_shutdown: false,
            shutdown_called: AtomicBool::new(false),
        }
    }

    pub(super) fn fail_settlement() -> Self {
        Self {
            inner: VolatileRuntimeStore::default(),
            panic_next_settlement: AtomicBool::new(false),
            fail_settlement: true,
            hang_shutdown: false,
            shutdown_called: AtomicBool::new(false),
        }
    }

    pub(super) fn hang_shutdown() -> Self {
        Self {
            inner: VolatileRuntimeStore::default(),
            panic_next_settlement: AtomicBool::new(false),
            fail_settlement: false,
            hang_shutdown: true,
            shutdown_called: AtomicBool::new(false),
        }
    }

    pub(super) fn shutdown_called(&self) -> bool {
        self.shutdown_called.load(Ordering::Acquire)
    }
}

impl RuntimeStore for FaultInjectingStore {
    fn load_runtime(&self) -> StoreFuture<'_, RecoveredRuntime> {
        self.inner.load_runtime()
    }

    fn load_memory_context(&self) -> StoreFuture<'_, MemoryContextSnapshot> {
        self.inner.load_memory_context()
    }

    fn load_work_plan(&self, session_id: &SessionId) -> StoreFuture<'_, Option<StoredWorkPlan>> {
        self.inner.load_work_plan(session_id)
    }

    fn mutate_work_plan(
        &self,
        mutation: WorkPlanMutation,
    ) -> StoreFuture<'_, WorkPlanMutationResult> {
        self.inner.mutate_work_plan(mutation)
    }

    fn clear_work_plan(&self, clear: WorkPlanClear) -> StoreFuture<'_, ()> {
        self.inner.clear_work_plan(clear)
    }

    fn get_persona(&self) -> StoreFuture<'_, PersonaSnapshot> {
        self.inner.get_persona()
    }

    fn set_persona(&self, mutation: PersonaMutation) -> StoreFuture<'_, PersonaSnapshot> {
        self.inner.set_persona(mutation)
    }

    fn list_pinned_memories(&self) -> StoreFuture<'_, Vec<StoredPinnedMemory>> {
        self.inner.list_pinned_memories()
    }

    fn mutate_pinned_memory(
        &self,
        mutation: PinnedMemoryMutation,
    ) -> StoreFuture<'_, PinnedMemoryMutationResult> {
        self.inner.mutate_pinned_memory(mutation)
    }

    fn register_workspace(
        &self,
        registration: NewWorkspaceRegistration,
    ) -> StoreFuture<'_, StoredWorkspace> {
        self.inner.register_workspace(registration)
    }

    fn remove_workspace(&self, removal: WorkspaceRemoval) -> StoreFuture<'_, StoredWorkspace> {
        self.inner.remove_workspace(removal)
    }

    fn upload_attachment(&self, upload: NewAttachmentUpload) -> StoreFuture<'_, StoredAttachment> {
        self.inner.upload_attachment(upload)
    }

    fn create_session(&self, session: NewStoredSession) -> StoreFuture<'_, StoredSession> {
        self.inner.create_session(session)
    }

    fn fork_session(&self, fork: SessionFork) -> StoreFuture<'_, StoredSessionFork> {
        self.inner.fork_session(fork)
    }

    fn inspect_session_deletion(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, assistant_protocol::DeleteSessionImpact> {
        self.inner.inspect_session_deletion(session_id)
    }

    fn delete_session(&self, deletion: SessionDeletion) -> StoreFuture<'_, ()> {
        self.inner.delete_session(deletion)
    }

    fn create_child_task(&self, task: NewStoredChildTask) -> StoreFuture<'_, StoredChildTask> {
        self.inner.create_child_task(task)
    }

    fn start_child_task(&self, start: ChildTaskStart) -> StoreFuture<'_, ()> {
        self.inner.start_child_task(start)
    }

    fn begin_child_tool_exchange(&self, pending: PendingChildToolExchange) -> StoreFuture<'_, ()> {
        self.inner.begin_child_tool_exchange(pending)
    }

    fn mark_child_tool_execution_started(
        &self,
        start: ChildToolExecutionStart,
    ) -> StoreFuture<'_, ()> {
        self.inner.mark_child_tool_execution_started(start)
    }

    fn complete_child_tool_exchange(
        &self,
        completed: CompletedChildToolExchange,
    ) -> StoreFuture<'_, ()> {
        self.inner.complete_child_tool_exchange(completed)
    }

    fn settle_child_task(&self, settlement: StoredChildTaskSettlement) -> StoreFuture<'_, ()> {
        self.inner.settle_child_task(settlement)
    }

    fn request_child_task_cancellation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StoreFuture<'_, StoredChildTask> {
        self.inner
            .request_child_task_cancellation(session_id, child_task_id)
    }

    fn load_child_conversation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StoreFuture<'_, ConversationSnapshot> {
        self.inner
            .load_child_conversation(session_id, child_task_id)
    }

    fn replace_context(&self, replacement: ContextReplacement) -> StoreFuture<'_, ()> {
        self.inner.replace_context(replacement)
    }

    fn accept_input(&self, input: NewStoredInput) -> StoreFuture<'_, AcceptedInput> {
        self.inner.accept_input(input)
    }

    fn cancel_queued_input(
        &self,
        session_id: &SessionId,
        input_id: &InputId,
    ) -> StoreFuture<'_, ()> {
        self.inner.cancel_queued_input(session_id, input_id)
    }

    fn prioritize_queued_input(&self, change: QueuePriorityChange) -> StoreFuture<'_, ()> {
        self.inner.prioritize_queued_input(change)
    }

    fn create_run_attempt(&self, attempt: NewStoredRunAttempt) -> StoreFuture<'_, StoredRun> {
        self.inner.create_run_attempt(attempt)
    }

    fn commit_user_message(&self, commit: UserMessageCommit) -> StoreFuture<'_, ()> {
        self.inner.commit_user_message(commit)
    }

    fn begin_tool_exchange(&self, pending: PendingToolExchange) -> StoreFuture<'_, ()> {
        self.inner.begin_tool_exchange(pending)
    }

    fn mark_tool_execution_started(&self, start: ToolExecutionStart) -> StoreFuture<'_, ()> {
        self.inner.mark_tool_execution_started(start)
    }

    fn complete_tool_exchange(&self, completed: CompletedToolExchange) -> StoreFuture<'_, ()> {
        self.inner.complete_tool_exchange(completed)
    }

    fn settle_run(
        &self,
        settlement: StoredRunSettlement,
    ) -> StoreFuture<'_, crate::StoredRunSettlementResult> {
        if self.panic_next_settlement.swap(false, Ordering::AcqRel) {
            return Box::pin(async { panic!("private injected settlement panic") });
        }
        if self.fail_settlement {
            return Box::pin(std::future::ready(Err(StoreError::new(
                StoreErrorKind::Unavailable,
                "injected settlement failure",
            ))));
        }
        self.inner.settle_run(settlement)
    }

    fn stop_goal(&self, stop: crate::GoalStop) -> StoreFuture<'_, crate::GoalStopResult> {
        self.inner.stop_goal(stop)
    }

    fn clear_goal(&self, clear: crate::GoalClear) -> StoreFuture<'_, ()> {
        self.inner.clear_goal(clear)
    }

    fn resume_goal_with_held_input(
        &self,
        resume: crate::GoalHeldInputResume,
    ) -> StoreFuture<'_, crate::GoalHeldInputResumeResult> {
        self.inner.resume_goal_with_held_input(resume)
    }

    fn load_conversation(&self, session_id: &SessionId) -> StoreFuture<'_, ConversationSnapshot> {
        self.inner.load_conversation(session_id)
    }

    fn get_session_usage(&self, session_id: &SessionId) -> StoreFuture<'_, StoredSessionUsage> {
        self.inner.get_session_usage(session_id)
    }

    fn load_conversation_window(
        &self,
        request: ConversationWindowRequest,
    ) -> StoreFuture<'_, StoredConversationWindow> {
        self.inner.load_conversation_window(request)
    }

    fn load_conversation_raw_window(
        &self,
        request: ConversationRawWindowRequest,
    ) -> StoreFuture<'_, StoredConversationRawWindow> {
        self.inner.load_conversation_raw_window(request)
    }

    fn locate_conversation_message(
        &self,
        request: ConversationMessageLocationRequest,
    ) -> StoreFuture<'_, Option<StoredConversationMessageLocation>> {
        self.inner.locate_conversation_message(request)
    }

    fn search_conversations(
        &self,
        request: ConversationSearchRequest,
    ) -> StoreFuture<'_, ConversationSearchPage> {
        self.inner.search_conversations(request)
    }

    fn set_session_archive(&self, change: ArchiveChange) -> StoreFuture<'_, ()> {
        self.inner.set_session_archive(change)
    }

    fn rename_session(&self, change: SessionTitleChange) -> StoreFuture<'_, ()> {
        self.inner.rename_session(change)
    }

    fn set_session_pinned(&self, change: SessionPinnedChange) -> StoreFuture<'_, ()> {
        self.inner.set_session_pinned(change)
    }

    fn set_message_feedback(&self, change: MessageFeedbackChange) -> StoreFuture<'_, ()> {
        self.inner.set_message_feedback(change)
    }

    fn load_message_feedback(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, Vec<StoredMessageFeedback>> {
        self.inner.load_message_feedback(session_id)
    }

    fn set_session_model(&self, change: ModelChange) -> StoreFuture<'_, ()> {
        self.inner.set_session_model(change)
    }

    fn set_session_reasoning_effort(
        &self,
        change: crate::ReasoningEffortChange,
    ) -> StoreFuture<'_, ()> {
        self.inner.set_session_reasoning_effort(change)
    }

    fn set_session_variant(&self, change: VariantChange) -> StoreFuture<'_, ()> {
        self.inner.set_session_variant(change)
    }

    fn set_session_approval_mode(&self, change: ApprovalModeChange) -> StoreFuture<'_, ()> {
        self.inner.set_session_approval_mode(change)
    }

    fn rewrite_from_user(&self, rewrite: ConversationRewrite) -> StoreFuture<'_, RewriteResult> {
        self.inner.rewrite_from_user(rewrite)
    }

    fn shutdown(&self) -> StoreFuture<'_, ()> {
        self.shutdown_called.store(true, Ordering::Release);
        if self.hang_shutdown {
            return Box::pin(std::future::pending());
        }
        self.inner.shutdown()
    }
}
