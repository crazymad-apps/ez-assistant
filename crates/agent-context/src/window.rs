//! 基于 Provider usage 的上下文窗口判断。

use agent_model::ModelService;
use agent_types::{
    ContextUsageAdjustment, ConversationMessage, ConversationSnapshot, ToolResultPart, UserPart,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// 唯一的上下文窗口判断入口。
#[derive(Clone, Debug, PartialEq)]
pub struct ContextWindowEvaluator {
    compaction_threshold_ratio: f64,
}

impl ContextWindowEvaluator {
    /// 使用压缩触发比例创建 Evaluator。
    pub fn new(compaction_threshold_ratio: f64) -> Result<Self, ContextWindowError> {
        if !compaction_threshold_ratio.is_finite()
            || compaction_threshold_ratio <= 0.0
            || compaction_threshold_ratio > 1.0
        {
            return Err(ContextWindowError::InvalidThresholdRatio);
        }
        Ok(Self {
            compaction_threshold_ratio,
        })
    }

    /// 返回构造期验证后的压缩触发比例。
    pub fn compaction_threshold_ratio(&self) -> f64 {
        self.compaction_threshold_ratio
    }

    /// 根据最近一条完整 Assistant Result 的真实 usage 判断窗口占用。
    pub fn evaluate(
        &self,
        snapshot: &ConversationSnapshot,
        model: &dyn ModelService,
    ) -> Result<ContextWindowEvaluation, ContextWindowError> {
        let context_window_tokens = model.context_window_tokens();
        if context_window_tokens == 0 {
            return Err(ContextWindowError::ZeroContextWindow);
        }

        let max_input_tokens = model.max_input_tokens();
        if max_input_tokens.is_some_and(|limit| limit == 0 || limit > context_window_tokens) {
            return Err(ContextWindowError::InvalidInputLimit);
        }

        let Some(used_tokens) = context_token_usage(snapshot).total_tokens() else {
            return Ok(ContextWindowEvaluation {
                used_tokens: None,
                context_window_tokens,
                max_input_tokens,
                used_ratio: None,
                decision: ContextWindowDecision::UsageUnavailable,
            });
        };

        let used_ratio = used_tokens as f64 / context_window_tokens as f64;
        // 输入上限保留为模型参数，不将其另行解释为自动压缩阈值。
        let decision = if used_ratio >= self.compaction_threshold_ratio {
            ContextWindowDecision::CompactionRequired
        } else {
            ContextWindowDecision::Ready
        };
        Ok(ContextWindowEvaluation {
            used_tokens: Some(used_tokens),
            context_window_tokens,
            max_input_tokens,
            used_ratio: Some(used_ratio),
            decision,
        })
    }
}

/// 一次窗口判断的可观察结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextWindowEvaluation {
    /// 最近完整 Assistant Result 报告的总 token，加上此后尚未被 Provider 计量的文本增量；usage 不可用时为空。
    pub used_tokens: Option<u64>,
    /// 当前模型服务显式配置的上下文窗口。
    pub context_window_tokens: u64,
    /// 当前模式独立声明的输入上限；未知时为空。
    #[serde(default)]
    pub max_input_tokens: Option<u64>,
    /// `used_tokens / context_window_tokens`；usage 不可用时为空。
    pub used_ratio: Option<f64>,
    /// 本次判断结论。
    pub decision: ContextWindowDecision,
}

/// 上下文占用的终态与临时态；从当前规范快照派生，不保存第二份可变账本。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextTokenUsage {
    /// 最近完整响应报告的实际总量；缺失时不借用更早 step 的用量。
    pub completed_tokens: Option<u64>,
    /// 该响应之后尚未被 Provider 计量的新增内容估算量。
    pub pending_tokens: u64,
}

impl ContextTokenUsage {
    /// 合并展示和窗口判断口径；终态未知时不把局部估算冒充完整占用。
    pub fn total_tokens(self) -> Option<u64> {
        self.completed_tokens
            .map(|completed| completed.saturating_add(self.pending_tokens))
    }
}

/// 分别投影已完成响应的终态用量和后续新增内容的临时用量。
///
/// 已完成响应以 Provider 报告的 `total_tokens` 为权威；只对该响应之后新追加、尚未经过
/// Provider 计量的 User/Tool 文本做轻量增量估算。ProviderState 已包含在产生它的响应用量中，
/// 不按密文字节数重复计费。
pub fn context_token_usage(snapshot: &ConversationSnapshot) -> ContextTokenUsage {
    let latest = snapshot
        .messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| match message {
            ConversationMessage::Assistant(message) => Some((index, message)),
            _ => None,
        });
    let adjustment =
        snapshot
            .messages
            .iter()
            .enumerate()
            .find_map(|(index, message)| match message {
                ConversationMessage::ContextSummary(message) => {
                    Some((index, message.usage_adjustment.as_ref(), message))
                }
                _ => None,
            });
    let completed_tokens = match adjustment {
        Some((_, Some(ContextUsageAdjustment::Unavailable), _)) => None,
        Some((
            summary_index,
            Some(ContextUsageAdjustment::Subtract {
                retained_assistant_id,
                subtract_total_tokens,
            }),
            summary,
        )) => adjusted_completed_tokens(
            snapshot,
            latest,
            summary_index,
            retained_assistant_id,
            *subtract_total_tokens,
            summary,
        ),
        _ => latest.and_then(|(_, message)| message.usage.as_ref().map(|usage| usage.total_tokens)),
    };
    // 新的完整响应到达后，边界前移：旧临时态由真实 usage 替换，不再叠加。
    let pending_start = latest.map_or(0, |(index, _)| index + 1);
    let pending_tokens = snapshot.messages[pending_start..]
        .iter()
        .map(estimate_unreported_message_tokens)
        .fold(0_u64, u64::saturating_add);
    ContextTokenUsage {
        completed_tokens,
        pending_tokens,
    }
}

fn adjusted_completed_tokens(
    snapshot: &ConversationSnapshot,
    latest: Option<(usize, &agent_types::AssistantMessage)>,
    summary_index: usize,
    retained_assistant_id: &agent_types::MessageId,
    subtract_total_tokens: u64,
    summary: &agent_types::ContextSummaryMessage,
) -> Option<u64> {
    let retained = snapshot.messages[summary_index + 1..]
        .iter()
        .enumerate()
        .find_map(|(offset, message)| match message {
            ConversationMessage::Assistant(message)
                if &message.id == retained_assistant_id && message.usage.is_some() =>
            {
                Some((summary_index + 1 + offset, message))
            }
            _ => None,
        })?;
    let (latest_index, latest) = latest?;
    let latest_usage = latest.usage.as_ref()?;
    if latest_index < retained.0 {
        return None;
    }
    if latest.id != retained.1.id {
        return Some(latest_usage.total_tokens);
    }
    Some(
        latest_usage
            .total_tokens
            .saturating_sub(subtract_total_tokens)
            .saturating_add(MESSAGE_OVERHEAD)
            .saturating_add(estimate_text_tokens(&summary.model_visible_text())),
    )
}

const MESSAGE_OVERHEAD: u64 = 4;
const PART_OVERHEAD: u64 = 2;

fn estimate_unreported_message_tokens(message: &ConversationMessage) -> u64 {
    match message {
        ConversationMessage::System(message) => {
            MESSAGE_OVERHEAD.saturating_add(estimate_text_tokens(&message.text))
        }
        ConversationMessage::ContextSummary(message) => {
            MESSAGE_OVERHEAD.saturating_add(estimate_text_tokens(&message.model_visible_text()))
        }
        ConversationMessage::User(message) => {
            message.parts.iter().fold(MESSAGE_OVERHEAD, |total, part| {
                let content = match part {
                    UserPart::Text(part) | UserPart::Injected(part) => {
                        estimate_text_tokens(&part.text)
                    }
                    UserPart::InternalContext(part) => estimate_text_tokens(&part.text),
                    UserPart::QuotedText(part) => estimate_text_tokens(&part.exact),
                    UserPart::FileReferences(part) => part.files.iter().fold(0_u64, |sum, file| {
                        sum.saturating_add(estimate_text_tokens(&file.original_name))
                            .saturating_add(estimate_text_tokens(&file.readable_path))
                    }),
                };
                total.saturating_add(PART_OVERHEAD).saturating_add(content)
            })
        }
        ConversationMessage::Tool(message) => {
            message
                .result
                .content
                .as_parts()
                .iter()
                .fold(MESSAGE_OVERHEAD, |total, part| {
                    let content = match part {
                        ToolResultPart::Text { text } => estimate_text_tokens(text),
                        ToolResultPart::Json { value } => estimate_json_tokens(value),
                        ToolResultPart::Image { image } => {
                            estimate_text_tokens(image.relative_path())
                                .saturating_add(estimate_text_tokens(image.media_type()))
                        }
                    };
                    total.saturating_add(PART_OVERHEAD).saturating_add(content)
                })
        }
        // latest_index 指向最后一条 Assistant；正常快照不会进入此分支。
        ConversationMessage::Assistant(_) => 0,
    }
}

fn estimate_text_tokens(text: &str) -> u64 {
    let (ascii, non_ascii) = text.chars().fold((0_u64, 0_u64), |(ascii, non_ascii), ch| {
        if ch.is_ascii() {
            (ascii.saturating_add(1), non_ascii)
        } else {
            (ascii, non_ascii.saturating_add(1))
        }
    });
    ascii.div_ceil(4).saturating_add(non_ascii)
}

fn estimate_json_tokens(value: &Value) -> u64 {
    match value {
        Value::Null => 1,
        Value::Bool(_) | Value::Number(_) => estimate_text_tokens(&value.to_string()),
        Value::String(value) => estimate_text_tokens(value),
        Value::Array(values) => values
            .iter()
            .map(estimate_json_tokens)
            .fold(2_u64, u64::saturating_add),
        Value::Object(values) => values.iter().fold(2_u64, |total, (key, value)| {
            total
                .saturating_add(estimate_text_tokens(key))
                .saturating_add(estimate_json_tokens(value))
        }),
    }
}

/// 窗口判断结论。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextWindowDecision {
    /// 当前已知占用低于阈值，可以继续。
    Ready,
    /// 当前已知占用达到或超过阈值，需要交给 Runtime 压缩。
    CompactionRequired,
    /// 最近 Assistant Result 没有 usage，继续调用并由 Provider Overflow 兜底。
    UsageUnavailable,
}

/// Evaluator 配置或模型窗口不满足约束。
#[derive(Clone, Debug, Error, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextWindowError {
    /// 压缩阈值必须是 `(0, 1]` 内的有限小数。
    #[error("compaction threshold ratio must be finite and within (0, 1]")]
    InvalidThresholdRatio,
    /// 模型服务必须显式提供非零上下文窗口。
    #[error("model context window must be greater than zero")]
    ZeroContextWindow,
    /// 独立输入上限必须为正整数，且不能超过当前上下文窗口。
    #[error("model input limit must be positive and within the context window")]
    InvalidInputLimit,
}

#[cfg(test)]
mod tests {
    use agent_model::{
        ModelCallContext, ModelCapabilities, ModelError, ModelRequest, ModelStreamFuture,
    };
    use agent_types::{
        AssistantMessage, AssistantPart, ContextSummaryMessage, ContextUsageAdjustment,
        ConversationMessage, ConversationSnapshot, FinishReason, MessageId, ModelIdentity,
        OpaqueProviderState, ProtocolId, ProviderId, TokenUsage, ToolCall, ToolCallId, ToolMessage,
        ToolName, ToolResult, ToolResultContent, ToolResultStatus, UserMessage, UserPart,
    };

    use super::*;

    struct WindowModel {
        capabilities: ModelCapabilities,
        context_window_tokens: u64,
        max_input_tokens: Option<u64>,
    }

    impl ModelService for WindowModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            self.context_window_tokens
        }

        fn max_input_tokens(&self) -> Option<u64> {
            self.max_input_tokens
        }

        fn stream(
            &self,
            _request: ModelRequest,
            _context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            Box::pin(std::future::ready(Err(ModelError::Config(
                "window test model does not stream".to_owned(),
            ))))
        }
    }

    fn model(context_window_tokens: u64) -> WindowModel {
        WindowModel {
            capabilities: ModelCapabilities::default(),
            context_window_tokens,
            max_input_tokens: None,
        }
    }

    fn assistant(id: &str, total_tokens: Option<u64>) -> ConversationMessage {
        ConversationMessage::Assistant(AssistantMessage {
            id: MessageId::new(id).expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("test").expect("valid provider id"),
                "test-model",
            ),
            parts: vec![],
            finish_reason: FinishReason::Stop,
            usage: total_tokens.map(|total_tokens| TokenUsage {
                input_tokens: total_tokens,
                output_tokens: 0,
                total_tokens,
                cached_input_tokens: None,
                reasoning_tokens: None,
            }),
        })
    }

    fn assistant_with_provider_state(
        total_tokens: u64,
        payload_bytes: usize,
    ) -> ConversationMessage {
        ConversationMessage::Assistant(AssistantMessage {
            id: MessageId::new("assistant_state").expect("message id"),
            model: ModelIdentity::new(ProviderId::new("test").expect("provider"), "test-model"),
            parts: vec![AssistantPart::ProviderState(
                OpaqueProviderState::new(
                    ProviderId::new("test").expect("provider"),
                    ProtocolId::new("responses").expect("protocol"),
                    "opaque",
                    "application/json",
                    1,
                    vec![0; payload_bytes],
                )
                .expect("provider state"),
            )],
            finish_reason: FinishReason::Stop,
            usage: Some(TokenUsage {
                input_tokens: total_tokens,
                output_tokens: 0,
                total_tokens,
                cached_input_tokens: None,
                reasoning_tokens: None,
            }),
        })
    }

    fn user(id: &str) -> ConversationMessage {
        ConversationMessage::User(UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
            id: MessageId::new(id).expect("valid message id"),
            parts: vec![],
        })
    }

    fn user_text(id: &str, text: &str) -> ConversationMessage {
        ConversationMessage::User(UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
            id: MessageId::new(id).expect("valid message id"),
            parts: vec![UserPart::Text(agent_types::TextPart {
                id: agent_types::PartId::new(format!("{id}_text")).expect("valid part id"),
                text: text.to_owned(),
            })],
        })
    }

    fn summary(adjustment: ContextUsageAdjustment) -> ConversationMessage {
        ConversationMessage::ContextSummary(ContextSummaryMessage {
            id: MessageId::new("summary_1").expect("message id"),
            text: "abcd".to_owned(),
            model: None,
            usage: None,
            compacted_usage: None,
            usage_adjustment: Some(adjustment),
            programmatic_context: None,
        })
    }

    fn assistant_tool_call(id: &str, total_tokens: u64) -> ConversationMessage {
        ConversationMessage::Assistant(AssistantMessage {
            id: MessageId::new(id).expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("test").expect("valid provider id"),
                "test-model",
            ),
            parts: vec![AssistantPart::ToolCall(ToolCall {
                id: ToolCallId::new("call_1").expect("valid call id"),
                name: ToolName::new("lookup").expect("valid tool name"),
                arguments: serde_json::json!({}),
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: Some(TokenUsage {
                input_tokens: total_tokens,
                output_tokens: 0,
                total_tokens,
                cached_input_tokens: None,
                reasoning_tokens: None,
            }),
        })
    }

    fn tool_result() -> ConversationMessage {
        ConversationMessage::Tool(ToolMessage {
            id: MessageId::new("tool_1").expect("valid message id"),
            result: ToolResult {
                call_id: ToolCallId::new("call_1").expect("valid call id"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("ok".to_owned()),
                metadata: None,
            },
        })
    }

    #[test]
    fn constructor_rejects_invalid_ratios_and_accepts_upper_bound() {
        for ratio in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 0.0, 1.1] {
            assert_eq!(
                ContextWindowEvaluator::new(ratio),
                Err(ContextWindowError::InvalidThresholdRatio)
            );
        }
        assert_eq!(
            ContextWindowEvaluator::new(1.0)
                .expect("upper bound is valid")
                .compaction_threshold_ratio(),
            1.0
        );
    }

    #[test]
    fn threshold_is_inclusive() {
        let evaluator = ContextWindowEvaluator::new(0.8).expect("valid evaluator");
        let snapshot = ConversationSnapshot::new(vec![assistant("assistant_1", Some(80))]);

        assert_eq!(
            evaluator
                .evaluate(&snapshot, &model(100))
                .expect("evaluation"),
            ContextWindowEvaluation {
                used_tokens: Some(80),
                context_window_tokens: 100,
                max_input_tokens: None,
                used_ratio: Some(0.8),
                decision: ContextWindowDecision::CompactionRequired,
            }
        );
    }

    #[test]
    fn input_limit_does_not_change_the_context_compaction_threshold() {
        let evaluator = ContextWindowEvaluator::new(0.8).unwrap();
        let mut limited = model(1000);
        limited.max_input_tokens = Some(100);
        for (usage, expected) in [
            (79, ContextWindowDecision::Ready),
            (80, ContextWindowDecision::Ready),
            (800, ContextWindowDecision::CompactionRequired),
        ] {
            let snapshot = ConversationSnapshot::new(vec![assistant("latest", Some(usage))]);
            let result = evaluator.evaluate(&snapshot, &limited).unwrap();
            assert_eq!(result.decision, expected);
            assert_eq!(result.context_window_tokens, 1000);
            assert_eq!(result.max_input_tokens, Some(100));
            assert_eq!(result.used_ratio, Some(usage as f64 / 1000.0));
        }
        let missing = ConversationSnapshot::new(vec![assistant("latest", None)]);
        assert_eq!(
            evaluator.evaluate(&missing, &limited).unwrap().decision,
            ContextWindowDecision::UsageUnavailable
        );
        for invalid in [0, 1001] {
            limited.max_input_tokens = Some(invalid);
            assert_eq!(
                evaluator.evaluate(&missing, &limited),
                Err(ContextWindowError::InvalidInputLimit)
            );
        }
    }

    #[test]
    fn latest_assistant_usage_is_authoritative() {
        let evaluator = ContextWindowEvaluator::new(0.8).expect("valid evaluator");
        let snapshot = ConversationSnapshot::new(vec![
            assistant("assistant_1", Some(90)),
            user("user_2"),
            assistant("assistant_2", Some(20)),
        ]);

        assert_eq!(
            evaluator
                .evaluate(&snapshot, &model(100))
                .expect("evaluation")
                .decision,
            ContextWindowDecision::Ready
        );
    }

    #[test]
    fn opaque_state_bytes_are_not_counted_twice() {
        let evaluator = ContextWindowEvaluator::new(0.8).expect("valid evaluator");
        let snapshot =
            ConversationSnapshot::new(vec![user("user_1"), assistant_with_provider_state(70, 10)]);

        let evaluation = evaluator
            .evaluate(&snapshot, &model(100))
            .expect("evaluation");
        assert_eq!(evaluation.used_tokens, Some(70));
        assert_eq!(evaluation.decision, ContextWindowDecision::Ready);
    }

    #[test]
    fn unreported_user_text_is_added_to_the_latest_provider_total() {
        let evaluator = ContextWindowEvaluator::new(0.8).expect("valid evaluator");
        let snapshot = ConversationSnapshot::new(vec![
            assistant("assistant_1", Some(70)),
            user_text("user_2", "abcdefghijklmnopqrst"),
        ]);

        let evaluation = evaluator
            .evaluate(&snapshot, &model(100))
            .expect("evaluation");
        assert_eq!(evaluation.used_tokens, Some(81));
        assert_eq!(
            evaluation.decision,
            ContextWindowDecision::CompactionRequired
        );
    }

    #[test]
    fn completed_step_replaces_pending_estimate_even_when_actual_usage_is_lower() {
        let mut snapshot =
            ConversationSnapshot::new(vec![assistant_tool_call("assistant_1", 70), tool_result()]);
        let pending = context_token_usage(&snapshot);
        assert_eq!(pending.completed_tokens, Some(70));
        assert_eq!(pending.pending_tokens, 7);
        assert_eq!(pending.total_tokens(), Some(77));

        snapshot.messages.push(assistant("assistant_2", Some(72)));
        assert_eq!(
            context_token_usage(&snapshot),
            ContextTokenUsage {
                completed_tokens: Some(72),
                pending_tokens: 0,
            }
        );
        snapshot.messages.push(user_text("user_3", "abcdefgh"));
        let pending = context_token_usage(&snapshot);
        assert_eq!(pending.completed_tokens, Some(72));
        assert_eq!(pending.pending_tokens, 8);
        assert_eq!(pending.total_tokens(), Some(80));
    }

    #[test]
    fn summary_subtraction_uses_retained_usage_plus_rendered_summary_and_pending_tail() {
        let snapshot = ConversationSnapshot::new(vec![
            summary(ContextUsageAdjustment::Subtract {
                retained_assistant_id: MessageId::new("retained").expect("message id"),
                subtract_total_tokens: 60,
            }),
            user("user_1"),
            assistant("retained", Some(100)),
            user_text("user_2", "abcdefgh"),
        ]);

        assert_eq!(
            context_token_usage(&snapshot),
            ContextTokenUsage {
                completed_tokens: Some(45),
                pending_tokens: 8,
            }
        );
    }

    #[test]
    fn assistant_after_the_retained_boundary_replaces_the_usage_approximation() {
        let snapshot = ConversationSnapshot::new(vec![
            summary(ContextUsageAdjustment::Subtract {
                retained_assistant_id: MessageId::new("retained").expect("message id"),
                subtract_total_tokens: 60,
            }),
            user("user_1"),
            assistant("retained", Some(100)),
            user("user_2"),
            assistant("latest", Some(50)),
        ]);

        assert_eq!(
            context_token_usage(&snapshot),
            ContextTokenUsage {
                completed_tokens: Some(50),
                pending_tokens: 0,
            }
        );
    }

    #[test]
    fn unavailable_summary_adjustment_fails_safe_without_reusing_retained_usage() {
        let snapshot = ConversationSnapshot::new(vec![
            summary(ContextUsageAdjustment::Unavailable),
            user("user_1"),
            assistant("retained", Some(100)),
        ]);

        assert_eq!(context_token_usage(&snapshot).total_tokens(), None);
    }

    #[test]
    fn missing_completed_usage_keeps_pending_separate_without_reusing_old_base() {
        let snapshot = ConversationSnapshot::new(vec![
            assistant("assistant_1", Some(90)),
            user_text("user_2", "previous pending"),
            assistant("assistant_2", None),
            user_text("user_3", "abcdefgh"),
        ]);
        let usage = context_token_usage(&snapshot);
        assert_eq!(usage.completed_tokens, None);
        assert_eq!(usage.pending_tokens, 8);
        assert_eq!(usage.total_tokens(), None);
    }

    #[test]
    fn missing_latest_usage_does_not_fall_back_to_older_results() {
        let evaluator = ContextWindowEvaluator::new(0.8).expect("valid evaluator");
        let snapshot = ConversationSnapshot::new(vec![
            assistant("assistant_1", Some(90)),
            user("user_2"),
            assistant("assistant_2", None),
        ]);

        assert_eq!(
            evaluator
                .evaluate(&snapshot, &model(100))
                .expect("evaluation"),
            ContextWindowEvaluation {
                used_tokens: None,
                context_window_tokens: 100,
                max_input_tokens: None,
                used_ratio: None,
                decision: ContextWindowDecision::UsageUnavailable,
            }
        );
    }

    #[test]
    fn trailing_tool_results_do_not_hide_latest_assistant_usage() {
        let evaluator = ContextWindowEvaluator::new(0.8).expect("valid evaluator");
        let snapshot = ConversationSnapshot::new(vec![
            user("user_1"),
            assistant_tool_call("assistant_1", 80),
            tool_result(),
        ]);

        assert_eq!(
            evaluator
                .evaluate(&snapshot, &model(100))
                .expect("evaluation")
                .decision,
            ContextWindowDecision::CompactionRequired
        );
    }

    #[test]
    fn no_assistant_usage_is_unavailable_and_zero_window_is_an_error() {
        let evaluator = ContextWindowEvaluator::new(0.8).expect("valid evaluator");
        let snapshot = ConversationSnapshot::new(vec![user("user_1")]);

        assert_eq!(
            evaluator
                .evaluate(&snapshot, &model(100))
                .expect("evaluation")
                .decision,
            ContextWindowDecision::UsageUnavailable
        );
        assert_eq!(
            evaluator.evaluate(&snapshot, &model(0)),
            Err(ContextWindowError::ZeroContextWindow)
        );
    }
}
