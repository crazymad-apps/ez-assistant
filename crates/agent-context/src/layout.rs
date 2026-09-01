//! 规范历史块与 protected tail 布局。

use agent_types::{
    AssistantPart, ConversationMessage, ConversationSnapshot, ConversationValidationError,
};
use thiserror::Error;

/// 一份经过结构校验、按完整历史块切分的规范快照。
#[derive(Clone, Debug, PartialEq)]
pub struct ContextLayout {
    protected_prefix: Vec<ConversationMessage>,
    blocks: Vec<ContextBlock>,
}

impl ContextLayout {
    /// 校验快照并按 System prefix、Context Summary 和完整 User Turn 分块。
    pub fn build(snapshot: &ConversationSnapshot) -> Result<Self, ContextLayoutError> {
        snapshot
            .validate_tool_exchange_pairs()
            .map_err(ContextLayoutError::InvalidConversation)?;

        let mut protected_prefix = Vec::new();
        let mut blocks = Vec::new();
        let mut current_turn: Option<Vec<ConversationMessage>> = None;
        let mut summary_seen = false;
        let mut conversation_started = false;

        for message in &snapshot.messages {
            match message {
                ConversationMessage::System(_) if !conversation_started && !summary_seen => {
                    protected_prefix.push(message.clone());
                }
                ConversationMessage::System(_) => {
                    return Err(ContextLayoutError::SystemAfterConversationStart);
                }
                ConversationMessage::ContextSummary(_) => {
                    if conversation_started {
                        return Err(ContextLayoutError::MisplacedContextSummary);
                    }
                    if summary_seen {
                        return Err(ContextLayoutError::DuplicateContextSummary);
                    }
                    summary_seen = true;
                    blocks.push(ContextBlock::context_summary(message.clone()));
                }
                ConversationMessage::User(_) => {
                    if let Some(turn) = current_turn.take() {
                        // 下一条 User 已经开始，说明前一轮无论以普通 Assistant 回复还是
                        // 中断后的 Tool Result 结束，都已成为历史轮次。只有快照末尾的
                        // User Turn 才需要根据末条消息判断是否仍在执行。
                        blocks.push(ContextBlock::completed_user_turn(turn));
                    }
                    conversation_started = true;
                    current_turn = Some(vec![message.clone()]);
                }
                ConversationMessage::Assistant(_) | ConversationMessage::Tool(_) => {
                    conversation_started = true;
                    let Some(turn) = &mut current_turn else {
                        return Err(ContextLayoutError::MessageOutsideUserTurn);
                    };
                    turn.push(message.clone());
                }
            }
        }
        if let Some(turn) = current_turn {
            blocks.push(ContextBlock::user_turn(turn));
        }

        Ok(Self {
            protected_prefix,
            blocks,
        })
    }

    /// 始终原样保留的开头连续 System 消息。
    pub fn protected_prefix(&self) -> &[ConversationMessage] {
        &self.protected_prefix
    }

    /// Context Summary 与完整 User Turn 的有序原子块。
    pub fn blocks(&self) -> &[ContextBlock] {
        &self.blocks
    }

    /// 按 Rolling Summary 的最少近期轮次要求计算唯一 head/tail 边界。
    pub fn partition(&self, minimum_recent_user_turns: u32) -> ContextPartition<'_> {
        let mut split_index = self.blocks.len();
        let mut remaining = usize::try_from(minimum_recent_user_turns).unwrap_or(usize::MAX);

        for (index, block) in self.blocks.iter().enumerate().rev() {
            if block.kind == ContextBlockKind::UserTurn && remaining > 0 {
                split_index = index;
                remaining -= 1;
            }
        }

        if let Some((index, _)) = self
            .blocks
            .iter()
            .enumerate()
            .rev()
            .find(|(_, block)| block.kind == ContextBlockKind::UserTurn && block.is_active())
        {
            split_index = split_index.min(index);
        }

        ContextPartition {
            layout: self,
            split_index,
        }
    }
}

/// 可压缩历史中的原子块类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextBlockKind {
    /// 先前压缩生成的摘要；再次滚动时进入 head。
    ContextSummary,
    /// 从 UserMessage 开始、覆盖到下一 UserMessage 之前的完整轮次。
    UserTurn,
}

/// 一个不可从中间切开的规范历史块。
#[derive(Clone, Debug, PartialEq)]
pub struct ContextBlock {
    kind: ContextBlockKind,
    messages: Vec<ConversationMessage>,
    active: bool,
}

impl ContextBlock {
    fn context_summary(message: ConversationMessage) -> Self {
        Self {
            kind: ContextBlockKind::ContextSummary,
            messages: vec![message],
            active: false,
        }
    }

    fn completed_user_turn(messages: Vec<ConversationMessage>) -> Self {
        Self {
            kind: ContextBlockKind::UserTurn,
            messages,
            active: false,
        }
    }

    fn user_turn(messages: Vec<ConversationMessage>) -> Self {
        let active = !matches!(
            messages.last(),
            Some(ConversationMessage::Assistant(message))
                if !message
                    .parts
                    .iter()
                    .any(|part| matches!(part, AssistantPart::ToolCall(_)))
        );
        Self {
            kind: ContextBlockKind::UserTurn,
            messages,
            active,
        }
    }

    /// 返回块类别。
    pub fn kind(&self) -> ContextBlockKind {
        self.kind
    }

    /// 返回块内保持原顺序的完整消息。
    pub fn messages(&self) -> &[ConversationMessage] {
        &self.messages
    }

    /// 当前 User Turn 是否仍等待后续模型 Result。
    pub fn is_active(&self) -> bool {
        self.active
    }
}

/// 一次 Rolling Summary 使用的原子 head/tail 视图。
#[derive(Clone, Copy, Debug)]
pub struct ContextPartition<'a> {
    layout: &'a ContextLayout,
    split_index: usize,
}

impl<'a> ContextPartition<'a> {
    /// 始终原样保留的 System prefix。
    pub fn protected_prefix(&self) -> &'a [ConversationMessage] {
        self.layout.protected_prefix()
    }

    /// 可以整体交给压缩策略的较早历史块。
    pub fn compressible_head(&self) -> &'a [ContextBlock] {
        &self.layout.blocks[..self.split_index]
    }

    /// 必须原样进入 replacement 的近期历史块。
    pub fn protected_tail(&self) -> &'a [ContextBlock] {
        &self.layout.blocks[self.split_index..]
    }

    /// 是否存在至少一个可压缩原子块。
    pub fn has_compressible_head(&self) -> bool {
        self.split_index > 0
    }
}

/// 快照无法形成合法的原子历史布局。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ContextLayoutError {
    /// Tool Call/Result 不满足共享结构约束。
    #[error("invalid conversation: {0}")]
    InvalidConversation(ConversationValidationError),
    /// System 只能出现在开头连续 protected prefix。
    #[error("system message appears after conversation history started")]
    SystemAfterConversationStart,
    /// Context Summary 只能位于 System prefix 之后、首个 User Turn 之前。
    #[error("context summary appears after a user turn started")]
    MisplacedContextSummary,
    /// 有效快照最多包含一条最新 Context Summary。
    #[error("conversation contains more than one context summary")]
    DuplicateContextSummary,
    /// Assistant/Tool 消息必须归属于一个 User Turn。
    #[error("assistant or tool message appears outside a user turn")]
    MessageOutsideUserTurn,
}

#[cfg(test)]
mod tests {
    use agent_types::{
        AssistantMessage, ContextSummaryMessage, FinishReason, InternalContextPart, MessageId,
        ModelIdentity, OpaqueProviderState, PartId, ProtocolId, ProviderId, SystemMessage,
        TextPart, ToolCall, ToolCallId, ToolMessage, ToolName, ToolResult, ToolResultContent,
        ToolResultStatus, TranscriptVisibility, UserMessage, UserMessageOrigin, UserPart,
    };

    use super::*;

    fn id(value: &str) -> MessageId {
        MessageId::new(value).expect("valid message id")
    }

    fn user(value: &str) -> ConversationMessage {
        ConversationMessage::User(UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
            id: id(value),
            parts: vec![],
        })
    }

    fn assistant(value: &str) -> ConversationMessage {
        ConversationMessage::Assistant(AssistantMessage {
            id: id(value),
            model: ModelIdentity::new(
                ProviderId::new("test").expect("valid provider id"),
                "test-model",
            ),
            parts: vec![AssistantPart::Text(TextPart {
                id: PartId::new(format!("{value}_text")).expect("valid part id"),
                text: "done".to_owned(),
            })],
            finish_reason: FinishReason::Stop,
            usage: None,
        })
    }

    fn tool_call(value: &str, call_id: &str) -> ConversationMessage {
        ConversationMessage::Assistant(AssistantMessage {
            id: id(value),
            model: ModelIdentity::new(
                ProviderId::new("test").expect("valid provider id"),
                "test-model",
            ),
            parts: vec![AssistantPart::ToolCall(ToolCall {
                id: ToolCallId::new(call_id).expect("valid call id"),
                name: ToolName::new("lookup").expect("valid tool name"),
                arguments: serde_json::json!({}),
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        })
    }

    fn tool_result(value: &str, call_id: &str) -> ConversationMessage {
        ConversationMessage::Tool(ToolMessage {
            id: id(value),
            result: ToolResult {
                call_id: ToolCallId::new(call_id).expect("valid call id"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("ok".to_owned()),
                metadata: None,
            },
        })
    }

    fn assistant_with_provider_state(value: &str) -> ConversationMessage {
        ConversationMessage::Assistant(AssistantMessage {
            id: id(value),
            model: ModelIdentity::new(
                ProviderId::new("test").expect("valid provider id"),
                "test-model",
            ),
            parts: vec![AssistantPart::ProviderState(
                OpaqueProviderState::new(
                    ProviderId::new("test").expect("valid provider id"),
                    ProtocolId::new("chat").expect("valid protocol id"),
                    "continuation",
                    "application/octet-stream",
                    1,
                    vec![1],
                )
                .expect("valid provider state"),
            )],
            finish_reason: FinishReason::Stop,
            usage: None,
        })
    }

    #[test]
    fn layout_preserves_prefix_summary_turns_and_tool_exchange() {
        let snapshot = ConversationSnapshot::new(vec![
            ConversationMessage::System(SystemMessage {
                id: id("system_1"),
                text: "system".to_owned(),
            }),
            ConversationMessage::ContextSummary(ContextSummaryMessage {
                id: id("summary_1"),
                text: "summary".to_owned(),
                model: None,
                usage: None,
                compacted_usage: None,
            }),
            user("user_1"),
            assistant("assistant_1"),
            user("user_2"),
            tool_call("assistant_2a", "call_1"),
            tool_result("tool_1", "call_1"),
            assistant("assistant_2b"),
        ]);

        let layout = ContextLayout::build(&snapshot).expect("valid layout");
        assert_eq!(layout.protected_prefix().len(), 1);
        assert_eq!(
            layout
                .blocks()
                .iter()
                .map(ContextBlock::kind)
                .collect::<Vec<_>>(),
            vec![
                ContextBlockKind::ContextSummary,
                ContextBlockKind::UserTurn,
                ContextBlockKind::UserTurn,
            ]
        );
        assert_eq!(layout.blocks()[2].messages().len(), 4);

        let partition = layout.partition(1);
        assert_eq!(partition.compressible_head().len(), 2);
        assert_eq!(partition.protected_tail().len(), 1);
        assert!(partition.has_compressible_head());
    }

    #[test]
    fn hidden_runtime_user_message_remains_in_the_model_context_layout() {
        let hidden = ConversationMessage::User(UserMessage {
            id: id("runtime_user_1"),
            origin: UserMessageOrigin::Runtime,
            transcript_visibility: TranscriptVisibility::Hidden,
            parts: vec![UserPart::Injected(TextPart {
                id: PartId::new("runtime_user_1_injected").expect("valid part id"),
                text: "continue the active goal".to_owned(),
            })],
        });
        assert!(!hidden.is_transcript_visible());

        let snapshot = ConversationSnapshot::new(vec![hidden.clone(), assistant("assistant_1")]);
        let layout = ContextLayout::build(&snapshot).expect("valid layout");

        assert_eq!(layout.blocks().len(), 1);
        assert_eq!(layout.blocks()[0].kind(), ContextBlockKind::UserTurn);
        assert_eq!(
            layout.blocks()[0].messages(),
            &[hidden, assistant("assistant_1")]
        );
    }

    #[test]
    fn only_active_turn_forces_a_tail_while_old_provider_state_remains_compressible() {
        let active = ContextLayout::build(&ConversationSnapshot::new(vec![
            user("user_1"),
            assistant("assistant_1"),
            user("user_2"),
        ]))
        .expect("valid active layout");
        let partition = active.partition(0);
        assert_eq!(partition.compressible_head().len(), 1);
        assert!(partition.protected_tail()[0].is_active());

        let stateful = ContextLayout::build(&ConversationSnapshot::new(vec![
            user("user_1"),
            assistant_with_provider_state("assistant_1"),
            user("user_2"),
            assistant("assistant_2"),
        ]))
        .expect("valid stateful layout");
        let partition = stateful.partition(0);
        assert_eq!(partition.compressible_head().len(), 2);
        assert!(partition.protected_tail().is_empty());
    }

    #[test]
    fn interrupted_tool_turn_becomes_completed_when_the_next_user_turn_starts() {
        let layout = ContextLayout::build(&ConversationSnapshot::new(vec![
            user("user_1"),
            tool_call("assistant_1", "call_1"),
            tool_result("tool_1", "call_1"),
            user("user_2"),
            assistant("assistant_2"),
        ]))
        .expect("interrupted historical turn remains compactable");

        assert_eq!(layout.blocks().len(), 2);
        assert!(!layout.blocks()[0].is_active());
        assert!(!layout.blocks()[1].is_active());
        assert_eq!(layout.partition(1).compressible_head().len(), 1);
    }

    #[test]
    fn minimum_recent_turns_never_split_a_user_turn() {
        let layout = ContextLayout::build(&ConversationSnapshot::new(vec![
            ConversationMessage::ContextSummary(ContextSummaryMessage {
                id: id("summary_1"),
                text: "summary".to_owned(),
                model: None,
                usage: None,
                compacted_usage: None,
            }),
            user("user_1"),
            assistant("assistant_1"),
            user("user_2"),
            assistant("assistant_2"),
        ]))
        .expect("valid layout");

        let partition = layout.partition(10);
        assert_eq!(partition.compressible_head().len(), 1);
        assert_eq!(partition.protected_tail().len(), 2);
    }

    #[test]
    fn hidden_internal_messages_follow_the_same_latest_turn_rule() {
        let internal_user = |message_id: &str, boundary_id: &str, kind: &str| {
            ConversationMessage::User(UserMessage {
                id: id(message_id),
                origin: UserMessageOrigin::Runtime,
                transcript_visibility: TranscriptVisibility::Hidden,
                parts: vec![UserPart::InternalContext(
                    InternalContextPart::new(
                        PartId::new(format!("{message_id}-part")).expect("part id"),
                        boundary_id,
                        kind,
                        "context",
                    )
                    .expect("internal context"),
                )],
            })
        };
        let layout = ContextLayout::build(&ConversationSnapshot::new(vec![
            internal_user("user-proxy", "boundary-proxy", "proxy_report"),
            assistant("assistant-proxy"),
            user("user-ordinary"),
            assistant("assistant-ordinary"),
            internal_user("user-goal", "boundary-goal", "goal_continuation"),
            assistant("assistant-goal"),
        ]))
        .expect("valid layout");

        let partition = layout.partition(1);
        assert_eq!(partition.compressible_head().len(), 2);
        assert_eq!(partition.protected_tail().len(), 1);
        assert_eq!(
            partition.protected_tail()[0].messages()[0],
            internal_user("user-goal", "boundary-goal", "goal_continuation")
        );
    }

    #[test]
    fn structural_misplacements_are_rejected() {
        let system_after_user = ConversationSnapshot::new(vec![
            user("user_1"),
            assistant("assistant_1"),
            ConversationMessage::System(SystemMessage {
                id: id("system_1"),
                text: "late".to_owned(),
            }),
        ]);
        assert_eq!(
            ContextLayout::build(&system_after_user),
            Err(ContextLayoutError::SystemAfterConversationStart)
        );

        let assistant_without_user = ConversationSnapshot::new(vec![assistant("assistant_1")]);
        assert_eq!(
            ContextLayout::build(&assistant_without_user),
            Err(ContextLayoutError::MessageOutsideUserTurn)
        );

        let duplicate_summary = ConversationSnapshot::new(vec![
            ConversationMessage::ContextSummary(ContextSummaryMessage {
                id: id("summary_1"),
                text: "first".to_owned(),
                model: None,
                usage: None,
                compacted_usage: None,
            }),
            ConversationMessage::ContextSummary(ContextSummaryMessage {
                id: id("summary_2"),
                text: "second".to_owned(),
                model: None,
                usage: None,
                compacted_usage: None,
            }),
        ]);
        assert_eq!(
            ContextLayout::build(&duplicate_summary),
            Err(ContextLayoutError::DuplicateContextSummary)
        );

        let misplaced_summary = ConversationSnapshot::new(vec![
            user("user_1"),
            assistant("assistant_1"),
            ConversationMessage::ContextSummary(ContextSummaryMessage {
                id: id("summary_1"),
                text: "late".to_owned(),
                model: None,
                usage: None,
                compacted_usage: None,
            }),
        ]);
        assert_eq!(
            ContextLayout::build(&misplaced_summary),
            Err(ContextLayoutError::MisplacedContextSummary)
        );
    }

    #[test]
    fn invalid_tool_exchange_uses_the_shared_validator() {
        let call_id = ToolCallId::new("call_1").expect("valid call id");
        let snapshot =
            ConversationSnapshot::new(vec![user("user_1"), tool_call("assistant_1", "call_1")]);
        assert_eq!(
            ContextLayout::build(&snapshot),
            Err(ContextLayoutError::InvalidConversation(
                ConversationValidationError::MissingToolResult { call_id }
            ))
        );
    }
}
