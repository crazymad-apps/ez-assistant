//! 异步存储客户端与阻塞存储线程之间的类型化命令。

use assistant_protocol::{ChildTaskId, InputId, SessionId};
use assistant_runtime::{
    AcceptedInput, ApprovalModeChange, ArchiveChange, ChildTaskStart, ChildToolExecutionStart,
    CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementResult, ConversationMessageLocationRequest, ConversationRawWindowRequest,
    ConversationRewrite, ConversationSearchPage, ConversationSearchRequest,
    ConversationWindowRequest, DeviceNameChange, DeviceRevocation, DeviceRevocationResult,
    GoalClear, GoalHeldInputResume, GoalHeldInputResumeResult, GoalStop, GoalStopResult,
    MemoryContextSnapshot, MessageFeedbackChange, ModelChange, NewAttachmentUpload,
    NewPairedDevice, NewStoredChildTask, NewStoredInput, NewStoredRunAttempt, NewStoredSession,
    NewWorkspaceRegistration, PairedDevice, PcOutputHostingChange, PendingChildToolExchange,
    PendingToolExchange, PermissionFileLoad, PermissionFileRevision, PermissionFileScope,
    PersonaMutation, PersonaSnapshot, PinnedMemoryMutation, PinnedMemoryMutationResult,
    QueuePriorityChange, ReasoningEffortChange, RecoveredRuntime, RewriteResult, SessionDeletion,
    SessionFork, SessionHistoryClear, SessionHistoryClearResult, SessionHistoryCompactionFinish,
    SessionHistoryCompactionPreparation, SessionHistoryCompactionPreparationResult,
    SessionPinnedChange, SessionProxyChange, SessionTitleChange, SkillNameState,
    SkillNameStateChange, StoreError, StoredAttachment, StoredChildTask, StoredChildTaskSettlement,
    StoredConversationMessageLocation, StoredConversationRawWindow, StoredConversationWindow,
    StoredMessageFeedback, StoredPinnedMemory, StoredRun, StoredRunContinuation,
    StoredRunContinuationResult, StoredRunSettlement, StoredRunSettlementResult, StoredSession,
    StoredSessionFork, StoredSessionUsage, StoredWorkPlan, StoredWorkspace, ToolExecutionStart,
    UserMessageCommit, VariantChange, WorkPlanClear, WorkPlanMutation, WorkPlanMutationResult,
    WorkspaceRemoval,
};
use tokio::sync::oneshot;

/// 异步 RuntimeStore 到单一阻塞 StorageEngine 的类型化调用集合。
///
/// 每个变体只携带业务参数和一次性回复通道；命令按 mpsc 接收顺序在同一线程执行，
/// 因此不得在调用方假设并行数据库事务或另存一份存储状态。
pub(super) enum Command {
    #[cfg(test)]
    PanicForTest {
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    LoadRuntime {
        reply: oneshot::Sender<Result<RecoveredRuntime, StoreError>>,
    },
    RegisterPairedDevice {
        device: NewPairedDevice,
        reply: oneshot::Sender<Result<PairedDevice, StoreError>>,
    },
    RenameDevice {
        change: DeviceNameChange,
        reply: oneshot::Sender<Result<PairedDevice, StoreError>>,
    },
    RevokeDevice {
        change: DeviceRevocation,
        reply: oneshot::Sender<Result<DeviceRevocationResult, StoreError>>,
    },
    SetPcOutputHosting {
        change: PcOutputHostingChange,
        reply: oneshot::Sender<Result<bool, StoreError>>,
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
    CommitRunContinuation {
        continuation: Box<StoredRunContinuation>,
        reply: oneshot::Sender<Result<StoredRunContinuationResult, StoreError>>,
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
