//! RuntimeStore 的异步队列客户端与 worker 生命周期所有权。

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

use assistant_protocol::{ChildTaskId, InputId, SessionId};
use assistant_runtime::{
    AcceptedInput, AcceptedStoredSessionCommand, ApprovalModeChange, ArchiveChange, ChildTaskStart,
    ChildToolExecutionStart, CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementResult, ConversationMessageLocationRequest, ConversationRawWindowRequest,
    ConversationRewrite, ConversationSearchPage, ConversationSearchRequest,
    ConversationWindowRequest, DeviceNameChange, DeviceRevocation, DeviceRevocationResult,
    GoalClear, GoalHeldInputResume, GoalHeldInputResumeResult, GoalStop, GoalStopResult,
    MemoryContextSnapshot, MessageFeedbackChange, ModelChange, NewAttachmentUpload,
    NewPairedDevice, NewStoredChildTask, NewStoredInput, NewStoredRunAttempt, NewStoredSession,
    NewStoredSessionCommand, NewStoredSessionMaterialization, NewWorkspaceRegistration,
    PairedDevice, PcOutputHostingChange, PendingChildToolExchange, PendingToolExchange,
    PermissionFileLoad, PermissionFileRevision, PermissionFileScope, PermissionFileStore,
    PermissionStoreFuture, PersonaMutation, PersonaSnapshot, PinnedMemoryMutation,
    PinnedMemoryMutationResult, QueuePriorityChange, ReasoningEffortChange, RecoveredRuntime,
    RewriteResult, RuntimeStore, SessionCommandCommit, SessionDeletion, SessionFork,
    SessionHistoryClear, SessionHistoryClearResult, SessionHistoryCompactionFinish,
    SessionHistoryCompactionPreparation, SessionHistoryCompactionPreparationResult,
    SessionPinnedChange, SessionProxyChange, SessionTitleChange, SessionTitleGenerationCommit,
    SessionTitleGenerationCommitResult, SkillNameState, SkillNameStateChange, StoreError,
    StoreErrorKind, StoreFuture, StoredAttachment, StoredChildTask, StoredChildTaskSettlement,
    StoredConversationMessageLocation, StoredConversationRawWindow, StoredConversationWindow,
    StoredMessageFeedback, StoredPinnedMemory, StoredRun, StoredRunContinuation,
    StoredRunContinuationResult, StoredRunSettlement, StoredRunSettlementResult, StoredSession,
    StoredSessionCommand, StoredSessionFork, StoredSessionMaterialization, StoredSessionUsage,
    StoredWorkPlan, StoredWorkspace, ToolExecutionStart, UserMessageCommit, VariantChange,
    WorkPlanClear, WorkPlanMutation, WorkPlanMutationResult, WorkspaceRemoval, WorkspaceUpdate,
};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use super::{
    command::Command,
    thread::{run_worker, worker_unavailable},
};

const WORKER_NAME: &str = "runtime-storage";

impl PermissionFileStore for LocalRuntimeStore {
    fn load_permission_file(
        &self,
        scope: &PermissionFileScope,
    ) -> PermissionStoreFuture<'_, PermissionFileLoad> {
        let scope = scope.clone();
        Box::pin(async move {
            self.request(|reply| Command::LoadPermissionFile { scope, reply })
                .await
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
            self.request(|reply| Command::ReplacePermissionFile {
                scope,
                expected_revision,
                content,
                reply,
            })
            .await
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

    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, StoreError>>) -> Command,
    ) -> Result<T, StoreError> {
        let (reply, result) = oneshot::channel();
        self.enqueue(build(reply)).await?;
        result.await.map_err(|_| worker_unavailable())?
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
    pub(in crate::storage) async fn panic_worker_for_test(&self) -> Result<(), StoreError> {
        let (reply, result) = oneshot::channel();
        self.enqueue(Command::PanicForTest { reply }).await?;
        result.await.map_err(|_| worker_unavailable())?
    }
}

impl RuntimeStore for LocalRuntimeStore {
    fn load_runtime(&self) -> StoreFuture<'_, RecoveredRuntime> {
        Box::pin(async move { self.request(|reply| Command::LoadRuntime { reply }).await })
    }

    fn register_paired_device(&self, device: NewPairedDevice) -> StoreFuture<'_, PairedDevice> {
        Box::pin(async move {
            self.request(|reply| Command::RegisterPairedDevice { device, reply })
                .await
        })
    }

    fn rename_device(&self, change: DeviceNameChange) -> StoreFuture<'_, PairedDevice> {
        Box::pin(async move {
            self.request(|reply| Command::RenameDevice { change, reply })
                .await
        })
    }

    fn revoke_device(&self, change: DeviceRevocation) -> StoreFuture<'_, DeviceRevocationResult> {
        Box::pin(async move {
            self.request(|reply| Command::RevokeDevice { change, reply })
                .await
        })
    }

    fn set_pc_output_hosting(&self, change: PcOutputHostingChange) -> StoreFuture<'_, bool> {
        Box::pin(async move {
            self.request(|reply| Command::SetPcOutputHosting { change, reply })
                .await
        })
    }

    fn load_memory_context(&self) -> StoreFuture<'_, MemoryContextSnapshot> {
        Box::pin(async move {
            self.request(|reply| Command::LoadMemoryContext { reply })
                .await
        })
    }

    fn list_skill_name_states(&self) -> StoreFuture<'_, Vec<SkillNameState>> {
        Box::pin(async move {
            self.request(|reply| Command::ListSkillNameStates { reply })
                .await
        })
    }

    fn set_skill_enabled(&self, change: SkillNameStateChange) -> StoreFuture<'_, SkillNameState> {
        Box::pin(async move {
            self.request(|reply| Command::SetSkillEnabled { change, reply })
                .await
        })
    }

    fn load_work_plan(&self, session_id: &SessionId) -> StoreFuture<'_, Option<StoredWorkPlan>> {
        let session_id = session_id.clone();
        Box::pin(async move {
            self.request(|reply| Command::LoadWorkPlan { session_id, reply })
                .await
        })
    }

    fn mutate_work_plan(
        &self,
        mutation: WorkPlanMutation,
    ) -> StoreFuture<'_, WorkPlanMutationResult> {
        Box::pin(async move {
            self.request(|reply| Command::MutateWorkPlan { mutation, reply })
                .await
        })
    }

    fn clear_work_plan(&self, clear: WorkPlanClear) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::ClearWorkPlan { clear, reply })
                .await
        })
    }

    fn get_persona(&self) -> StoreFuture<'_, PersonaSnapshot> {
        Box::pin(async move { self.request(|reply| Command::GetPersona { reply }).await })
    }

    fn set_persona(&self, mutation: PersonaMutation) -> StoreFuture<'_, PersonaSnapshot> {
        Box::pin(async move {
            self.request(|reply| Command::SetPersona { mutation, reply })
                .await
        })
    }

    fn list_pinned_memories(&self) -> StoreFuture<'_, Vec<StoredPinnedMemory>> {
        Box::pin(async move {
            self.request(|reply| Command::ListPinnedMemories { reply })
                .await
        })
    }

    fn mutate_pinned_memory(
        &self,
        mutation: PinnedMemoryMutation,
    ) -> StoreFuture<'_, PinnedMemoryMutationResult> {
        Box::pin(async move {
            self.request(|reply| Command::MutatePinnedMemory { mutation, reply })
                .await
        })
    }

    fn register_workspace(
        &self,
        registration: NewWorkspaceRegistration,
    ) -> StoreFuture<'_, StoredWorkspace> {
        Box::pin(async move {
            self.request(|reply| Command::RegisterWorkspace {
                registration,
                reply,
            })
            .await
        })
    }

    fn update_workspace(&self, update: WorkspaceUpdate) -> StoreFuture<'_, StoredWorkspace> {
        Box::pin(async move {
            self.request(|reply| Command::UpdateWorkspace { update, reply })
                .await
        })
    }

    fn remove_workspace(&self, removal: WorkspaceRemoval) -> StoreFuture<'_, StoredWorkspace> {
        Box::pin(async move {
            self.request(|reply| Command::RemoveWorkspace { removal, reply })
                .await
        })
    }

    fn upload_attachment(&self, upload: NewAttachmentUpload) -> StoreFuture<'_, StoredAttachment> {
        Box::pin(async move {
            self.request(|reply| Command::UploadAttachment { upload, reply })
                .await
        })
    }

    fn create_session(&self, session: NewStoredSession) -> StoreFuture<'_, StoredSession> {
        Box::pin(async move {
            self.request(|reply| Command::CreateSession { session, reply })
                .await
        })
    }

    fn materialize_session(
        &self,
        materialization: NewStoredSessionMaterialization,
    ) -> StoreFuture<'_, StoredSessionMaterialization> {
        Box::pin(async move {
            self.request(|reply| Command::MaterializeSession {
                materialization: Box::new(materialization),
                reply,
            })
            .await
        })
    }

    fn fork_session(&self, fork: SessionFork) -> StoreFuture<'_, StoredSessionFork> {
        Box::pin(async move {
            self.request(|reply| Command::ForkSession {
                fork: Box::new(fork),
                reply,
            })
            .await
        })
    }

    fn inspect_session_deletion(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, assistant_protocol::DeleteSessionImpact> {
        let session_id = session_id.clone();
        Box::pin(async move {
            self.request(|reply| Command::InspectSessionDeletion { session_id, reply })
                .await
        })
    }

    fn delete_session(&self, deletion: SessionDeletion) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::DeleteSession { deletion, reply })
                .await
        })
    }

    fn clear_session_history(
        &self,
        clear: SessionHistoryClear,
    ) -> StoreFuture<'_, SessionHistoryClearResult> {
        Box::pin(async move {
            self.request(|reply| Command::ClearSessionHistory {
                clear: Box::new(clear),
                reply,
            })
            .await
        })
    }

    fn prepare_session_compaction(
        &self,
        preparation: SessionHistoryCompactionPreparation,
    ) -> StoreFuture<'_, SessionHistoryCompactionPreparationResult> {
        Box::pin(async move {
            self.request(|reply| Command::PrepareSessionCompaction { preparation, reply })
                .await
        })
    }

    fn finish_session_compaction(
        &self,
        finish: SessionHistoryCompactionFinish,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::FinishSessionCompaction { finish, reply })
                .await
        })
    }

    fn create_child_task(&self, task: NewStoredChildTask) -> StoreFuture<'_, StoredChildTask> {
        Box::pin(async move {
            self.request(|reply| Command::CreateChildTask { task, reply })
                .await
        })
    }

    fn start_child_task(&self, start: ChildTaskStart) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::StartChildTask { start, reply })
                .await
        })
    }

    fn begin_child_tool_exchange(&self, pending: PendingChildToolExchange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::BeginChildToolExchange { pending, reply })
                .await
        })
    }

    fn mark_child_tool_execution_started(
        &self,
        start: ChildToolExecutionStart,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::MarkChildToolExecutionStarted { start, reply })
                .await
        })
    }

    fn complete_child_tool_exchange(
        &self,
        completed: CompletedChildToolExchange,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::CompleteChildToolExchange { completed, reply })
                .await
        })
    }

    fn settle_child_task(&self, settlement: StoredChildTaskSettlement) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::SettleChildTask { settlement, reply })
                .await
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
            self.request(|reply| Command::RequestChildTaskCancellation {
                session_id,
                child_task_id,
                reply,
            })
            .await
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
            self.request(|reply| Command::LoadChildConversation {
                session_id,
                child_task_id,
                reply,
            })
            .await
        })
    }

    fn replace_context(
        &self,
        replacement: ContextReplacement,
    ) -> StoreFuture<'_, ContextReplacementResult> {
        Box::pin(async move {
            self.request(|reply| Command::ReplaceContext { replacement, reply })
                .await
        })
    }

    fn accept_input(&self, input: NewStoredInput) -> StoreFuture<'_, AcceptedInput> {
        Box::pin(async move {
            self.request(|reply| Command::AcceptInput {
                input: Box::new(input),
                reply,
            })
            .await
        })
    }

    fn accept_session_command(
        &self,
        command: NewStoredSessionCommand,
    ) -> StoreFuture<'_, AcceptedStoredSessionCommand> {
        Box::pin(async move {
            self.request(|reply| Command::AcceptSessionControl {
                command: Box::new(command),
                reply,
            })
            .await
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
            self.request(|reply| Command::CancelQueuedInput {
                session_id,
                input_id,
                reply,
            })
            .await
        })
    }

    fn prioritize_queued_input(&self, change: QueuePriorityChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::PrioritizeQueuedInput { change, reply })
                .await
        })
    }

    fn create_run_attempt(&self, attempt: NewStoredRunAttempt) -> StoreFuture<'_, StoredRun> {
        Box::pin(async move {
            self.request(|reply| Command::CreateRunAttempt { attempt, reply })
                .await
        })
    }

    fn commit_user_message(&self, commit: UserMessageCommit) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::CommitUserMessage { commit, reply })
                .await
        })
    }

    fn commit_session_command(
        &self,
        commit: SessionCommandCommit,
    ) -> StoreFuture<'_, StoredSessionCommand> {
        Box::pin(async move {
            self.request(|reply| Command::CommitSessionControl {
                commit: Box::new(commit),
                reply,
            })
            .await
        })
    }

    fn commit_run_continuation(
        &self,
        continuation: StoredRunContinuation,
    ) -> StoreFuture<'_, StoredRunContinuationResult> {
        Box::pin(async move {
            self.request(|reply| Command::CommitRunContinuation {
                continuation: Box::new(continuation),
                reply,
            })
            .await
        })
    }

    fn settle_run(
        &self,
        settlement: StoredRunSettlement,
    ) -> StoreFuture<'_, StoredRunSettlementResult> {
        Box::pin(async move {
            self.request(|reply| Command::SettleRun {
                settlement: Box::new(settlement),
                reply,
            })
            .await
        })
    }

    fn stop_goal(&self, stop: GoalStop) -> StoreFuture<'_, GoalStopResult> {
        Box::pin(async move {
            self.request(|reply| Command::StopGoal { stop, reply })
                .await
        })
    }

    fn clear_goal(&self, clear: GoalClear) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::ClearGoal { clear, reply })
                .await
        })
    }

    fn resume_goal_with_held_input(
        &self,
        resume: GoalHeldInputResume,
    ) -> StoreFuture<'_, GoalHeldInputResumeResult> {
        Box::pin(async move {
            self.request(|reply| Command::ResumeGoalWithHeldInput { resume, reply })
                .await
        })
    }

    fn begin_tool_exchange(&self, pending: PendingToolExchange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::BeginToolExchange { pending, reply })
                .await
        })
    }

    fn mark_tool_execution_started(&self, start: ToolExecutionStart) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::MarkToolExecutionStarted { start, reply })
                .await
        })
    }

    fn complete_tool_exchange(&self, completed: CompletedToolExchange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::CompleteToolExchange { completed, reply })
                .await
        })
    }

    fn load_conversation(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, agent_types::ConversationSnapshot> {
        let session_id = session_id.clone();
        Box::pin(async move {
            self.request(|reply| Command::LoadConversation { session_id, reply })
                .await
        })
    }

    fn get_session_usage(&self, session_id: &SessionId) -> StoreFuture<'_, StoredSessionUsage> {
        let session_id = session_id.clone();
        Box::pin(async move {
            self.request(|reply| Command::GetSessionUsage { session_id, reply })
                .await
        })
    }

    fn load_conversation_window(
        &self,
        request: ConversationWindowRequest,
    ) -> StoreFuture<'_, StoredConversationWindow> {
        Box::pin(async move {
            self.request(|reply| Command::LoadConversationWindow { request, reply })
                .await
        })
    }

    fn load_conversation_raw_window(
        &self,
        request: ConversationRawWindowRequest,
    ) -> StoreFuture<'_, StoredConversationRawWindow> {
        Box::pin(async move {
            self.request(|reply| Command::LoadConversationRawWindow { request, reply })
                .await
        })
    }

    fn locate_conversation_message(
        &self,
        request: ConversationMessageLocationRequest,
    ) -> StoreFuture<'_, Option<StoredConversationMessageLocation>> {
        Box::pin(async move {
            self.request(|reply| Command::LocateConversationMessage { request, reply })
                .await
        })
    }

    fn search_conversations(
        &self,
        request: ConversationSearchRequest,
    ) -> StoreFuture<'_, ConversationSearchPage> {
        Box::pin(async move {
            self.request(|reply| Command::SearchConversations { request, reply })
                .await
        })
    }

    fn set_session_archive(&self, change: ArchiveChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::SetSessionArchive { change, reply })
                .await
        })
    }

    fn set_session_proxy(&self, change: SessionProxyChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::SetSessionProxy { change, reply })
                .await
        })
    }

    fn rename_session(&self, change: SessionTitleChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::RenameSession { change, reply })
                .await
        })
    }

    fn disable_automatic_title(&self, session_id: &SessionId) -> StoreFuture<'_, ()> {
        let session_id = session_id.clone();
        Box::pin(async move {
            self.request(|reply| Command::DisableAutomaticTitle { session_id, reply })
                .await
        })
    }

    fn commit_session_title_generation(
        &self,
        commit: SessionTitleGenerationCommit,
    ) -> StoreFuture<'_, SessionTitleGenerationCommitResult> {
        Box::pin(async move {
            self.request(|reply| Command::CommitSessionTitleGeneration { commit, reply })
                .await
        })
    }

    fn set_session_pinned(&self, change: SessionPinnedChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::SetSessionPinned { change, reply })
                .await
        })
    }

    fn set_message_feedback(&self, change: MessageFeedbackChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::SetMessageFeedback { change, reply })
                .await
        })
    }

    fn load_message_feedback(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, Vec<StoredMessageFeedback>> {
        let session_id = session_id.clone();
        Box::pin(async move {
            self.request(|reply| Command::LoadMessageFeedback { session_id, reply })
                .await
        })
    }

    fn set_session_model(&self, change: ModelChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::SetSessionModel { change, reply })
                .await
        })
    }

    fn set_session_reasoning_effort(&self, change: ReasoningEffortChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::SetSessionReasoningEffort { change, reply })
                .await
        })
    }

    fn set_session_variant(&self, change: VariantChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::SetSessionVariant { change, reply })
                .await
        })
    }

    fn set_session_approval_mode(&self, change: ApprovalModeChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.request(|reply| Command::SetSessionApprovalMode { change, reply })
                .await
        })
    }

    fn rewrite_from_user(&self, rewrite: ConversationRewrite) -> StoreFuture<'_, RewriteResult> {
        Box::pin(async move {
            self.request(|reply| Command::RewriteFromUser {
                rewrite: Box::new(rewrite),
                reply,
            })
            .await
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
