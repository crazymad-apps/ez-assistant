//! 有界命令队列与专用阻塞存储线程。

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

use assistant_protocol::{ChildTaskId, InputId, SessionId};
use assistant_runtime::{
    AcceptedInput, ApprovalModeChange, ArchiveChange, ChildTaskStart, ChildToolExecutionStart,
    CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementResult, ConversationMessageLocationRequest, ConversationRawWindowRequest,
    ConversationRewrite, ConversationSearchPage, ConversationSearchRequest,
    ConversationWindowRequest, GoalClear, GoalHeldInputResume, GoalHeldInputResumeResult, GoalStop,
    GoalStopResult, MemoryContextSnapshot, MessageFeedbackChange, ModelChange, NewAttachmentUpload,
    NewStoredChildTask, NewStoredInput, NewStoredRunAttempt, NewStoredSession,
    NewWorkspaceRegistration, PendingChildToolExchange, PendingToolExchange, PermissionFileLoad,
    PermissionFileRevision, PermissionFileScope, PermissionFileStore, PermissionStoreFuture,
    PersonaMutation, PersonaSnapshot, PinnedMemoryMutation, PinnedMemoryMutationResult,
    QueuePriorityChange, ReasoningEffortChange, RecoveredRuntime, RewriteResult, RuntimeStore,
    SessionDeletion, SessionFork, SessionHistoryClear, SessionHistoryClearResult,
    SessionHistoryCompactionFinish, SessionHistoryCompactionPreparation,
    SessionHistoryCompactionPreparationResult, SessionPinnedChange, SessionProxyChange,
    SessionTitleChange, SkillNameState, SkillNameStateChange, StoreError, StoreErrorKind,
    StoreFuture, StoredAttachment, StoredChildTask, StoredChildTaskSettlement,
    StoredConversationMessageLocation, StoredConversationRawWindow, StoredConversationWindow,
    StoredMessageFeedback, StoredPinnedMemory, StoredRun, StoredRunSettlement,
    StoredRunSettlementResult, StoredSession, StoredSessionFork, StoredSessionUsage,
    StoredWorkPlan, StoredWorkspace, ToolExecutionStart, UserMessageCommit, VariantChange,
    WorkPlanClear, WorkPlanMutation, WorkPlanMutationResult, WorkspaceRemoval,
};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use super::StorageEngine;

const WORKER_NAME: &str = "runtime-storage";

enum Command {
    #[cfg(test)]
    PanicForTest {
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    LoadRuntime {
        reply: oneshot::Sender<Result<RecoveredRuntime, StoreError>>,
    },
    LoadMemoryContext {
        reply: oneshot::Sender<Result<MemoryContextSnapshot, StoreError>>,
    },
    ListSkillNameStates {
        reply: oneshot::Sender<Result<Vec<SkillNameState>, StoreError>>,
    },
    SetSkillEnabled {
        change: SkillNameStateChange,
        reply: oneshot::Sender<Result<SkillNameState, StoreError>>,
    },
    LoadWorkPlan {
        session_id: SessionId,
        reply: oneshot::Sender<Result<Option<StoredWorkPlan>, StoreError>>,
    },
    MutateWorkPlan {
        mutation: WorkPlanMutation,
        reply: oneshot::Sender<Result<WorkPlanMutationResult, StoreError>>,
    },
    ClearWorkPlan {
        clear: WorkPlanClear,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    GetPersona {
        reply: oneshot::Sender<Result<PersonaSnapshot, StoreError>>,
    },
    SetPersona {
        mutation: PersonaMutation,
        reply: oneshot::Sender<Result<PersonaSnapshot, StoreError>>,
    },
    ListPinnedMemories {
        reply: oneshot::Sender<Result<Vec<StoredPinnedMemory>, StoreError>>,
    },
    MutatePinnedMemory {
        mutation: PinnedMemoryMutation,
        reply: oneshot::Sender<Result<PinnedMemoryMutationResult, StoreError>>,
    },
    RegisterWorkspace {
        registration: NewWorkspaceRegistration,
        reply: oneshot::Sender<Result<StoredWorkspace, StoreError>>,
    },
    RemoveWorkspace {
        removal: WorkspaceRemoval,
        reply: oneshot::Sender<Result<StoredWorkspace, StoreError>>,
    },
    UploadAttachment {
        upload: NewAttachmentUpload,
        reply: oneshot::Sender<Result<StoredAttachment, StoreError>>,
    },
    CreateSession {
        session: NewStoredSession,
        reply: oneshot::Sender<Result<StoredSession, StoreError>>,
    },
    ForkSession {
        fork: Box<SessionFork>,
        reply: oneshot::Sender<Result<StoredSessionFork, StoreError>>,
    },
    InspectSessionDeletion {
        session_id: SessionId,
        reply: oneshot::Sender<Result<assistant_protocol::DeleteSessionImpact, StoreError>>,
    },
    DeleteSession {
        deletion: SessionDeletion,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    ClearSessionHistory {
        clear: Box<SessionHistoryClear>,
        reply: oneshot::Sender<Result<SessionHistoryClearResult, StoreError>>,
    },
    PrepareSessionCompaction {
        preparation: SessionHistoryCompactionPreparation,
        reply: oneshot::Sender<Result<SessionHistoryCompactionPreparationResult, StoreError>>,
    },
    FinishSessionCompaction {
        finish: SessionHistoryCompactionFinish,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    CreateChildTask {
        task: NewStoredChildTask,
        reply: oneshot::Sender<Result<StoredChildTask, StoreError>>,
    },
    StartChildTask {
        start: ChildTaskStart,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    BeginChildToolExchange {
        pending: PendingChildToolExchange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    MarkChildToolExecutionStarted {
        start: ChildToolExecutionStart,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    CompleteChildToolExchange {
        completed: CompletedChildToolExchange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    SettleChildTask {
        settlement: StoredChildTaskSettlement,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    RequestChildTaskCancellation {
        session_id: SessionId,
        child_task_id: ChildTaskId,
        reply: oneshot::Sender<Result<StoredChildTask, StoreError>>,
    },
    LoadChildConversation {
        session_id: SessionId,
        child_task_id: ChildTaskId,
        reply: oneshot::Sender<Result<agent_types::ConversationSnapshot, StoreError>>,
    },
    ReplaceContext {
        replacement: ContextReplacement,
        reply: oneshot::Sender<Result<ContextReplacementResult, StoreError>>,
    },
    AcceptInput {
        input: Box<NewStoredInput>,
        reply: oneshot::Sender<Result<AcceptedInput, StoreError>>,
    },
    CancelQueuedInput {
        session_id: SessionId,
        input_id: InputId,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    PrioritizeQueuedInput {
        change: QueuePriorityChange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    CreateRunAttempt {
        attempt: NewStoredRunAttempt,
        reply: oneshot::Sender<Result<StoredRun, StoreError>>,
    },
    CommitUserMessage {
        commit: UserMessageCommit,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    BeginToolExchange {
        pending: PendingToolExchange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    MarkToolExecutionStarted {
        start: ToolExecutionStart,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    CompleteToolExchange {
        completed: CompletedToolExchange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    SettleRun {
        settlement: Box<StoredRunSettlement>,
        reply: oneshot::Sender<Result<StoredRunSettlementResult, StoreError>>,
    },
    StopGoal {
        stop: GoalStop,
        reply: oneshot::Sender<Result<GoalStopResult, StoreError>>,
    },
    ClearGoal {
        clear: GoalClear,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    ResumeGoalWithHeldInput {
        resume: GoalHeldInputResume,
        reply: oneshot::Sender<Result<GoalHeldInputResumeResult, StoreError>>,
    },
    LoadConversation {
        session_id: SessionId,
        reply: oneshot::Sender<Result<agent_types::ConversationSnapshot, StoreError>>,
    },
    GetSessionUsage {
        session_id: SessionId,
        reply: oneshot::Sender<Result<StoredSessionUsage, StoreError>>,
    },
    LoadConversationWindow {
        request: ConversationWindowRequest,
        reply: oneshot::Sender<Result<StoredConversationWindow, StoreError>>,
    },
    LoadConversationRawWindow {
        request: ConversationRawWindowRequest,
        reply: oneshot::Sender<Result<StoredConversationRawWindow, StoreError>>,
    },
    LocateConversationMessage {
        request: ConversationMessageLocationRequest,
        reply: oneshot::Sender<Result<Option<StoredConversationMessageLocation>, StoreError>>,
    },
    SearchConversations {
        request: ConversationSearchRequest,
        reply: oneshot::Sender<Result<ConversationSearchPage, StoreError>>,
    },
    SetSessionArchive {
        change: ArchiveChange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    SetSessionProxy {
        change: SessionProxyChange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    RenameSession {
        change: SessionTitleChange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    SetSessionPinned {
        change: SessionPinnedChange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    SetMessageFeedback {
        change: MessageFeedbackChange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    LoadMessageFeedback {
        session_id: SessionId,
        reply: oneshot::Sender<Result<Vec<StoredMessageFeedback>, StoreError>>,
    },
    SetSessionModel {
        change: ModelChange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    SetSessionReasoningEffort {
        change: ReasoningEffortChange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    SetSessionVariant {
        change: VariantChange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    SetSessionApprovalMode {
        change: ApprovalModeChange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    RewriteFromUser {
        rewrite: Box<ConversationRewrite>,
        reply: oneshot::Sender<Result<RewriteResult, StoreError>>,
    },
    LoadPermissionFile {
        scope: PermissionFileScope,
        reply: oneshot::Sender<Result<PermissionFileLoad, StoreError>>,
    },
    ReplacePermissionFile {
        scope: PermissionFileScope,
        expected_revision: PermissionFileRevision,
        content: Vec<u8>,
        reply: oneshot::Sender<Result<PermissionFileRevision, StoreError>>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
}

impl PermissionFileStore for LocalRuntimeStore {
    fn load_permission_file(
        &self,
        scope: &PermissionFileScope,
    ) -> PermissionStoreFuture<'_, PermissionFileLoad> {
        let scope = scope.clone();
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::LoadPermissionFile { scope, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn replace_permission_file(
        &self,
        scope: &PermissionFileScope,
        expected_revision: &PermissionFileRevision,
        content: Vec<u8>,
    ) -> PermissionStoreFuture<'_, PermissionFileRevision> {
        let scope = scope.clone();
        let expected_revision = expected_revision.clone();
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::ReplacePermissionFile {
                scope,
                expected_revision,
                content,
                reply,
            })
            .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }
}

/// Host 本地 RuntimeStore；所有阻塞 I/O 均由其拥有的命名线程执行。
pub(crate) struct LocalRuntimeStore {
    sender: mpsc::Sender<Command>,
    send_gate: AsyncMutex<()>,
    worker: Arc<Mutex<Option<JoinHandle<()>>>>,
    is_closing: std::sync::atomic::AtomicBool,
}

impl LocalRuntimeStore {
    pub(crate) async fn open(runtime_home: &Path, capacity: usize) -> Result<Self, StoreError> {
        if capacity == 0 {
            return Err(StoreError::new(
                StoreErrorKind::InvalidInput,
                "runtime storage queue capacity must be greater than zero",
            ));
        }
        let (sender, receiver) = mpsc::channel(capacity);
        let (ready_sender, ready_receiver) = oneshot::channel();
        let runtime_home = PathBuf::from(runtime_home);
        let worker = thread::Builder::new()
            .name(WORKER_NAME.to_owned())
            .spawn(move || run_worker(runtime_home, receiver, ready_sender))
            .map_err(|source| {
                StoreError::with_source(
                    StoreErrorKind::Unavailable,
                    "runtime storage worker could not be started",
                    source,
                )
            })?;

        match ready_receiver.await {
            Ok(Ok(())) => Ok(Self {
                sender,
                send_gate: AsyncMutex::new(()),
                worker: Arc::new(Mutex::new(Some(worker))),
                is_closing: std::sync::atomic::AtomicBool::new(false),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(worker_unavailable())
            }
        }
    }

    async fn enqueue(&self, command: Command) -> Result<(), StoreError> {
        use std::sync::atomic::Ordering;

        let _gate = self.send_gate.lock().await;
        if self.is_closing.load(Ordering::Acquire) {
            return Err(worker_unavailable());
        }
        self.sender
            .send(command)
            .await
            .map_err(|_| worker_unavailable())
    }

    async fn begin_shutdown(&self, command: Command) -> Result<(), StoreError> {
        use std::sync::atomic::Ordering;

        let _gate = self.send_gate.lock().await;
        if self.is_closing.load(Ordering::Acquire) {
            return Ok(());
        }
        self.sender
            .send(command)
            .await
            .map_err(|_| worker_unavailable())?;
        // 只在 Shutdown 命令已进入 worker 队列后关闭后续准入。若 send future 被
        // 上层关闭超时取消，下一次 shutdown 仍可安全重试，而不会遗漏退出命令。
        self.is_closing.store(true, Ordering::Release);
        Ok(())
    }

    async fn join_worker(&self) -> Result<(), StoreError> {
        let worker = self.worker.lock().map_err(|_| worker_unavailable())?.take();
        let Some(worker) = worker else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || worker.join())
            .await
            .map_err(|_| worker_unavailable())?
            .map_err(|_| worker_unavailable())
    }

    /// 只供所有权回归测试使用：让命名 worker 异常退出并观察调用方错误。
    #[cfg(test)]
    pub(super) async fn panic_worker_for_test(&self) -> Result<(), StoreError> {
        let (reply, result) = oneshot::channel();
        self.enqueue(Command::PanicForTest { reply }).await?;
        result.await.map_err(|_| worker_unavailable())?
    }
}

impl RuntimeStore for LocalRuntimeStore {
    fn load_runtime(&self) -> StoreFuture<'_, RecoveredRuntime> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::LoadRuntime { reply }).await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn load_memory_context(&self) -> StoreFuture<'_, MemoryContextSnapshot> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::LoadMemoryContext { reply }).await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn list_skill_name_states(&self) -> StoreFuture<'_, Vec<SkillNameState>> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::ListSkillNameStates { reply }).await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn set_skill_enabled(&self, change: SkillNameStateChange) -> StoreFuture<'_, SkillNameState> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SetSkillEnabled { change, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn load_work_plan(&self, session_id: &SessionId) -> StoreFuture<'_, Option<StoredWorkPlan>> {
        let session_id = session_id.clone();
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::LoadWorkPlan { session_id, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn mutate_work_plan(
        &self,
        mutation: WorkPlanMutation,
    ) -> StoreFuture<'_, WorkPlanMutationResult> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::MutateWorkPlan { mutation, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn clear_work_plan(&self, clear: WorkPlanClear) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::ClearWorkPlan { clear, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn get_persona(&self) -> StoreFuture<'_, PersonaSnapshot> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::GetPersona { reply }).await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn set_persona(&self, mutation: PersonaMutation) -> StoreFuture<'_, PersonaSnapshot> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SetPersona { mutation, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn list_pinned_memories(&self) -> StoreFuture<'_, Vec<StoredPinnedMemory>> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::ListPinnedMemories { reply }).await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn mutate_pinned_memory(
        &self,
        mutation: PinnedMemoryMutation,
    ) -> StoreFuture<'_, PinnedMemoryMutationResult> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::MutatePinnedMemory { mutation, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn register_workspace(
        &self,
        registration: NewWorkspaceRegistration,
    ) -> StoreFuture<'_, StoredWorkspace> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::RegisterWorkspace {
                registration,
                reply,
            })
            .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn remove_workspace(&self, removal: WorkspaceRemoval) -> StoreFuture<'_, StoredWorkspace> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::RemoveWorkspace { removal, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn upload_attachment(&self, upload: NewAttachmentUpload) -> StoreFuture<'_, StoredAttachment> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::UploadAttachment { upload, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn create_session(&self, session: NewStoredSession) -> StoreFuture<'_, StoredSession> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::CreateSession { session, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn fork_session(&self, fork: SessionFork) -> StoreFuture<'_, StoredSessionFork> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::ForkSession {
                fork: Box::new(fork),
                reply,
            })
            .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn inspect_session_deletion(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, assistant_protocol::DeleteSessionImpact> {
        let session_id = session_id.clone();
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::InspectSessionDeletion { session_id, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn delete_session(&self, deletion: SessionDeletion) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::DeleteSession { deletion, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn clear_session_history(
        &self,
        clear: SessionHistoryClear,
    ) -> StoreFuture<'_, SessionHistoryClearResult> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::ClearSessionHistory {
                clear: Box::new(clear),
                reply,
            })
            .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn prepare_session_compaction(
        &self,
        preparation: SessionHistoryCompactionPreparation,
    ) -> StoreFuture<'_, SessionHistoryCompactionPreparationResult> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::PrepareSessionCompaction { preparation, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn finish_session_compaction(
        &self,
        finish: SessionHistoryCompactionFinish,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::FinishSessionCompaction { finish, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn create_child_task(&self, task: NewStoredChildTask) -> StoreFuture<'_, StoredChildTask> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::CreateChildTask { task, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn start_child_task(&self, start: ChildTaskStart) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::StartChildTask { start, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn begin_child_tool_exchange(&self, pending: PendingChildToolExchange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::BeginChildToolExchange { pending, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn mark_child_tool_execution_started(
        &self,
        start: ChildToolExecutionStart,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::MarkChildToolExecutionStarted { start, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn complete_child_tool_exchange(
        &self,
        completed: CompletedChildToolExchange,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::CompleteChildToolExchange { completed, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn settle_child_task(&self, settlement: StoredChildTaskSettlement) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SettleChildTask { settlement, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn request_child_task_cancellation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StoreFuture<'_, StoredChildTask> {
        let session_id = session_id.clone();
        let child_task_id = child_task_id.clone();
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::RequestChildTaskCancellation {
                session_id,
                child_task_id,
                reply,
            })
            .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn load_child_conversation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StoreFuture<'_, agent_types::ConversationSnapshot> {
        let session_id = session_id.clone();
        let child_task_id = child_task_id.clone();
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::LoadChildConversation {
                session_id,
                child_task_id,
                reply,
            })
            .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn replace_context(
        &self,
        replacement: ContextReplacement,
    ) -> StoreFuture<'_, ContextReplacementResult> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::ReplaceContext { replacement, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn accept_input(&self, input: NewStoredInput) -> StoreFuture<'_, AcceptedInput> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::AcceptInput {
                input: Box::new(input),
                reply,
            })
            .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn cancel_queued_input(
        &self,
        session_id: &SessionId,
        input_id: &InputId,
    ) -> StoreFuture<'_, ()> {
        let session_id = session_id.clone();
        let input_id = input_id.clone();
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::CancelQueuedInput {
                session_id,
                input_id,
                reply,
            })
            .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn prioritize_queued_input(&self, change: QueuePriorityChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::PrioritizeQueuedInput { change, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn create_run_attempt(&self, attempt: NewStoredRunAttempt) -> StoreFuture<'_, StoredRun> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::CreateRunAttempt { attempt, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn commit_user_message(&self, commit: UserMessageCommit) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::CommitUserMessage { commit, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn settle_run(
        &self,
        settlement: StoredRunSettlement,
    ) -> StoreFuture<'_, StoredRunSettlementResult> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SettleRun {
                settlement: Box::new(settlement),
                reply,
            })
            .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn stop_goal(&self, stop: GoalStop) -> StoreFuture<'_, GoalStopResult> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::StopGoal { stop, reply }).await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn clear_goal(&self, clear: GoalClear) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::ClearGoal { clear, reply }).await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn resume_goal_with_held_input(
        &self,
        resume: GoalHeldInputResume,
    ) -> StoreFuture<'_, GoalHeldInputResumeResult> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::ResumeGoalWithHeldInput { resume, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn begin_tool_exchange(&self, pending: PendingToolExchange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::BeginToolExchange { pending, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn mark_tool_execution_started(&self, start: ToolExecutionStart) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::MarkToolExecutionStarted { start, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn complete_tool_exchange(&self, completed: CompletedToolExchange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::CompleteToolExchange { completed, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn load_conversation(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, agent_types::ConversationSnapshot> {
        let session_id = session_id.clone();
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::LoadConversation { session_id, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn get_session_usage(&self, session_id: &SessionId) -> StoreFuture<'_, StoredSessionUsage> {
        let session_id = session_id.clone();
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::GetSessionUsage { session_id, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn load_conversation_window(
        &self,
        request: ConversationWindowRequest,
    ) -> StoreFuture<'_, StoredConversationWindow> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::LoadConversationWindow { request, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn load_conversation_raw_window(
        &self,
        request: ConversationRawWindowRequest,
    ) -> StoreFuture<'_, StoredConversationRawWindow> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::LoadConversationRawWindow { request, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn locate_conversation_message(
        &self,
        request: ConversationMessageLocationRequest,
    ) -> StoreFuture<'_, Option<StoredConversationMessageLocation>> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::LocateConversationMessage { request, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn search_conversations(
        &self,
        request: ConversationSearchRequest,
    ) -> StoreFuture<'_, ConversationSearchPage> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SearchConversations { request, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn set_session_archive(&self, change: ArchiveChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SetSessionArchive { change, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn set_session_proxy(&self, change: SessionProxyChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SetSessionProxy { change, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn rename_session(&self, change: SessionTitleChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::RenameSession { change, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn set_session_pinned(&self, change: SessionPinnedChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SetSessionPinned { change, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn set_message_feedback(&self, change: MessageFeedbackChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SetMessageFeedback { change, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn load_message_feedback(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, Vec<StoredMessageFeedback>> {
        let session_id = session_id.clone();
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::LoadMessageFeedback { session_id, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn set_session_model(&self, change: ModelChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SetSessionModel { change, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn set_session_reasoning_effort(&self, change: ReasoningEffortChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SetSessionReasoningEffort { change, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn set_session_variant(&self, change: VariantChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SetSessionVariant { change, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn set_session_approval_mode(&self, change: ApprovalModeChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SetSessionApprovalMode { change, reply })
                .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn rewrite_from_user(&self, rewrite: ConversationRewrite) -> StoreFuture<'_, RewriteResult> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::RewriteFromUser {
                rewrite: Box::new(rewrite),
                reply,
            })
            .await?;
            result.await.map_err(|_| worker_unavailable())?
        })
    }

    fn shutdown(&self) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            if let Err(error) = self.begin_shutdown(Command::Shutdown { reply }).await {
                let _ = self.join_worker().await;
                return Err(error);
            }
            match result.await {
                Ok(outcome) => outcome?,
                Err(_) if self.is_closing.load(std::sync::atomic::Ordering::Acquire) => {}
                Err(_) => return Err(worker_unavailable()),
            }
            self.join_worker().await
        })
    }
}

fn run_worker(
    runtime_home: PathBuf,
    mut receiver: mpsc::Receiver<Command>,
    ready: oneshot::Sender<Result<(), StoreError>>,
) {
    let mut engine = match StorageEngine::open(&runtime_home) {
        Ok(engine) => {
            let _ = ready.send(Ok(()));
            engine
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };

    while let Some(command) = receiver.blocking_recv() {
        match command {
            #[cfg(test)]
            Command::PanicForTest { reply } => {
                drop(reply);
                panic!("private storage worker panic payload");
            }
            Command::LoadRuntime { reply } => {
                let _ = reply.send(engine.load_runtime());
            }
            Command::LoadMemoryContext { reply } => {
                let _ = reply.send(engine.load_memory_context());
            }
            Command::ListSkillNameStates { reply } => {
                let _ = reply.send(engine.list_skill_name_states());
            }
            Command::SetSkillEnabled { change, reply } => {
                let _ = reply.send(engine.set_skill_enabled(change));
            }
            Command::LoadWorkPlan { session_id, reply } => {
                let _ = reply.send(engine.load_work_plan(&session_id));
            }
            Command::MutateWorkPlan { mutation, reply } => {
                let _ = reply.send(engine.mutate_work_plan(mutation));
            }
            Command::ClearWorkPlan { clear, reply } => {
                let _ = reply.send(engine.clear_work_plan(clear));
            }
            Command::GetPersona { reply } => {
                let _ = reply.send(engine.get_persona());
            }
            Command::SetPersona { mutation, reply } => {
                let _ = reply.send(engine.set_persona(mutation));
            }
            Command::ListPinnedMemories { reply } => {
                let _ = reply.send(engine.list_pinned_memories());
            }
            Command::MutatePinnedMemory { mutation, reply } => {
                let _ = reply.send(engine.mutate_pinned_memory(mutation));
            }
            Command::RegisterWorkspace {
                registration,
                reply,
            } => {
                let _ = reply.send(engine.register_workspace(registration));
            }
            Command::RemoveWorkspace { removal, reply } => {
                let _ = reply.send(engine.remove_workspace(removal));
            }
            Command::UploadAttachment { upload, reply } => {
                let _ = reply.send(engine.upload_attachment(upload));
            }
            Command::CreateSession { session, reply } => {
                let _ = reply.send(engine.create_session(session));
            }
            Command::ForkSession { fork, reply } => {
                let _ = reply.send(engine.fork_session(*fork));
            }
            Command::InspectSessionDeletion { session_id, reply } => {
                let _ = reply.send(engine.inspect_session_deletion(&session_id));
            }
            Command::DeleteSession { deletion, reply } => {
                let _ = reply.send(engine.delete_session(deletion));
            }
            Command::ClearSessionHistory { clear, reply } => {
                let _ = reply.send(engine.clear_session_history(*clear));
            }
            Command::PrepareSessionCompaction { preparation, reply } => {
                let _ = reply.send(engine.prepare_session_compaction(preparation));
            }
            Command::FinishSessionCompaction { finish, reply } => {
                let _ = reply.send(engine.finish_session_compaction(finish));
            }
            Command::CreateChildTask { task, reply } => {
                let _ = reply.send(engine.create_child_task(task));
            }
            Command::StartChildTask { start, reply } => {
                let _ = reply.send(engine.start_child_task(start));
            }
            Command::BeginChildToolExchange { pending, reply } => {
                let _ = reply.send(engine.begin_child_tool_exchange(pending));
            }
            Command::MarkChildToolExecutionStarted { start, reply } => {
                let _ = reply.send(engine.mark_child_tool_execution_started(start));
            }
            Command::CompleteChildToolExchange { completed, reply } => {
                let _ = reply.send(engine.complete_child_tool_exchange(completed));
            }
            Command::SettleChildTask { settlement, reply } => {
                let _ = reply.send(engine.settle_child_task(settlement));
            }
            Command::RequestChildTaskCancellation {
                session_id,
                child_task_id,
                reply,
            } => {
                let _ =
                    reply.send(engine.request_child_task_cancellation(&session_id, &child_task_id));
            }
            Command::LoadChildConversation {
                session_id,
                child_task_id,
                reply,
            } => {
                let _ = reply.send(engine.load_child_conversation(&session_id, &child_task_id));
            }
            Command::ReplaceContext { replacement, reply } => {
                let _ = reply.send(engine.replace_context(replacement));
            }
            Command::AcceptInput { input, reply } => {
                let _ = reply.send(engine.accept_input(*input));
            }
            Command::CancelQueuedInput {
                session_id,
                input_id,
                reply,
            } => {
                let _ = reply.send(engine.cancel_queued_input(&session_id, &input_id));
            }
            Command::PrioritizeQueuedInput { change, reply } => {
                let _ = reply.send(engine.prioritize_queued_input(change));
            }
            Command::CreateRunAttempt { attempt, reply } => {
                let _ = reply.send(engine.create_run_attempt(attempt));
            }
            Command::CommitUserMessage { commit, reply } => {
                let _ = reply.send(engine.commit_user_message(commit));
            }
            Command::BeginToolExchange { pending, reply } => {
                let _ = reply.send(engine.begin_tool_exchange(pending));
            }
            Command::MarkToolExecutionStarted { start, reply } => {
                let _ = reply.send(engine.mark_tool_execution_started(start));
            }
            Command::CompleteToolExchange { completed, reply } => {
                let _ = reply.send(engine.complete_tool_exchange(completed));
            }
            Command::SettleRun { settlement, reply } => {
                let _ = reply.send(engine.settle_run(*settlement));
            }
            Command::StopGoal { stop, reply } => {
                let _ = reply.send(engine.stop_goal(stop));
            }
            Command::ClearGoal { clear, reply } => {
                let _ = reply.send(engine.clear_goal(clear));
            }
            Command::ResumeGoalWithHeldInput { resume, reply } => {
                let _ = reply.send(engine.resume_goal_with_held_input(resume));
            }
            Command::LoadConversation { session_id, reply } => {
                let _ = reply.send(engine.load_conversation(&session_id));
            }
            Command::GetSessionUsage { session_id, reply } => {
                let _ = reply.send(engine.get_session_usage(&session_id));
            }
            Command::LoadConversationWindow { request, reply } => {
                let _ = reply.send(engine.load_conversation_window(request));
            }
            Command::LoadConversationRawWindow { request, reply } => {
                let _ = reply.send(engine.load_conversation_raw_window(request));
            }
            Command::LocateConversationMessage { request, reply } => {
                let _ = reply.send(engine.locate_conversation_message(request));
            }
            Command::SearchConversations { request, reply } => {
                let _ = reply.send(engine.search_conversations(request));
            }
            Command::SetSessionArchive { change, reply } => {
                let _ = reply.send(engine.set_session_archive(change));
            }
            Command::SetSessionProxy { change, reply } => {
                let _ = reply.send(engine.set_session_proxy(change));
            }
            Command::RenameSession { change, reply } => {
                let _ = reply.send(engine.rename_session(change));
            }
            Command::SetSessionPinned { change, reply } => {
                let _ = reply.send(engine.set_session_pinned(change));
            }
            Command::SetMessageFeedback { change, reply } => {
                let _ = reply.send(engine.set_message_feedback(change));
            }
            Command::LoadMessageFeedback { session_id, reply } => {
                let _ = reply.send(engine.load_message_feedback(&session_id));
            }
            Command::SetSessionModel { change, reply } => {
                let _ = reply.send(engine.set_session_model(change));
            }
            Command::SetSessionReasoningEffort { change, reply } => {
                let _ = reply.send(engine.set_session_reasoning_effort(change));
            }
            Command::SetSessionVariant { change, reply } => {
                let _ = reply.send(engine.set_session_variant(change));
            }
            Command::SetSessionApprovalMode { change, reply } => {
                let _ = reply.send(engine.set_session_approval_mode(change));
            }
            Command::RewriteFromUser { rewrite, reply } => {
                let _ = reply.send(engine.rewrite_from_user(*rewrite));
            }
            Command::LoadPermissionFile { scope, reply } => {
                let _ = reply.send(engine.load_permission_file(&scope));
            }
            Command::ReplacePermissionFile {
                scope,
                expected_revision,
                content,
                reply,
            } => {
                let _ = reply.send(engine.replace_permission_file(
                    &scope,
                    &expected_revision,
                    &content,
                ));
            }
            Command::Shutdown { reply } => {
                let _ = reply.send(Ok(()));
                break;
            }
        }
    }
}

fn worker_unavailable() -> StoreError {
    StoreError::new(
        StoreErrorKind::Unavailable,
        "runtime storage worker is unavailable",
    )
}
