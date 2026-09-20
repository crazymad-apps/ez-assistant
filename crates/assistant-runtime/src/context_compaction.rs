//! Runtime 对共享压缩策略的薄编排：生成 replacement、可靠切换正文并继续同一业务执行。

mod memory;

use std::sync::Arc;

use agent_context::{
    CompactionError, CompactionInput, CompressionStrategy, ContextLayout, RollingSummaryPolicy,
    RollingSummarySameModel, StrategyOutcome, validate_replacement, validate_replacement_effect,
};
use agent_core::{CompactionReason, ExecutionBudget, ModelRequestConfig};
use agent_model::{ModelRequest, ModelService, SystemPromptSnapshot};
use agent_types::{
    ConversationMessage, ConversationSnapshot, ToolChoice, TranscriptVisibility, UserMessageOrigin,
};
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
const SUMMARY_OUTPUT_TOKENS: u32 = 4_096;
const MINIMUM_RECENT_USER_TURNS: u32 = 1;

pub(crate) struct AutomaticCompactionInput {
    pub(crate) reason: CompactionReason,
    pub(crate) normal_request: ModelRequest,
}

/// 父、子 Agent 共用的冻结压缩能力；它复用本 Run 的模型服务和 System Prompt。
pub(crate) struct RuntimeContextCompactor {
    model: Arc<dyn ModelService>,
    system_prompt: SystemPromptSnapshot,
    strategy: RollingSummarySameModel,
    manual_request: Option<ModelRequestConfig>,
}

impl RuntimeContextCompactor {
    pub(crate) fn for_parent(
        model: Arc<dyn ModelService>,
        system_prompt: SystemPromptSnapshot,
    ) -> Self {
        Self::new(model, system_prompt, None)
    }

    /// 手动压缩与自动压缩都原样保留最近一个 User Turn。
    pub(crate) fn for_manual(
        model: Arc<dyn ModelService>,
        system_prompt: SystemPromptSnapshot,
        request: ModelRequestConfig,
    ) -> Self {
        Self::new(model, system_prompt, Some(request))
    }

    /// child 与 parent 使用相同的最近一轮规则；单轮自身溢出的压缩留待后续版本完善。
    pub(crate) fn for_child(
        model: Arc<dyn ModelService>,
        system_prompt: SystemPromptSnapshot,
    ) -> Self {
        Self::new(model, system_prompt, None)
    }

    fn new(
        model: Arc<dyn ModelService>,
        system_prompt: SystemPromptSnapshot,
        manual_request: Option<ModelRequestConfig>,
    ) -> Self {
        let policy = RollingSummaryPolicy::new(SUMMARY_OUTPUT_TOKENS, MINIMUM_RECENT_USER_TURNS)
            .expect("static Runtime compaction policy must be valid");
        Self {
            model,
            system_prompt,
            strategy: RollingSummarySameModel::new(policy),
            manual_request,
        }
    }

    pub(crate) async fn compact(
        &self,
        snapshot: ConversationSnapshot,
        product_history: ConversationSnapshot,
        normal_request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ConversationSnapshot, RuntimeCompactionError> {
        let outcome = self
            .compact_once(snapshot, product_history, normal_request, cancellation)
            .await?;
        match outcome {
            StrategyOutcome::Candidate(candidate) => Ok(candidate.replacement),
            StrategyOutcome::NoOp { .. } => Err(RuntimeCompactionError::NoCompressibleHistory),
        }
    }

    pub(crate) async fn compact_manual(
        &self,
        snapshot: ConversationSnapshot,
        product_history: ConversationSnapshot,
        cancellation: CancellationToken,
    ) -> Result<ManualCompactionCandidate, RuntimeCompactionError> {
        let source_message_count = u64::try_from(snapshot.messages.len())
            .map_err(|_| RuntimeCompactionError::InvalidConversation)?;
        let request = self
            .manual_request
            .as_ref()
            .ok_or(RuntimeCompactionError::MissingManualRequest)?;
        let normal_request = ModelRequest {
            system: self.system_prompt.clone(),
            conversation: snapshot.clone(),
            tools: vec![],
            tool_choice: ToolChoice::None,
            generation: request.generation.clone(),
            reasoning: request.reasoning.clone(),
            provider_options: request.provider_options.clone(),
        };
        let outcome = self
            .compact_once(snapshot, product_history, normal_request, cancellation)
            .await?;
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
        product_history: ConversationSnapshot,
        normal_request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<StrategyOutcome, RuntimeCompactionError> {
        if crate::execution_context_from_product_history(&product_history) != snapshot {
            return Err(RuntimeCompactionError::ProductHistoryMismatch);
        }
        let outcome = self
            .strategy
            .compact(
                self.compaction_input(&snapshot, normal_request)?,
                cancellation,
            )
            .await?;
        let StrategyOutcome::Candidate(mut candidate) = outcome else {
            return Ok(outcome);
        };
        let programmatic_context = memory::derive_pinned_memory_context(&product_history)?;
        let summary = candidate
            .replacement
            .messages
            .iter_mut()
            .find_map(|message| match message {
                ConversationMessage::ContextSummary(summary) => Some(summary),
                _ => None,
            })
            .ok_or(RuntimeCompactionError::InvalidConversation)?;
        summary.programmatic_context = programmatic_context;
        validate_replacement(&candidate.replacement)?;
        validate_replacement_effect(&snapshot, &candidate.replacement)?;
        crate::merge_context_replacement_with_product_history(
            &product_history,
            &candidate.replacement,
        )
        .map_err(|_| RuntimeCompactionError::ProductHistoryMismatch)?;
        Ok(StrategyOutcome::Candidate(candidate))
    }

    fn compaction_input(
        &self,
        snapshot: &ConversationSnapshot,
        normal_request: ModelRequest,
    ) -> Result<CompactionInput, RuntimeCompactionError> {
        validate_live_request(snapshot, &normal_request)?;
        let layout = ContextLayout::build(snapshot)
            .map_err(|_| RuntimeCompactionError::InvalidConversation)?;
        Ok(CompactionInput {
            model: self.model.clone(),
            normal_request,
            layout,
        })
    }
}

fn validate_live_request(
    snapshot: &ConversationSnapshot,
    request: &ModelRequest,
) -> Result<(), RuntimeCompactionError> {
    let mut source_index = 0_usize;
    for message in &request.conversation.messages {
        if snapshot.messages.get(source_index) == Some(message) {
            source_index += 1;
            continue;
        }
        let allowed_request_only = matches!(
            message,
            ConversationMessage::User(message)
                if message.origin == UserMessageOrigin::Runtime
                    && message.transcript_visibility == TranscriptVisibility::Hidden
        );
        if !allowed_request_only {
            return Err(RuntimeCompactionError::LiveRequestMismatch);
        }
    }
    if source_index != snapshot.messages.len() {
        return Err(RuntimeCompactionError::LiveRequestMismatch);
    }
    Ok(())
}

pub(crate) enum ManualCompactionCandidate {
    NoOp,
    Replacement {
        conversation: ConversationSnapshot,
        compacted_message_count: u64,
        retained_message_count: u64,
    },
}

#[derive(Debug, Error)]
pub(crate) enum RuntimeCompactionError {
    #[error("conversation cannot be laid out for context compaction")]
    InvalidConversation,
    #[error("conversation has no compressible history")]
    NoCompressibleHistory,
    #[error("live model request does not contain the authoritative conversation in order")]
    LiveRequestMismatch,
    #[error("product history does not project to the authoritative execution context")]
    ProductHistoryMismatch,
    #[error("manual compaction request configuration is unavailable")]
    MissingManualRequest,
    #[error(transparent)]
    PinnedMemory(#[from] memory::PinnedMemoryContextError),
    #[error(transparent)]
    ReplacementValidation(#[from] agent_context::ReplacementValidationError),
    #[error(transparent)]
    ReplacementEffect(#[from] agent_context::ReplacementEffectError),
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
    input: AutomaticCompactionInput,
    store: &dyn RuntimeStore,
    events: ObservationCoordinator,
    cancellation: CancellationToken,
) -> Result<ConversationSnapshot, RuntimeCompactionError> {
    let AutomaticCompactionInput {
        reason,
        normal_request,
    } = input;
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
    let product_history = store
        .load_conversation(session.id())
        .await
        .map_err(|_| RuntimeCompactionError::Persistence)?;
    let replacement = match compactor
        .compact(
            snapshot.clone(),
            product_history,
            normal_request,
            cancellation,
        )
        .await
    {
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
        state.message_count = committed.product_message_count;
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
    normal_request: ModelRequest,
    cancellation: CancellationToken,
) -> Result<(ConversationSnapshot, u64), RuntimeCompactionError> {
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
    let product_history = store
        .load_child_conversation(task.session_id(), task.id())
        .await
        .map_err(|_| RuntimeCompactionError::Persistence)?;
    let replacement = compactor
        .compact(
            snapshot.clone(),
            product_history,
            normal_request,
            cancellation,
        )
        .await?;
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
    let committed = store
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
    Ok((replacement, committed.product_message_count))
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
