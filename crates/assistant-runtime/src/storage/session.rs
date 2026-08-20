use agent_model::SystemPromptSnapshot;
use agent_types::{ConversationSnapshot, ToolImageReference};
use assistant_protocol::{
    AgentVariant, ApprovalMode, AttachmentId, MessageFeedback, ModelKey, ReasoningEffortKey,
    SessionId, SessionTitleOrigin,
};

use crate::SessionExecutionEnvironment;

use super::StoredAttachment;

/// Session 的持久化生命周期。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredSessionLifecycle {
    /// 可以接受业务变更的活动 Session。
    Active,
    /// 只允许查询、等待显式恢复的归档 Session。
    Archived,
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
    pub environment: SessionExecutionEnvironment,
    pub current_variant: AgentVariant,
    pub approval_mode: ApprovalMode,
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
}

/// Store 完成路径重写和跨介质提交后的 Fork 结果。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredSessionFork {
    pub session: StoredSession,
    pub conversation: ConversationSnapshot,
    pub attachments: Vec<StoredAttachment>,
}

/// 带预检影响摘要的永久删除业务原语。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDeletion {
    pub session_id: SessionId,
    pub operation_id: String,
    pub expected_impact: assistant_protocol::DeleteSessionImpact,
}

/// 从存储恢复或创建完成的 Session 投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSession {
    pub session_id: SessionId,
    pub title: String,
    pub model_key: ModelKey,
    pub reasoning_effort: Option<ReasoningEffortKey>,
    pub system_prompt: SystemPromptSnapshot,
    pub environment: SessionExecutionEnvironment,
    pub lifecycle: StoredSessionLifecycle,
    pub current_variant: AgentVariant,
    pub approval_mode: ApprovalMode,
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
    pub latest: Option<agent_types::TokenUsage>,
}

/// 原子切换 Session 归档状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveChange {
    pub session_id: SessionId,
    pub archived: bool,
    pub changed_at_ms: i64,
}

/// 原子修改 Session 标题并将来源标记为用户输入。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionTitleChange {
    pub session_id: SessionId,
    pub title: String,
    pub changed_at_ms: i64,
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
