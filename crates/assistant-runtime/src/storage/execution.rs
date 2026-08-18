use agent_core::ExchangeReceipt;
use agent_types::{AssistantMessage, ConversationMessage, MessageId, ToolMessage, UserMessage};
use assistant_protocol::{
    AgentVariant, ApprovalMode, IdempotencyKey, InputId, RunId, RunStatus, RuntimeErrorInfo,
    SessionId, ToolCallId,
};

/// 队列执行器领取一次 Run 时提交的 User Message 与结构化关联。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserMessageCommit {
    pub operation_id: String,
    pub input_id: InputId,
    pub run_id: RunId,
    pub session_id: SessionId,
    /// queued 输入首次开始时为 Some；已提交输入的新 attempt 不重复追加消息。
    pub message: Option<UserMessage>,
    pub created_at_ms: i64,
}

/// 工具副作用发生前必须可靠保存的完整 Assistant Tool Call 批次。
#[derive(Clone, Debug, PartialEq)]
pub struct PendingToolExchange {
    pub receipt: ExchangeReceipt,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub assistant: AssistantMessage,
    pub created_at_ms: i64,
}

/// 工具结果齐备后，把 pending 批次整体转入规范 Conversation 的命令。
#[derive(Clone, Debug, PartialEq)]
pub struct CompletedToolExchange {
    pub operation_id: String,
    pub receipt: ExchangeReceipt,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub results: Vec<ToolMessage>,
    pub completed_at_ms: i64,
}

/// Core 已获授权、即将进入工具副作用前的可靠 started 记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolExecutionStart {
    pub receipt: ExchangeReceipt,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub call_id: ToolCallId,
    pub started_at_ms: i64,
}

/// Input 是否已经进入规范 Conversation。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredInputState {
    /// 正文暂存在结构化队列中，尚可取消。
    Queued,
    /// User Message 已提交到规范 Conversation，不再属于可取消队列。
    Committed,
}

/// Runtime 从 Store 恢复的 Input 投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredInput {
    pub queue_order: u64,
    pub input_id: InputId,
    pub session_id: SessionId,
    pub idempotency_key: Option<IdempotencyKey>,
    pub agent_variant: AgentVariant,
    pub user_message_id: MessageId,
    pub state: StoredInputState,
    pub queued_message: Option<UserMessage>,
    pub accepted_at_ms: i64,
}

/// 原子接受 Input 及其首次 Run 所需的完整事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewStoredInput {
    pub input_id: InputId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub idempotency_key: Option<IdempotencyKey>,
    pub agent_variant: AgentVariant,
    pub approval_mode: ApprovalMode,
    pub message: UserMessage,
    /// 首条输入可提供的有界自动标题；Store 仅在标题来源仍为系统生成时采用。
    pub generated_title: Option<String>,
    pub accepted_at_ms: i64,
}

/// Store 接受结果；幂等命中时返回首次持久化事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedInput {
    pub input: StoredInput,
    pub run: StoredRun,
    pub is_duplicate: bool,
}

/// 把一条仍在排队的 Input 提升为当前持久队列的下一项。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuePriorityChange {
    pub session_id: SessionId,
    pub input_id: InputId,
}

/// 从失败或中断 Run 创建下一次执行尝试的命令。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewStoredRunAttempt {
    pub run_id: RunId,
    pub source_run_id: RunId,
    pub session_id: SessionId,
    pub approval_mode: ApprovalMode,
    pub created_at_ms: i64,
}

/// 一次 Run 的可靠终态以及尚未写入正文的完整消息批次。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredRunSettlement {
    pub operation_id: String,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub status: RunStatus,
    pub cancel_requested: bool,
    pub error: Option<RuntimeErrorInfo>,
    pub messages: Vec<ConversationMessage>,
    pub finished_at_ms: i64,
}

/// Runtime 启动时恢复的 Run 结构化投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredRun {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub input_id: InputId,
    pub attempt: u32,
    pub status: RunStatus,
    pub agent_variant: AgentVariant,
    pub approval_mode: ApprovalMode,
    pub cancel_requested: bool,
    pub error: Option<RuntimeErrorInfo>,
    pub message_ids: Vec<MessageId>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
}
