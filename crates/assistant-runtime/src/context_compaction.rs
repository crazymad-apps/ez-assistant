//! Runtime 对共享压缩策略的薄编排：生成 replacement、可靠切换正文并继续同一业务执行。

use std::sync::Arc;

use agent_context::{
    CompactionError, CompactionInput, CompressionStrategy, ContextLayout, RollingSummaryPolicy,
    RollingSummarySameModel, StrategyOutcome,
};
use agent_core::CompactionReason;
use agent_core::ExecutionBudget;
use agent_model::{ModelService, SystemPromptSnapshot};
use agent_types::ConversationSnapshot;
use assistant_protocol::RunId;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    ContextReplacement, ContextReplacementTarget, RuntimeError, RuntimeStore,
    session::SessionController,
};

/// 防止 Provider 持续报告 overflow 时形成无界“压缩—重试”循环。
pub(crate) const MAX_AUTOMATIC_COMPACTIONS: u32 = 2;
const SUMMARY_OUTPUT_TOKENS: u32 = 1_024;
const PARENT_MINIMUM_RECENT_USER_TURNS: u32 = 1;
const CHILD_MINIMUM_RECENT_USER_TURNS: u32 = 0;

/// 父、子 Agent 共用的冻结压缩能力；它复用本 Run 的模型服务和 System Prompt。
pub(crate) struct RuntimeContextCompactor {
    model: Arc<dyn ModelService>,
    system_prompt: SystemPromptSnapshot,
    strategy: RollingSummarySameModel,
    active_turn_fallback: Option<RollingSummarySameModel>,
}

impl RuntimeContextCompactor {
    pub(crate) fn for_parent(
        model: Arc<dyn ModelService>,
        system_prompt: SystemPromptSnapshot,
    ) -> Self {
        let mut compactor = Self::new(model, system_prompt, PARENT_MINIMUM_RECENT_USER_TURNS);
        compactor.active_turn_fallback = Some(active_turn_strategy());
        compactor
    }

    /// child 只有一条自包含 User Message；保留一个 Turn 会让整段历史永远不可压缩。
    pub(crate) fn for_child(
        model: Arc<dyn ModelService>,
        system_prompt: SystemPromptSnapshot,
    ) -> Self {
        Self::new(model, system_prompt, CHILD_MINIMUM_RECENT_USER_TURNS)
    }

    fn new(
        model: Arc<dyn ModelService>,
        system_prompt: SystemPromptSnapshot,
        minimum_recent_user_turns: u32,
    ) -> Self {
        let policy = if minimum_recent_user_turns == CHILD_MINIMUM_RECENT_USER_TURNS {
            RollingSummaryPolicy::for_active_continuation(
                SUMMARY_OUTPUT_TOKENS,
                minimum_recent_user_turns,
            )
        } else {
            RollingSummaryPolicy::new(SUMMARY_OUTPUT_TOKENS, minimum_recent_user_turns)
        }
        .expect("static Runtime compaction policy must be valid");
        Self {
            model,
            system_prompt,
            strategy: RollingSummarySameModel::new(policy),
            active_turn_fallback: None,
        }
    }

    pub(crate) async fn compact(
        &self,
        snapshot: ConversationSnapshot,
        cancellation: CancellationToken,
    ) -> Result<ConversationSnapshot, RuntimeCompactionError> {
        let layout = ContextLayout::build(&snapshot)
            .map_err(|_| RuntimeCompactionError::InvalidConversation)?;
        let input = CompactionInput {
            model: self.model.clone(),
            system_prompt: self.system_prompt.clone(),
            layout,
        };
        let outcome = self
            .strategy
            .compact(input.clone(), cancellation.clone())
            .await?;
        match outcome {
            StrategyOutcome::Candidate(candidate) => Ok(candidate.replacement),
            StrategyOutcome::NoOp { .. } => {
                let Some(fallback) = &self.active_turn_fallback else {
                    return Err(RuntimeCompactionError::NoCompressibleHistory);
                };
                match fallback.compact(input, cancellation).await? {
                    StrategyOutcome::Candidate(candidate) => Ok(candidate.replacement),
                    StrategyOutcome::NoOp { .. } => {
                        Err(RuntimeCompactionError::NoCompressibleHistory)
                    }
                }
            }
        }
    }
}

fn active_turn_strategy() -> RollingSummarySameModel {
    let policy = RollingSummaryPolicy::for_active_continuation(
        SUMMARY_OUTPUT_TOKENS,
        CHILD_MINIMUM_RECENT_USER_TURNS,
    )
    .expect("static active-turn compaction policy must be valid");
    RollingSummarySameModel::new(policy)
}

#[derive(Debug, Error)]
pub(crate) enum RuntimeCompactionError {
    #[error("conversation cannot be laid out for context compaction")]
    InvalidConversation,
    #[error("conversation has no compressible history")]
    NoCompressibleHistory,
    #[error(transparent)]
    Strategy(#[from] CompactionError),
    #[error("context replacement could not be persisted")]
    Persistence,
    #[error("context replacement could not be applied in memory")]
    Projection,
}

impl RuntimeCompactionError {
    pub(crate) fn is_cancelled(&self) -> bool {
        matches!(self, Self::Strategy(CompactionError::Cancelled))
    }
}

/// 生成后先切换 Host 权威 generation，再替换 Session 内存 Journal。
pub(crate) async fn compact_parent_context(
    compactor: &RuntimeContextCompactor,
    session: &SessionController,
    run_id: &RunId,
    store: &dyn RuntimeStore,
    cancellation: CancellationToken,
) -> Result<ConversationSnapshot, RuntimeCompactionError> {
    let snapshot = {
        let state = session
            .lock_state()
            .map_err(|_| RuntimeCompactionError::Projection)?;
        let journal = state
            .journal
            .as_ref()
            .ok_or(RuntimeCompactionError::Projection)?;
        if journal.has_pending() {
            return Err(RuntimeCompactionError::Projection);
        }
        journal.snapshot()
    };
    let replacement = compactor.compact(snapshot.clone(), cancellation).await?;
    let _mutation = session.mutation().await;
    {
        let state = session
            .lock_state()
            .map_err(|_| RuntimeCompactionError::Projection)?;
        let current = state
            .journal
            .as_ref()
            .ok_or(RuntimeCompactionError::Projection)?;
        if current.has_pending() || current.snapshot() != snapshot {
            return Err(RuntimeCompactionError::Projection);
        }
    }
    store
        .replace_context(ContextReplacement {
            target: ContextReplacementTarget::Run {
                session_id: session.id().clone(),
                run_id: run_id.clone(),
            },
            conversation: replacement.clone(),
            changed_at_ms: crate::runtime::now_ms()
                .map_err(|_| RuntimeCompactionError::Projection)?,
        })
        .await
        .map_err(|_| RuntimeCompactionError::Persistence)?;

    let mut state = session
        .lock_state()
        .map_err(|_| RuntimeCompactionError::Projection)?;
    let journal = state
        .journal
        .as_mut()
        .ok_or(RuntimeCompactionError::Projection)?;
    journal
        .replace_completed(replacement.clone())
        .map_err(|_| RuntimeCompactionError::Projection)?;
    state.persisted_message_count = journal.message_count();
    state.message_count = u64::try_from(state.persisted_message_count)
        .map_err(|_| RuntimeCompactionError::Projection)?;
    Ok(replacement)
}

/// 子任务沿用同一原子 replacement 语义，但只改自己的 JSONL generation。
pub(crate) async fn compact_child_context(
    compactor: &RuntimeContextCompactor,
    task: &crate::delegation::ChildTaskRecord,
    store: &dyn RuntimeStore,
    cancellation: CancellationToken,
) -> Result<ConversationSnapshot, RuntimeCompactionError> {
    let snapshot = {
        let state = task
            .lock_state()
            .map_err(|_| RuntimeCompactionError::Projection)?;
        let journal = state
            .journal
            .as_ref()
            .ok_or(RuntimeCompactionError::Projection)?;
        if journal.has_pending() {
            return Err(RuntimeCompactionError::Projection);
        }
        journal.snapshot()
    };
    let replacement = compactor.compact(snapshot.clone(), cancellation).await?;
    let _mutation = task.mutation().await;
    {
        let state = task
            .lock_state()
            .map_err(|_| RuntimeCompactionError::Projection)?;
        let current = state
            .journal
            .as_ref()
            .ok_or(RuntimeCompactionError::Projection)?;
        if current.has_pending() || current.snapshot() != snapshot {
            return Err(RuntimeCompactionError::Projection);
        }
    }
    store
        .replace_context(ContextReplacement {
            target: ContextReplacementTarget::ChildTask {
                session_id: task.session_id().clone(),
                child_task_id: task.id().clone(),
            },
            conversation: replacement.clone(),
            changed_at_ms: crate::runtime::now_ms()
                .map_err(|_| RuntimeCompactionError::Projection)?,
        })
        .await
        .map_err(|_| RuntimeCompactionError::Persistence)?;
    task.replace_conversation(replacement.clone())?;
    Ok(replacement)
}

pub(crate) fn compaction_reason_label(reason: CompactionReason) -> &'static str {
    match reason {
        CompactionReason::ThresholdReached => "threshold_reached",
        CompactionReason::ProviderOverflow => "provider_overflow",
    }
}

/// continuation 共享同一业务预算；每段 Core execution 只获得尚未消费的余量。
pub(crate) fn consume_execution_budget(budget: &mut ExecutionBudget, steps: u32, tool_calls: u32) {
    budget.max_steps = budget.max_steps.map(|limit| limit.saturating_sub(steps));
    budget.max_tool_calls = budget
        .max_tool_calls
        .map(|limit| limit.saturating_sub(tool_calls));
}

impl From<RuntimeError> for RuntimeCompactionError {
    fn from(_: RuntimeError) -> Self {
        Self::Projection
    }
}
