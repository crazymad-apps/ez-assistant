//! 基于 Provider usage 的上下文窗口判断。

use agent_model::ModelService;
use agent_types::{ConversationMessage, ConversationSnapshot};
use serde::{Deserialize, Serialize};
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

        let latest_assistant = snapshot
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                ConversationMessage::Assistant(message) => Some(message),
                _ => None,
            });
        let usage = latest_assistant.and_then(|message| message.usage.as_ref());

        let Some(usage) = usage else {
            return Ok(ContextWindowEvaluation {
                used_tokens: None,
                context_window_tokens,
                used_ratio: None,
                decision: ContextWindowDecision::UsageUnavailable,
            });
        };

        let used_tokens = usage.total_tokens;
        let used_ratio = used_tokens as f64 / context_window_tokens as f64;
        let decision = if used_ratio >= self.compaction_threshold_ratio {
            ContextWindowDecision::CompactionRequired
        } else {
            ContextWindowDecision::Ready
        };
        Ok(ContextWindowEvaluation {
            used_tokens: Some(used_tokens),
            context_window_tokens,
            used_ratio: Some(used_ratio),
            decision,
        })
    }
}

/// 一次窗口判断的可观察结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextWindowEvaluation {
    /// 最近完整 Assistant Result 报告的总 token；usage 不可用时为空。
    pub used_tokens: Option<u64>,
    /// 当前模型服务显式配置的上下文窗口。
    pub context_window_tokens: u64,
    /// `used_tokens / context_window_tokens`；usage 不可用时为空。
    pub used_ratio: Option<f64>,
    /// 本次判断结论。
    pub decision: ContextWindowDecision,
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
}

#[cfg(test)]
mod tests {
    use agent_model::{
        ModelCallContext, ModelCapabilities, ModelError, ModelRequest, ModelStreamFuture,
    };
    use agent_types::{
        AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FinishReason,
        MessageId, ModelIdentity, ProviderId, TokenUsage, ToolCall, ToolCallId, ToolMessage,
        ToolName, ToolResult, ToolResultContent, ToolResultStatus, UserMessage,
    };

    use super::*;

    struct WindowModel {
        capabilities: ModelCapabilities,
        context_window_tokens: u64,
    }

    impl ModelService for WindowModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            self.context_window_tokens
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

    fn user(id: &str) -> ConversationMessage {
        ConversationMessage::User(UserMessage {
            id: MessageId::new(id).expect("valid message id"),
            parts: vec![],
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
                content: ToolResultContent::Text("ok".to_owned()),
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
                used_ratio: Some(0.8),
                decision: ContextWindowDecision::CompactionRequired,
            }
        );
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
