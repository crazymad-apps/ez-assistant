//! replacement 特有完整性校验，复用 `agent-types` 的 Tool 配对校验。

use agent_types::{ConversationMessage, ConversationSnapshot, MessageId};
use thiserror::Error;

use crate::{ContextBlockKind, ContextLayout, ContextLayoutError};

/// 校验压缩策略生成的候选 replacement。
///
/// replacement 必须是 `System prefix → 一条非空 Context Summary → recent User Turn`
/// 的合法快照，并且派生快照中的旧 Assistant usage 已被清空。
pub fn validate_replacement(
    replacement: &ConversationSnapshot,
) -> Result<(), ReplacementValidationError> {
    let layout =
        ContextLayout::build(replacement).map_err(ReplacementValidationError::InvalidLayout)?;
    let Some(summary_block) = layout.blocks().first() else {
        return Err(ReplacementValidationError::MissingContextSummary);
    };
    if summary_block.kind() != ContextBlockKind::ContextSummary {
        return Err(ReplacementValidationError::MissingContextSummary);
    }
    let Some(ConversationMessage::ContextSummary(summary)) = summary_block.messages().first()
    else {
        return Err(ReplacementValidationError::MissingContextSummary);
    };
    if summary.text.trim().is_empty() {
        return Err(ReplacementValidationError::EmptyContextSummary);
    }

    for message in &replacement.messages {
        if let ConversationMessage::Assistant(message) = message
            && message.usage.is_some()
        {
            return Err(ReplacementValidationError::RetainedAssistantUsage {
                message_id: message.id.clone(),
            });
        }
    }
    Ok(())
}

/// 压缩候选 replacement 不满足提交前约束。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ReplacementValidationError {
    /// replacement 不是合法的规范历史布局。
    #[error("invalid replacement layout: {0}")]
    InvalidLayout(ContextLayoutError),
    /// replacement 必须包含一条最新 Context Summary。
    #[error("replacement is missing a context summary")]
    MissingContextSummary,
    /// Context Summary 不能是空白文本。
    #[error("replacement context summary must not be empty")]
    EmptyContextSummary,
    /// replacement 中保留的旧 Assistant Result 不能继续携带压缩前 usage。
    #[error("replacement retains usage on assistant message `{message_id}`")]
    RetainedAssistantUsage {
        /// 仍携带 usage 的消息。
        message_id: MessageId,
    },
}

#[cfg(test)]
mod tests {
    use agent_types::{
        AssistantMessage, AssistantPart, ContextSummaryMessage, FinishReason, ModelIdentity,
        ProviderId, TokenUsage, ToolCall, ToolCallId, ToolName, UserMessage,
    };

    use super::*;

    fn id(value: &str) -> MessageId {
        MessageId::new(value).expect("valid message id")
    }

    fn assistant(usage: Option<TokenUsage>) -> ConversationMessage {
        ConversationMessage::Assistant(AssistantMessage {
            id: id("assistant_1"),
            model: ModelIdentity::new(
                ProviderId::new("test").expect("valid provider id"),
                "test-model",
            ),
            parts: vec![],
            finish_reason: FinishReason::Stop,
            usage,
        })
    }

    fn replacement(summary: &str, usage: Option<TokenUsage>) -> ConversationSnapshot {
        ConversationSnapshot::new(vec![
            ConversationMessage::ContextSummary(ContextSummaryMessage {
                id: id("summary_1"),
                text: summary.to_owned(),
                model: None,
                usage: None,
                compacted_usage: None,
            }),
            ConversationMessage::User(UserMessage {
                id: id("user_1"),
                parts: vec![],
            }),
            assistant(usage),
        ])
    }

    #[test]
    fn valid_replacement_is_accepted() {
        assert_eq!(validate_replacement(&replacement("summary", None)), Ok(()));
    }

    #[test]
    fn summary_is_required_and_must_not_be_blank() {
        let missing = ConversationSnapshot::new(vec![
            ConversationMessage::User(UserMessage {
                id: id("user_1"),
                parts: vec![],
            }),
            assistant(None),
        ]);
        assert_eq!(
            validate_replacement(&missing),
            Err(ReplacementValidationError::MissingContextSummary)
        );
        assert_eq!(
            validate_replacement(&replacement("  ", None)),
            Err(ReplacementValidationError::EmptyContextSummary)
        );
    }

    #[test]
    fn retained_assistant_usage_is_rejected() {
        let usage = TokenUsage {
            input_tokens: 8,
            output_tokens: 2,
            total_tokens: 10,
            cached_input_tokens: None,
            reasoning_tokens: None,
        };
        assert_eq!(
            validate_replacement(&replacement("summary", Some(usage))),
            Err(ReplacementValidationError::RetainedAssistantUsage {
                message_id: id("assistant_1"),
            })
        );
    }

    #[test]
    fn invalid_tool_exchange_is_rejected_before_replacement_checks() {
        let call_id = ToolCallId::new("call_1").expect("valid call id");
        let invalid = ConversationSnapshot::new(vec![
            ConversationMessage::ContextSummary(ContextSummaryMessage {
                id: id("summary_1"),
                text: "summary".to_owned(),
                model: None,
                usage: None,
                compacted_usage: None,
            }),
            ConversationMessage::User(UserMessage {
                id: id("user_1"),
                parts: vec![],
            }),
            ConversationMessage::Assistant(AssistantMessage {
                id: id("assistant_1"),
                model: ModelIdentity::new(
                    ProviderId::new("test").expect("valid provider id"),
                    "test-model",
                ),
                parts: vec![AssistantPart::ToolCall(ToolCall {
                    id: call_id.clone(),
                    name: ToolName::new("lookup").expect("valid tool name"),
                    arguments: serde_json::json!({}),
                })],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            }),
        ]);

        assert_eq!(
            validate_replacement(&invalid),
            Err(ReplacementValidationError::InvalidLayout(
                ContextLayoutError::InvalidConversation(
                    agent_types::ConversationValidationError::MissingToolResult { call_id }
                )
            ))
        );
    }
}
