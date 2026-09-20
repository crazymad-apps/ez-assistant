//! replacement 特有完整性校验，复用 `agent-types` 的 Tool 配对校验。

use agent_types::{ContextUsageAdjustment, ConversationMessage, ConversationSnapshot};
use thiserror::Error;

use crate::{ContextBlockKind, ContextLayout, ContextLayoutError};

/// 校验 replacement 在紧凑 JSON 表示下确实小于源上下文。
///
/// Runtime 在策略候选中补入程序化上下文后会再次调用本函数，避免策略层的首次检查
/// 被后续确定性字段增长所绕过。
pub fn validate_replacement_effect(
    source: &ConversationSnapshot,
    replacement: &ConversationSnapshot,
) -> Result<(), ReplacementEffectError> {
    let source_bytes = compact_json_bytes(source)?;
    let replacement_bytes = compact_json_bytes(replacement)?;
    if replacement_bytes >= source_bytes {
        return Err(ReplacementEffectError::Ineffective {
            source_bytes,
            replacement_bytes,
        });
    }
    Ok(())
}

fn compact_json_bytes(snapshot: &ConversationSnapshot) -> Result<usize, ReplacementEffectError> {
    serde_json::to_vec(snapshot)
        .map(|encoded| encoded.len())
        .map_err(|_| ReplacementEffectError::Serialization)
}

/// replacement 的体积效果无法被确认。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ReplacementEffectError {
    /// 快照无法序列化为紧凑 JSON。
    #[error("context snapshot could not be serialized for replacement size validation")]
    Serialization,
    /// replacement 没有缩小上下文。
    #[error(
        "context replacement is not smaller than its source ({replacement_bytes} >= {source_bytes} bytes)"
    )]
    Ineffective {
        /// 源上下文的紧凑 JSON 字节数。
        source_bytes: usize,
        /// replacement 的紧凑 JSON 字节数。
        replacement_bytes: usize,
    },
}

/// 校验压缩策略生成的候选 replacement。
///
/// replacement 必须是 `System prefix → 一条非空 Context Summary → recent User Turn`
/// 的合法快照。
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
    if let Some(ContextUsageAdjustment::Subtract {
        retained_assistant_id,
        ..
    }) = &summary.usage_adjustment
    {
        let summary_index = replacement
            .messages
            .iter()
            .position(|message| matches!(message, ConversationMessage::ContextSummary(_)))
            .expect("validated layout contains summary");
        let valid_reference = replacement.messages[summary_index + 1..]
            .iter()
            .any(|message| match message {
                ConversationMessage::Assistant(message) => {
                    &message.id == retained_assistant_id && message.usage.is_some()
                }
                _ => false,
            });
        if !valid_reference {
            return Err(
                ReplacementValidationError::InvalidUsageAdjustmentReference {
                    retained_assistant_id: retained_assistant_id.clone(),
                },
            );
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
    /// Subtract 必须指向摘要之后仍被保留、且带 Provider usage 的 Assistant。
    #[error(
        "context summary usage adjustment references missing or unmeasured assistant `{retained_assistant_id}`"
    )]
    InvalidUsageAdjustmentReference {
        /// 无法在 replacement 中解析的 Assistant ID。
        retained_assistant_id: agent_types::MessageId,
    },
}

#[cfg(test)]
mod tests {
    use agent_types::{
        AssistantMessage, AssistantPart, ContextSummaryMessage, FinishReason, MessageId,
        ModelIdentity, ProviderId, TokenUsage, ToolCall, ToolCallId, ToolName, UserMessage,
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
                usage_adjustment: None,
                programmatic_context: None,
            }),
            ConversationMessage::User(UserMessage {
                origin: Default::default(),
                transcript_visibility: Default::default(),
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
    fn replacement_effect_requires_a_strictly_smaller_snapshot() {
        let source = replacement(&"history ".repeat(100), None);
        let smaller = replacement("summary", None);
        assert_eq!(validate_replacement_effect(&source, &smaller), Ok(()));

        assert_eq!(
            validate_replacement_effect(&smaller, &source),
            Err(ReplacementEffectError::Ineffective {
                source_bytes: serde_json::to_vec(&smaller).unwrap().len(),
                replacement_bytes: serde_json::to_vec(&source).unwrap().len(),
            })
        );
    }

    #[test]
    fn summary_is_required_and_must_not_be_blank() {
        let missing = ConversationSnapshot::new(vec![
            ConversationMessage::User(UserMessage {
                origin: Default::default(),
                transcript_visibility: Default::default(),
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
    fn retained_assistant_usage_is_accepted() {
        let usage = TokenUsage {
            input_tokens: 8,
            output_tokens: 2,
            total_tokens: 10,
            cached_input_tokens: None,
            reasoning_tokens: None,
        };
        assert_eq!(
            validate_replacement(&replacement("summary", Some(usage))),
            Ok(())
        );
    }

    #[test]
    fn subtraction_requires_a_retained_assistant_with_usage_after_the_summary() {
        let mut candidate = replacement("summary", None);
        let ConversationMessage::ContextSummary(summary) = &mut candidate.messages[0] else {
            unreachable!();
        };
        summary.usage_adjustment = Some(ContextUsageAdjustment::Subtract {
            retained_assistant_id: id("assistant_1"),
            subtract_total_tokens: 5,
        });
        assert_eq!(
            validate_replacement(&candidate),
            Err(
                ReplacementValidationError::InvalidUsageAdjustmentReference {
                    retained_assistant_id: id("assistant_1"),
                }
            )
        );

        let ConversationMessage::Assistant(assistant) = &mut candidate.messages[2] else {
            unreachable!();
        };
        assistant.usage = Some(TokenUsage {
            input_tokens: 8,
            output_tokens: 2,
            total_tokens: 10,
            cached_input_tokens: None,
            reasoning_tokens: None,
        });
        assert_eq!(validate_replacement(&candidate), Ok(()));
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
                usage_adjustment: None,
                programmatic_context: None,
            }),
            ConversationMessage::User(UserMessage {
                origin: Default::default(),
                transcript_visibility: Default::default(),
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
