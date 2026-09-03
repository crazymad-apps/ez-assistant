use agent_types::ConversationSnapshot;
use assistant_protocol::{ChildTaskId, InputId, SessionId};

use crate::{
    DeviceNameChange, DeviceRevocation, DeviceRevocationResult, MemoryContextSnapshot,
    NewPairedDevice, PairedDevice, PcOutputHostingChange, PersonaMutation, PersonaSnapshot,
    PinnedMemoryMutation, PinnedMemoryMutationResult, SkillNameState, SkillNameStateChange,
    StoredPinnedMemory, StoredSkillActivation,
};

use super::{
    AcceptedInput, AcceptedStoredSessionCommand, ApprovalModeChange, ArchiveChange, ChildTaskStart,
    ChildToolExecutionStart, CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementResult, ConversationMessageLocationRequest, ConversationRawWindowRequest,
    ConversationRewrite, ConversationSearchPage, ConversationSearchRequest,
    ConversationWindowRequest, GoalClear, GoalHeldInputResume, GoalHeldInputResumeResult, GoalStop,
    GoalStopResult, MessageFeedbackChange, ModelChange, NewAttachmentUpload, NewStoredChildTask,
    NewStoredInput, NewStoredRunAttempt, NewStoredSession, NewStoredSessionCommand,
    NewStoredSessionMaterialization, NewWorkspaceRegistration, PendingChildToolExchange,
    PendingToolExchange, QueuePriorityChange, ReasoningEffortChange, RewriteResult,
    SessionCommandCommit, SessionDeletion, SessionFork, SessionHistoryClear,
    SessionHistoryClearResult, SessionHistoryCompactionFinish, SessionHistoryCompactionPreparation,
    SessionHistoryCompactionPreparationResult, SessionPinnedChange, SessionProxyChange,
    SessionTitleChange, SessionTitleGenerationCommit, SessionTitleGenerationCommitResult,
    StoreFuture, StoredAttachment, StoredChildTask, StoredChildTaskSettlement,
    StoredConversationMessageLocation, StoredConversationRawWindow, StoredConversationWindow,
    StoredGoal, StoredInput, StoredMcpSelection, StoredMessageFeedback, StoredRun,
    StoredRunContinuation, StoredRunContinuationResult, StoredRunSettlement,
    StoredRunSettlementResult, StoredSession, StoredSessionFork, StoredSessionMaterialization,
    StoredSessionUsage, StoredWorkPlan, StoredWorkspace, ToolExecutionStart, UserMessageCommit,
    VariantChange, WorkPlanClear, WorkPlanMutation, WorkPlanMutationResult, WorkspaceRemoval,
    WorkspaceUpdate,
};

/// Runtime 启动时一次性取得的结构化恢复结果。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveredRuntime {
    pub devices: Vec<PairedDevice>,
    pub workspaces: Vec<StoredWorkspace>,
    pub attachments: Vec<StoredAttachment>,
    pub sessions: Vec<StoredSession>,
    pub inputs: Vec<StoredInput>,
    pub session_commands: Vec<super::StoredSessionCommand>,
    pub mcp_input_selections: Vec<StoredMcpSelection>,
    pub runs: Vec<StoredRun>,
    pub child_tasks: Vec<StoredChildTask>,
    pub work_plans: Vec<StoredWorkPlan>,
    pub goals: Vec<StoredGoal>,
    pub skill_activations: Vec<StoredSkillActivation>,
}

/// Assistant Runtime 使用的持久化能力端口。
///
/// 该端口以业务原子操作表达 Runtime 对持久化的需求，禁止退化为通用 SQL 或键值接口。
pub trait RuntimeStore: Send + Sync {
    /// 恢复未完成提交并加载 Runtime 的结构化启动投影。
    fn load_runtime(&self) -> StoreFuture<'_, RecoveredRuntime>;

    fn register_paired_device(&self, device: NewPairedDevice) -> StoreFuture<'_, PairedDevice>;

    fn rename_device(&self, change: DeviceNameChange) -> StoreFuture<'_, PairedDevice>;

    fn revoke_device(&self, change: DeviceRevocation) -> StoreFuture<'_, DeviceRevocationResult>;

    fn set_pc_output_hosting(&self, change: PcOutputHostingChange) -> StoreFuture<'_, bool>;

    /// 为新 Session 一致读取当前 Persona 与 Pinned Memory。
    fn load_memory_context(&self) -> StoreFuture<'_, MemoryContextSnapshot>;

    /// 读取用户显式保存的全局 Skill 名称开关；缺少记录的名称默认启用。
    fn list_skill_name_states(&self) -> StoreFuture<'_, Vec<SkillNameState>>;

    /// 以通过格式校验的名称为唯一键原子写入一个全局 Skill 开关。
    fn set_skill_enabled(&self, change: SkillNameStateChange) -> StoreFuture<'_, SkillNameState>;

    /// 读取一个 Session 的当前 WorkPlan；不存在时返回 `None`。
    fn load_work_plan(&self, session_id: &SessionId) -> StoreFuture<'_, Option<StoredWorkPlan>>;

    /// 原子执行完整 WorkPlan 替换；全部 Todo 完成时同事务清除当前计划。
    /// 同 operation id 即使已自动清除，也返回首次成功结果。
    fn mutate_work_plan(
        &self,
        mutation: WorkPlanMutation,
    ) -> StoreFuture<'_, WorkPlanMutationResult>;

    /// 以当前修订号 CAS 清除 WorkPlan；空计划与 expected revision 0 幂等成功。
    fn clear_work_plan(&self, clear: WorkPlanClear) -> StoreFuture<'_, ()>;

    /// 读取单例 Persona 的当前权威投影。
    fn get_persona(&self) -> StoreFuture<'_, PersonaSnapshot>;

    /// 使用修订号 CAS 更新单例 Persona。
    fn set_persona(&self, mutation: PersonaMutation) -> StoreFuture<'_, PersonaSnapshot>;

    /// 读取当前全部 Pinned Memory；正式分页产品投影在后续里程碑提供。
    fn list_pinned_memories(&self) -> StoreFuture<'_, Vec<StoredPinnedMemory>>;

    /// 原子执行一条 Pinned Memory CAS 变更并递增集合修订。
    fn mutate_pinned_memory(
        &self,
        mutation: PinnedMemoryMutation,
    ) -> StoreFuture<'_, PinnedMemoryMutationResult>;

    /// 按 canonical path 幂等登记或恢复 Workspace。
    fn register_workspace(
        &self,
        registration: NewWorkspaceRegistration,
    ) -> StoreFuture<'_, StoredWorkspace>;

    /// 更新 Workspace 当前元数据，不修改既有 Session 冻结资源。
    fn update_workspace(&self, update: WorkspaceUpdate) -> StoreFuture<'_, StoredWorkspace>;

    /// 假删 Workspace，不删除任何目录或历史绑定。
    fn remove_workspace(&self, removal: WorkspaceRemoval) -> StoreFuture<'_, StoredWorkspace>;

    /// 完成已流式接收到 staging 的上传；同 Session、同 Blob Hash 返回首次结果。
    fn upload_attachment(&self, upload: NewAttachmentUpload) -> StoreFuture<'_, StoredAttachment>;

    /// 创建 Session 稳定事实及其空 Conversation。
    fn create_session(&self, session: NewStoredSession) -> StoreFuture<'_, StoredSession>;

    /// 原子物化新 Session、附件、首个 Input/Run 及其 Goal/Skill 伴随事实。
    fn materialize_session(
        &self,
        materialization: NewStoredSessionMaterialization,
    ) -> StoreFuture<'_, StoredSessionMaterialization>;

    /// 基于已校验的正文前缀原子创建独立 Session，并重写 Attachment 稳定视图。
    fn fork_session(&self, fork: SessionFork) -> StoreFuture<'_, StoredSessionFork>;

    /// 读取永久删除的当前精确影响；实现不得缓存这个结果。
    fn inspect_session_deletion(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, assistant_protocol::DeleteSessionImpact>;

    /// 在再次核对影响后永久删除 Session 私有事实。
    fn delete_session(&self, deletion: SessionDeletion) -> StoreFuture<'_, ()>;

    /// 原子切换到空 Conversation generation、刷新冻结上下文并删除旧历史事实。
    ///
    /// 权威切换后的物理清理失败不得作为业务回滚，而应通过结果中的
    /// `Pending` 状态与启动恢复继续收敛。
    fn clear_session_history(
        &self,
        clear: SessionHistoryClear,
    ) -> StoreFuture<'_, SessionHistoryClearResult>;

    /// 幂等登记手动压缩；同一 operation 的既有终态直接返回。
    fn prepare_session_compaction(
        &self,
        preparation: SessionHistoryCompactionPreparation,
    ) -> StoreFuture<'_, SessionHistoryCompactionPreparationResult>;

    /// 将尚未提交 replacement 的手动压缩收敛为 no-op、cancelled 或 interrupted。
    fn finish_session_compaction(
        &self,
        finish: SessionHistoryCompactionFinish,
    ) -> StoreFuture<'_, ()>;

    /// 创建 accepted 子任务关系及其空的 generation 1 正文。
    fn create_child_task(&self, task: NewStoredChildTask) -> StoreFuture<'_, StoredChildTask>;

    /// 写入初始 User Message，并把 accepted 子任务可靠切到 running。
    fn start_child_task(&self, start: ChildTaskStart) -> StoreFuture<'_, ()>;

    /// 在任何子任务工具副作用前保存完整 Tool Call 批次。
    fn begin_child_tool_exchange(&self, pending: PendingChildToolExchange) -> StoreFuture<'_, ()>;

    /// 在子任务 Tool SPI 产生副作用前写入 started 标记。
    fn mark_child_tool_execution_started(
        &self,
        start: ChildToolExecutionStart,
    ) -> StoreFuture<'_, ()>;

    /// 提交子任务完整工具结果并清除对应 pending 事实。
    fn complete_child_tool_exchange(
        &self,
        completed: CompletedChildToolExchange,
    ) -> StoreFuture<'_, ()>;

    /// 可靠写入最终消息并结算子任务终态。
    fn settle_child_task(&self, settlement: StoredChildTaskSettlement) -> StoreFuture<'_, ()>;

    /// 在发布活动 child 取消令牌前可靠记录取消意图；终态任务幂等返回原投影。
    fn request_child_task_cancellation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StoreFuture<'_, StoredChildTask>;

    /// 加载子任务独立的完整规范 Conversation，同时核对 Session 所有权。
    fn load_child_conversation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StoreFuture<'_, ConversationSnapshot>;

    /// 把父 Run、child 或空闲 Session 的压缩上下文合并进完整产品历史并可靠切换 generation。
    fn replace_context(
        &self,
        replacement: ContextReplacement,
    ) -> StoreFuture<'_, ContextReplacementResult>;

    /// 原子创建 Input 与首次 Accepted Run，或返回同 Session 幂等 key 的首次结果。
    fn accept_input(&self, input: NewStoredInput) -> StoreFuture<'_, AcceptedInput>;

    /// 原子接受一个不创建 Run 的结构化 Session Command。
    fn accept_session_command(
        &self,
        command: NewStoredSessionCommand,
    ) -> StoreFuture<'_, AcceptedStoredSessionCommand>;

    /// 删除尚未进入规范 Conversation 的排队 Input 及其 Run。
    fn cancel_queued_input(
        &self,
        session_id: &SessionId,
        input_id: &InputId,
    ) -> StoreFuture<'_, ()>;

    /// 可靠调整尚未开始的输入优先级；不得改写原始接收时间或 Conversation。
    fn prioritize_queued_input(&self, change: QueuePriorityChange) -> StoreFuture<'_, ()>;

    /// 为最新的 Failed/Interrupted Run 创建递增 attempt。
    fn create_run_attempt(&self, attempt: NewStoredRunAttempt) -> StoreFuture<'_, StoredRun>;

    /// 可靠写入 User Message，并将对应 Run 从 accepted 转为 running。
    fn commit_user_message(&self, commit: UserMessageCommit) -> StoreFuture<'_, ()>;

    /// 原子提交 Command 结果及其 Runtime user-role Conversation 消息。
    fn commit_session_command(
        &self,
        commit: SessionCommandCommit,
    ) -> StoreFuture<'_, super::StoredSessionCommand>;

    /// 在任何工具副作用前保存完整 Tool Call 批次并返回确认。
    fn begin_tool_exchange(&self, pending: PendingToolExchange) -> StoreFuture<'_, ()>;

    /// 在 Tool SPI 产生任何外部副作用前写入临时 started 标记。
    fn mark_tool_execution_started(&self, start: ToolExecutionStart) -> StoreFuture<'_, ()>;

    /// 保存完整结果、整批提交正文并清除对应 pending 事实。
    fn complete_tool_exchange(&self, completed: CompletedToolExchange) -> StoreFuture<'_, ()>;

    /// 在活动 Run 保持 running 时可靠追加下一次 AgentExecution 所需的消息与领域效果。
    fn commit_run_continuation(
        &self,
        continuation: StoredRunContinuation,
    ) -> StoreFuture<'_, StoredRunContinuationResult>;

    /// 可靠写入本 Run 尚未提交的完整消息，并同时结算 Run 终态。
    fn settle_run(
        &self,
        settlement: StoredRunSettlement,
    ) -> StoreFuture<'_, StoredRunSettlementResult>;

    /// CAS 暂停 Goal、作废排队 continuation，并可靠记录活动 Run 取消意图。
    fn stop_goal(&self, stop: GoalStop) -> StoreFuture<'_, GoalStopResult>;

    /// CAS 删除非运行中的 Goal 控制器；WorkPlan 与普通用户队列保持不变。
    fn clear_goal(&self, clear: GoalClear) -> StoreFuture<'_, ()>;

    /// 原子把一条 held 用户 Input 绑定到恢复后的新 Goal generation。
    fn resume_goal_with_held_input(
        &self,
        resume: GoalHeldInputResume,
    ) -> StoreFuture<'_, GoalHeldInputResumeResult>;

    /// 按当前权威 generation 加载并校验完整规范 Conversation。
    fn load_conversation(&self, session_id: &SessionId) -> StoreFuture<'_, ConversationSnapshot>;

    /// 读取 Session 已可靠提交的模型请求用量汇总。
    fn get_session_usage(&self, session_id: &SessionId) -> StoreFuture<'_, StoredSessionUsage>;

    /// 按可显示 User/Assistant 消息边界读取历史窗口；实现可使用可重建的私有索引。
    fn load_conversation_window(
        &self,
        request: ConversationWindowRequest,
    ) -> StoreFuture<'_, StoredConversationWindow>;

    /// 按权威 JSONL 原始消息序号读取有限窗口，仅供签名 Recall 引用二次定位。
    fn load_conversation_raw_window(
        &self,
        request: ConversationRawWindowRequest,
    ) -> StoreFuture<'_, StoredConversationRawWindow>;

    /// 在当前权威 generation 中按稳定 Message ID 定位；不存在时返回 `None`。
    fn locate_conversation_message(
        &self,
        request: ConversationMessageLocationRequest,
    ) -> StoreFuture<'_, Option<StoredConversationMessageLocation>>;

    /// 检索可重建的 Conversation 派生索引；不得以索引正文替代权威 JSONL。
    fn search_conversations(
        &self,
        request: ConversationSearchRequest,
    ) -> StoreFuture<'_, ConversationSearchPage>;

    /// 原子切换 Session 归档状态；正文和运行历史保持不变。
    fn set_session_archive(&self, change: ArchiveChange) -> StoreFuture<'_, ()>;

    /// 显式设置普通 Session 的主控代理终态。
    fn set_session_proxy(&self, change: SessionProxyChange) -> StoreFuture<'_, ()>;

    /// 修改 Session 标题并持久化用户来源。
    fn rename_session(&self, change: SessionTitleChange) -> StoreFuture<'_, ()>;

    /// 手动触发开始前可靠撤销后续自动标题资格。
    fn disable_automatic_title(&self, session_id: &SessionId) -> StoreFuture<'_, ()>;

    /// 原子结算标题候选、自动资格与旁路模型用量。
    fn commit_session_title_generation(
        &self,
        commit: SessionTitleGenerationCommit,
    ) -> StoreFuture<'_, SessionTitleGenerationCommitResult>;

    /// 幂等设置 Session 固定状态。
    fn set_session_pinned(&self, change: SessionPinnedChange) -> StoreFuture<'_, ()>;

    /// 保存或清除 Assistant Message 反馈。
    fn set_message_feedback(&self, change: MessageFeedbackChange) -> StoreFuture<'_, ()>;

    /// 加载 Session 当前仍有效的 Assistant Message 反馈。
    fn load_message_feedback(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, Vec<StoredMessageFeedback>>;

    /// 原子切换 Session 后续 Run 使用的模型 key。
    fn set_session_model(&self, change: ModelChange) -> StoreFuture<'_, ()>;

    fn set_session_reasoning_effort(&self, change: ReasoningEffortChange) -> StoreFuture<'_, ()>;

    /// 原子切换 Session 当前 Agent 变体。
    fn set_session_variant(&self, change: VariantChange) -> StoreFuture<'_, ()>;

    /// 原子切换 Session 当前审批模式。
    fn set_session_approval_mode(&self, change: ApprovalModeChange) -> StoreFuture<'_, ()>;

    /// 原子切换正文 generation、销毁目标及尾段关联，并创建新的 committed Input/Run。
    fn rewrite_from_user(&self, rewrite: ConversationRewrite) -> StoreFuture<'_, RewriteResult>;

    /// 停止接收新命令，flush 已接受操作并等待基础设施 worker 退出。
    fn shutdown(&self) -> StoreFuture<'_, ()>;
}
