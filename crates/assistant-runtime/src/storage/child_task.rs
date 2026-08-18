use agent_core::ExchangeReceipt;
use agent_model::SystemPromptSnapshot;
use agent_types::{AssistantMessage, ConversationMessage, MessageId, ToolMessage, UserMessage};
use assistant_protocol::{
    AgentVariant, ChildTaskId, ChildTaskStatus, RunId, RuntimeErrorInfo, SessionId, ToolCallId,
};

use super::StoredConversationState;

/// 创建 accepted 子任务关系及空独立正文所需的冻结事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewStoredChildTask {
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub parent_run_id: RunId,
    pub parent_tool_call_id: ToolCallId,
    pub title: String,
    pub system_prompt: SystemPromptSnapshot,
    pub agent_variant: AgentVariant,
    pub created_at_ms: i64,
}

/// Runtime 从 Store 恢复或创建完成的子任务结构化投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredChildTask {
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub parent_run_id: RunId,
    pub parent_tool_call_id: ToolCallId,
    pub title: String,
    pub system_prompt: SystemPromptSnapshot,
    pub agent_variant: AgentVariant,
    pub status: ChildTaskStatus,
    pub cancel_requested: bool,
    pub body_generation: u64,
    pub message_count: u64,
    pub final_message_id: Option<MessageId>,
    pub error: Option<RuntimeErrorInfo>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    /// 子任务独立正文是否可以安全读取；不额外持久化到 SQLite。
    pub conversation_state: StoredConversationState,
}

/// 把子任务初始 User Message 可靠写入独立正文并切到 running。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildTaskStart {
    pub operation_id: String,
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub message: UserMessage,
    pub started_at_ms: i64,
}

/// 子任务工具副作用前必须保存的完整 Assistant Tool Call 批次。
#[derive(Clone, Debug, PartialEq)]
pub struct PendingChildToolExchange {
    pub receipt: ExchangeReceipt,
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub assistant: AssistantMessage,
    pub created_at_ms: i64,
}

/// 子任务 Core 已获授权、即将进入工具副作用前的可靠 started 记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildToolExecutionStart {
    pub receipt: ExchangeReceipt,
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub call_id: ToolCallId,
    pub started_at_ms: i64,
}

/// 子任务工具结果齐备后的可靠完整批次提交。
#[derive(Clone, Debug, PartialEq)]
pub struct CompletedChildToolExchange {
    pub operation_id: String,
    pub receipt: ExchangeReceipt,
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub results: Vec<ToolMessage>,
    pub completed_at_ms: i64,
}

/// 子任务最终状态及尚未写入独立正文的完整消息批次。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredChildTaskSettlement {
    pub operation_id: String,
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub status: ChildTaskStatus,
    pub cancel_requested: bool,
    pub error: Option<RuntimeErrorInfo>,
    pub messages: Vec<ConversationMessage>,
    pub final_message_id: Option<MessageId>,
    pub finished_at_ms: i64,
}
