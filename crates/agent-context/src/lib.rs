//! Provider-neutral 的共享模型上下文能力。
//!
//! 本 crate 负责上下文窗口判断、规范历史布局、replacement 校验和可替换压缩策略。
//! v0.3.0 的正式调用方是 `agent-core`；临时 `runtime-harness` 直接装配其余能力完成
//! 版本验证。正式 Runtime 的 Session、Run、Checkpoint 与调度接口留待总体设计。

mod layout;
mod rolling;
mod strategy;
mod validate;
mod window;

pub use layout::{
    ContextBlock, ContextBlockKind, ContextLayout, ContextLayoutError, ContextPartition,
};
pub use rolling::{RollingSummaryPolicy, RollingSummaryPolicyError, RollingSummarySameModel};
pub use strategy::{
    CompactionCandidate, CompactionError, CompactionFuture, CompactionInput, CompressionStrategy,
    StrategyOutcome, StrategyReport,
};
pub use validate::{
    ReplacementEffectError, ReplacementValidationError, validate_replacement,
    validate_replacement_effect,
};
pub use window::{
    ContextTokenUsage, ContextUsageAccuracy, ContextWindowDecision, ContextWindowError,
    ContextWindowEvaluation, ContextWindowEvaluator, context_token_usage,
};

/// 根据冻结机制选择有效上下文；原生切取与工具配对复用 Types 的共同操作。
///
/// # Errors
/// 输入结构无效，或本地机制混入原生替代状态时返回稳定模型错误。
pub fn project_effective_context(
    snapshot: &agent_types::ConversationSnapshot,
    policy: &agent_model::ContextPolicy,
) -> Result<agent_types::ConversationSnapshot, agent_model::ModelError> {
    match policy {
        agent_model::ContextPolicy::LocalSummary => {
            if snapshot.messages.iter().any(|message| matches!(message,
                agent_types::ConversationMessage::Assistant(assistant) if assistant.parts.iter().any(|part| matches!(part,
                    agent_types::AssistantPart::ProviderState(state) if state.context_effect().is_some()
                ))
            )) {
                return Err(agent_model::ModelError::Config("native context is incompatible with local summary mode".into()));
            }
            snapshot.validate_tool_exchange_pairs().map_err(|_| {
                agent_model::ModelError::Protocol("invalid effective context".into())
            })?;
            Ok(snapshot.clone())
        }
        agent_model::ContextPolicy::ServerCompaction { .. } => {
            snapshot.project_provider_context().map_err(|_| {
                agent_model::ModelError::Protocol("invalid native effective context".into())
            })
        }
    }
}
