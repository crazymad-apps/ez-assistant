use agent_model::SystemPromptSnapshot;
use agent_types::{ConversationSnapshot, ToolImageReference};
use assistant_protocol::{
    AgentVariant, ApprovalMode, AttachmentId, CompactSessionOutcome, IdempotencyKey,
    MessageFeedback, ModelKey, ReasoningEffortKey, SessionHistoryCleanupStatus, SessionId,
    SessionTitleGenerationTriggerSnapshot, SessionTitleOrigin,
};

use crate::{
    PcOutputHosting, SessionExecutionEnvironment, SessionSkillCatalog, StoredMcpSelection,
    StoredSkillActivation,
};

use super::{StoredAttachment, StoredGoal, StoredSessionCommand, StoredWorkPlan};

/// Session 的持久化生命周期。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredSessionLifecycle {
    /// 可以接受业务变更的活动 Session。
    Active,
    /// 只允许查询、等待显式恢复的归档 Session。
    Archived,
}

/// Session 的持久产品角色。存储允许存在多个 Controller，产品层只选择一个当前主控。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SessionRole {
    #[default]
    Standard,
    Controller,
}

/// 普通 Session 当前绑定的主控代理事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionProxyState {
    pub controller_session_id: SessionId,
    pub changed_at_ms: i64,
}

/// 启动恢复后正文是否可以安全加载。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredConversationState {
    /// 当前 generation 可读取且没有未决的一致性故障。
    Available,
    /// 该 Session 的正文存在无法自动判定的持久化状态。
    Unavailable,
}

/// 创建持久化 Session 所需的完整冻结事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewStoredSession {
    pub session_id: SessionId,
    pub title: String,
    pub title_origin: SessionTitleOrigin,
    pub model_key: ModelKey,
    pub reasoning_effort: Option<ReasoningEffortKey>,
    pub system_prompt: SystemPromptSnapshot,
    pub skill_catalog: SessionSkillCatalog,
    pub environment: SessionExecutionEnvironment,
    pub current_variant: AgentVariant,
    pub approval_mode: ApprovalMode,
    pub role: SessionRole,
    /// 首次发送物化的进程外幂等身份；现有创建入口保持为空。
    pub materialization_key: Option<IdempotencyKey>,
    /// 是否仍具备自动标题资格；M0 的现有入口统一写入 false。
    pub automatic_title_pending: bool,
    pub created_at_ms: i64,
}

/// Fork 中一个源 Attachment 到新 Session 引用的稳定映射。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkedAttachmentReference {
    pub source_attachment_id: AttachmentId,
    pub attachment_id: AttachmentId,
}

/// Runtime 已校验 fork point 后交给 Store 的完整原子创建事实。
#[derive(Clone, Debug, PartialEq)]
pub struct SessionFork {
    pub source_session_id: SessionId,
    pub source_generation: u64,
    pub session: NewStoredSession,
    pub conversation: ConversationSnapshot,
    pub attachments: Vec<ForkedAttachmentReference>,
    pub tool_images: Vec<ToolImageReference>,
    /// 位于 Fork Conversation 前缀内、已改绑到目标 Session 的 Activation ledger。
    pub skill_activations: Vec<StoredSkillActivation>,
    /// 位于 Fork Conversation 前缀内、已改绑到目标 Session 的 MCP Selection ledger。
    pub mcp_selections: Vec<StoredMcpSelection>,
    /// 历史控制结果沿 Fork 复制；目标 Input/Message 重新分配身份，不复制排队指令。
    pub session_commands: Vec<ForkedSessionCommand>,
    /// Fork 时冻结的源 WorkPlan；Store 在同一事务中为目标 Session 创建 revision 1。
    pub work_plan: Option<StoredWorkPlan>,
    /// 仅当 objective source 位于前缀时提供的新 Goal；状态固定为 Paused(Forked)。
    pub goal: Option<StoredGoal>,
}

/// Store 完成路径重写和跨介质提交后的 Fork 结果。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredSessionFork {
    pub session: StoredSession,
    pub conversation: ConversationSnapshot,
    pub attachments: Vec<StoredAttachment>,
    pub skill_activations: Vec<StoredSkillActivation>,
    pub mcp_selections: Vec<StoredMcpSelection>,
    pub session_commands: Vec<StoredSessionCommand>,
    pub work_plan: Option<StoredWorkPlan>,
    pub goal: Option<StoredGoal>,
}

/// Fork 控制结果的可核验来源与重新绑定后的目标事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkedSessionCommand {
    pub source_input_id: assistant_protocol::InputId,
    pub command: StoredSessionCommand,
}

/// 带预检影响摘要的永久删除业务原语。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDeletion {
    pub session_id: SessionId,
    pub operation_id: String,
    pub expected_impact: assistant_protocol::DeleteSessionImpact,
}

/// Runtime 已在 Session mutation gate 内准备完成的历史清空事实。
///
/// Store 必须先可靠创建新空 generation，再于单一事务中切换
/// Session 指针、替换冻结上下文并删除旧历史结构事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionHistoryClear {
    pub operation_id: IdempotencyKey,
    pub session_id: SessionId,
    pub expected_generation: u64,
    pub system_prompt: SystemPromptSnapshot,
    pub skill_catalog: SessionSkillCatalog,
    /// 重建后的环境必须与 Session 现有稳定资源身份完全一致。
    pub environment: SessionExecutionEnvironment,
    pub expected_role: SessionRole,
    pub changed_at_ms: i64,
}

/// clear 的权威切换结果；不携带任何旧正文。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionHistoryClearResult {
    pub session: StoredSession,
    pub source_generation: u64,
    pub result_generation: u64,
    pub cleanup_status: SessionHistoryCleanupStatus,
}

/// 在模型调用前可靠占用一个手动压缩 operation ID。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionHistoryCompactionPreparation {
    pub operation_id: IdempotencyKey,
    pub session_id: SessionId,
    pub expected_generation: u64,
    pub created_at_ms: i64,
}

/// prepare 要么取得本次执行权，要么返回同一 operation 的既有终态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionHistoryCompactionPreparationResult {
    Prepared,
    Completed(CompactSessionOutcome),
}

/// 未提交 replacement 的手动压缩收敛状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionHistoryCompactionFinishKind {
    NoOp,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionHistoryCompactionFinish {
    pub operation_id: IdempotencyKey,
    pub session_id: SessionId,
    pub expected_generation: u64,
    pub kind: SessionHistoryCompactionFinishKind,
    pub finished_at_ms: i64,
}

/// 从存储恢复或创建完成的 Session 投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSession {
    pub session_id: SessionId,
    pub title: String,
    pub model_key: ModelKey,
    pub reasoning_effort: Option<ReasoningEffortKey>,
    pub system_prompt: SystemPromptSnapshot,
    pub skill_catalog: SessionSkillCatalog,
    pub environment: SessionExecutionEnvironment,
    pub lifecycle: StoredSessionLifecycle,
    pub current_variant: AgentVariant,
    pub approval_mode: ApprovalMode,
    pub role: SessionRole,
    pub materialization_key: Option<IdempotencyKey>,
    pub automatic_title_pending: bool,
    pub proxy: Option<SessionProxyState>,
    pub pc_output_hosting: Option<PcOutputHosting>,
    pub body_generation: u64,
    pub message_count: u64,
    pub created_at_ms: i64,
    /// 最近一次 Run 可靠终结的时间；尚无 Run 时等于创建时间。
    pub updated_at_ms: i64,
    pub archived_at_ms: Option<i64>,
    pub is_pinned: bool,
    pub title_origin: SessionTitleOrigin,
    pub conversation_state: StoredConversationState,
}

/// Session 级模型请求用量滚动汇总。
///
/// 该投影只统计主 Session 已可靠提交的模型响应；子任务和识图辅助模型拥有独立调用语义，
/// 不混入主会话右侧面板。`cached_request_count` 用于区分“缓存命中为 0”与“Provider 未报告”。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoredSessionUsage {
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: u64,
    pub cached_request_count: u64,
    pub reasoning_tokens: u64,
    pub reasoning_request_count: u64,
    /// 不进入主 Agent Turn 的 Session 旁路模型请求次数。
    pub auxiliary_request_count: u64,
    pub auxiliary_input_tokens: u64,
    pub auxiliary_output_tokens: u64,
    pub auxiliary_total_tokens: u64,
    pub latest: Option<agent_types::TokenUsage>,
}

/// 原子切换 Session 归档状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveChange {
    pub session_id: SessionId,
    pub archived: bool,
    pub changed_at_ms: i64,
}

/// 显式设置普通 Session 的代理终态；不使用 toggle 或修订号。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionProxyChange {
    pub target_session_id: SessionId,
    pub controller_session_id: SessionId,
    pub enabled: bool,
    pub changed_at_ms: i64,
}

/// 原子修改 Session 标题并将来源标记为用户输入。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionTitleChange {
    pub session_id: SessionId,
    pub title: String,
    pub changed_at_ms: i64,
}

/// 标题旁路模型调用的可靠收敛事实。
///
/// `expected_title` 仅用于自动触发的 compare-and-set，避免覆盖调用期间发生的直接编辑；
/// 手动触发允许替换任意现有标题。无有效候选时 `title` 为空，只结算 pending 与旁路用量。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionTitleGenerationCommit {
    pub session_id: SessionId,
    pub trigger: SessionTitleGenerationTriggerSnapshot,
    pub expected_title: Option<String>,
    pub title: Option<String>,
    pub request_attempted: bool,
    pub usage: Option<agent_types::TokenUsage>,
    pub completed_at_ms: i64,
}

/// Store 结算后供 Runtime 更新内存投影的权威结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionTitleGenerationCommitResult {
    pub applied: bool,
    pub title: String,
    pub title_origin: SessionTitleOrigin,
    pub automatic_title_pending: bool,
}

/// 幂等设置 Session 固定状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionPinnedChange {
    pub session_id: SessionId,
    pub is_pinned: bool,
    pub changed_at_ms: i64,
}

/// 一条 Assistant Message 的本地反馈事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMessageFeedback {
    pub message_id: assistant_protocol::MessageId,
    pub feedback: MessageFeedback,
}

/// 保存或清除一条 Assistant Message 的反馈。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageFeedbackChange {
    pub session_id: SessionId,
    pub message_id: assistant_protocol::MessageId,
    pub feedback: Option<MessageFeedback>,
    pub changed_at_ms: i64,
}

/// 原子切换 Session 后续 Run 使用的模型 key。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelChange {
    pub session_id: SessionId,
    pub model_key: ModelKey,
    pub reasoning_effort: Option<ReasoningEffortKey>,
    pub changed_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningEffortChange {
    pub session_id: SessionId,
    pub reasoning_effort: Option<ReasoningEffortKey>,
    pub changed_at_ms: i64,
}

/// 原子切换 Session 当前 Agent 变体。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariantChange {
    pub session_id: SessionId,
    pub variant: AgentVariant,
    pub changed_at_ms: i64,
}

/// 原子切换 Session 当前审批模式。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalModeChange {
    pub session_id: SessionId,
    pub approval_mode: ApprovalMode,
    pub changed_at_ms: i64,
}
