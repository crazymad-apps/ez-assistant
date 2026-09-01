use agent_types::{ConversationMessage, ConversationSnapshot, MessageId};
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

/// 提交压缩后的 Agent 有效上下文；Store 把它合并进完整产品 Conversation 的新 generation。
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
    /// 新 generation 中完整产品 Conversation 的物理消息数。
    ///
    /// 它可能大于压缩后的执行上下文消息数；Runtime 不能再用 Journal 长度覆盖该值。
    pub product_message_count: u64,
}

/// 从完整产品 Conversation 派生 Agent 当前应读取的有效上下文。
///
/// 压缩摘要既是产品历史中的边界标记，也是执行上下文的新起点；开头连续的 System prefix
/// 始终保留。没有摘要时整份 Conversation 都是有效上下文。该投影不修改持久化正文。
pub fn execution_context_from_product_history(
    history: &ConversationSnapshot,
) -> ConversationSnapshot {
    let prefix_end = history
        .messages
        .iter()
        .take_while(|message| matches!(message, ConversationMessage::System(_)))
        .count();
    let summary_index = history
        .messages
        .iter()
        .rposition(|message| matches!(message, ConversationMessage::ContextSummary(_)))
        .unwrap_or(prefix_end);
    let mut messages = history.messages[..prefix_end].to_vec();
    messages.extend_from_slice(&history.messages[summary_index..]);
    ConversationSnapshot::new(messages)
}

/// 把一次压缩后的执行上下文合并回唯一、完整的产品 Conversation。
///
/// replacement 必须是 `System prefix → 新 Context Summary → 当前完整历史的原样尾部`。
/// 合并时复用产品历史开头的 System prefix，只把新摘要插入保留尾部之前，因此保留全部旧
/// 轮次且不会复制稳定消息 ID。
pub fn merge_context_replacement_with_product_history(
    history: &ConversationSnapshot,
    replacement: &ConversationSnapshot,
) -> Result<ConversationSnapshot, super::StoreError> {
    let replacement_prefix_end = replacement
        .messages
        .iter()
        .take_while(|message| matches!(message, ConversationMessage::System(_)))
        .count();
    if !matches!(
        replacement.messages.get(replacement_prefix_end),
        Some(ConversationMessage::ContextSummary(_))
    ) {
        return Err(super::StoreError::new(
            super::StoreErrorKind::InvalidInput,
            "context replacement must contain a context summary after its system prefix",
        ));
    }
    let history_prefix_end = history
        .messages
        .iter()
        .take_while(|message| matches!(message, ConversationMessage::System(_)))
        .count();
    if replacement.messages[..replacement_prefix_end] != history.messages[..history_prefix_end] {
        return Err(super::StoreError::new(
            super::StoreErrorKind::InvalidInput,
            "context replacement changed the protected system prefix",
        ));
    }
    let retained = &replacement.messages[replacement_prefix_end + 1..];
    if history.messages.len() < retained.len()
        || history.messages[history.messages.len() - retained.len()..] != *retained
    {
        return Err(super::StoreError::new(
            super::StoreErrorKind::InvalidInput,
            "context replacement does not retain an exact conversation suffix",
        ));
    }
    let prefix_len = history.messages.len() - retained.len();
    let replacement_body = &replacement.messages[replacement_prefix_end..];
    let mut messages = Vec::with_capacity(prefix_len + replacement_body.len());
    messages.extend_from_slice(&history.messages[..prefix_len]);
    messages.extend_from_slice(replacement_body);
    let merged = ConversationSnapshot::new(messages);
    merged.validate_tool_exchange_pairs().map_err(|source| {
        super::StoreError::with_source(
            super::StoreErrorKind::InvalidInput,
            "merged product conversation is invalid",
            source,
        )
    })?;
    Ok(merged)
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

#[cfg(test)]
mod tests {
    use agent_types::{ContextSummaryMessage, SystemMessage, UserMessage};

    use super::*;

    fn message_id(value: &str) -> MessageId {
        MessageId::new(value).expect("message id")
    }

    fn system(value: &str) -> ConversationMessage {
        ConversationMessage::System(SystemMessage {
            id: message_id(value),
            text: value.to_owned(),
        })
    }

    fn user(value: &str) -> ConversationMessage {
        ConversationMessage::User(UserMessage {
            id: message_id(value),
            origin: Default::default(),
            transcript_visibility: Default::default(),
            parts: Vec::new(),
        })
    }

    fn summary(value: &str) -> ConversationMessage {
        ConversationMessage::ContextSummary(ContextSummaryMessage {
            id: message_id(value),
            text: value.to_owned(),
            model: None,
            usage: None,
            compacted_usage: None,
        })
    }

    #[test]
    fn repeated_compaction_preserves_product_history_and_moves_only_the_execution_boundary() {
        let prefix = system("system");
        let first = user("first");
        let second = user("second");
        let original =
            ConversationSnapshot::new(vec![prefix.clone(), first.clone(), second.clone()]);
        let first_replacement =
            ConversationSnapshot::new(vec![prefix.clone(), summary("summary-one"), second.clone()]);
        let first_product =
            merge_context_replacement_with_product_history(&original, &first_replacement)
                .expect("first compaction");
        assert_eq!(
            first_product.messages,
            vec![prefix.clone(), first, summary("summary-one"), second,]
        );
        assert_eq!(
            execution_context_from_product_history(&first_product),
            first_replacement
        );

        let third = user("third");
        let mut before_second = first_product.messages.clone();
        before_second.push(third.clone());
        let before_second = ConversationSnapshot::new(before_second);
        let second_replacement =
            ConversationSnapshot::new(vec![prefix.clone(), summary("summary-two"), third.clone()]);
        let second_product =
            merge_context_replacement_with_product_history(&before_second, &second_replacement)
                .expect("second compaction");
        assert_eq!(
            second_product.messages,
            vec![
                prefix.clone(),
                user("first"),
                summary("summary-one"),
                user("second"),
                summary("summary-two"),
                third,
            ]
        );
        assert_eq!(
            execution_context_from_product_history(&second_product),
            second_replacement
        );
    }
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
