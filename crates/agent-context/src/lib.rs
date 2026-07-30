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
pub use validate::{ReplacementValidationError, validate_replacement};
pub use window::{
    ContextWindowDecision, ContextWindowError, ContextWindowEvaluation, ContextWindowEvaluator,
};
