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
use assistant_protocol::{
    RunId, RuntimeErrorCode, RuntimeEvent, SessionCompactionFinishedOutcome,
    SessionCompactionReasonSnapshot, SessionCompactionSnapshot, SessionCompactionTriggerSnapshot,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    ContextReplacement, ContextReplacementTarget, RuntimeError, RuntimeStore,
    observation::ObservationCoordinator,
    session::{ActiveSessionCompaction, SessionController},
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

    /// 手动压缩保留最近一个完整 User Turn，且不启用活动 Turn fallback。
    pub(crate) fn for_manual(
        model: Arc<dyn ModelService>,
        system_prompt: SystemPromptSnapshot,
    ) -> Self {
        Self::new(model, system_prompt, PARENT_MINIMUM_RECENT_USER_TURNS)
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
        let outcome = self
            .compact_once(snapshot.clone(), cancellation.clone())
            .await?;
        match outcome {
            StrategyOutcome::Candidate(candidate) => Ok(candidate.replacement),
            StrategyOutcome::NoOp { .. } => {
                let Some(fallback) = &self.active_turn_fallback else {
                    return Err(RuntimeCompactionError::NoCompressibleHistory);
                };
                match fallback
                    .compact(self.compaction_input(&snapshot)?, cancellation)
                    .await?
                {
                    StrategyOutcome::Candidate(candidate) => Ok(candidate.replacement),
                    StrategyOutcome::NoOp { .. } => {
                        Err(RuntimeCompactionError::NoCompressibleHistory)
                    }
                }
            }
        }
    }

    pub(crate) async fn compact_manual(
        &self,
        snapshot: ConversationSnapshot,
        cancellation: CancellationToken,
    ) -> Result<ManualCompactionCandidate, RuntimeCompactionError> {
        let source_message_count = u64::try_from(snapshot.messages.len())
            .map_err(|_| RuntimeCompactionError::InvalidConversation)?;
        let outcome = self.compact_once(snapshot, cancellation).await?;
        match outcome {
            StrategyOutcome::NoOp { .. } => Ok(ManualCompactionCandidate::NoOp),
            StrategyOutcome::Candidate(candidate) => {
                let replacement_message_count = u64::try_from(candidate.replacement.messages.len())
                    .map_err(|_| RuntimeCompactionError::InvalidConversation)?;
                // replacement 固定新增一条 Context Summary；其余才是原样保留的源消息。
                let retained_message_count = replacement_message_count.saturating_sub(1);
                let compacted_message_count =
                    source_message_count.saturating_sub(retained_message_count);
                Ok(ManualCompactionCandidate::Replacement {
                    conversation: candidate.replacement,
                    compacted_message_count,
                    retained_message_count,
                })
            }
        }
    }

    /// 自动与手动压缩共用的单次 rolling-summary 调用；各入口只解释不同的 NoOp 后续语义。
    async fn compact_once(
        &self,
        snapshot: ConversationSnapshot,
        cancellation: CancellationToken,
    ) -> Result<StrategyOutcome, RuntimeCompactionError> {
        Ok(self
            .strategy
            .compact(self.compaction_input(&snapshot)?, cancellation)
            .await?)
    }

    fn compaction_input(
        &self,
        snapshot: &ConversationSnapshot,
    ) -> Result<CompactionInput, RuntimeCompactionError> {
        let layout = ContextLayout::build(snapshot)
            .map_err(|_| RuntimeCompactionError::InvalidConversation)?;
        Ok(CompactionInput {
            model: self.model.clone(),
            system_prompt: self.system_prompt.clone(),
            layout,
        })
    }
}

pub(crate) enum ManualCompactionCandidate {
    NoOp,
    Replacement {
        conversation: ConversationSnapshot,
        compacted_message_count: u64,
        retained_message_count: u64,
    },
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

/// 自动与手动 parent compaction 共用的易失状态和成对事件收口。
pub(crate) struct SessionCompactionGuard {
    session: Arc<SessionController>,
    events: ObservationCoordinator,
    compaction_id: String,
    finished: bool,
}

impl SessionCompactionGuard {
    pub(crate) fn begin(
        session: Arc<SessionController>,
        events: ObservationCoordinator,
        snapshot: SessionCompactionSnapshot,
        cancellation: Option<CancellationToken>,
    ) -> Result<Self, RuntimeCompactionError> {
        {
            let mut state = session
                .lock_state()
                .map_err(|_| RuntimeCompactionError::Projection)?;
            if state.active_compaction.is_some() {
                return Err(RuntimeCompactionError::Projection);
            }
            state.active_compaction = Some(ActiveSessionCompaction {
                snapshot: snapshot.clone(),
                cancellation,
            });
        }
        let _ = events.send(RuntimeEvent::SessionCompactionStarted {
            session_id: session.id().clone(),
            compaction: snapshot.clone(),
        });
        Ok(Self {
            session,
            events,
            compaction_id: snapshot.compaction_id,
            finished: false,
        })
    }

    pub(crate) fn finish(&mut self, outcome: SessionCompactionFinishedOutcome) {
        if !self.finished {
            self.clear_and_publish(outcome);
            self.finished = true;
        }
    }

    fn clear_and_publish(&self, outcome: SessionCompactionFinishedOutcome) {
        if let Ok(mut state) = self.session.lock_state()
            && state
                .active_compaction
                .as_ref()
                .is_some_and(|active| active.snapshot.compaction_id == self.compaction_id)
        {
            state.active_compaction = None;
            let _ = self.events.send(RuntimeEvent::SessionCompactionFinished {
                session_id: self.session.id().clone(),
                compaction_id: self.compaction_id.clone(),
                outcome,
            });
        }
    }
}

impl Drop for SessionCompactionGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.clear_and_publish(SessionCompactionFinishedOutcome::Failed {
                code: RuntimeErrorCode::Internal,
            });
        }
    }
}

impl RuntimeCompactionError {
    pub(crate) fn is_cancelled(&self) -> bool {
        matches!(self, Self::Strategy(CompactionError::Cancelled))
    }

    /// 与标准 Model Request 保持同一错误边界：模型错误保留原始
    /// `ModelError` 并由协议层脱敏分类，非模型的压缩错误保留完整 source 链。
    pub(crate) fn into_runtime_error(self) -> RuntimeError {
        match self {
            Self::Strategy(CompactionError::Model(source)) => {
                RuntimeError::ModelExecutionFailed { source }
            }
            source => RuntimeError::ContextCompactionFailed {
                source: Box::new(source),
            },
        }
    }
}

/// 生成后先切换 Host 权威 generation，再替换 Session 内存 Journal。
pub(crate) async fn compact_parent_context(
    compactor: &RuntimeContextCompactor,
    session: Arc<SessionController>,
    run_id: &RunId,
    reason: CompactionReason,
    store: &dyn RuntimeStore,
    events: ObservationCoordinator,
    cancellation: CancellationToken,
) -> Result<ConversationSnapshot, RuntimeCompactionError> {
    let (snapshot, source_generation) = {
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
        (journal.snapshot(), state.body_generation)
    };
    let compaction_id =
        crate::id::generate("compaction").map_err(|_| RuntimeCompactionError::Projection)?;
    let started_at_ms = crate::runtime::now_ms().map_err(|_| RuntimeCompactionError::Projection)?;
    let trigger_reason = match reason {
        CompactionReason::ThresholdReached => SessionCompactionReasonSnapshot::ThresholdReached,
        CompactionReason::ProviderOverflow => SessionCompactionReasonSnapshot::ProviderOverflow,
    };
    let mut guard = SessionCompactionGuard::begin(
        session.clone(),
        events.clone(),
        SessionCompactionSnapshot {
            compaction_id,
            trigger: SessionCompactionTriggerSnapshot::Automatic {
                run_id: run_id.clone(),
                reason: trigger_reason,
            },
            source_generation,
            started_at_ms,
            cancellable: false,
        },
        None,
    )?;
    let replacement = match compactor.compact(snapshot.clone(), cancellation).await {
        Ok(replacement) => replacement,
        Err(error) => {
            let outcome = if error.is_cancelled() {
                SessionCompactionFinishedOutcome::Cancelled
            } else {
                SessionCompactionFinishedOutcome::Failed {
                    code: RuntimeErrorCode::ContextCompactionFailed,
                }
            };
            guard.finish(outcome);
            return Err(error);
        }
    };
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
    let committed = match store
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
    {
        Ok(committed) => committed,
        Err(_) => {
            guard.finish(SessionCompactionFinishedOutcome::Failed {
                code: RuntimeErrorCode::ContextCompactionFailed,
            });
            return Err(RuntimeCompactionError::Persistence);
        }
    };

    let projection = (|| -> Result<(), RuntimeCompactionError> {
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
        state.body_generation = committed.result_generation;
        Ok(())
    })();
    if let Err(error) = projection {
        let _ = session.mark_faulted();
        return Err(error);
    }
    let _ = events.send(RuntimeEvent::ConversationCommitted {
        owner: assistant_protocol::ConversationOwner::MainSession {
            session_id: session.id().clone(),
        },
        generation: committed.result_generation,
    });
    guard.finish(SessionCompactionFinishedOutcome::Compacted {
        source_generation: committed.source_generation,
        result_generation: committed.result_generation,
    });
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
