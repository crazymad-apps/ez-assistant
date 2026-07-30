//! 可替换的上下文压缩策略契约。

use std::{future::Future, pin::Pin, sync::Arc};

use agent_model::{ModelError, ModelService};
use agent_types::{ConversationSnapshot, ModelIdentity, TokenUsage};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{ContextLayout, ReplacementValidationError};

/// 一次策略压缩 Future。
pub type CompactionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<StrategyOutcome, CompactionError>> + Send + 'a>>;

/// 可替换的上下文压缩策略。
pub trait CompressionStrategy: Send + Sync {
    /// 生成候选 replacement 或明确 NoOp；不提交 Checkpoint，也不续跑。
    fn compact<'a>(
        &'a self,
        input: CompactionInput,
        cancellation: CancellationToken,
    ) -> CompactionFuture<'a>;
}

/// 压缩策略所需的共享输入。
#[derive(Clone)]
pub struct CompactionInput {
    /// 当前 ExecutionSpec 使用的同一模型服务。
    pub model: Arc<dyn ModelService>,
    /// 正常模型请求使用的原始 system instructions；压缩请求必须保持相同前缀。
    pub instructions: Vec<String>,
    /// 已完成共享结构校验的历史布局。
    pub layout: ContextLayout,
}

/// 可以交给 Runtime 统一校验和提交的候选。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompactionCandidate {
    /// 策略生成的派生有效快照。
    pub replacement: ConversationSnapshot,
    /// 不含 Session、Run 或正文的策略报告。
    pub report: StrategyReport,
}

/// 策略执行结局。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum StrategyOutcome {
    /// 已生成候选，由 Runtime 决定是否提交。
    Candidate(CompactionCandidate),
    /// 当前布局没有可压缩 head，未生成摘要。
    NoOp {
        /// 可审计的策略报告。
        report: StrategyReport,
    },
}

/// 不包含 prompt、credential 或摘要正文的策略报告。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StrategyReport {
    /// 稳定策略名称。
    pub strategy: String,
    /// 被候选摘要替换的原子块数量。
    pub compressed_blocks: u32,
    /// 原样保留的原子块数量。
    pub retained_blocks: u32,
    /// 实际执行压缩请求的模型；NoOp 时为空。
    pub model: Option<ModelIdentity>,
    /// 压缩请求的 Provider usage；未调用或 Provider 未返回时为空。
    pub usage: Option<TokenUsage>,
}

/// 压缩策略无法生成受控结果。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CompactionError {
    /// 调用方取消了压缩。
    #[error("context compaction was cancelled")]
    Cancelled,
    /// 压缩模型调用失败。
    #[error(transparent)]
    Model(#[from] ModelError),
    /// 压缩模型返回了不满足摘要约束的 Result。
    #[error("invalid compaction response: {message}")]
    InvalidResponse {
        /// 不包含响应正文的脱敏诊断。
        message: String,
    },
    /// 候选 replacement 不满足共享提交前约束。
    #[error(transparent)]
    InvalidReplacement(#[from] ReplacementValidationError),
}

#[cfg(test)]
mod tests {
    use agent_model::{ModelCallContext, ModelCapabilities, ModelRequest, ModelStreamFuture};
    use futures_util::FutureExt;

    use super::*;

    struct NoopModel {
        capabilities: ModelCapabilities,
    }

    impl ModelService for NoopModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            100
        }

        fn stream(
            &self,
            _request: ModelRequest,
            _context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            Box::pin(std::future::ready(Err(ModelError::Config(
                "noop strategy model does not stream".to_owned(),
            ))))
        }
    }

    struct NoopStrategy;

    impl CompressionStrategy for NoopStrategy {
        fn compact<'a>(
            &'a self,
            _input: CompactionInput,
            cancellation: CancellationToken,
        ) -> CompactionFuture<'a> {
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(CompactionError::Cancelled);
                }
                Ok(StrategyOutcome::NoOp {
                    report: StrategyReport {
                        strategy: "noop-test".to_owned(),
                        compressed_blocks: 0,
                        retained_blocks: 0,
                        model: None,
                        usage: None,
                    },
                })
            })
        }
    }

    #[test]
    fn strategy_contract_distinguishes_noop_and_cancellation() {
        let input = CompactionInput {
            model: Arc::new(NoopModel {
                capabilities: ModelCapabilities::default(),
            }),
            instructions: vec!["normal instruction".to_owned()],
            layout: ContextLayout::build(&ConversationSnapshot::default())
                .expect("empty layout is valid"),
        };
        let outcome = NoopStrategy
            .compact(input.clone(), CancellationToken::new())
            .now_or_never()
            .expect("noop future must be ready")
            .expect("noop outcome");
        assert!(matches!(outcome, StrategyOutcome::NoOp { .. }));

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            NoopStrategy
                .compact(input, cancellation)
                .now_or_never()
                .expect("cancelled future must be ready"),
            Err(CompactionError::Cancelled)
        );
    }
}
