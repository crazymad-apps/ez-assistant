//! 卡片独立于 Conversation 正文保存；可靠工具收据由 Store 在发布事务内核实。

use assistant_protocol::{
    ConversationOwner, MessageCardSnapshot, MessageCardSource, MessageCardSummary, RunId,
    ToolCallId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Runtime 绑定的发布意图；Store 必须先找到该 owner 正在执行的可靠工具调用。
#[derive(Clone)]
pub struct NewMessageCard {
    pub card_id: String,
    pub owner: ConversationOwner,
    pub run_id: Option<RunId>,
    pub tool_call_id: ToolCallId,
    pub title: String,
    pub source: MessageCardSource,
    pub arguments: Value,
    pub tool_result: Value,
    /// 独立 MCP 绑定发布时的配置身份，防止同名重配后的资源读取历史私有结果。
    pub source_fingerprint: Option<String>,
}

/// UI 私有持久值不进入启动恢复、模型历史、全文索引或普通 Debug。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredMessageCard {
    pub summary: MessageCardSummary,
    pub receipt_id: String,
    pub tool_call_id: ToolCallId,
    pub arguments: Value,
    pub tool_result: Value,
    pub source_fingerprint: Option<String>,
    pub snapshot: Option<MessageCardSnapshot>,
}

#[derive(Clone)]
pub struct MessageCardSnapshotWrite {
    pub owner: ConversationOwner,
    pub card_id: String,
    pub expected_revision: u64,
    pub snapshot: MessageCardSnapshot,
}

impl std::fmt::Debug for StoredMessageCard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredMessageCard")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

/// 主／子 owner 只改变查询身份；不创建新会话或可执行 Run。
pub fn message_card_owner(
    owner: &ConversationOwner,
) -> (
    &assistant_protocol::SessionId,
    Option<&assistant_protocol::ChildTaskId>,
) {
    match owner {
        ConversationOwner::MainSession { session_id } => (session_id, None),
        ConversationOwner::ChildTask {
            session_id,
            child_task_id,
        } => (session_id, Some(child_task_id)),
    }
}
