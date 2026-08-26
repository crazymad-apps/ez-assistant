use agent_types::{ConversationSnapshot, MessageId};
use assistant_protocol::{
    ChildTaskId, ConversationOwner, GoalId, IdempotencyKey, RunId, SessionId, WorkspaceId,
};

use super::{NewStoredInput, StoredGoal, StoredInput, StoredRun};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RewriteGoalEffect {
    pub expected_goal_id: GoalId,
    pub expected_generation: u64,
    pub goal: StoredGoal,
}

/// 历史重新输入所需的完整新正文和结构化关联。
#[derive(Clone, Debug, PartialEq)]
pub struct ConversationRewrite {
    pub session_id: SessionId,
    pub target_user_message_id: MessageId,
    pub conversation: ConversationSnapshot,
    pub input: NewStoredInput,
    pub goal_effect: Option<RewriteGoalEffect>,
    pub changed_at_ms: i64,
}

/// 自动压缩要原子替换的正文所有者；Run 与 child 都必须仍处于活动执行期。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextReplacementTarget {
    Run {
        session_id: SessionId,
        run_id: RunId,
    },
    ChildTask {
        session_id: SessionId,
        child_task_id: ChildTaskId,
    },
    /// 不依附业务 Run 的手动 Session 压缩。
    IdleSession {
        session_id: SessionId,
        expected_generation: u64,
        operation_id: IdempotencyKey,
        compacted_message_count: u64,
        retained_message_count: u64,
    },
}

/// 使用新 generation 替换当前有效 Conversation，不改写历史 Input/Run/child 关系。
#[derive(Clone, Debug, PartialEq)]
pub struct ContextReplacement {
    pub target: ContextReplacementTarget,
    pub conversation: ConversationSnapshot,
    pub changed_at_ms: i64,
}

/// Store 实际提交的 generation 边界；新 generation 可能跳过遗留孤儿文件。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextReplacementResult {
    pub source_generation: u64,
    pub result_generation: u64,
}

/// generation 切换成功后创建的新 Input 与首次 Run。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RewriteResult {
    pub input: StoredInput,
    pub run: StoredRun,
    pub body_generation: u64,
}

/// Runtime 向 Store 请求的一段规范 Conversation 窗口；`end` 使用可显示消息序号。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationWindowRequest {
    pub owner: ConversationOwner,
    pub generation: u64,
    pub end: Option<usize>,
    pub limit: usize,
}

/// Store 基于可重建索引返回的规范消息窗口；offset 不进入产品协议。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredConversationWindow {
    pub generation: u64,
    pub start: usize,
    pub end: usize,
    pub total: usize,
    pub conversation: ConversationSnapshot,
}

/// Runtime 内部以权威 JSONL 原始消息序号读取的有限窗口。
///
/// 该端口只供 Conversation Recall 对签名引用进行二次定位；原始序号不进入产品协议。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationRawWindowRequest {
    pub owner: ConversationOwner,
    pub generation: u64,
    pub start: usize,
    pub limit: usize,
}

/// Store 返回的原始消息窗口；调用方必须继续按 Recall 收录规则过滤可见正文。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredConversationRawWindow {
    pub generation: u64,
    pub start: usize,
    pub end: usize,
    pub total: usize,
    pub conversation: ConversationSnapshot,
}

/// Runtime 请求按稳定 Message ID 在当前权威 generation 中重新定位消息。
///
/// 该操作只用于校验旧 Recall 引用；Store 不依赖引用携带的旧 ordinal 或 generation。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationMessageLocationRequest {
    pub owner: ConversationOwner,
    pub message_id: MessageId,
}

/// 当前权威 Conversation 中一条消息的位置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredConversationMessageLocation {
    pub generation: u64,
    /// JSONL 中包含工具与系统消息的原始序号，供 Recall 读取原始窗口。
    pub message_ordinal: u64,
    /// User/Assistant 可展示消息中的零基序号；非展示消息没有该位置。
    pub display_ordinal: Option<u64>,
}

/// Conversation Recall 的存储级检索范围；调用方权限仍由 Runtime 在进入 Store 前判定。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConversationSearchScope {
    /// 当前 Session 及其子任务 Conversation。
    Session { session_id: SessionId },
    /// 绑定到同一 Workspace 的全部 Session 及其子任务 Conversation。
    Workspace { workspace_id: WorkspaceId },
    /// Runtime 中全部未删除 Conversation。
    Global,
}

/// Runtime 向派生 Conversation 索引发起的一页检索请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationSearchRequest {
    pub query: String,
    pub scope: ConversationSearchScope,
    pub limit: usize,
}

/// 一条派生索引命中；正文仍须以对应 JSONL Conversation 为权威来源。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationSearchHit {
    pub owner: ConversationOwner,
    pub generation: u64,
    pub message_id: MessageId,
    pub message_ordinal: u64,
    pub created_at_ms: i64,
    pub text: String,
}

/// 一页派生检索结果。`partial` 表示仍有脏索引等待后续有界重建。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationSearchPage {
    pub hits: Vec<ConversationSearchHit>,
    pub partial: bool,
    pub failed_owners: Vec<ConversationOwner>,
}
