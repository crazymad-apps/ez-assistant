//! Runtime 对共享压缩策略的薄编排：生成 replacement、可靠切换正文并继续同一业务执行。

pub(crate) mod memory;

#[cfg(test)]
mod tests;

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
    RunId, RuntimeErrorCode, RuntimeErrorInfo, RuntimeEvent, SessionCompactionFinishedOutcome,
    SessionCompactionReasonSnapshot, SessionCompactionSnapshot, SessionCompactionTriggerSnapshot,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    ContextReplacement, ContextReplacementResult, ContextReplacementTarget, RuntimeError,
    RuntimeResult, RuntimeStore, StoreError, StoreErrorKind,
    observation::ObservationCoordinator,
    session::{ActiveSessionCompaction, SessionController},
};

/// 防止 Provider 持续报告 overflow 时形成无界“压缩—重试”循环。
pub(crate) const MAX_AUTOMATIC_COMPACTIONS: u32 = 2;

pub(crate) fn latest_model_result_id(
    snapshot: &ConversationSnapshot,
) -> Option<&agent_types::MessageId> {
    snapshot
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            ConversationMessage::Assistant(message) if message.purpose.is_model_response() => {
                Some(&message.id)
            }
            _ => None,
        })
}
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
    restored_context: Vec<agent_types::UserMessage>,
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

    /// child 与 parent 共用完整轮次与轮内闭合交换的回收规则。
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
            restored_context: Vec::new(),
        }
    }

    /// 冻结执行已披露的能力上下文；仅在压缩候选缺失或过期时重新落账。
    pub(crate) fn with_restored_context(
        mut self,
        mcp: Option<agent_types::UserMessage>,
        shell: Option<agent_types::UserMessage>,
    ) -> Self {
        self.restored_context = [mcp, shell].into_iter().flatten().collect();
        self
    }

    fn restore_context(
        &self,
        replacement: &mut ConversationSnapshot,
    ) -> Result<(), RuntimeCompactionError> {
        use crate::internal_boundary::{
            InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
        };
        use agent_types::UserPart;
        let mut restored: Option<agent_types::UserMessage> = None;
        for context in &self.restored_context {
            for part in &context.parts {
                let UserPart::InternalContext(expected) = part else {
                    continue;
                };
                let actual = replacement.messages.iter().rev().find_map(|message| {
                    let ConversationMessage::User(user) = message else {
                        return None;
                    };
                    user.parts.iter().rev().find_map(|part| match part {
                        UserPart::InternalContext(part) if part.kind == expected.kind => {
                            Some(&part.text)
                        }
                        _ => None,
                    })
                });
                if actual == Some(&expected.text) {
                    continue;
                }
                let source = match expected.kind.as_str() {
                    "mcp_server_directory" => InternalBoundarySource::McpServerDirectory,
                    "mcp_server_selection" => InternalBoundarySource::McpServerSelection,
                    "shell_environment" => InternalBoundarySource::ShellEnvironment,
                    _ => return Err(RuntimeCompactionError::InvalidConversation),
                };
                let request = InternalBoundaryRequest {
                    source,
                    text: expected.text.clone(),
                };
                if let Some(message) = &mut restored {
                    InternalBoundaryCoordinator::append(message, request)?;
                } else {
                    restored = Some(InternalBoundaryCoordinator::hidden_message(request)?.0);
                }
            }
        }
        if let Some(message) = restored {
            // 新身份和原样保留尾部一起原子提交；放到尾端使刷新事实不会被旧目录覆盖。
            replacement
                .messages
                .push(ConversationMessage::User(message));
        }
        Ok(())
    }

    pub(crate) async fn compact(
        &self,
        snapshot: ConversationSnapshot,
        product_history: ConversationSnapshot,
        normal_request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ConversationSnapshot, RuntimeCompactionError> {
        if self.model.context_policy().mode()
            == agent_model::ContextManagementMode::ServerCompaction
        {
            return self
                .compact_native(snapshot, product_history, normal_request, cancellation)
                .await;
        }
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
        if self.model.context_policy().mode()
            == agent_model::ContextManagementMode::ServerCompaction
        {
            let conversation = self
                .compact_native(snapshot, product_history, normal_request, cancellation)
                .await?;
            return Ok(ManualCompactionCandidate::Replacement {
                conversation,
                compacted_message_count: source_message_count,
                retained_message_count: 0,
            });
        }
        let outcome = self
            .compact_once(
                snapshot.clone(),
                product_history,
                normal_request,
                cancellation,
            )
            .await?;
        match outcome {
            StrategyOutcome::NoOp { .. } => Ok(ManualCompactionCandidate::NoOp),
            StrategyOutcome::Candidate(candidate) => {
                // 恢复的目录/环境消息是新事实，不计入原样保留的源消息。
                let retained_message_count = candidate
                    .replacement
                    .messages
                    .iter()
                    .filter(|message| snapshot.messages.contains(message))
                    .count() as u64;
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

    pub(crate) fn with_model(&self, model: Arc<dyn ModelService>) -> Self {
        Self {
            model,
            system_prompt: self.system_prompt.clone(),
            strategy: self.strategy.clone(),
            manual_request: self.manual_request.clone(),
            restored_context: self.restored_context.clone(),
        }
    }

    async fn compact_native(
        &self,
        snapshot: ConversationSnapshot,
        product_history: ConversationSnapshot,
        mut normal_request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ConversationSnapshot, RuntimeCompactionError> {
        let policy = self.model.context_policy();
        let source = agent_context::project_effective_context(&product_history, policy)
            .map_err(CompactionError::Model)?;
        let effective_snapshot = agent_context::project_effective_context(&snapshot, policy)
            .map_err(CompactionError::Model)?;
        if source != effective_snapshot {
            return Err(RuntimeCompactionError::ProductHistoryMismatch);
        }
        normal_request.conversation =
            agent_context::project_effective_context(&normal_request.conversation, policy)
                .map_err(CompactionError::Model)?;
        validate_live_request(&effective_snapshot, &normal_request)?;
        let message = self
            .model
            .compact(
                normal_request,
                agent_model::ModelCallContext::new(cancellation.clone()),
            )
            .await
            .map_err(CompactionError::Model)?;
        if cancellation.is_cancelled() {
            return Err(CompactionError::Cancelled.into());
        }
        let agent_model::ContextPolicy::ServerCompaction {
            state_scope_fingerprint,
            ..
        } = policy
        else {
            unreachable!();
        };
        if message.purpose != agent_types::AssistantMessagePurpose::ContextCompaction
            || !matches!(message.parts.as_slice(), [agent_types::AssistantPart::ProviderState(state)]
                if state.context_effect() == Some(agent_types::ProviderContextEffect::ReplacePriorContext)
                    && state.route_fingerprint() == Some(state_scope_fingerprint.as_str()))
        {
            return Err(RuntimeCompactionError::InvalidConversation);
        }
        let mut messages: Vec<_> = snapshot
            .messages
            .iter()
            .take_while(|m| matches!(m, ConversationMessage::System(_)))
            .cloned()
            .collect();
        messages.push(ConversationMessage::Assistant(message));
        let replacement = ConversationSnapshot::new(messages);
        crate::merge_context_replacement_with_product_history(&product_history, &replacement)
            .map_err(|_| RuntimeCompactionError::ProductHistoryMismatch)?;
        Ok(replacement)
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
        self.restore_context(&mut candidate.replacement)?;
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
    Persistence(#[from] StoreError),
    #[error("context replacement could not be applied in memory")]
    Projection {
        committed: Option<ContextReplacementResult>,
        #[source]
        source: Option<RuntimeError>,
    },
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
        events.publish_transition(|| {
            let mut state = session.lock_state()?;
            if state.active_compaction.is_some() {
                return Err(RuntimeError::SessionCompactionInProgress {
                    session_id: session.id().clone(),
                });
            }
            state.active_compaction = Some(ActiveSessionCompaction {
                snapshot: snapshot.clone(),
                cancellation,
            });
            Ok((
                (),
                Some(RuntimeEvent::SessionCompactionStarted {
                    session_id: session.id().clone(),
                    compaction: snapshot.clone(),
                }),
            ))
        })?;
        Ok(Self {
            session,
            events,
            compaction_id: snapshot.compaction_id,
            finished: false,
        })
    }

    pub(crate) fn finish(
        &mut self,
        outcome: SessionCompactionFinishedOutcome,
    ) -> RuntimeResult<()> {
        if !self.finished {
            self.clear_and_publish(outcome)?;
            self.finished = true;
        }
        Ok(())
    }

    fn clear_and_publish(&self, outcome: SessionCompactionFinishedOutcome) -> RuntimeResult<()> {
        self.events.publish_transition(|| {
            let mut state = self.session.lock_state()?;
            if !state
                .active_compaction
                .as_ref()
                .is_some_and(|active| active.snapshot.compaction_id == self.compaction_id)
            {
                return Ok(((), None));
            }
            state.active_compaction = None;
            Ok((
                (),
                Some(RuntimeEvent::SessionCompactionFinished {
                    session_id: self.session.id().clone(),
                    compaction_id: self.compaction_id.clone(),
                    outcome,
                }),
            ))
        })
    }
}

impl Drop for SessionCompactionGuard {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.clear_and_publish(SessionCompactionFinishedOutcome::Failed {
                code: RuntimeErrorCode::Internal,
            });
        }
    }
}

impl RuntimeCompactionError {
    pub(crate) fn projection() -> Self {
        Self::Projection {
            committed: None,
            source: None,
        }
    }

    pub(crate) fn is_committed(&self) -> bool {
        matches!(
            self,
            Self::Projection {
                committed: Some(_),
                ..
            }
        )
    }

    pub(crate) fn after_commit(committed: ContextReplacementResult, source: RuntimeError) -> Self {
        Self::Projection {
            committed: Some(committed),
            source: Some(source),
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        matches!(
            self,
            Self::Strategy(
                CompactionError::Cancelled
                    | CompactionError::Model(agent_model::ModelError::Cancelled)
            )
        )
    }

    /// 仅输出固定分类与模型已有的 HTTP 状态；source 链和正文留在进程内。
    pub(crate) fn to_protocol_info(&self) -> RuntimeErrorInfo {
        let reason = match self {
            Self::Strategy(
                CompactionError::Cancelled
                | CompactionError::Model(agent_model::ModelError::Cancelled),
            ) => {
                return RuntimeErrorInfo::new(
                    RuntimeErrorCode::Cancelled,
                    "context compaction was cancelled",
                );
            }
            Self::Strategy(CompactionError::Model(source)) => {
                let kind =
                    crate::run::model_failure_kind_value(crate::run::model_failure_kind(source));
                let status = match source {
                    agent_model::ModelError::Provider { status, .. }
                    | agent_model::ModelError::Unavailable { status, .. } => *status,
                    _ => None,
                };
                let suffix = status.map_or_else(String::new, |status| format!(", status={status}"));
                return RuntimeErrorInfo::new(
                    RuntimeErrorCode::ContextCompactionFailed,
                    format!("context compaction failed (kind={kind}{suffix})"),
                );
            }
            Self::Persistence(source) if source.kind() == StoreErrorKind::Conflict => {
                return RuntimeError::SnapshotStale.to_protocol_info();
            }
            Self::Persistence(_) => {
                return RuntimeErrorInfo::new(
                    RuntimeErrorCode::StorageUnavailable,
                    "context compaction storage is unavailable",
                );
            }
            Self::Projection {
                committed: Some(_), ..
            } => {
                return RuntimeErrorInfo::new(
                    RuntimeErrorCode::Internal,
                    "context compaction was committed but its projection is unavailable",
                );
            }
            Self::Projection {
                source: Some(source),
                ..
            } => return source.to_protocol_info(),
            Self::Projection { .. } | Self::MissingManualRequest => {
                return RuntimeErrorInfo::new(
                    RuntimeErrorCode::Internal,
                    "context compaction state is unavailable",
                );
            }
            Self::ProductHistoryMismatch => return RuntimeError::SnapshotStale.to_protocol_info(),
            Self::NoCompressibleHistory => "no_compressible_history",
            Self::LiveRequestMismatch => "live_request_mismatch",
            Self::InvalidConversation => "invalid_conversation",
            Self::PinnedMemory(_) => "invalid_pinned_memory",
            Self::ReplacementValidation(_)
            | Self::Strategy(CompactionError::InvalidReplacement(_)) => "invalid_replacement",
            Self::ReplacementEffect(_) | Self::Strategy(CompactionError::Ineffective { .. }) => {
                "ineffective"
            }
            Self::Strategy(CompactionError::InvalidResponse { .. }) => "invalid_response",
        };
        RuntimeErrorInfo::new(
            RuntimeErrorCode::ContextCompactionFailed,
            format!("context compaction failed (kind={reason})"),
        )
    }

    pub(crate) fn finished_outcome(&self) -> SessionCompactionFinishedOutcome {
        if self.is_cancelled() {
            SessionCompactionFinishedOutcome::Cancelled
        } else {
            SessionCompactionFinishedOutcome::Failed {
                code: self.to_protocol_info().code,
            }
        }
    }

    pub(crate) fn into_runtime_error(self) -> RuntimeError {
        RuntimeError::ContextCompactionFailed {
            source: Box::new(self),
        }
    }
}

/// 调用方必须已取得目标 mutation 屏障；模型已结束，核验及安装回调仅做同步工作。
/// Store 成功后不再检查取消，可靠事实获胜；安装失败隔离所属 Session 并返回提交边界，
/// 调用方不得拿陈旧 Journal 结算或再次提交。此处不发布业务身份或启动后续执行。
pub(crate) async fn commit_context_replacement(
    store: &dyn RuntimeStore,
    request: ContextReplacement,
    source_generation: u64,
    cancellation: &CancellationToken,
    session: &SessionController,
    validate: impl FnOnce() -> RuntimeResult<()>,
    install: impl FnOnce(&ContextReplacementResult) -> RuntimeResult<()>,
) -> Result<ContextReplacementResult, RuntimeCompactionError> {
    if cancellation.is_cancelled() {
        return Err(CompactionError::Cancelled.into());
    }
    validate()?;
    let committed = store.replace_context(request).await?;
    let installation = if committed.source_generation != source_generation {
        Err(RuntimeError::InternalStateUnavailable {
            component: "compaction commit boundary",
        })
    } else {
        // 回调 panic 同样不能丢掉已提交的事实或进入普通失败结算。
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| install(&committed)))
            .unwrap_or_else(|_| {
                Err(RuntimeError::InternalStateUnavailable {
                    component: "compaction projection",
                })
            })
    };
    if let Err(source) = installation {
        let _ = session.mark_faulted();
        if let Ok(mut state) = session.lock_state()
            && let Some(active) = state.active_run.take()
        {
            // 撤销当前进程内的执行占用并级联停止 child；Store 的未完成控制保持供重启恢复。
            // 与普通未提交 Recorder 失败区别：后者仍可用可靠前缀结算，不能一概禁止。
            active.cancellation.cancel();
        }
        return Err(RuntimeCompactionError::after_commit(committed, source));
    }
    Ok(committed)
}

pub(crate) fn validate_session_replacement(
    session: &SessionController,
    source: &ConversationSnapshot,
    source_generation: u64,
    target: &ContextReplacementTarget,
    compaction_id: &str,
) -> RuntimeResult<()> {
    session.ensure_healthy()?;
    let state = session.lock_state()?;
    let current = state
        .journal
        .as_ref()
        .ok_or(RuntimeError::InternalStateUnavailable {
            component: "compaction source",
        })?;
    let owner_matches = match target {
        ContextReplacementTarget::Run { session_id, run_id } => session.id() == session_id
            && state.active_run.as_ref().is_some_and(|active| &active.run_id == run_id)
            && state.active_compaction.as_ref().is_some_and(|active| matches!(&active.snapshot.trigger,
                SessionCompactionTriggerSnapshot::Automatic { run_id: active_run, .. } if active_run == run_id)),
        ContextReplacementTarget::IdleSession { session_id, .. } => session.id() == session_id && state.active_run.is_none()
            && state.active_compaction.as_ref().is_some_and(|active| matches!(active.snapshot.trigger, SessionCompactionTriggerSnapshot::Manual)),
        ContextReplacementTarget::ChildTask { .. } => false,
    };
    if !owner_matches
        || state.body_generation != source_generation
        || current.has_pending()
        || current.snapshot() != *source
        || !state.active_compaction.as_ref().is_some_and(|active| {
            active.snapshot.compaction_id == compaction_id
                && active.snapshot.source_generation == source_generation
        })
    {
        return Err(RuntimeError::SnapshotStale);
    }
    Ok(())
}

/// 共享 Session 投影及其通知；调用方已持 mutation，观察锁只覆盖同步安装。
pub(crate) fn install_session_replacement(
    session: &SessionController,
    conversation: ConversationSnapshot,
    committed: &ContextReplacementResult,
    events: &ObservationCoordinator,
) -> RuntimeResult<()> {
    events.publish_transition(|| {
        let mut state = session.lock_state()?;
        let journal = state
            .journal
            .as_mut()
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "compaction projection",
            })?;
        journal.replace_completed(conversation).map_err(|_| {
            RuntimeError::InternalStateUnavailable {
                component: "compaction projection",
            }
        })?;
        state.persisted_message_count = journal.message_count();
        state.message_count = committed.product_message_count;
        state.body_generation = committed.result_generation;
        state.context_measurements.clear();
        Ok((
            (),
            Some(RuntimeEvent::ConversationCommitted {
                owner: assistant_protocol::ConversationOwner::MainSession {
                    session_id: session.id().clone(),
                },
                generation: committed.result_generation,
            }),
        ))
    })
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
            .map_err(|_| RuntimeCompactionError::projection())?;
        let journal = state
            .journal
            .as_ref()
            .ok_or(RuntimeCompactionError::projection())?;
        if journal.has_pending() {
            return Err(RuntimeCompactionError::projection());
        }
        (journal.snapshot(), state.body_generation)
    };
    let compaction_id =
        crate::id::generate("compaction").map_err(|_| RuntimeCompactionError::projection())?;
    let started_at_ms =
        crate::runtime::now_ms().map_err(|_| RuntimeCompactionError::projection())?;
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
    let outcome: Result<ConversationSnapshot, RuntimeCompactionError> = async {
        let product_history = store.load_conversation(session.id()).await?;
        let replacement = compactor
            .compact(
                snapshot.clone(),
                product_history,
                normal_request,
                cancellation.clone(),
            )
            .await?;
        let _mutation = session.mutation().await;
        let target = ContextReplacementTarget::Run {
            session_id: session.id().clone(),
            run_id: run_id.clone(),
        };
        let compaction_id = guard.compaction_id.clone();
        commit_context_replacement(
            store,
            ContextReplacement {
                target: target.clone(),
                conversation: replacement.clone(),
                changed_at_ms: crate::runtime::now_ms()?,
            },
            source_generation,
            &cancellation,
            &session,
            || {
                validate_session_replacement(
                    &session,
                    &snapshot,
                    source_generation,
                    &target,
                    &compaction_id,
                )
            },
            |committed| {
                install_session_replacement(&session, replacement.clone(), committed, &events)?;
                guard.finish(SessionCompactionFinishedOutcome::Compacted {
                    source_generation: committed.source_generation,
                    result_generation: committed.result_generation,
                })
            },
        )
        .await?;
        Ok(replacement)
    }
    .await;
    if let Err(error) = &outcome {
        let _ = guard.finish(error.finished_outcome());
    }
    outcome
}

/// 子任务沿用同一原子 replacement 语义，但只改自己的 JSONL generation。
pub(crate) async fn compact_child_context(
    compactor: &RuntimeContextCompactor,
    task: &crate::delegation::ChildTaskRecord,
    parent: &SessionController,
    store: &dyn RuntimeStore,
    normal_request: ModelRequest,
    cancellation: CancellationToken,
    install: impl FnOnce(&ConversationSnapshot, &ContextReplacementResult) -> RuntimeResult<()>,
) -> Result<(ConversationSnapshot, ContextReplacementResult), RuntimeCompactionError> {
    let (snapshot, source_generation) = {
        let state = task
            .lock_state()
            .map_err(|_| RuntimeCompactionError::projection())?;
        let journal = state
            .journal
            .as_ref()
            .ok_or_else(RuntimeCompactionError::projection)?;
        if journal.has_pending() {
            return Err(RuntimeCompactionError::projection());
        }
        (journal.snapshot(), state.body_generation)
    };
    let product_history = store
        .load_child_conversation(task.session_id(), task.id())
        .await?;
    let replacement = compactor
        .compact(
            snapshot.clone(),
            product_history,
            normal_request,
            cancellation.clone(),
        )
        .await?;
    // 模型等待已经结束；锁顺序始终 parent → child → Store。
    let _parent = parent.mutation().await;
    let _mutation = task.mutation().await;
    let committed = commit_context_replacement(
        store,
        ContextReplacement {
            target: ContextReplacementTarget::ChildTask {
                session_id: task.session_id().clone(),
                child_task_id: task.id().clone(),
            },
            conversation: replacement.clone(),
            changed_at_ms: crate::runtime::now_ms()?,
        },
        source_generation,
        &cancellation,
        parent,
        || {
            parent.ensure_healthy()?;
            let owner_matches = task.session_id() == parent.id()
                && parent
                    .lock_state()?
                    .active_run
                    .as_ref()
                    .is_some_and(|active| &active.run_id == task.journal_owner());
            let state = task
                .lock_state()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "child compaction source",
                })?;
            let current = state
                .journal
                .as_ref()
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "child compaction source",
                })?;
            if !owner_matches
                || state.body_generation != source_generation
                || current.has_pending()
                || current.snapshot() != snapshot
            {
                return Err(RuntimeError::SnapshotStale);
            }
            Ok(())
        },
        |committed| install(&replacement, committed),
    )
    .await?;
    Ok((replacement, committed))
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
    fn from(source: RuntimeError) -> Self {
        Self::Projection {
            committed: None,
            source: Some(source),
        }
    }
}
