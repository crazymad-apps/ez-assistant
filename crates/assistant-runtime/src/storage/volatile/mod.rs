//! 仅供无本地 Host 的嵌入式调用与 Runtime 单元测试使用的易失 Store。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, MessageId,
    TokenUsage, UserPart,
};
use assistant_protocol::{
    AttachmentId, ChildTaskId, ChildTaskStatus, CompactSessionOutcome, ConversationOwner,
    IdempotencyKey, InputId, MessageFeedback, MessageId as ProtocolMessageId, RunId, RunStatus,
    SessionHistoryCleanupStatus, SessionId, SessionTitleGenerationTriggerSnapshot,
    SessionTitleOrigin, WorkspaceId,
};

use super::{
    AcceptedInput, ApprovalModeChange, ArchiveChange, ChildTaskStart, ChildToolExecutionStart,
    CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementResult, ContextReplacementTarget, ConversationMessageLocationRequest,
    ConversationRawWindowRequest, ConversationRewrite, ConversationSearchHit,
    ConversationSearchPage, ConversationSearchRequest, ConversationSearchScope,
    ConversationWindowRequest, CrossSessionInputBinding, GoalClear, GoalHeldInputResume,
    GoalHeldInputResumeResult, GoalStop, GoalStopResult, InputOrigin, MessageFeedbackChange,
    ModelChange, NewAttachmentUpload, NewStoredChildTask, NewStoredInput, NewStoredRunAttempt,
    NewStoredSession, NewStoredSessionCommand, NewStoredSessionMaterialization,
    NewWorkspaceRegistration, PendingChildToolExchange, PendingToolExchange, QueuePriorityChange,
    ReasoningEffortChange, RecoveredRuntime, RewriteResult, RuntimeStore, SessionCommandCommit,
    SessionDeletion, SessionFork, SessionHistoryClear, SessionHistoryClearResult,
    SessionHistoryCompactionFinish, SessionHistoryCompactionFinishKind,
    SessionHistoryCompactionPreparation, SessionHistoryCompactionPreparationResult,
    SessionPinnedChange, SessionProxyChange, SessionProxyState, SessionRole, SessionTitleChange,
    SessionTitleGenerationCommit, SessionTitleGenerationCommitResult, StoreError, StoreErrorKind,
    StoreFuture, StoredAttachment, StoredAttachmentState, StoredChildTask,
    StoredChildTaskSettlement, StoredConversationMessageLocation, StoredConversationRawWindow,
    StoredConversationState, StoredConversationWindow, StoredGoal, StoredGoalPauseReason,
    StoredGoalSettlementEffect, StoredGoalState, StoredInput, StoredInputState,
    StoredMessageFeedback, StoredRun, StoredRunContinuation, StoredRunContinuationResult,
    StoredRunSettlement, StoredRunSettlementResult, StoredSession, StoredSessionCommand,
    StoredSessionCommandState, StoredSessionFork, StoredSessionLifecycle,
    StoredSessionMaterialization, StoredSessionUsage, StoredTerminalRunInputReconciliation,
    StoredTodoItemStatus, StoredWorkPlan, StoredWorkspace, StoredWorkspaceLifecycle,
    ToolExecutionStart, UserMessageCommit, VariantChange, WorkPlanClear, WorkPlanMutation,
    WorkPlanMutationResult, WorkspaceRemoval, WorkspaceUpdate, validate_input_message,
    validate_input_message_with_channel_source,
};
use crate::{
    AcceptedStoredSessionCommand, DeviceLifecycle, DeviceNameChange, DeviceRevocation,
    DeviceRevocationResult, MemoryContextSnapshot, NewPairedDevice, PairedDevice, PcOutputHosting,
    PcOutputHostingChange, PersonaMutation, PersonaSnapshot, PinnedMemoryMutation,
    PinnedMemoryMutationResult, SkillActivationOwner, SkillActivationTrigger, SkillName,
    SkillNameState, SkillNameStateChange, StoredMcpSelection, StoredPinnedMemory,
    StoredSkillActivation,
};

mod adapter;
mod child_task;
mod context;
mod conversation;
mod global_state;
mod goal;
mod input;
mod lifecycle;
mod model_management;
mod run;
mod session;
mod session_history;
mod session_metadata;
mod work_plan;
mod workspace;

// 虚拟资源仍须满足平台绝对路径契约；不创建或访问该目录。
const VOLATILE_ROOT: &str = if cfg!(windows) {
    "C:/volatile"
} else {
    "/volatile"
};

struct VolatilePendingExchange {
    session_id: SessionId,
    run_id: RunId,
    step: u32,
    assistant: AssistantMessage,
    started_calls: BTreeSet<String>,
}

struct VolatileChildPendingExchange {
    child_task_id: ChildTaskId,
    session_id: SessionId,
    step: u32,
    assistant: AssistantMessage,
    started_calls: BTreeSet<String>,
}

struct VolatileCompactionReceipt {
    session_id: SessionId,
    source_generation: u64,
    outcome: Option<CompactSessionOutcome>,
}

#[derive(Default)]
struct State {
    default_agent_shell: Option<assistant_protocol::ShellKind>,
    providers: BTreeMap<assistant_protocol::ProviderInstanceId, crate::StoredProvider>,
    model_settings: assistant_protocol::ModelSettings,
    fixed_models:
        BTreeMap<assistant_protocol::ModelSelection, assistant_protocol::ModelFixedConfig>,
    devices: BTreeMap<assistant_protocol::DeviceId, PairedDevice>,
    persona: PersonaSnapshot,
    pinned_collection_revision: u64,
    pinned_memories: BTreeMap<String, StoredPinnedMemory>,
    skill_name_states: BTreeMap<SkillName, SkillNameState>,
    workspaces: BTreeMap<WorkspaceId, StoredWorkspace>,
    attachments: BTreeMap<AttachmentId, StoredAttachment>,
    sessions: BTreeMap<SessionId, StoredSession>,
    conversations: BTreeMap<SessionId, ConversationSnapshot>,
    inputs: BTreeMap<InputId, StoredInput>,
    session_commands: BTreeMap<InputId, StoredSessionCommand>,
    mcp_input_selections: BTreeMap<String, StoredMcpSelection>,
    runs: BTreeMap<RunId, StoredRun>,
    child_tasks: BTreeMap<ChildTaskId, StoredChildTask>,
    child_conversations: BTreeMap<ChildTaskId, ConversationSnapshot>,
    message_feedback: BTreeMap<(SessionId, ProtocolMessageId), MessageFeedback>,
    pending_tool_exchanges: BTreeMap<String, VolatilePendingExchange>,
    pending_child_tool_exchanges: BTreeMap<String, VolatileChildPendingExchange>,
    session_usage: BTreeMap<SessionId, StoredSessionUsage>,
    work_plans: BTreeMap<SessionId, StoredWorkPlan>,
    work_plan_completion_receipts: BTreeMap<(SessionId, String), StoredWorkPlan>,
    goals: BTreeMap<SessionId, StoredGoal>,
    skill_activations: BTreeMap<String, StoredSkillActivation>,
    usage_request_ids: BTreeSet<(SessionId, String)>,
    session_history_clears: BTreeMap<IdempotencyKey, SessionHistoryClearResult>,
    session_history_compactions: BTreeMap<IdempotencyKey, VolatileCompactionReceipt>,
    next_queue_order: u64,
}

/// 不跨进程保留数据的 RuntimeStore；正式 Runtime Host 不使用该实现。
#[derive(Default)]
pub(crate) struct VolatileRuntimeStore {
    state: Mutex<State>,
}

impl VolatileRuntimeStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, StoreError> {
        self.state.lock().map_err(|_| {
            StoreError::new(
                StoreErrorKind::Unavailable,
                "volatile runtime storage is unavailable",
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn force_terminal_run_with_queued_input(
        &self,
        run_id: &RunId,
        status: RunStatus,
        finished_at_ms: i64,
    ) -> Result<(), StoreError> {
        if !status.is_terminal() {
            return Err(StoreError::new(
                StoreErrorKind::InvalidInput,
                "test run status is not terminal",
            ));
        }
        let mut state = self.lock()?;
        let input_id = state
            .runs
            .get(run_id)
            .ok_or_else(|| conflict("test run does not exist"))?
            .input_id
            .clone();
        let input = state
            .inputs
            .get(&input_id)
            .ok_or_else(|| conflict("test run input does not exist"))?;
        if input.state != StoredInputState::Queued || input.queued_message.is_none() {
            return Err(conflict("test run input is not queued"));
        }
        let run = state.runs.get_mut(run_id).expect("run existence checked");
        run.status = status;
        run.finished_at_ms = Some(finished_at_ms);
        Ok(())
    }
}

fn append(
    state: &mut State,
    session_id: &SessionId,
    messages: &[ConversationMessage],
) -> Result<(), StoreError> {
    if messages.is_empty() {
        return Ok(());
    }
    let conversation = state
        .conversations
        .get_mut(session_id)
        .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
    let mut candidate = conversation.messages.clone();
    candidate.extend_from_slice(messages);
    let candidate = ConversationSnapshot::new(candidate);
    candidate.validate_tool_exchange_pairs().map_err(|source| {
        StoreError::with_source(
            StoreErrorKind::InvalidInput,
            "conversation append is invalid",
            source,
        )
    })?;
    *conversation = candidate;
    let count = u64::try_from(conversation.messages.len()).map_err(|source| {
        StoreError::with_source(
            StoreErrorKind::Internal,
            "conversation message count exceeds storage range",
            source,
        )
    })?;
    state
        .sessions
        .get_mut(session_id)
        .ok_or_else(|| conflict("session does not exist in runtime storage"))?
        .message_count = count;
    Ok(())
}

fn validate_volatile_goal_effect(
    state: &State,
    settlement: &StoredRunSettlement,
) -> Result<(), StoreError> {
    let Some(effect) = settlement.goal_effect.as_ref() else {
        return Ok(());
    };
    let (expected_goal_id, expected_generation, goal) = match effect {
        StoredGoalSettlementEffect::Progress {
            expected_goal_id,
            expected_generation,
            goal,
        } => (expected_goal_id, *expected_generation, goal),
        StoredGoalSettlementEffect::Transition {
            expected_goal_id,
            expected_generation,
            goal,
            ..
        } => (expected_goal_id, *expected_generation, goal),
    };
    let current = state
        .goals
        .get(&settlement.session_id)
        .ok_or_else(|| conflict("Goal settlement has no current Goal"))?;
    if current.goal_id != *expected_goal_id
        || current.generation != expected_generation
        || current.state != StoredGoalState::Running
        || goal.goal_id != current.goal_id
        || goal.session_id != settlement.session_id
        || goal.objective != current.objective
        || goal.budget.max_runs != current.budget.max_runs
        || goal.budget.max_total_tokens != current.budget.max_total_tokens
        || goal.budget.max_consecutive_failures != current.budget.max_consecutive_failures
        || goal.budget.used_runs != current.budget.used_runs.saturating_add(1)
        || goal.budget.used_total_tokens < current.budget.used_total_tokens
        || (!current.budget.usage_complete && goal.budget.usage_complete)
        || goal.updated_at_ms != settlement.finished_at_ms
        || crate::goal::GoalControl::try_from(goal.clone()).is_err()
    {
        return Err(conflict("Goal settlement CAS or projection is invalid"));
    }
    let run = state
        .runs
        .get(&settlement.run_id)
        .ok_or_else(|| conflict("Goal settlement run does not exist"))?;
    let input = state
        .inputs
        .get(&run.input_id)
        .ok_or_else(|| conflict("Goal settlement input does not exist"))?;
    if input.goal_binding.as_ref().is_none_or(|binding| {
        binding.goal_id != *expected_goal_id || binding.generation != expected_generation
    }) {
        return Err(conflict("Goal settlement run binding is stale"));
    }
    match effect {
        StoredGoalSettlementEffect::Progress { .. } => {
            if goal.state != StoredGoalState::Running
                || goal.pause_reason.is_some()
                || goal.generation != expected_generation
                || goal.turn != current.turn.saturating_add(1)
            {
                return Err(conflict("Goal continuation projection is invalid"));
            }
        }
        StoredGoalSettlementEffect::Transition { .. } => {
            if goal.state == StoredGoalState::Running
                || goal.generation != expected_generation.saturating_add(1)
                || goal.turn != current.turn
            {
                return Err(conflict("Goal terminal transition is invalid"));
            }
        }
    }
    Ok(())
}

fn validate_volatile_proxy_report(
    state: &State,
    settlement: &StoredRunSettlement,
) -> Result<(), StoreError> {
    let Some(report) = settlement.proxy_report.as_deref() else {
        return Ok(());
    };
    validate_input_message_with_channel_source(
        report.origin,
        report.goal_binding.as_ref(),
        report.cross_session.as_ref(),
        report.channel_source.as_ref(),
        &report.message,
    )
    .map_err(|_| conflict("proxy report message is invalid"))?;
    let Some(CrossSessionInputBinding::ProxyReport {
        source_session_id,
        source_run_id,
        source_goal_id,
        source_run_status,
        ..
    }) = report
        .cross_session
        .as_ref()
        .map(|envelope| &envelope.binding)
    else {
        return Err(conflict("proxy report binding is missing"));
    };
    let source_run = state
        .runs
        .get(&settlement.run_id)
        .ok_or_else(|| conflict("proxy report source Run does not exist"))?;
    let source_input = state
        .inputs
        .get(&source_run.input_id)
        .ok_or_else(|| conflict("proxy report source Input does not exist"))?;
    let source = state
        .sessions
        .get(&settlement.session_id)
        .ok_or_else(|| conflict("proxy report source Session does not exist"))?;
    let target_valid = state
        .sessions
        .get(&report.session_id)
        .is_some_and(|target| {
            target.role == SessionRole::Controller
                && target.lifecycle == StoredSessionLifecycle::Active
        });
    let source_queue_empty = !state.inputs.values().any(|input| {
        input.session_id == settlement.session_id
            && input.state == StoredInputState::Queued
            && input.input_id != source_input.input_id
    });
    if report.origin != InputOrigin::Runtime
        || report.goal_binding.is_some()
        || report.skill_activation.is_some()
        || report.new_goal.is_some()
        || report.resumed_goal.is_some()
        || report.generated_title.is_some()
        || report.idempotency_key.is_none()
        || source_session_id != &settlement.session_id
        || source_run_id != &settlement.run_id
        || *source_run_status != settlement.status
        || source_goal_id.as_ref()
            != source_input
                .goal_binding
                .as_ref()
                .map(|binding| &binding.goal_id)
        || source.role != SessionRole::Standard
        || source.lifecycle != StoredSessionLifecycle::Active
        || source
            .proxy
            .as_ref()
            .map(|proxy| &proxy.controller_session_id)
            != Some(&report.session_id)
        || !target_valid
        || !source_queue_empty
        || matches!(
            settlement.goal_effect,
            Some(StoredGoalSettlementEffect::Progress { .. })
        )
        || state.inputs.contains_key(&report.input_id)
        || state.runs.contains_key(&report.run_id)
    {
        return Err(conflict("proxy report is not currently accepted"));
    }
    Ok(())
}

fn insert_volatile_proxy_report(
    state: &mut State,
    report: NewStoredInput,
) -> Result<AcceptedInput, StoreError> {
    state.next_queue_order = state.next_queue_order.saturating_add(1);
    let stored_input = StoredInput {
        agent_shell_target: None,
        queue_order: state.next_queue_order,
        input_id: report.input_id.clone(),
        session_id: report.session_id.clone(),
        idempotency_key: report.idempotency_key,
        agent_variant: report.agent_variant,
        origin: report.origin,
        goal_binding: None,
        cross_session: report.cross_session,
        channel_source: report.channel_source,
        skill_activation: None,
        user_message_id: report.message.id.clone(),
        state: StoredInputState::Queued,
        queued_message: Some(report.message),
        accepted_at_ms: report.accepted_at_ms,
    };
    let stored_run = StoredRun {
        shell: None,
        run_id: report.run_id,
        session_id: report.session_id,
        input_id: report.input_id,
        attempt: 1,
        status: RunStatus::Accepted,
        agent_variant: report.agent_variant,
        approval_mode: report.approval_mode,
        reasoning_effort: None,
        cancel_requested: false,
        error: None,
        message_ids: Vec::new(),
        message_steps: std::collections::HashMap::new(),
        created_at_ms: report.accepted_at_ms,
        started_at_ms: None,
        finished_at_ms: None,
    };
    state
        .inputs
        .insert(stored_input.input_id.clone(), stored_input.clone());
    state
        .runs
        .insert(stored_run.run_id.clone(), stored_run.clone());
    Ok(AcceptedInput {
        input: stored_input,
        run: stored_run,
        is_duplicate: false,
    })
}

fn apply_volatile_goal_effect(
    state: &mut State,
    effect: Option<StoredGoalSettlementEffect>,
) -> Result<StoredRunSettlementResult, StoreError> {
    let Some(effect) = effect else {
        return Ok(StoredRunSettlementResult::default());
    };
    match effect {
        StoredGoalSettlementEffect::Progress { goal, .. } => {
            state.goals.insert(goal.session_id.clone(), goal.clone());
            Ok(StoredRunSettlementResult {
                goal: Some(goal),
                accepted_proxy_report: None,
                resume_required: false,
            })
        }
        StoredGoalSettlementEffect::Transition {
            goal,
            resume_required,
            ..
        } => {
            if goal.state == StoredGoalState::Completed {
                state.goals.remove(&goal.session_id);
            } else {
                state.goals.insert(goal.session_id.clone(), goal.clone());
            }
            Ok(StoredRunSettlementResult {
                goal: Some(goal),
                accepted_proxy_report: None,
                resume_required,
            })
        }
    }
}

fn append_child(
    state: &mut State,
    child_task_id: &ChildTaskId,
    messages: &[ConversationMessage],
) -> Result<(), StoreError> {
    if messages.is_empty() {
        return Ok(());
    }
    let conversation = state
        .child_conversations
        .get_mut(child_task_id)
        .ok_or_else(|| conflict("child task conversation does not exist"))?;
    let mut candidate = conversation.messages.clone();
    candidate.extend_from_slice(messages);
    let candidate = ConversationSnapshot::new(candidate);
    candidate.validate_tool_exchange_pairs().map_err(|source| {
        StoreError::with_source(
            StoreErrorKind::InvalidInput,
            "child task conversation append is invalid",
            source,
        )
    })?;
    *conversation = candidate;
    let count = u64::try_from(conversation.messages.len()).map_err(|source| {
        StoreError::with_source(
            StoreErrorKind::Internal,
            "child task message count exceeds storage range",
            source,
        )
    })?;
    state
        .child_tasks
        .get_mut(child_task_id)
        .ok_or_else(|| conflict("child task does not exist"))?
        .message_count = count;
    Ok(())
}

fn ensure_child_owner(
    state: &State,
    session_id: &SessionId,
    child_task_id: &ChildTaskId,
) -> Result<(), StoreError> {
    let task = state
        .child_tasks
        .get(child_task_id)
        .ok_or_else(|| conflict("child task does not exist"))?;
    if task.session_id != *session_id {
        return Err(conflict("child task belongs to a different session"));
    }
    Ok(())
}

fn ensure_child_running(
    state: &State,
    session_id: &SessionId,
    child_task_id: &ChildTaskId,
) -> Result<(), StoreError> {
    ensure_child_owner(state, session_id, child_task_id)?;
    if state
        .child_tasks
        .get(child_task_id)
        .is_none_or(|task| task.status != ChildTaskStatus::Running)
    {
        return Err(conflict("child task is not running"));
    }
    Ok(())
}

fn ensure_child_settleable(
    state: &State,
    session_id: &SessionId,
    child_task_id: &ChildTaskId,
) -> Result<(), StoreError> {
    ensure_child_owner(state, session_id, child_task_id)?;
    if state.child_tasks.get(child_task_id).is_none_or(|task| {
        !matches!(
            task.status,
            ChildTaskStatus::Accepted | ChildTaskStatus::Running
        )
    }) {
        return Err(conflict("child task is not settleable"));
    }
    Ok(())
}

fn message_id(message: &ConversationMessage) -> &MessageId {
    match message {
        ConversationMessage::System(message) => &message.id,
        ConversationMessage::ContextSummary(message) => &message.id,
        ConversationMessage::User(message) => &message.id,
        ConversationMessage::Assistant(message) => &message.id,
        ConversationMessage::Tool(message) => &message.id,
    }
}

fn record_session_usage(
    state: &mut State,
    session_id: &SessionId,
    messages: &[ConversationMessage],
) {
    let requests = messages.iter().filter_map(|message| match message {
        ConversationMessage::Assistant(message) => message
            .usage
            .as_ref()
            .map(|usage| (message.id.as_str().to_owned(), usage.clone())),
        ConversationMessage::ContextSummary(message) => message
            .usage
            .as_ref()
            .map(|usage| (message.id.as_str().to_owned(), usage.clone())),
        _ => None,
    });
    for (request_id, usage) in requests {
        if !state
            .usage_request_ids
            .insert((session_id.clone(), request_id))
        {
            continue;
        }
        accumulate_usage(
            state.session_usage.entry(session_id.clone()).or_default(),
            usage,
        );
    }
}

fn accumulate_usage(total: &mut StoredSessionUsage, usage: TokenUsage) {
    total.request_count = total.request_count.saturating_add(1);
    total.input_tokens = total.input_tokens.saturating_add(usage.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(usage.output_tokens);
    total.total_tokens = total.total_tokens.saturating_add(usage.total_tokens);
    if let Some(cached) = usage.cached_input_tokens {
        total.cached_input_tokens = total.cached_input_tokens.saturating_add(cached);
        total.cached_request_count = total.cached_request_count.saturating_add(1);
    }
    if let Some(reasoning) = usage.reasoning_tokens {
        total.reasoning_tokens = total.reasoning_tokens.saturating_add(reasoning);
        total.reasoning_request_count = total.reasoning_request_count.saturating_add(1);
    }
    total.latest = Some(usage);
}

fn ensure_idle(state: &State, session_id: &SessionId) -> Result<(), StoreError> {
    if state
        .inputs
        .values()
        .any(|input| input.session_id == *session_id && input.state == StoredInputState::Queued)
        || state.session_commands.values().any(|command| {
            command.session_id == *session_id && command.state == StoredSessionCommandState::Queued
        })
        || state
            .runs
            .values()
            .any(|run| run.session_id == *session_id && !run.status.is_terminal())
        || state
            .pending_tool_exchanges
            .values()
            .any(|exchange| exchange.session_id == *session_id)
        || state
            .child_tasks
            .values()
            .any(|task| task.session_id == *session_id && !task.status.is_terminal())
        || state
            .pending_child_tool_exchanges
            .values()
            .any(|exchange| exchange.session_id == *session_id)
    {
        return Err(conflict("session is not idle"));
    }
    Ok(())
}

fn conversation_window(
    snapshot: ConversationSnapshot,
    request: &ConversationWindowRequest,
) -> StoredConversationWindow {
    let display_indices = snapshot
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            (message.is_transcript_visible()
                || matches!(message, ConversationMessage::ContextSummary(_)))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let total = display_indices.len();
    let end = request.end.unwrap_or(total).min(total);
    let start = end.saturating_sub(request.limit);
    let raw_start = display_indices
        .get(start)
        .copied()
        .unwrap_or(snapshot.messages.len());
    let raw_end = display_indices
        .get(end)
        .copied()
        .unwrap_or(snapshot.messages.len());
    StoredConversationWindow {
        generation: request.generation,
        start,
        end,
        total,
        conversation: ConversationSnapshot::new(snapshot.messages[raw_start..raw_end].to_vec()),
    }
}

fn volatile_scope_matches(scope: &ConversationSearchScope, session: &StoredSession) -> bool {
    match scope {
        ConversationSearchScope::Session { session_id } => session.session_id == *session_id,
        ConversationSearchScope::Workspace { workspace_id } => {
            session.environment.workspace_id.as_ref() == Some(workspace_id)
        }
        ConversationSearchScope::Global => true,
    }
}

fn collect_volatile_hits(
    hits: &mut Vec<ConversationSearchHit>,
    owner: ConversationOwner,
    generation: u64,
    created_at_ms: i64,
    snapshot: &ConversationSnapshot,
    query: &str,
) {
    for (ordinal, message) in snapshot.messages.iter().enumerate() {
        if !message.is_transcript_visible() {
            continue;
        }
        let (message_id, text) = match message {
            ConversationMessage::User(message) => {
                let mut parts = Vec::new();
                for part in &message.parts {
                    match part {
                        UserPart::Text(part) => parts.push(part.text.clone()),
                        UserPart::FileReferences(references) => parts.extend(
                            references
                                .files
                                .iter()
                                .map(|file| file.original_name.clone()),
                        ),
                        UserPart::QuotedText(quoted) => parts.push(quoted.exact.clone()),
                        UserPart::Injected(_) | UserPart::InternalContext(_) => {}
                    }
                }
                let text = parts.join("\n");
                (&message.id, text)
            }
            ConversationMessage::Assistant(message) => {
                let text = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        AssistantPart::Text(part) => Some(part.text.as_str()),
                        AssistantPart::Reasoning(_)
                        | AssistantPart::ToolCall(_)
                        | AssistantPart::ProviderState(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (&message.id, text)
            }
            ConversationMessage::System(_)
            | ConversationMessage::ContextSummary(_)
            | ConversationMessage::Tool(_) => continue,
        };
        let normalized = normalize_recall_text(&text);
        if !normalized.contains(query) {
            continue;
        }
        hits.push(ConversationSearchHit {
            child_task_title: None,
            owner: owner.clone(),
            generation,
            message_id: message_id.clone(),
            message_ordinal: u64::try_from(ordinal).unwrap_or(u64::MAX),
            created_at_ms,
            text,
        });
    }
}

fn normalize_recall_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn rewrite_file_reference_paths(
    conversation: &mut ConversationSnapshot,
    rewrites: &BTreeMap<String, String>,
) -> Result<(), StoreError> {
    for message in &mut conversation.messages {
        let ConversationMessage::User(user) = message else {
            continue;
        };
        for part in &mut user.parts {
            let UserPart::FileReferences(references) = part else {
                continue;
            };
            for file in &mut references.files {
                file.readable_path =
                    rewrites.get(&file.readable_path).cloned().ok_or_else(|| {
                        conflict("fork conversation references an unmapped attachment")
                    })?;
            }
        }
    }
    Ok(())
}

fn count_u64(value: usize) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| conflict("session impact exceeds supported range"))
}

fn conflict(message: &'static str) -> StoreError {
    StoreError::new(StoreErrorKind::Conflict, message)
}

fn validate_volatile_input_activation(
    state: &State,
    input: &NewStoredInput,
) -> Result<(), StoreError> {
    let skill_parts = input
        .message
        .parts
        .iter()
        .filter_map(|part| match part {
            UserPart::InternalContext(part) if part.kind == "skill_activation" => Some(part),
            _ => None,
        })
        .collect::<Vec<_>>();
    let Some(activation) = input.skill_activation.as_ref() else {
        return if skill_parts.is_empty() {
            Ok(())
        } else {
            Err(conflict("input has an unbound skill activation context"))
        };
    };
    if state
        .skill_activations
        .contains_key(&activation.activation_id)
        || input.origin != InputOrigin::User
        || activation.trigger != SkillActivationTrigger::User
        || activation.session_id != input.session_id
        || activation.run_id.as_ref() != Some(&input.run_id)
        || activation.input_id.as_ref() != Some(&input.input_id)
        || activation.message_id != input.message.id
        || !matches!(
            &activation.owner,
            SkillActivationOwner::Session(session_id) if session_id == &input.session_id
        )
        || skill_parts.len() != 1
    {
        return Err(conflict("input skill activation is inconsistent"));
    }
    Ok(())
}

fn stored_session(session: NewStoredSession) -> StoredSession {
    StoredSession {
        agent_shell_kind: session.agent_shell_kind,
        agent_shell_environment: session.agent_shell_environment.clone(),
        session_id: session.session_id,
        title: session.title,
        title_origin: session.title_origin,
        model_selection: session.model_selection,
        reasoning_effort: session.reasoning_effort,
        system_prompt: session.system_prompt,

        environment: session.environment,
        lifecycle: StoredSessionLifecycle::Active,
        current_variant: session.current_variant,
        approval_mode: session.approval_mode,
        role: session.role,
        materialization_key: session.materialization_key,
        automatic_title_pending: session.automatic_title_pending,
        proxy: None,
        pc_output_hosting: None,
        body_generation: 1,
        message_count: 0,
        created_at_ms: session.created_at_ms,
        updated_at_ms: session.created_at_ms,
        archived_at_ms: None,
        is_pinned: false,
        conversation_state: StoredConversationState::Available,
    }
}

fn materialization_semantically_matches(
    existing: &StoredSession,
    attachments: &[StoredAttachment],
    input: &StoredInput,
    persisted_message: Option<&agent_types::UserMessage>,
    candidate: &NewStoredSessionMaterialization,
) -> bool {
    let mut existing_files = attachments
        .iter()
        .map(|file| {
            (
                file.original_name.as_str(),
                file.blob_hash.as_str(),
                file.size_bytes,
                file.media_type.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    let mut candidate_files = candidate
        .attachments
        .iter()
        .map(|file| {
            (
                file.original_name.as_str(),
                file.blob_hash.as_str(),
                file.size_bytes,
                file.media_type.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    existing_files.sort_unstable();
    candidate_files.sort_unstable();
    existing.title == candidate.session.title
        && existing.model_selection == candidate.session.model_selection
        && existing.reasoning_effort == candidate.session.reasoning_effort
        && existing.environment.workspace_id == candidate.session.environment.workspace_id
        && (existing.environment.workspace_id.is_none()
            || existing.environment.working_directory
                == candidate.session.environment.working_directory)
        && existing.environment.additional_workspace_directories
            == candidate
                .session
                .environment
                .additional_workspace_directories
        && existing.current_variant == candidate.session.current_variant
        && existing.approval_mode == candidate.session.approval_mode
        && existing_files == candidate_files
        && input.agent_variant == candidate.input.agent_variant
        && input.goal_binding.is_some() == candidate.input.goal_binding.is_some()
        && input.skill_activation.as_ref().map(|value| &value.name)
            == candidate
                .input
                .skill_activation
                .as_ref()
                .map(|value| &value.name)
        && normalized_message(persisted_message)
            == normalized_message(Some(&candidate.input.message))
}

fn normalized_message(message: Option<&agent_types::UserMessage>) -> Option<serde_json::Value> {
    let mut message = message?.clone();
    message
        .parts
        .retain(|part| !matches!(part, UserPart::InternalContext(_)));
    let mut value = serde_json::to_value(message).ok()?;
    remove_generated_message_fields(&mut value);
    Some(value)
}

fn remove_generated_message_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("id");
            map.remove("quote_id");
            map.remove("readable_path");
            for value in map.values_mut() {
                remove_generated_message_fields(value);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                remove_generated_message_fields(value);
            }
        }
        _ => {}
    }
}

fn validate_model_activations(
    state: &State,
    session_id: &SessionId,
    expected_owner: &SkillActivationOwner,
    expected_run_id: &RunId,
    message: Option<&agent_types::UserMessage>,
    activations: &[StoredSkillActivation],
) -> Result<(), StoreError> {
    if activations.is_empty() {
        return if message.is_none() {
            Ok(())
        } else {
            Err(conflict(
                "tool exchange has an unbound skill activation message",
            ))
        };
    }
    let message = message.ok_or_else(|| conflict("model skill activation message is missing"))?;
    let activation_ids = activations
        .iter()
        .map(|activation| activation.activation_id.as_str())
        .collect::<BTreeSet<_>>();
    let names = activations
        .iter()
        .map(|activation| &activation.name)
        .collect::<BTreeSet<_>>();
    if message.origin != agent_types::UserMessageOrigin::Runtime
        || message.transcript_visibility != agent_types::TranscriptVisibility::Hidden
        || activation_ids.len() != activations.len()
        || names.len() != activations.len()
        || activations.iter().any(|activation| {
            state
                .skill_activations
                .contains_key(&activation.activation_id)
                || activation.session_id != *session_id
                || &activation.owner != expected_owner
                || activation.trigger != SkillActivationTrigger::Model
                || activation.run_id.as_ref() != Some(expected_run_id)
                || activation.input_id.is_some()
                || activation.message_id != message.id
        })
    {
        return Err(conflict("model skill activation is inconsistent"));
    }
    let boundary_count = message
        .parts
        .iter()
        .filter(|part| {
            matches!(
                part,
                UserPart::InternalContext(part) if part.kind == "skill_activation"
            )
        })
        .count();
    if boundary_count != activations.len() {
        return Err(conflict("model skill activation boundary is inconsistent"));
    }
    Ok(())
}

fn pause_running_goals_for_recovery(
    goals: &mut BTreeMap<SessionId, StoredGoal>,
) -> Result<(), StoreError> {
    for goal in goals.values_mut() {
        if goal.state != StoredGoalState::Running {
            continue;
        }
        goal.generation = goal.generation.checked_add(1).ok_or_else(|| {
            StoreError::new(
                StoreErrorKind::InvalidData,
                "stored goal generation is exhausted",
            )
        })?;
        goal.state = StoredGoalState::Paused;
        goal.pause_reason = Some(StoredGoalPauseReason::RecoveryRequired);
    }
    Ok(())
}

impl State {
    fn provider_usage(
        &self,
        id: &assistant_protocol::ProviderInstanceId,
    ) -> assistant_protocol::ProviderUsage {
        let references = |selection: &Option<assistant_protocol::ModelSelection>| {
            selection
                .as_ref()
                .is_some_and(|selection| &selection.provider_instance_id == id)
        };
        let sessions = self
            .sessions
            .values()
            .filter(|session| references(&session.model_selection));
        assistant_protocol::ProviderUsage {
            default_model: references(&self.model_settings.default_model),
            vision_model: references(&self.model_settings.vision_model),
            session_count: sessions.clone().count() as u64,
            fixed_config_count: self
                .fixed_models
                .keys()
                .filter(|selection| &selection.provider_instance_id == id)
                .count() as u64,
            sessions: sessions
                .take(20)
                .map(|session| assistant_protocol::ProviderSessionUsage {
                    session_id: session.session_id.clone(),
                    title: session.title.clone(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests;
