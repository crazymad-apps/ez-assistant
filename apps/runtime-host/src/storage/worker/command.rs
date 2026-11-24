//! 异步存储客户端与阻塞存储线程之间的类型化命令。

use assistant_protocol::{ChildTaskId, InputId, RunId, SessionId};
use assistant_runtime::{
    AcceptedInput, AcceptedStoredSessionCommand, ApprovalModeChange, ArchiveChange, ChildTaskStart,
    ChildToolExecutionStart, CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementResult, ControllerPromptMutation, ControllerPromptSnapshot,
    ConversationMessageLocationRequest, ConversationNavigationRequest,
    ConversationRawWindowRequest, ConversationRewrite, ConversationSearchPage,
    ConversationSearchRequest, ConversationWindowRequest, DeviceNameChange, DeviceRevocation,
    DeviceRevocationResult, GoalClear, GoalHeldInputResume, GoalHeldInputResumeResult, GoalStop,
    GoalStopResult, MemoryContextSnapshot, MessageFeedbackChange, ModelChange, NewAttachmentUpload,
    NewPairedDevice, NewStoredChildTask, NewStoredInput, NewStoredRunAttempt, NewStoredSession,
    NewStoredSessionCommand, NewStoredSessionMaterialization, NewWorkspaceRegistration,
    PairedDevice, PcOutputHostingChange, PendingChildToolExchange, PendingToolExchange,
    PermissionFileLoad, PermissionFileRevision, PermissionFileScope, PersonaMutation,
    PersonaSnapshot, PinnedMemoryMutation, PinnedMemoryMutationResult, QueuePriorityChange,
    ReasoningEffortChange, RecoveredRuntime, RewriteResult, SessionCommandCommit, SessionDeletion,
    SessionFork, SessionHistoryClear, SessionHistoryClearResult, SessionHistoryCompactionFinish,
    SessionHistoryCompactionPreparation, SessionHistoryCompactionPreparationResult,
    SessionPinnedChange, SessionProxyChange, SessionTitleChange, SessionTitleGenerationCommit,
    SessionTitleGenerationCommitResult, SkillNameState, SkillNameStateChange, StoreError,
    StoredAttachment, StoredChildTask, StoredChildTaskSettlement,
    StoredConversationMessageLocation, StoredConversationNavigation, StoredConversationRawWindow,
    StoredConversationWindow, StoredMessageFeedback, StoredPermissionGrant, StoredPinnedMemory,
    StoredRun, StoredRunContinuation, StoredRunContinuationResult, StoredRunSettlement,
    StoredRunSettlementResult, StoredSession, StoredSessionCommand, StoredSessionFork,
    StoredSessionMaterialization, StoredSessionUsage, StoredTerminalRunInputReconciliation,
    StoredWorkPlan, StoredWorkspace, ToolExecutionStart, UserMessageCommit, VariantChange,
    WorkPlanClear, WorkPlanMutation, WorkPlanMutationResult, WorkspaceRemoval,
    WorkspaceRestoration, WorkspaceUpdate,
};
use tokio::sync::oneshot;

/// 异步 RuntimeStore 到单一阻塞 StorageEngine 的类型化调用集合。
///
/// 每个变体只携带业务参数和一次性回复通道；命令按 mpsc 接收顺序在同一线程执行，
/// 因此不得在调用方假设并行数据库事务或另存一份存储状态。
pub(super) enum Command {
    NextScheduleMaintenance {
        after: Option<String>,
        reply: oneshot::Sender<
            Result<Option<(String, assistant_runtime::ScheduleMaintenance)>, StoreError>,
        >,
    },
    MarkScheduleDeleting {
        id: assistant_protocol::ScheduleId,
        reply: oneshot::Sender<Result<bool, StoreError>>,
    },
    ArchiveScheduleResult {
        id: assistant_protocol::ScheduleExecutionId,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    CommitScheduleReport {
        commit: assistant_runtime::ScheduleReportCommit,
        reply: oneshot::Sender<Result<Option<assistant_runtime::AcceptedInput>, StoreError>>,
    },
    FinishScheduleResult {
        id: assistant_protocol::ScheduleExecutionId,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    ResolveScheduleFile {
        id: assistant_protocol::ScheduleExecutionId,
        file_id: String,
        reply: oneshot::Sender<Result<Option<assistant_runtime::ResolvedScheduleFile>, StoreError>>,
    },
    NextScheduleDue {
        reply: oneshot::Sender<Result<Option<assistant_runtime::StoredSchedule>, StoreError>>,
    },
    AdmitSchedule {
        command: assistant_runtime::ScheduleAdmission,
        reply: oneshot::Sender<Result<assistant_runtime::ScheduleAdmitted, StoreError>>,
    },
    SettleScheduleExecution {
        settlement: assistant_runtime::ScheduleSettlement,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    RecoverScheduleExecutions {
        now: i64,
        cleanup_verified: bool,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    GetScheduleExecution {
        id: assistant_protocol::ScheduleExecutionId,
        reply: oneshot::Sender<Result<Option<assistant_protocol::ScheduleExecution>, StoreError>>,
    },
    ListScheduleExecutions {
        id: assistant_protocol::ScheduleId,
        offset: u32,
        limit: u32,
        reply: oneshot::Sender<
            Result<Option<assistant_protocol::ListScheduleExecutionsResult>, StoreError>,
        >,
    },
    ListSchedules {
        offset: u32,
        limit: u32,
        reply: oneshot::Sender<Result<assistant_runtime::StoredSchedulePage, StoreError>>,
    },
    GetSchedule {
        id: assistant_protocol::ScheduleId,
        reply: oneshot::Sender<Result<Option<assistant_runtime::StoredSchedule>, StoreError>>,
    },
    InsertSchedule {
        schedule: assistant_runtime::StoredSchedule,
        reply: oneshot::Sender<Result<bool, StoreError>>,
    },
    ReplaceSchedule {
        schedule: assistant_runtime::StoredSchedule,
        reply: oneshot::Sender<Result<bool, StoreError>>,
    },
    DeleteSchedule {
        id: assistant_protocol::ScheduleId,
        reply: oneshot::Sender<Result<bool, StoreError>>,
    },
    PublishMessageCard {
        card: assistant_runtime::NewMessageCard,
        reply: oneshot::Sender<Result<assistant_runtime::StoredMessageCard, StoreError>>,
    },
    ListMessageCards {
        request: assistant_protocol::ListMessageCardsRequest,
        reply: oneshot::Sender<Result<assistant_protocol::ListMessageCardsResult, StoreError>>,
    },
    GetMessageCard {
        owner: assistant_protocol::ConversationOwner,
        card_id: String,
        reply: oneshot::Sender<Result<Option<assistant_runtime::StoredMessageCard>, StoreError>>,
    },
    SaveMessageCardSnapshot {
        write: assistant_runtime::MessageCardSnapshotWrite,
        reply:
            oneshot::Sender<Result<assistant_protocol::SaveMessageCardSnapshotResult, StoreError>>,
    },
    LoadConversationHistory {
        owner: assistant_protocol::ConversationOwner,
        reply: oneshot::Sender<Result<assistant_runtime::StoredConversationHistory, StoreError>>,
    },
    LoadFeedback {
        session_id: SessionId,
        reply:
            oneshot::Sender<Result<Vec<assistant_protocol::FeedbackRequestSnapshot>, StoreError>>,
    },
    MutateFeedback {
        mutation: assistant_runtime::FeedbackMutation,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    ListCustomAgents {
        reply: oneshot::Sender<Result<Vec<assistant_runtime::StoredCustomAgent>, StoreError>>,
    },
    MutateCustomAgent {
        mutation: assistant_runtime::CustomAgentMutation,
        reply: oneshot::Sender<Result<Option<assistant_runtime::StoredCustomAgent>, StoreError>>,
    },
    BindPluginOrganization {
        key: crate::access::enterprise::UserKey,
        legacy_home: bool,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    ListPlugins {
        reply: oneshot::Sender<Result<Vec<assistant_runtime::StoredPlugin>, StoreError>>,
    },
    ChangePluginSettings {
        change: assistant_runtime::PluginSettingsChange,
        reply: oneshot::Sender<Result<assistant_runtime::StoredPlugin, StoreError>>,
    },
    ListPluginOperations {
        reply: oneshot::Sender<Result<Vec<assistant_runtime::StoredPluginOperation>, StoreError>>,
    },
    AcceptPluginOperation {
        operation: assistant_runtime::StoredPluginOperation,
        reply: oneshot::Sender<Result<assistant_runtime::StoredPluginOperation, StoreError>>,
    },
    AdvancePluginOperation {
        update: assistant_runtime::PluginOperationUpdate,
        reply: oneshot::Sender<Result<assistant_runtime::StoredPluginOperation, StoreError>>,
    },
    LoadDefaultAgentShell {
        reply: oneshot::Sender<Result<Option<assistant_protocol::ShellKind>, StoreError>>,
    },
    SaveDefaultAgentShell {
        kind: assistant_protocol::ShellKind,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    LoadProviders {
        reply: oneshot::Sender<Result<Vec<assistant_runtime::StoredProvider>, StoreError>>,
    },
    PutProvider {
        provider: assistant_runtime::StoredProvider,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    ReplaceProviderModelCatalog {
        replacement: assistant_runtime::ProviderModelCatalogReplacement,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    RemoveProvider {
        id: assistant_protocol::ProviderInstanceId,
        reply: oneshot::Sender<Result<assistant_protocol::ProviderUsage, StoreError>>,
    },
    ProviderUsage {
        id: assistant_protocol::ProviderInstanceId,
        reply: oneshot::Sender<Result<assistant_protocol::ProviderUsage, StoreError>>,
    },
    LoadModelSettings {
        reply: oneshot::Sender<Result<assistant_protocol::ModelSettings, StoreError>>,
    },
    SaveModelSettings {
        settings: assistant_protocol::ModelSettings,
        server_history_key: Option<String>,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    InvalidateServerHistoryBindings {
        key: String,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    InvalidateControllerServerHistoryBindings {
        key: String,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    GetModelFixedConfig {
        selection: assistant_protocol::ModelSelection,
        reply: oneshot::Sender<Result<Option<assistant_protocol::ModelFixedConfig>, StoreError>>,
    },
    ListModelFixedConfigs {
        id: assistant_protocol::ProviderInstanceId,
        offset: u32,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<assistant_protocol::ModelFixedConfig>, StoreError>>,
    },
    PutModelFixedConfig {
        config: assistant_protocol::ModelFixedConfig,
        server_history_key: Option<assistant_runtime::FixedModelHistoryInvalidation>,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    ResetModelFixedConfig {
        selection: assistant_protocol::ModelSelection,
        server_history_key: Option<assistant_runtime::FixedModelHistoryInvalidation>,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },

    #[cfg(test)]
    PanicForTest {
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    SearchConversationTitles {
        request: ConversationSearchRequest,
        reply: oneshot::Sender<
            Result<ConversationSearchPage<assistant_protocol::ConversationHistoryHit>, StoreError>,
        >,
    },
    LoadRuntimeGlobals {
        reply: oneshot::Sender<Result<RecoveredRuntime, StoreError>>,
    },
    PrepareSessionExecution {
        session_id: SessionId,
        reply: oneshot::Sender<Result<assistant_runtime::LoadedSession, StoreError>>,
    },
    LoadSessionEnvironment {
        session_id: SessionId,
        reply: oneshot::Sender<Result<assistant_runtime::SessionExecutionEnvironment, StoreError>>,
    },
    LoadSessionState {
        session_id: SessionId,
        reply: oneshot::Sender<Result<assistant_runtime::LoadedSession, StoreError>>,
    },
    MoveSidebarItem {
        request: assistant_protocol::MoveSidebarItemRequest,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    ResetSidebarOrder {
        request: assistant_protocol::ResetSidebarOrderRequest,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    QueryWorkspaces {
        request: assistant_protocol::ListWorkspacesRequest,
        reply: oneshot::Sender<Result<assistant_runtime::WorkspacePage, StoreError>>,
    },
    QuerySidebar {
        request: assistant_protocol::GetApplicationSnapshotRequest,
        reply: oneshot::Sender<Result<assistant_runtime::SidebarSnapshot, StoreError>>,
    },
    QuerySessionSummaries {
        query: assistant_runtime::SessionSummaryQuery,
        reply: oneshot::Sender<Result<Vec<assistant_protocol::SessionSummary>, StoreError>>,
    },
    QueuedInputIds {
        reply: oneshot::Sender<Result<Vec<InputId>, StoreError>>,
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
    GetControllerPrompt {
        reply: oneshot::Sender<Result<ControllerPromptSnapshot, StoreError>>,
    },
    SetControllerPrompt {
        mutation: ControllerPromptMutation,
        reply: oneshot::Sender<Result<ControllerPromptSnapshot, StoreError>>,
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
    UpdateWorkspace {
        update: WorkspaceUpdate,
        reply: oneshot::Sender<Result<StoredWorkspace, StoreError>>,
    },
    RemoveWorkspace {
        removal: WorkspaceRemoval,
        reply: oneshot::Sender<Result<StoredWorkspace, StoreError>>,
    },
    RestoreWorkspace {
        restoration: WorkspaceRestoration,
        reply: oneshot::Sender<Result<StoredWorkspace, StoreError>>,
    },
    UploadAttachment {
        upload: NewAttachmentUpload,
        reply: oneshot::Sender<Result<StoredAttachment, StoreError>>,
    },
    CreateSession {
        session: Box<NewStoredSession>,
        reply: oneshot::Sender<Result<StoredSession, StoreError>>,
    },
    MaterializeSession {
        materialization: Box<NewStoredSessionMaterialization>,
        reply: oneshot::Sender<Result<StoredSessionMaterialization, StoreError>>,
    },
    GetSessionMaterialization {
        key: assistant_protocol::IdempotencyKey,
        reply: oneshot::Sender<Result<Option<StoredSessionMaterialization>, StoreError>>,
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
    ClaimInput {
        run: StoredRun,
        goal_resume: Option<Box<GoalHeldInputResume>>,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    CheckConversationIdentity {
        session_id: SessionId,
        identity: assistant_runtime::NewConversationIdentity,
        reply: oneshot::Sender<Result<bool, StoreError>>,
    },
    LoadRunConversation {
        session_id: SessionId,
        run_id: RunId,
        reply: oneshot::Sender<Result<assistant_runtime::StoredConversationRawWindow, StoreError>>,
    },
    FindRequestAcceptance {
        session_id: SessionId,
        key: assistant_protocol::IdempotencyKey,
        reply:
            oneshot::Sender<Result<Option<assistant_runtime::StoredRequestAcceptance>, StoreError>>,
    },
    AcceptInput {
        input: Box<NewStoredInput>,
        reply: oneshot::Sender<Result<AcceptedInput, StoreError>>,
    },
    AcceptSessionControl {
        command: Box<NewStoredSessionCommand>,
        reply: oneshot::Sender<Result<AcceptedStoredSessionCommand, StoreError>>,
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
    CommitSessionControl {
        commit: Box<SessionCommandCommit>,
        reply: oneshot::Sender<Result<StoredSessionCommand, StoreError>>,
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
    ReconcileTerminalRunInput {
        reconciliation: Box<StoredTerminalRunInputReconciliation>,
        reply: oneshot::Sender<Result<(), StoreError>>,
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
    LoadExecutionWindow {
        session_id: SessionId,
        reply: oneshot::Sender<Result<assistant_runtime::ExecutionWindow, StoreError>>,
    },
    LoadConversation {
        session_id: SessionId,
        reply: oneshot::Sender<Result<agent_types::ConversationSnapshot, StoreError>>,
    },
    GetConversationUsage {
        owner: assistant_protocol::ConversationOwner,
        reply: oneshot::Sender<Result<assistant_runtime::StoredConversationUsage, StoreError>>,
    },
    GetSessionUsage {
        session_id: SessionId,
        reply: oneshot::Sender<Result<StoredSessionUsage, StoreError>>,
    },
    LoadConversationWindow {
        request: ConversationWindowRequest,
        reply: oneshot::Sender<Result<StoredConversationWindow, StoreError>>,
    },
    ReadConversationNavigation {
        request: ConversationNavigationRequest,
        reply: oneshot::Sender<Result<StoredConversationNavigation, StoreError>>,
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
    DisableAutomaticTitle {
        session_id: SessionId,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    CommitSessionTitleGeneration {
        commit: SessionTitleGenerationCommit,
        reply: oneshot::Sender<Result<SessionTitleGenerationCommitResult, StoreError>>,
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
    ListPermissionGrants {
        include_exhausted: bool,
        reply: oneshot::Sender<Result<Vec<StoredPermissionGrant>, StoreError>>,
    },
    CreatePermissionGrant {
        grant: StoredPermissionGrant,
        reply: oneshot::Sender<Result<StoredPermissionGrant, StoreError>>,
    },
    ConsumePermissionGrant {
        grant_id: String,
        now_ms: i64,
        reply: oneshot::Sender<Result<Option<StoredPermissionGrant>, StoreError>>,
    },
    RevokePermissionGrant {
        grant_id: String,
        reply: oneshot::Sender<Result<bool, StoreError>>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
}
