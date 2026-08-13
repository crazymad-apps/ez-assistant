//! 有界命令队列与专用阻塞存储线程。

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

use assistant_protocol::{ChildTaskId, InputId, SessionId};
use assistant_runtime::{
    AcceptedInput, ApprovalModeChange, ArchiveChange, ChildTaskStart, ChildToolExecutionStart,
    CompletedChildToolExchange, CompletedToolExchange, ContextReplacement, ConversationRewrite,
    ModelChange, NewAttachmentUpload, NewStoredChildTask, NewStoredInput, NewStoredRunAttempt,
    NewStoredSession, NewWorkspaceRegistration, PendingChildToolExchange, PendingToolExchange,
    PermissionFileLoad, PermissionFileRevision, PermissionFileScope, PermissionFileStore,
    PermissionStoreFuture, RecoveredRuntime, RewriteResult, RuntimeStore, StoreError,
    StoreErrorKind, StoreFuture, StoredAttachment, StoredChildTask, StoredChildTaskSettlement,
    StoredRun, StoredRunSettlement, StoredSession, StoredWorkspace, ToolExecutionStart,
    UserMessageCommit, VariantChange, WorkspaceRemoval,
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
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    AcceptInput {
        input: NewStoredInput,
        reply: oneshot::Sender<Result<AcceptedInput, StoreError>>,
    },
    CancelQueuedInput {
        session_id: SessionId,
        input_id: InputId,
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
        settlement: StoredRunSettlement,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    LoadConversation {
        session_id: SessionId,
        reply: oneshot::Sender<Result<agent_types::ConversationSnapshot, StoreError>>,
    },
    SetSessionArchive {
        change: ArchiveChange,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    SetSessionModel {
        change: ModelChange,
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
        rewrite: ConversationRewrite,
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

    fn replace_context(&self, replacement: ContextReplacement) -> StoreFuture<'_, ()> {
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
            self.enqueue(Command::AcceptInput { input, reply }).await?;
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

    fn settle_run(&self, settlement: StoredRunSettlement) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SettleRun { settlement, reply })
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

    fn set_session_archive(&self, change: ArchiveChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.enqueue(Command::SetSessionArchive { change, reply })
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
            self.enqueue(Command::RewriteFromUser { rewrite, reply })
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
                let _ = reply.send(engine.accept_input(input));
            }
            Command::CancelQueuedInput {
                session_id,
                input_id,
                reply,
            } => {
                let _ = reply.send(engine.cancel_queued_input(&session_id, &input_id));
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
                let _ = reply.send(engine.settle_run(settlement));
            }
            Command::LoadConversation { session_id, reply } => {
                let _ = reply.send(engine.load_conversation(&session_id));
            }
            Command::SetSessionArchive { change, reply } => {
                let _ = reply.send(engine.set_session_archive(change));
            }
            Command::SetSessionModel { change, reply } => {
                let _ = reply.send(engine.set_session_model(change));
            }
            Command::SetSessionVariant { change, reply } => {
                let _ = reply.send(engine.set_session_variant(change));
            }
            Command::SetSessionApprovalMode { change, reply } => {
                let _ = reply.send(engine.set_session_approval_mode(change));
            }
            Command::RewriteFromUser { rewrite, reply } => {
                let _ = reply.send(engine.rewrite_from_user(rewrite));
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
