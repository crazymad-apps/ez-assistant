//! Core Demo 私有的上下文压缩协调器。
//!
//! 公共 `agent-context` 只生成候选 replacement；是否提交 Journal、是否续跑以及
//! 交接次数上限仍由宿主负责。

use std::sync::Arc;

use agent_context::{
    CompactionInput, CompressionStrategy, ContextLayout, RollingSummaryPolicy,
    RollingSummarySameModel, StrategyOutcome, validate_replacement,
};
use agent_model::{ModelService, SystemPromptSnapshot};
use agent_types::ConversationSnapshot;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const SUMMARY_OUTPUT_TOKENS: u32 = 1_024;
const MINIMUM_RECENT_USER_TURNS: u32 = 1;

pub(crate) struct CompactionCoordinator {
    strategy: Arc<dyn CompressionStrategy>,
}

impl Default for CompactionCoordinator {
    fn default() -> Self {
        let policy = RollingSummaryPolicy::new(SUMMARY_OUTPUT_TOKENS, MINIMUM_RECENT_USER_TURNS)
            .expect("static compaction policy is valid");
        Self {
            strategy: Arc::new(RollingSummarySameModel::new(policy)),
        }
    }
}

impl CompactionCoordinator {
    pub(crate) async fn compact(
        &self,
        model: Arc<dyn ModelService>,
        system_prompt: SystemPromptSnapshot,
        checkpoint: ConversationSnapshot,
        cancellation: CancellationToken,
    ) -> Result<ConversationSnapshot, CompactionCoordinatorError> {
        let layout = ContextLayout::build(&checkpoint)
            .map_err(|error| CompactionCoordinatorError::InvalidCheckpoint(error.to_string()))?;
        let outcome = self
            .strategy
            .compact(
                CompactionInput {
                    model,
                    system_prompt,
                    layout,
                },
                cancellation,
            )
            .await
            .map_err(|error| CompactionCoordinatorError::Strategy(error.to_string()))?;
        let StrategyOutcome::Candidate(candidate) = outcome else {
            return Err(CompactionCoordinatorError::NoCompressibleHistory);
        };
        validate_replacement(&candidate.replacement)
            .map_err(|error| CompactionCoordinatorError::InvalidReplacement(error.to_string()))?;
        Ok(candidate.replacement)
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum CompactionCoordinatorError {
    #[error("the Journal checkpoint is not a valid context layout: {0}")]
    InvalidCheckpoint(String),
    #[error("context compaction failed: {0}")]
    Strategy(String),
    #[error("context compaction has no older complete history to summarize")]
    NoCompressibleHistory,
    #[error("context compaction produced an invalid replacement: {0}")]
    InvalidReplacement(String),
}
