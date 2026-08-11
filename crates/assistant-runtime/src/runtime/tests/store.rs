//! Runtime 异常收敛测试使用的易失 Store 包装；不接触真实 Runtime Home。

use std::sync::atomic::{AtomicBool, Ordering};

use agent_types::ConversationSnapshot;
use assistant_protocol::{InputId, SessionId};

use crate::{
    AcceptedInput, ArchiveChange, CompletedToolExchange, ConversationRewrite, ModelChange,
    NewAttachmentUpload, NewStoredInput, NewStoredRunAttempt, NewStoredSession,
    NewWorkspaceRegistration, PendingToolExchange, RecoveredRuntime, RewriteResult, RuntimeStore,
    StoreError, StoreErrorKind, StoreFuture, StoredAttachment, StoredRun, StoredRunSettlement,
    StoredSession, StoredWorkspace, UserMessageCommit, WorkspaceRemoval,
    storage::VolatileRuntimeStore,
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

    fn create_run_attempt(&self, attempt: NewStoredRunAttempt) -> StoreFuture<'_, StoredRun> {
        self.inner.create_run_attempt(attempt)
    }

    fn commit_user_message(&self, commit: UserMessageCommit) -> StoreFuture<'_, ()> {
        self.inner.commit_user_message(commit)
    }

    fn begin_tool_exchange(&self, pending: PendingToolExchange) -> StoreFuture<'_, ()> {
        self.inner.begin_tool_exchange(pending)
    }

    fn complete_tool_exchange(&self, completed: CompletedToolExchange) -> StoreFuture<'_, ()> {
        self.inner.complete_tool_exchange(completed)
    }

    fn settle_run(&self, settlement: StoredRunSettlement) -> StoreFuture<'_, ()> {
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

    fn load_conversation(&self, session_id: &SessionId) -> StoreFuture<'_, ConversationSnapshot> {
        self.inner.load_conversation(session_id)
    }

    fn set_session_archive(&self, change: ArchiveChange) -> StoreFuture<'_, ()> {
        self.inner.set_session_archive(change)
    }

    fn set_session_model(&self, change: ModelChange) -> StoreFuture<'_, ()> {
        self.inner.set_session_model(change)
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
