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
    SessionHistoryCleanupStatus, SessionId, SessionTitleOrigin, WorkspaceId,
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
    NewStoredSession, NewWorkspaceRegistration, PendingChildToolExchange, PendingToolExchange,
    QueuePriorityChange, ReasoningEffortChange, RecoveredRuntime, RewriteResult, RuntimeStore,
    SessionDeletion, SessionFork, SessionHistoryClear, SessionHistoryClearResult,
    SessionHistoryCompactionFinish, SessionHistoryCompactionFinishKind,
    SessionHistoryCompactionPreparation, SessionHistoryCompactionPreparationResult,
    SessionPinnedChange, SessionProxyChange, SessionProxyState, SessionRole, SessionTitleChange,
    StoreError, StoreErrorKind, StoreFuture, StoredAttachment, StoredAttachmentState,
    StoredChildTask, StoredChildTaskSettlement, StoredConversationMessageLocation,
    StoredConversationRawWindow, StoredConversationState, StoredConversationWindow, StoredGoal,
    StoredGoalPauseReason, StoredGoalSettlementEffect, StoredGoalState, StoredInput,
    StoredInputState, StoredMessageFeedback, StoredRun, StoredRunSettlement,
    StoredRunSettlementResult, StoredSession, StoredSessionFork, StoredSessionLifecycle,
    StoredSessionUsage, StoredTodoItemStatus, StoredWorkPlan, StoredWorkspace,
    StoredWorkspaceLifecycle, ToolExecutionStart, UserMessageCommit, VariantChange, WorkPlanClear,
    WorkPlanMutation, WorkPlanMutationResult, WorkspaceRemoval, validate_input_message,
};
use crate::{
    MemoryContextSnapshot, PersonaMutation, PersonaSnapshot, PinnedMemoryMutation,
    PinnedMemoryMutationResult, SkillActivationOwner, SkillActivationTrigger, SkillName,
    SkillNameState, SkillNameStateChange, StoredPinnedMemory, StoredSkillActivation,
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
    persona: PersonaSnapshot,
    pinned_collection_revision: u64,
    pinned_memories: BTreeMap<String, StoredPinnedMemory>,
    skill_name_states: BTreeMap<SkillName, SkillNameState>,
    workspaces: BTreeMap<WorkspaceId, StoredWorkspace>,
    attachments: BTreeMap<AttachmentId, StoredAttachment>,
    sessions: BTreeMap<SessionId, StoredSession>,
    conversations: BTreeMap<SessionId, ConversationSnapshot>,
    inputs: BTreeMap<InputId, StoredInput>,
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

impl RuntimeStore for VolatileRuntimeStore {
    fn load_runtime(&self) -> StoreFuture<'_, RecoveredRuntime> {
        Box::pin(async move {
            let mut state = self.lock()?;
            state.work_plans.retain(|_, plan| {
                plan.items.is_empty()
                    || plan
                        .items
                        .iter()
                        .any(|item| item.status != StoredTodoItemStatus::Completed)
            });
            state
                .goals
                .retain(|_, goal| goal.state != StoredGoalState::Completed);
            pause_running_goals_for_recovery(&mut state.goals)?;
            let stale_goal_inputs = state
                .inputs
                .values()
                .filter(|input| {
                    input.state == StoredInputState::Queued
                        && input.origin == InputOrigin::Runtime
                        && input.goal_binding.as_ref().is_some_and(|binding| {
                            state.goals.get(&input.session_id).is_some_and(|goal| {
                                goal.goal_id == binding.goal_id
                                    && binding.generation < goal.generation
                            })
                        })
                })
                .map(|input| input.input_id.clone())
                .collect::<BTreeSet<_>>();
            state
                .inputs
                .retain(|input_id, _| !stale_goal_inputs.contains(input_id));
            state
                .runs
                .retain(|_, run| !stale_goal_inputs.contains(&run.input_id));
            state.skill_activations.retain(|_, activation| {
                activation
                    .input_id
                    .as_ref()
                    .is_none_or(|input_id| !stale_goal_inputs.contains(input_id))
            });
            Ok(RecoveredRuntime {
                workspaces: state.workspaces.values().cloned().collect(),
                attachments: state.attachments.values().cloned().collect(),
                sessions: state.sessions.values().cloned().collect(),
                inputs: state.inputs.values().cloned().collect(),
                runs: state.runs.values().cloned().collect(),
                child_tasks: state.child_tasks.values().cloned().collect(),
                work_plans: state.work_plans.values().cloned().collect(),
                goals: state.goals.values().cloned().collect(),
                skill_activations: state.skill_activations.values().cloned().collect(),
            })
        })
    }

    fn list_skill_name_states(&self) -> StoreFuture<'_, Vec<SkillNameState>> {
        Box::pin(async move { Ok(self.lock()?.skill_name_states.values().cloned().collect()) })
    }

    fn set_skill_enabled(&self, change: SkillNameStateChange) -> StoreFuture<'_, SkillNameState> {
        Box::pin(async move {
            let state = SkillNameState {
                name: change.name,
                enabled: change.enabled,
                updated_at_ms: change.updated_at_ms,
            };
            self.lock()?
                .skill_name_states
                .insert(state.name.clone(), state.clone());
            Ok(state)
        })
    }

    fn load_work_plan(&self, session_id: &SessionId) -> StoreFuture<'_, Option<StoredWorkPlan>> {
        let session_id = session_id.clone();
        Box::pin(async move { Ok(self.lock()?.work_plans.get(&session_id).cloned()) })
    }

    fn mutate_work_plan(
        &self,
        mutation: WorkPlanMutation,
    ) -> StoreFuture<'_, WorkPlanMutationResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get(&mutation.session_id)
                .ok_or_else(|| conflict("work plan session does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("work plan session is archived"));
            }
            let receipt_key = (mutation.session_id.clone(), mutation.operation_id.clone());
            if let Some(plan) = state.work_plan_completion_receipts.get(&receipt_key) {
                return Ok(WorkPlanMutationResult {
                    plan: plan.clone(),
                    cleared: true,
                });
            }
            if let Some(current) = state.work_plans.get(&mutation.session_id) {
                if current.last_operation_id == mutation.operation_id {
                    return Ok(WorkPlanMutationResult {
                        plan: current.clone(),
                        cleared: false,
                    });
                }
                if current.revision != mutation.expected_revision {
                    return Err(conflict("work plan revision changed"));
                }
            } else if mutation.expected_revision != 0 {
                return Err(conflict("work plan revision changed"));
            }
            let revision = mutation
                .expected_revision
                .checked_add(1)
                .ok_or_else(|| conflict("work plan revision exhausted"))?;
            let stored = StoredWorkPlan {
                session_id: mutation.session_id.clone(),
                revision,
                objective: mutation.objective,
                items: mutation.items,
                last_operation_id: mutation.operation_id,
                updated_at_ms: mutation.updated_at_ms,
            };
            let cleared = stored.items.is_empty()
                || stored
                    .items
                    .iter()
                    .all(|item| item.status == StoredTodoItemStatus::Completed);
            if cleared {
                state.work_plans.remove(&mutation.session_id);
                state
                    .work_plan_completion_receipts
                    .insert(receipt_key, stored.clone());
            } else {
                state.work_plans.insert(mutation.session_id, stored.clone());
            }
            Ok(WorkPlanMutationResult {
                plan: stored,
                cleared,
            })
        })
    }

    fn clear_work_plan(&self, clear: WorkPlanClear) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get(&clear.session_id)
                .ok_or_else(|| conflict("work plan session does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("work plan session is archived"));
            }
            match state.work_plans.get(&clear.session_id) {
                Some(current) if current.revision == clear.expected_revision => {
                    state.work_plans.remove(&clear.session_id);
                    Ok(())
                }
                None if clear.expected_revision == 0 => Ok(()),
                _ => Err(conflict("work plan revision changed")),
            }
        })
    }

    fn load_memory_context(&self) -> StoreFuture<'_, MemoryContextSnapshot> {
        Box::pin(async move {
            let state = self.lock()?;
            Ok(MemoryContextSnapshot {
                persona: state.persona.clone(),
                pinned_collection_revision: state.pinned_collection_revision,
                pinned_memories: state.pinned_memories.values().cloned().collect(),
            })
        })
    }

    fn get_persona(&self) -> StoreFuture<'_, PersonaSnapshot> {
        Box::pin(async move { Ok(self.lock()?.persona.clone()) })
    }

    fn set_persona(&self, mutation: PersonaMutation) -> StoreFuture<'_, PersonaSnapshot> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if state.persona.revision != mutation.expected_revision {
                return Err(conflict("persona revision changed"));
            }
            let next_revision = mutation
                .expected_revision
                .checked_add(1)
                .ok_or_else(|| conflict("persona revision exhausted"))?;
            state.persona = PersonaSnapshot {
                enabled: mutation.enabled,
                content: mutation.content,
                revision: next_revision,
                updated_at_ms: mutation.updated_at_ms,
            };
            Ok(state.persona.clone())
        })
    }

    fn list_pinned_memories(&self) -> StoreFuture<'_, Vec<StoredPinnedMemory>> {
        Box::pin(async move { Ok(self.lock()?.pinned_memories.values().cloned().collect()) })
    }

    fn mutate_pinned_memory(
        &self,
        mutation: PinnedMemoryMutation,
    ) -> StoreFuture<'_, PinnedMemoryMutationResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let memory = match mutation {
                PinnedMemoryMutation::Create {
                    entry,
                    created_by,
                    expected_collection_revision,
                    changed_at_ms,
                } => {
                    if state.pinned_collection_revision != expected_collection_revision {
                        return Err(conflict("pinned memory collection revision changed"));
                    }
                    if state.pinned_memories.contains_key(entry.id.as_str()) {
                        return Err(conflict("pinned memory already exists"));
                    }
                    let stored = StoredPinnedMemory {
                        entry,
                        created_by,
                        created_at_ms: changed_at_ms,
                        updated_at_ms: changed_at_ms,
                        revision: 1,
                    };
                    state
                        .pinned_memories
                        .insert(stored.entry.id.as_str().to_owned(), stored.clone());
                    Some(stored)
                }
                PinnedMemoryMutation::Replace {
                    entry,
                    expected_revision,
                    changed_at_ms,
                } => {
                    let existing = state
                        .pinned_memories
                        .get_mut(entry.id.as_str())
                        .ok_or_else(|| conflict("pinned memory does not exist"))?;
                    if existing.revision != expected_revision {
                        return Err(conflict("pinned memory revision changed"));
                    }
                    let next_revision = existing
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| conflict("pinned memory revision exhausted"))?;
                    existing.entry = entry;
                    existing.updated_at_ms = changed_at_ms;
                    existing.revision = next_revision;
                    Some(existing.clone())
                }
                PinnedMemoryMutation::Delete {
                    id,
                    expected_revision,
                    changed_at_ms: _,
                } => {
                    let existing = state
                        .pinned_memories
                        .get(id.as_str())
                        .ok_or_else(|| conflict("pinned memory does not exist"))?;
                    if existing.revision != expected_revision {
                        return Err(conflict("pinned memory revision changed"));
                    }
                    state.pinned_memories.remove(id.as_str());
                    None
                }
            };
            state.pinned_collection_revision = state
                .pinned_collection_revision
                .checked_add(1)
                .ok_or_else(|| conflict("pinned memory collection revision exhausted"))?;
            Ok(PinnedMemoryMutationResult {
                memory,
                collection_revision: state.pinned_collection_revision,
            })
        })
    }

    fn register_workspace(
        &self,
        registration: NewWorkspaceRegistration,
    ) -> StoreFuture<'_, StoredWorkspace> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if let Some(existing) = state
                .workspaces
                .values_mut()
                .find(|workspace| workspace.user_directory == registration.requested_directory)
            {
                existing.lifecycle = StoredWorkspaceLifecycle::Active;
                existing.updated_at_ms = registration.changed_at_ms;
                existing.removed_at_ms = None;
                return Ok(existing.clone());
            }
            let stored = StoredWorkspace {
                workspace_id: registration.workspace_id.clone(),
                user_directory: registration.requested_directory,
                agent_directory: format!(
                    "/volatile/workspaces/{}/agent",
                    registration.workspace_id
                ),
                lifecycle: StoredWorkspaceLifecycle::Active,
                created_at_ms: registration.changed_at_ms,
                updated_at_ms: registration.changed_at_ms,
                removed_at_ms: None,
            };
            if state
                .workspaces
                .insert(registration.workspace_id, stored.clone())
                .is_some()
            {
                return Err(conflict("workspace already exists in runtime storage"));
            }
            Ok(stored)
        })
    }

    fn remove_workspace(&self, removal: WorkspaceRemoval) -> StoreFuture<'_, StoredWorkspace> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let workspace = state
                .workspaces
                .get_mut(&removal.workspace_id)
                .ok_or_else(|| conflict("workspace does not exist in runtime storage"))?;
            if workspace.lifecycle == StoredWorkspaceLifecycle::Active {
                workspace.lifecycle = StoredWorkspaceLifecycle::Removed;
                workspace.updated_at_ms = removal.changed_at_ms;
                workspace.removed_at_ms = Some(removal.changed_at_ms);
            }
            Ok(workspace.clone())
        })
    }

    fn upload_attachment(&self, upload: NewAttachmentUpload) -> StoreFuture<'_, StoredAttachment> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if let Some(existing) = state.attachments.values().find(|attachment| {
                attachment.session_id == upload.session_id
                    && attachment.blob_hash == upload.blob_hash
            }) {
                return Ok(existing.clone());
            }
            if !state.sessions.contains_key(&upload.session_id) {
                return Err(conflict("attachment session does not exist"));
            }
            let agent_readable_path = format!(
                "/volatile/sessions/{}/attachments/{}/file",
                upload.session_id, upload.attachment_id
            );
            let stored = StoredAttachment {
                attachment_id: upload.attachment_id.clone(),
                session_id: upload.session_id,
                original_name: upload.original_name,
                blob_hash: upload.blob_hash,
                size_bytes: upload.size_bytes,
                media_type: upload.media_type,
                agent_readable_path,
                state: StoredAttachmentState::Ready,
                created_at_ms: upload.created_at_ms,
            };
            if state
                .attachments
                .insert(upload.attachment_id, stored.clone())
                .is_some()
            {
                return Err(conflict("attachment already exists"));
            }
            Ok(stored)
        })
    }

    fn accept_input(&self, input: NewStoredInput) -> StoreFuture<'_, AcceptedInput> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if let Some(key) = input.idempotency_key.as_ref()
                && let Some(existing) = state.inputs.values().find(|candidate| {
                    candidate.session_id == input.session_id
                        && candidate.idempotency_key.as_ref() == Some(key)
                })
            {
                let existing = existing.clone();
                let run = state
                    .runs
                    .values()
                    .find(|run| run.input_id == existing.input_id && run.attempt == 1)
                    .cloned()
                    .ok_or_else(|| conflict("accepted input has no first run"))?;
                return Ok(AcceptedInput {
                    input: existing,
                    run,
                    is_duplicate: true,
                });
            }
            validate_input_message(
                input.origin,
                input.goal_binding.as_ref(),
                input.cross_session_binding.as_ref(),
                &input.message,
            )
            .map_err(|_| conflict("input message origin or Goal binding is invalid"))?;
            validate_volatile_input_activation(&state, &input)?;
            if input.new_goal.is_some() && input.resumed_goal.is_some() {
                return Err(conflict("input cannot start and resume a Goal together"));
            }
            if let Some(goal) = input.new_goal.as_ref() {
                let binding = input
                    .goal_binding
                    .as_ref()
                    .ok_or_else(|| conflict("new Goal input has no Goal binding"))?;
                if input.origin != InputOrigin::User
                    || goal.session_id != input.session_id
                    || goal.goal_id != binding.goal_id
                    || goal.generation != binding.generation
                    || goal.turn != binding.turn
                    || goal.objective.source_message_id != input.message.id
                    || state.goals.contains_key(&goal.session_id)
                {
                    return Err(conflict("new Goal does not match its first input"));
                }
            }
            if let Some(goal) = input.resumed_goal.as_ref() {
                let binding = input
                    .goal_binding
                    .as_ref()
                    .ok_or_else(|| conflict("resumed Goal input has no Goal binding"))?;
                let current = state
                    .goals
                    .get(&input.session_id)
                    .ok_or_else(|| conflict("resumed Goal does not exist"))?;
                if goal.session_id != input.session_id
                    || goal.goal_id != binding.goal_id
                    || goal.generation != binding.generation
                    || goal.turn != binding.turn
                    || current.state != StoredGoalState::Paused
                    || goal.state != StoredGoalState::Running
                    || goal.pause_reason.is_some()
                    || goal.generation
                        != current
                            .generation
                            .checked_add(1)
                            .ok_or_else(|| conflict("Goal generation is exhausted"))?
                    || goal.turn
                        != current
                            .turn
                            .checked_add(1)
                            .ok_or_else(|| conflict("Goal turn is exhausted"))?
                    || goal.objective != current.objective
                    || goal.budget != current.budget
                    || goal.consecutive_failures != current.consecutive_failures
                    || goal.created_at_ms != current.created_at_ms
                    || goal.updated_at_ms < current.updated_at_ms
                    || goal.completed_at_ms.is_some()
                    || (input.origin == InputOrigin::Runtime
                        && (input.idempotency_key.is_some()
                            || input.generated_title.is_some()
                            || input.new_goal.is_some()))
                {
                    return Err(conflict("resumed Goal projection is invalid"));
                }
            }
            match input.cross_session_binding.as_ref() {
                Some(CrossSessionInputBinding::ControllerDelivery {
                    controller_session_id,
                    controller_run_id,
                    ..
                }) => {
                    let source_valid =
                        state
                            .sessions
                            .get(controller_session_id)
                            .is_some_and(|session| {
                                session.role == SessionRole::Controller
                                    && session.lifecycle == StoredSessionLifecycle::Active
                            })
                            && state.runs.get(controller_run_id).is_some_and(|run| {
                                run.session_id == *controller_session_id
                                    && matches!(
                                        run.status,
                                        RunStatus::Running | RunStatus::Cancelling
                                    )
                            });
                    let target_valid =
                        state
                            .sessions
                            .get(&input.session_id)
                            .is_some_and(|session| {
                                session.role == SessionRole::Standard
                                    && session.lifecycle == StoredSessionLifecycle::Active
                                    && session.proxy.as_ref().is_some_and(|proxy| {
                                        proxy.controller_session_id == *controller_session_id
                                    })
                            });
                    let queue_exists = state.inputs.values().any(|candidate| {
                        candidate.session_id == input.session_id
                            && candidate.state == StoredInputState::Queued
                    });
                    if input.origin != InputOrigin::Runtime
                        || input.goal_binding.is_some()
                        || input.skill_activation.is_some()
                        || input.new_goal.is_some()
                        || input.resumed_goal.is_some()
                        || input.generated_title.is_some()
                        || input.idempotency_key.is_none()
                        || !source_valid
                        || !target_valid
                        || queue_exists
                    {
                        return Err(conflict("controller delivery is not currently accepted"));
                    }
                }
                Some(CrossSessionInputBinding::ProxyReport { .. }) => {
                    return Err(conflict(
                        "proxy reports must be accepted through run settlement",
                    ));
                }
                None if input.origin == InputOrigin::User => {
                    let removed = state
                        .inputs
                        .values()
                        .filter(|candidate| {
                            candidate.session_id == input.session_id
                                && candidate.state == StoredInputState::Queued
                                && matches!(
                                    candidate.cross_session_binding,
                                    Some(CrossSessionInputBinding::ControllerDelivery { .. })
                                )
                        })
                        .map(|candidate| candidate.input_id.clone())
                        .collect::<std::collections::BTreeSet<_>>();
                    state
                        .inputs
                        .retain(|input_id, _| !removed.contains(input_id));
                    state.runs.retain(|_, run| !removed.contains(&run.input_id));
                    state.skill_activations.retain(|_, activation| {
                        !activation
                            .input_id
                            .as_ref()
                            .is_some_and(|input_id| removed.contains(input_id))
                    });
                    if let Some(session) = state.sessions.get_mut(&input.session_id) {
                        session.proxy = None;
                    }
                }
                None => {}
            }
            state.next_queue_order += 1;
            let stored = StoredInput {
                queue_order: state.next_queue_order,
                input_id: input.input_id.clone(),
                session_id: input.session_id.clone(),
                idempotency_key: input.idempotency_key,
                agent_variant: input.agent_variant,
                origin: input.origin,
                goal_binding: input.goal_binding,
                cross_session_binding: input.cross_session_binding,
                skill_activation: input.skill_activation.clone(),
                user_message_id: input.message.id.clone(),
                state: StoredInputState::Queued,
                queued_message: Some(input.message),
                accepted_at_ms: input.accepted_at_ms,
            };
            let run = StoredRun {
                run_id: input.run_id,
                session_id: input.session_id,
                input_id: input.input_id.clone(),
                attempt: 1,
                status: RunStatus::Accepted,
                agent_variant: input.agent_variant,
                approval_mode: input.approval_mode,
                reasoning_effort: None,
                cancel_requested: false,
                error: None,
                message_ids: Vec::new(),
                message_steps: std::collections::HashMap::new(),
                created_at_ms: input.accepted_at_ms,
                started_at_ms: None,
                finished_at_ms: None,
            };
            let session = state
                .sessions
                .get_mut(&stored.session_id)
                .ok_or_else(|| conflict("input session does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("input session is archived"));
            }
            session.current_variant = stored.agent_variant;
            if session.title_origin == SessionTitleOrigin::Generated
                && let Some(title) = input.generated_title
            {
                session.title = title;
            }
            if let Some(goal) = input.new_goal {
                state.goals.insert(goal.session_id.clone(), goal);
            }
            if let Some(goal) = input.resumed_goal {
                state.goals.insert(goal.session_id.clone(), goal);
            }
            state.inputs.insert(input.input_id, stored.clone());
            state.runs.insert(run.run_id.clone(), run.clone());
            if let Some(activation) = input.skill_activation {
                let previous = state
                    .skill_activations
                    .insert(activation.activation_id.clone(), activation);
                debug_assert!(previous.is_none(), "activation was prevalidated");
            }
            Ok(AcceptedInput {
                input: stored,
                run,
                is_duplicate: false,
            })
        })
    }

    fn cancel_queued_input(
        &self,
        session_id: &SessionId,
        input_id: &InputId,
    ) -> StoreFuture<'_, ()> {
        let session_id = session_id.clone();
        let input_id = input_id.clone();
        Box::pin(async move {
            let mut state = self.lock()?;
            let input = state
                .inputs
                .get(&input_id)
                .ok_or_else(|| conflict("input does not exist"))?;
            if input.session_id != session_id
                || input.state != StoredInputState::Queued
                || input.origin != InputOrigin::User
                || input.goal_binding.is_some()
            {
                return Err(conflict("input is not queued"));
            }
            state.inputs.remove(&input_id);
            state.runs.retain(|_, run| run.input_id != input_id);
            state
                .skill_activations
                .retain(|_, activation| activation.input_id.as_ref() != Some(&input_id));
            Ok(())
        })
    }

    fn prioritize_queued_input(&self, change: QueuePriorityChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let mut ordered = state
                .inputs
                .values()
                .filter(|input| {
                    input.session_id == change.session_id
                        && input.state == StoredInputState::Queued
                        && input.origin == InputOrigin::User
                        && input.goal_binding.is_none()
                })
                .map(|input| (input.queue_order, input.input_id.clone()))
                .collect::<Vec<_>>();
            ordered.sort_by_key(|(queue_order, _)| *queue_order);
            let position = ordered
                .iter()
                .position(|(_, input_id)| input_id == &change.input_id)
                .ok_or_else(|| conflict("input is not queued"))?;
            let selected = ordered.remove(position);
            ordered.insert(0, selected);
            for (queue_order, (_, input_id)) in ordered.into_iter().enumerate() {
                state
                    .inputs
                    .get_mut(&input_id)
                    .expect("queued input id came from the same map")
                    .queue_order = u64::try_from(queue_order)
                    .map_err(|_| conflict("queue order exceeds storage range"))?;
            }
            Ok(())
        })
    }

    fn create_run_attempt(&self, attempt: NewStoredRunAttempt) -> StoreFuture<'_, StoredRun> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let source = state
                .runs
                .get(&attempt.source_run_id)
                .cloned()
                .ok_or_else(|| conflict("source run does not exist"))?;
            if !matches!(source.status, RunStatus::Failed | RunStatus::Interrupted) {
                return Err(conflict("run is not retryable"));
            }
            let next = state
                .runs
                .values()
                .filter(|run| run.input_id == source.input_id)
                .map(|run| run.attempt)
                .max()
                .unwrap_or(0)
                + 1;
            if next != source.attempt + 1 {
                return Err(conflict("only the latest run can be retried"));
            }
            let run = StoredRun {
                run_id: attempt.run_id,
                session_id: attempt.session_id,
                input_id: source.input_id,
                attempt: next,
                status: RunStatus::Accepted,
                agent_variant: source.agent_variant,
                approval_mode: attempt.approval_mode,
                reasoning_effort: None,
                cancel_requested: false,
                error: None,
                message_ids: Vec::new(),
                message_steps: std::collections::HashMap::new(),
                created_at_ms: attempt.created_at_ms,
                started_at_ms: None,
                finished_at_ms: None,
            };
            state.runs.insert(run.run_id.clone(), run.clone());
            Ok(run)
        })
    }

    fn create_session(&self, session: NewStoredSession) -> StoreFuture<'_, StoredSession> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if state.sessions.contains_key(&session.session_id) {
                return Err(conflict("session already exists in runtime storage"));
            }
            let stored = StoredSession {
                session_id: session.session_id.clone(),
                title: session.title,
                title_origin: session.title_origin,
                model_key: session.model_key,
                reasoning_effort: session.reasoning_effort,
                system_prompt: session.system_prompt,
                skill_catalog: session.skill_catalog,
                environment: session.environment,
                lifecycle: StoredSessionLifecycle::Active,
                current_variant: session.current_variant,
                approval_mode: session.approval_mode,
                role: session.role,
                proxy: None,
                body_generation: 1,
                message_count: 0,
                created_at_ms: session.created_at_ms,
                updated_at_ms: session.created_at_ms,
                archived_at_ms: None,
                is_pinned: false,
                conversation_state: StoredConversationState::Available,
            };
            state
                .conversations
                .insert(session.session_id, ConversationSnapshot::new(Vec::new()));
            state
                .sessions
                .insert(stored.session_id.clone(), stored.clone());
            state
                .session_usage
                .insert(stored.session_id.clone(), StoredSessionUsage::default());
            Ok(stored)
        })
    }

    fn fork_session(&self, fork: SessionFork) -> StoreFuture<'_, StoredSessionFork> {
        Box::pin(async move {
            fork.conversation
                .validate_tool_exchange_pairs()
                .map_err(|_| conflict("fork conversation splits a tool exchange"))?;
            let mut state = self.lock()?;
            let source = state
                .sessions
                .get(&fork.source_session_id)
                .ok_or_else(|| conflict("fork source session does not exist"))?;
            if source.role != SessionRole::Standard || fork.session.role != SessionRole::Standard {
                return Err(conflict("session role cannot be forked"));
            }
            if source.body_generation != fork.source_generation {
                return Err(conflict("fork source generation changed"));
            }
            if state.sessions.contains_key(&fork.session.session_id) {
                return Err(conflict("fork session already exists"));
            }
            if let Some(goal) = fork.goal.as_ref() {
                let source_goal = state
                    .goals
                    .get(&fork.source_session_id)
                    .ok_or_else(|| conflict("fork source Goal does not exist"))?;
                if goal.session_id != fork.session.session_id
                    || goal.goal_id == source_goal.goal_id
                    || goal.state != StoredGoalState::Paused
                    || goal.pause_reason != Some(StoredGoalPauseReason::Forked)
                    || goal.generation != 1
                    || goal.turn != source_goal.turn
                    || goal.objective != source_goal.objective
                    || goal.budget != source_goal.budget
                    || goal.consecutive_failures != source_goal.consecutive_failures
                    || goal.created_at_ms != fork.session.created_at_ms
                    || goal.updated_at_ms != fork.session.created_at_ms
                    || goal.completed_at_ms.is_some()
                    || !fork.conversation.messages.iter().any(|message| {
                        matches!(message, ConversationMessage::User(user)
                            if user.id == goal.objective.source_message_id)
                    })
                    || state
                        .goals
                        .values()
                        .any(|current| current.goal_id == goal.goal_id)
                {
                    return Err(conflict("fork Goal projection is invalid"));
                }
            }
            let mut new_attachment_ids = BTreeSet::new();
            for reference in &fork.attachments {
                if !new_attachment_ids.insert(reference.attachment_id.clone())
                    || state.attachments.contains_key(&reference.attachment_id)
                {
                    return Err(conflict("fork attachment already exists"));
                }
            }
            let message_ids = fork
                .conversation
                .messages
                .iter()
                .map(|message| message_id(message).as_str().to_owned())
                .collect::<BTreeSet<_>>();
            let mut activation_ids = BTreeSet::new();
            for activation in &fork.skill_activations {
                if !activation_ids.insert(activation.activation_id.clone())
                    || state
                        .skill_activations
                        .contains_key(&activation.activation_id)
                    || activation.session_id != fork.session.session_id
                    || !matches!(
                        &activation.owner,
                        SkillActivationOwner::Session(session_id)
                            if session_id == &fork.session.session_id
                    )
                    || activation.run_id.is_some()
                    || activation.input_id.is_some()
                    || !message_ids.contains(activation.message_id.as_str())
                    || activation.catalog_revision != fork.session.skill_catalog.revision
                {
                    return Err(conflict("fork skill activation is invalid"));
                }
            }

            let mut path_rewrites = BTreeMap::new();
            let mut attachments = Vec::with_capacity(fork.attachments.len());
            for reference in &fork.attachments {
                let source = state
                    .attachments
                    .get(&reference.source_attachment_id)
                    .filter(|attachment| attachment.session_id == fork.source_session_id)
                    .ok_or_else(|| conflict("fork attachment does not belong to source session"))?;
                let readable_path = format!(
                    "/volatile/sessions/{}/attachments/{}/file",
                    fork.session.session_id, reference.attachment_id
                );
                path_rewrites.insert(source.agent_readable_path.clone(), readable_path.clone());
                attachments.push(StoredAttachment {
                    attachment_id: reference.attachment_id.clone(),
                    session_id: fork.session.session_id.clone(),
                    original_name: source.original_name.clone(),
                    blob_hash: source.blob_hash.clone(),
                    size_bytes: source.size_bytes,
                    media_type: source.media_type.clone(),
                    agent_readable_path: readable_path,
                    state: source.state,
                    created_at_ms: fork.session.created_at_ms,
                });
            }
            let mut conversation = fork.conversation;
            rewrite_file_reference_paths(&mut conversation, &path_rewrites)?;
            let message_count = u64::try_from(conversation.messages.len())
                .map_err(|_| conflict("fork conversation is too large"))?;
            let stored = StoredSession {
                session_id: fork.session.session_id.clone(),
                title: fork.session.title,
                title_origin: fork.session.title_origin,
                model_key: fork.session.model_key,
                reasoning_effort: fork.session.reasoning_effort,
                system_prompt: fork.session.system_prompt,
                skill_catalog: fork.session.skill_catalog,
                environment: fork.session.environment,
                lifecycle: StoredSessionLifecycle::Active,
                current_variant: fork.session.current_variant,
                approval_mode: fork.session.approval_mode,
                role: fork.session.role,
                proxy: None,
                body_generation: 1,
                message_count,
                created_at_ms: fork.session.created_at_ms,
                updated_at_ms: fork.session.created_at_ms,
                archived_at_ms: None,
                is_pinned: false,
                conversation_state: StoredConversationState::Available,
            };
            for attachment in &attachments {
                state
                    .attachments
                    .insert(attachment.attachment_id.clone(), attachment.clone());
            }
            state
                .conversations
                .insert(stored.session_id.clone(), conversation.clone());
            state
                .sessions
                .insert(stored.session_id.clone(), stored.clone());
            state
                .session_usage
                .insert(stored.session_id.clone(), StoredSessionUsage::default());
            let work_plan = fork.work_plan.map(|source| StoredWorkPlan {
                session_id: stored.session_id.clone(),
                revision: 1,
                objective: source.objective,
                items: source.items,
                last_operation_id: format!("fork:{}", fork.source_session_id),
                updated_at_ms: stored.created_at_ms,
            });
            if let Some(plan) = &work_plan {
                state
                    .work_plans
                    .insert(stored.session_id.clone(), plan.clone());
            }
            let goal = fork.goal;
            if let Some(goal) = &goal {
                state.goals.insert(stored.session_id.clone(), goal.clone());
            }
            for activation in &fork.skill_activations {
                state
                    .skill_activations
                    .insert(activation.activation_id.clone(), activation.clone());
            }
            Ok(StoredSessionFork {
                session: stored,
                conversation,
                attachments,
                skill_activations: fork.skill_activations,
                work_plan,
                goal,
            })
        })
    }

    fn inspect_session_deletion(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, assistant_protocol::DeleteSessionImpact> {
        let session_id = session_id.clone();
        Box::pin(async move {
            let state = self.lock()?;
            let session = state
                .sessions
                .get(&session_id)
                .ok_or_else(|| conflict("delete session does not exist"))?;
            Ok(assistant_protocol::DeleteSessionImpact {
                message_count: session.message_count,
                run_count: count_u64(
                    state
                        .runs
                        .values()
                        .filter(|run| run.session_id == session_id)
                        .count(),
                )?,
                child_task_count: count_u64(
                    state
                        .child_tasks
                        .values()
                        .filter(|task| task.session_id == session_id)
                        .count(),
                )?,
                attachment_count: count_u64(
                    state
                        .attachments
                        .values()
                        .filter(|attachment| attachment.session_id == session_id)
                        .count(),
                )?,
            })
        })
    }

    fn delete_session(&self, deletion: SessionDeletion) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get(&deletion.session_id)
                .ok_or_else(|| conflict("delete session does not exist"))?;
            if session.role != SessionRole::Standard {
                return Err(conflict("session role cannot be deleted"));
            }
            let current = assistant_protocol::DeleteSessionImpact {
                message_count: session.message_count,
                run_count: count_u64(
                    state
                        .runs
                        .values()
                        .filter(|run| run.session_id == deletion.session_id)
                        .count(),
                )?,
                child_task_count: count_u64(
                    state
                        .child_tasks
                        .values()
                        .filter(|task| task.session_id == deletion.session_id)
                        .count(),
                )?,
                attachment_count: count_u64(
                    state
                        .attachments
                        .values()
                        .filter(|attachment| attachment.session_id == deletion.session_id)
                        .count(),
                )?,
            };
            if current != deletion.expected_impact {
                return Err(conflict("delete session impact changed"));
            }
            let run_ids = state
                .runs
                .values()
                .filter(|run| run.session_id == deletion.session_id)
                .map(|run| run.run_id.clone())
                .collect::<BTreeSet<_>>();
            let input_ids = state
                .inputs
                .values()
                .filter(|input| input.session_id == deletion.session_id)
                .map(|input| input.input_id.clone())
                .collect::<BTreeSet<_>>();
            let child_ids = state
                .child_tasks
                .values()
                .filter(|task| task.session_id == deletion.session_id)
                .map(|task| task.child_task_id.clone())
                .collect::<BTreeSet<_>>();
            state.sessions.remove(&deletion.session_id);
            state.conversations.remove(&deletion.session_id);
            state.session_usage.remove(&deletion.session_id);
            state.work_plans.remove(&deletion.session_id);
            state.goals.remove(&deletion.session_id);
            state
                .usage_request_ids
                .retain(|(session_id, _)| session_id != &deletion.session_id);
            state.inputs.retain(|id, _| !input_ids.contains(id));
            state.runs.retain(|id, _| !run_ids.contains(id));
            state.child_tasks.retain(|id, _| !child_ids.contains(id));
            state
                .child_conversations
                .retain(|id, _| !child_ids.contains(id));
            state
                .attachments
                .retain(|_, attachment| attachment.session_id != deletion.session_id);
            state
                .message_feedback
                .retain(|(session_id, _), _| session_id != &deletion.session_id);
            state
                .skill_activations
                .retain(|_, activation| activation.session_id != deletion.session_id);
            state
                .pending_tool_exchanges
                .retain(|_, exchange| exchange.session_id != deletion.session_id);
            state
                .pending_child_tool_exchanges
                .retain(|_, exchange| exchange.session_id != deletion.session_id);
            Ok(())
        })
    }

    fn clear_session_history(
        &self,
        clear: SessionHistoryClear,
    ) -> StoreFuture<'_, SessionHistoryClearResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if let Some(existing) = state.session_history_clears.get(&clear.operation_id) {
                if existing.session.session_id != clear.session_id
                    || existing.source_generation != clear.expected_generation
                {
                    return Err(conflict(
                        "session history clear operation identity was reused",
                    ));
                }
                return Ok(existing.clone());
            }

            let current = state
                .sessions
                .get(&clear.session_id)
                .cloned()
                .ok_or_else(|| conflict("clear session does not exist"))?;
            if current.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("clear session is archived"));
            }
            if current.role != clear.expected_role {
                return Err(conflict("clear session role changed"));
            }
            if current.body_generation != clear.expected_generation {
                return Err(conflict("clear session generation changed"));
            }
            if current.environment != clear.environment {
                return Err(conflict("clear session environment changed"));
            }
            ensure_idle(&state, &clear.session_id)?;

            let result_generation = current
                .body_generation
                .checked_add(1)
                .ok_or_else(|| conflict("clear session generation exhausted"))?;
            let mut cleared = current;
            cleared.system_prompt = clear.system_prompt;
            cleared.skill_catalog = clear.skill_catalog;
            cleared.environment = clear.environment;
            cleared.body_generation = result_generation;
            cleared.message_count = 0;
            cleared.updated_at_ms = clear.changed_at_ms;
            cleared.conversation_state = StoredConversationState::Available;
            if cleared.title_origin == SessionTitleOrigin::Generated {
                cleared.title = match cleared.role {
                    SessionRole::Standard => "New Session",
                    SessionRole::Controller => "主控会话",
                }
                .to_owned();
            }
            if cleared.role == SessionRole::Standard {
                cleared.proxy = None;
            }

            let child_ids = state
                .child_tasks
                .values()
                .filter(|task| task.session_id == clear.session_id)
                .map(|task| task.child_task_id.clone())
                .collect::<BTreeSet<_>>();
            state
                .inputs
                .retain(|_, input| input.session_id != clear.session_id);
            state
                .runs
                .retain(|_, run| run.session_id != clear.session_id);
            state
                .child_tasks
                .retain(|_, task| task.session_id != clear.session_id);
            state
                .child_conversations
                .retain(|child_id, _| !child_ids.contains(child_id));
            state
                .message_feedback
                .retain(|(session_id, _), _| session_id != &clear.session_id);
            state
                .pending_tool_exchanges
                .retain(|_, exchange| exchange.session_id != clear.session_id);
            state
                .pending_child_tool_exchanges
                .retain(|_, exchange| exchange.session_id != clear.session_id);
            state.work_plans.remove(&clear.session_id);
            state
                .work_plan_completion_receipts
                .retain(|(session_id, _), _| session_id != &clear.session_id);
            state.goals.remove(&clear.session_id);
            state
                .skill_activations
                .retain(|_, activation| activation.session_id != clear.session_id);
            state
                .usage_request_ids
                .retain(|(session_id, _)| session_id != &clear.session_id);
            state
                .session_history_clears
                .retain(|_, result| result.session.session_id != clear.session_id);
            state.conversations.insert(
                clear.session_id.clone(),
                ConversationSnapshot::new(Vec::new()),
            );
            state
                .session_usage
                .insert(clear.session_id.clone(), StoredSessionUsage::default());
            state
                .sessions
                .insert(clear.session_id.clone(), cleared.clone());

            let result = SessionHistoryClearResult {
                session: cleared,
                source_generation: clear.expected_generation,
                result_generation,
                cleanup_status: SessionHistoryCleanupStatus::Completed,
            };
            state
                .session_history_clears
                .insert(clear.operation_id, result.clone());
            Ok(result)
        })
    }

    fn prepare_session_compaction(
        &self,
        preparation: SessionHistoryCompactionPreparation,
    ) -> StoreFuture<'_, SessionHistoryCompactionPreparationResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if let Some(existing) = state
                .session_history_compactions
                .get(&preparation.operation_id)
            {
                if existing.session_id != preparation.session_id
                    || existing.source_generation != preparation.expected_generation
                {
                    return Err(conflict("session compaction operation identity was reused"));
                }
                return existing.outcome.clone().map_or_else(
                    || Err(conflict("session compaction is already preparing")),
                    |outcome| {
                        Ok(SessionHistoryCompactionPreparationResult::Completed(
                            outcome,
                        ))
                    },
                );
            }
            let session = state
                .sessions
                .get(&preparation.session_id)
                .ok_or_else(|| conflict("compact session does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active
                || session.body_generation != preparation.expected_generation
            {
                return Err(conflict("compact session snapshot changed"));
            }
            ensure_idle(&state, &preparation.session_id)?;
            state.session_history_compactions.insert(
                preparation.operation_id,
                VolatileCompactionReceipt {
                    session_id: preparation.session_id,
                    source_generation: preparation.expected_generation,
                    outcome: None,
                },
            );
            Ok(SessionHistoryCompactionPreparationResult::Prepared)
        })
    }

    fn finish_session_compaction(
        &self,
        finish: SessionHistoryCompactionFinish,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let current_generation = state
                .sessions
                .get(&finish.session_id)
                .ok_or_else(|| conflict("compact session does not exist"))?
                .body_generation;
            if current_generation != finish.expected_generation {
                return Err(conflict("compact generation changed before finish"));
            }
            let receipt = state
                .session_history_compactions
                .get_mut(&finish.operation_id)
                .ok_or_else(|| conflict("compact receipt does not exist"))?;
            if receipt.session_id != finish.session_id
                || receipt.source_generation != finish.expected_generation
            {
                return Err(conflict("session compaction operation identity was reused"));
            }
            let outcome = match finish.kind {
                SessionHistoryCompactionFinishKind::NoOp => Some(CompactSessionOutcome::NoOp),
                SessionHistoryCompactionFinishKind::Cancelled => {
                    Some(CompactSessionOutcome::Cancelled)
                }
                SessionHistoryCompactionFinishKind::Interrupted => None,
            };
            if finish.kind == SessionHistoryCompactionFinishKind::Interrupted {
                state
                    .session_history_compactions
                    .remove(&finish.operation_id);
            } else if receipt.outcome.is_none() {
                receipt.outcome = outcome;
            } else if receipt.outcome != outcome {
                return Err(conflict("compact receipt already has a different outcome"));
            }
            Ok(())
        })
    }

    fn create_child_task(&self, task: NewStoredChildTask) -> StoreFuture<'_, StoredChildTask> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let parent = state
                .runs
                .get(&task.parent_run_id)
                .ok_or_else(|| conflict("child task parent run does not exist"))?;
            if parent.session_id != task.session_id {
                return Err(conflict("child task parent belongs to a different session"));
            }
            if state.child_tasks.values().any(|existing| {
                existing.parent_run_id == task.parent_run_id
                    && existing.parent_tool_call_id == task.parent_tool_call_id
            }) {
                return Err(conflict("parent tool call already owns a child task"));
            }
            if state.child_tasks.contains_key(&task.child_task_id) {
                return Err(conflict("child task already exists"));
            }
            let stored = StoredChildTask {
                child_task_id: task.child_task_id.clone(),
                session_id: task.session_id,
                parent_run_id: task.parent_run_id,
                parent_tool_call_id: task.parent_tool_call_id,
                title: task.title,
                system_prompt: task.system_prompt,
                agent_variant: task.agent_variant,
                status: ChildTaskStatus::Accepted,
                cancel_requested: false,
                body_generation: 1,
                message_count: 0,
                final_message_id: None,
                error: None,
                created_at_ms: task.created_at_ms,
                started_at_ms: None,
                finished_at_ms: None,
                conversation_state: StoredConversationState::Available,
            };
            state
                .child_tasks
                .insert(task.child_task_id.clone(), stored.clone());
            state
                .child_conversations
                .insert(task.child_task_id, ConversationSnapshot::new(Vec::new()));
            Ok(stored)
        })
    }

    fn start_child_task(&self, start: ChildTaskStart) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            ensure_child_owner(&state, &start.session_id, &start.child_task_id)?;
            if state
                .child_tasks
                .get(&start.child_task_id)
                .is_none_or(|task| task.status != ChildTaskStatus::Accepted)
            {
                return Err(conflict("child task cannot be started"));
            }
            append_child(
                &mut state,
                &start.child_task_id,
                &[ConversationMessage::User(start.message)],
            )?;
            let task = state
                .child_tasks
                .get_mut(&start.child_task_id)
                .expect("checked child task");
            task.status = ChildTaskStatus::Running;
            task.started_at_ms = Some(start.started_at_ms);
            Ok(())
        })
    }

    fn begin_child_tool_exchange(&self, pending: PendingChildToolExchange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut call_ids = BTreeSet::new();
            let mut call_count = 0_u64;
            for call_id in pending
                .assistant
                .parts
                .iter()
                .filter_map(|part| match part {
                    AssistantPart::ToolCall(call) => Some(call.id.as_str()),
                    _ => None,
                })
            {
                call_count += 1;
                if !call_ids.insert(call_id.to_owned()) {
                    return Err(StoreError::new(
                        StoreErrorKind::InvalidInput,
                        "pending child assistant message has duplicate tool calls",
                    ));
                }
            }
            if call_count == 0 {
                return Err(StoreError::new(
                    StoreErrorKind::InvalidInput,
                    "pending child assistant message has no tool calls",
                ));
            }
            let mut state = self.lock()?;
            ensure_child_running(&state, &pending.session_id, &pending.child_task_id)?;
            if state
                .pending_child_tool_exchanges
                .values()
                .any(|exchange| exchange.child_task_id == pending.child_task_id)
            {
                return Err(conflict("child task already has a pending tool exchange"));
            }
            if state
                .pending_child_tool_exchanges
                .contains_key(pending.receipt.as_str())
            {
                return Err(conflict("child tool exchange receipt already exists"));
            }
            state.pending_child_tool_exchanges.insert(
                pending.receipt.as_str().to_owned(),
                VolatileChildPendingExchange {
                    child_task_id: pending.child_task_id,
                    session_id: pending.session_id,
                    step: pending.step,
                    assistant: pending.assistant,
                    started_calls: BTreeSet::new(),
                },
            );
            Ok(())
        })
    }

    fn mark_child_tool_execution_started(
        &self,
        start: ChildToolExecutionStart,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let pending = state
                .pending_child_tool_exchanges
                .get_mut(start.receipt.as_str())
                .ok_or_else(|| conflict("child tool start has no pending exchange"))?;
            let belongs = pending.child_task_id == start.child_task_id
                && pending.session_id == start.session_id
                && pending.assistant.parts.iter().any(|part| {
                    matches!(part, AssistantPart::ToolCall(call) if call.id.as_str() == start.call_id.as_str())
                });
            if !belongs {
                return Err(conflict("child tool start ownership does not match"));
            }
            if !pending
                .started_calls
                .insert(start.call_id.as_str().to_owned())
            {
                return Err(conflict("child tool start is already recorded"));
            }
            Ok(())
        })
    }

    fn complete_child_tool_exchange(
        &self,
        completed: CompletedChildToolExchange,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let pending = state
                .pending_child_tool_exchanges
                .get(completed.receipt.as_str())
                .ok_or_else(|| conflict("pending child tool exchange does not exist"))?;
            if pending.child_task_id != completed.child_task_id
                || pending.session_id != completed.session_id
                || pending.step != completed.step
            {
                return Err(conflict(
                    "pending child tool exchange ownership does not match",
                ));
            }
            let mut messages = vec![ConversationMessage::Assistant(pending.assistant.clone())];
            messages.extend(
                completed
                    .results
                    .iter()
                    .cloned()
                    .map(ConversationMessage::Tool),
            );
            if let Some(message) = completed.activation_message.clone() {
                messages.push(ConversationMessage::User(message));
            }
            validate_model_activations(
                &state,
                &completed.session_id,
                &SkillActivationOwner::ChildTask(completed.child_task_id.as_str().to_owned()),
                &state
                    .child_tasks
                    .get(&completed.child_task_id)
                    .ok_or_else(|| conflict("child task does not exist"))?
                    .parent_run_id,
                completed.activation_message.as_ref(),
                &completed.skill_activations,
            )?;
            append_child(&mut state, &completed.child_task_id, &messages)?;
            for activation in completed.skill_activations {
                state
                    .skill_activations
                    .insert(activation.activation_id.clone(), activation);
            }
            state
                .pending_child_tool_exchanges
                .remove(completed.receipt.as_str());
            Ok(())
        })
    }

    fn settle_child_task(&self, settlement: StoredChildTaskSettlement) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            if !settlement.status.is_terminal() {
                return Err(StoreError::new(
                    StoreErrorKind::InvalidInput,
                    "child task settlement status is not terminal",
                ));
            }
            let mut state = self.lock()?;
            ensure_child_settleable(&state, &settlement.session_id, &settlement.child_task_id)?;
            if state
                .pending_child_tool_exchanges
                .values()
                .any(|pending| pending.child_task_id == settlement.child_task_id)
            {
                return Err(conflict("child task has a pending tool exchange"));
            }
            if let Some(final_message_id) = settlement.final_message_id.as_ref() {
                let exists = state
                    .child_conversations
                    .get(&settlement.child_task_id)
                    .into_iter()
                    .flat_map(|conversation| &conversation.messages)
                    .chain(settlement.messages.iter())
                    .any(|message| message_id(message) == final_message_id);
                if !exists {
                    return Err(StoreError::new(
                        StoreErrorKind::InvalidInput,
                        "child task final message does not exist in its conversation",
                    ));
                }
            }
            append_child(&mut state, &settlement.child_task_id, &settlement.messages)?;
            let task = state
                .child_tasks
                .get_mut(&settlement.child_task_id)
                .expect("checked child task");
            task.status = settlement.status;
            task.cancel_requested |= settlement.cancel_requested;
            task.error = settlement.error;
            task.final_message_id = settlement.final_message_id;
            task.finished_at_ms = Some(settlement.finished_at_ms);
            Ok(())
        })
    }

    fn request_child_task_cancellation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StoreFuture<'_, StoredChildTask> {
        let session_id = session_id.clone();
        let child_task_id = child_task_id.clone();
        Box::pin(async move {
            let mut state = self.lock()?;
            let task = state
                .child_tasks
                .get_mut(&child_task_id)
                .filter(|task| task.session_id == session_id)
                .ok_or_else(|| conflict("child task does not exist in runtime storage"))?;
            if !task.status.is_terminal() {
                task.cancel_requested = true;
            }
            Ok(task.clone())
        })
    }

    fn load_child_conversation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StoreFuture<'_, ConversationSnapshot> {
        let session_id = session_id.clone();
        let child_task_id = child_task_id.clone();
        Box::pin(async move {
            let state = self.lock()?;
            ensure_child_owner(&state, &session_id, &child_task_id)?;
            state
                .child_conversations
                .get(&child_task_id)
                .cloned()
                .ok_or_else(|| conflict("child task conversation does not exist"))
        })
    }

    fn replace_context(
        &self,
        replacement: ContextReplacement,
    ) -> StoreFuture<'_, ContextReplacementResult> {
        Box::pin(async move {
            replacement
                .conversation
                .validate_tool_exchange_pairs()
                .map_err(|_| {
                    StoreError::new(
                        StoreErrorKind::InvalidInput,
                        "replacement conversation is invalid",
                    )
                })?;
            let committed_main = match &replacement.target {
                ContextReplacementTarget::Run { session_id, .. } => Some((
                    session_id.clone(),
                    replacement.conversation.messages.clone(),
                )),
                ContextReplacementTarget::ChildTask { .. } => None,
                ContextReplacementTarget::IdleSession { session_id, .. } => Some((
                    session_id.clone(),
                    replacement.conversation.messages.clone(),
                )),
            };
            let mut state = self.lock()?;
            let result = match replacement.target {
                ContextReplacementTarget::Run { session_id, run_id } => {
                    let run = state
                        .runs
                        .get(&run_id)
                        .ok_or_else(|| conflict("replacement run does not exist"))?;
                    if run.session_id != session_id || run.status != RunStatus::Running {
                        return Err(conflict("replacement run is not active"));
                    }
                    if state
                        .pending_tool_exchanges
                        .values()
                        .any(|pending| pending.run_id == run_id)
                    {
                        return Err(conflict("replacement run has a pending tool exchange"));
                    }
                    let message_count = u64::try_from(replacement.conversation.messages.len())
                        .map_err(|_| {
                            StoreError::new(
                                StoreErrorKind::InvalidInput,
                                "replacement conversation is too large",
                            )
                        })?;
                    state
                        .conversations
                        .insert(session_id.clone(), replacement.conversation);
                    let session = state
                        .sessions
                        .get_mut(&session_id)
                        .expect("run session exists");
                    session.body_generation = session
                        .body_generation
                        .checked_add(1)
                        .ok_or_else(|| conflict("conversation generation is exhausted"))?;
                    session.message_count = message_count;
                    ContextReplacementResult {
                        source_generation: session.body_generation.saturating_sub(1),
                        result_generation: session.body_generation,
                    }
                }
                ContextReplacementTarget::ChildTask {
                    session_id,
                    child_task_id,
                } => {
                    ensure_child_running(&state, &session_id, &child_task_id)?;
                    if state
                        .pending_child_tool_exchanges
                        .values()
                        .any(|pending| pending.child_task_id == child_task_id)
                    {
                        return Err(conflict(
                            "replacement child task has a pending tool exchange",
                        ));
                    }
                    let message_count = u64::try_from(replacement.conversation.messages.len())
                        .map_err(|_| {
                            StoreError::new(
                                StoreErrorKind::InvalidInput,
                                "replacement conversation is too large",
                            )
                        })?;
                    state
                        .child_conversations
                        .insert(child_task_id.clone(), replacement.conversation);
                    let task = state
                        .child_tasks
                        .get_mut(&child_task_id)
                        .expect("checked child task");
                    task.body_generation = task
                        .body_generation
                        .checked_add(1)
                        .ok_or_else(|| conflict("child conversation generation is exhausted"))?;
                    task.message_count = message_count;
                    ContextReplacementResult {
                        source_generation: task.body_generation.saturating_sub(1),
                        result_generation: task.body_generation,
                    }
                }
                ContextReplacementTarget::IdleSession {
                    session_id,
                    expected_generation,
                    operation_id,
                    compacted_message_count,
                    retained_message_count,
                } => {
                    ensure_idle(&state, &session_id)?;
                    let session = state
                        .sessions
                        .get(&session_id)
                        .ok_or_else(|| conflict("compact session does not exist"))?;
                    if session.lifecycle != StoredSessionLifecycle::Active
                        || session.body_generation != expected_generation
                    {
                        return Err(conflict("compact session snapshot changed"));
                    }
                    let receipt = state
                        .session_history_compactions
                        .get(&operation_id)
                        .ok_or_else(|| conflict("compact receipt does not exist"))?;
                    if receipt.session_id != session_id
                        || receipt.source_generation != expected_generation
                        || receipt.outcome.is_some()
                    {
                        return Err(conflict("compact receipt is not preparing"));
                    }
                    let result_generation = expected_generation
                        .checked_add(1)
                        .ok_or_else(|| conflict("conversation generation is exhausted"))?;
                    let message_count = u64::try_from(replacement.conversation.messages.len())
                        .map_err(|_| {
                            StoreError::new(
                                StoreErrorKind::InvalidInput,
                                "replacement conversation is too large",
                            )
                        })?;
                    state
                        .conversations
                        .insert(session_id.clone(), replacement.conversation);
                    let session = state
                        .sessions
                        .get_mut(&session_id)
                        .expect("checked compact session");
                    session.body_generation = result_generation;
                    session.message_count = message_count;
                    state
                        .session_history_compactions
                        .get_mut(&operation_id)
                        .expect("checked compact receipt")
                        .outcome = Some(CompactSessionOutcome::Compacted {
                        source_generation: expected_generation,
                        result_generation,
                        compacted_message_count,
                        retained_message_count,
                    });
                    ContextReplacementResult {
                        source_generation: expected_generation,
                        result_generation,
                    }
                }
            };
            if let Some((session_id, messages)) = committed_main {
                record_session_usage(&mut state, &session_id, &messages);
            }
            Ok(result)
        })
    }

    fn commit_user_message(&self, commit: UserMessageCommit) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if state.runs.contains_key(&commit.run_id) {
                let run = state.runs.get(&commit.run_id).expect("checked run");
                if run.input_id != commit.input_id || run.status != RunStatus::Accepted {
                    return Err(conflict("run cannot start"));
                }
            }
            let message = commit.message.map(ConversationMessage::User);
            if let Some(message) = message.as_ref() {
                append(
                    &mut state,
                    &commit.session_id,
                    std::slice::from_ref(message),
                )?;
                let input = state
                    .inputs
                    .get_mut(&commit.input_id)
                    .ok_or_else(|| conflict("input does not exist"))?;
                input.state = StoredInputState::Committed;
                input.queued_message = None;
            } else if state
                .inputs
                .get(&commit.input_id)
                .is_none_or(|input| input.state != StoredInputState::Committed)
            {
                return Err(conflict("input is not committed"));
            }
            let run = state.runs.get_mut(&commit.run_id).expect("checked run");
            run.status = RunStatus::Running;
            run.reasoning_effort = commit.reasoning_effort;
            run.started_at_ms = Some(commit.created_at_ms);
            if let Some(message) = message.as_ref() {
                run.message_ids.push(message_id(message).clone());
            }
            Ok(())
        })
    }

    fn begin_tool_exchange(&self, pending: PendingToolExchange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            if !pending
                .assistant
                .parts
                .iter()
                .any(|part| matches!(part, AssistantPart::ToolCall(_)))
            {
                return Err(StoreError::new(
                    StoreErrorKind::InvalidInput,
                    "pending assistant message has no tool calls",
                ));
            }
            let mut state = self.lock()?;
            let run = state
                .runs
                .get(&pending.run_id)
                .ok_or_else(|| conflict("run does not exist in runtime storage"))?;
            if run.session_id != pending.session_id || run.status != RunStatus::Running {
                return Err(conflict("run cannot begin a tool exchange"));
            }
            if state
                .pending_tool_exchanges
                .values()
                .any(|exchange| exchange.session_id == pending.session_id)
            {
                return Err(conflict("session already has a pending tool exchange"));
            }
            if state
                .pending_tool_exchanges
                .contains_key(pending.receipt.as_str())
            {
                return Err(conflict("tool exchange receipt already exists"));
            }
            state.pending_tool_exchanges.insert(
                pending.receipt.as_str().to_owned(),
                VolatilePendingExchange {
                    session_id: pending.session_id,
                    run_id: pending.run_id,
                    step: pending.step,
                    assistant: pending.assistant,
                    started_calls: BTreeSet::new(),
                },
            );
            Ok(())
        })
    }

    fn mark_tool_execution_started(&self, start: ToolExecutionStart) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let pending = state
                .pending_tool_exchanges
                .get_mut(start.receipt.as_str())
                .ok_or_else(|| conflict("tool execution start has no pending exchange"))?;
            let belongs = pending.session_id == start.session_id
                && pending.run_id == start.run_id
                && pending.assistant.parts.iter().any(|part| {
                    matches!(part, AssistantPart::ToolCall(call) if call.id.as_str() == start.call_id.as_str())
                });
            if !belongs {
                return Err(conflict(
                    "tool execution start does not match pending exchange",
                ));
            }
            if !pending
                .started_calls
                .insert(start.call_id.as_str().to_owned())
            {
                return Err(conflict("tool execution start is already recorded"));
            }
            Ok(())
        })
    }

    fn complete_tool_exchange(&self, completed: CompletedToolExchange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let pending = state
                .pending_tool_exchanges
                .get(completed.receipt.as_str())
                .ok_or_else(|| conflict("pending tool exchange does not exist"))?;
            if pending.session_id != completed.session_id
                || pending.run_id != completed.run_id
                || pending.step != completed.step
            {
                return Err(conflict("pending tool exchange ownership does not match"));
            }
            let mut messages = vec![ConversationMessage::Assistant(pending.assistant.clone())];
            messages.extend(
                completed
                    .results
                    .iter()
                    .cloned()
                    .map(ConversationMessage::Tool),
            );
            if let Some(message) = completed.activation_message.clone() {
                messages.push(ConversationMessage::User(message));
            }
            validate_model_activations(
                &state,
                &completed.session_id,
                &SkillActivationOwner::Session(completed.session_id.clone()),
                &completed.run_id,
                completed.activation_message.as_ref(),
                &completed.skill_activations,
            )?;
            append(&mut state, &completed.session_id, &messages)?;
            let run = state
                .runs
                .get_mut(&completed.run_id)
                .ok_or_else(|| conflict("run does not exist in runtime storage"))?;
            run.message_ids
                .extend(messages.iter().map(message_id).cloned());
            for message in &messages {
                run.message_steps
                    .insert(message_id(message).clone(), completed.step);
            }
            for activation in completed.skill_activations {
                state
                    .skill_activations
                    .insert(activation.activation_id.clone(), activation);
            }
            state
                .pending_tool_exchanges
                .remove(completed.receipt.as_str());
            record_session_usage(&mut state, &completed.session_id, &messages);
            Ok(())
        })
    }

    fn settle_run(
        &self,
        settlement: StoredRunSettlement,
    ) -> StoreFuture<'_, StoredRunSettlementResult> {
        Box::pin(async move {
            if !settlement.status.is_terminal() {
                return Err(StoreError::new(
                    StoreErrorKind::InvalidInput,
                    "run settlement status is not terminal",
                ));
            }
            let mut state = self.lock()?;
            if state
                .pending_tool_exchanges
                .values()
                .any(|pending| pending.run_id == settlement.run_id)
            {
                return Err(conflict("run has a pending tool exchange"));
            }
            validate_volatile_goal_effect(&state, &settlement)?;
            validate_volatile_proxy_report(&state, &settlement)?;
            let proxy_report = settlement.proxy_report.clone();
            append(&mut state, &settlement.session_id, &settlement.messages)?;
            let run = state
                .runs
                .get_mut(&settlement.run_id)
                .ok_or_else(|| conflict("run does not exist in runtime storage"))?;
            if run.session_id != settlement.session_id {
                return Err(conflict("run belongs to a different session"));
            }
            run.message_ids
                .extend(settlement.messages.iter().map(message_id).cloned());
            if let Some(step) = settlement.message_step {
                for message in &settlement.messages {
                    run.message_steps.insert(message_id(message).clone(), step);
                }
            }
            run.status = settlement.status;
            run.cancel_requested = settlement.cancel_requested;
            run.error = settlement.error;
            run.finished_at_ms = Some(settlement.finished_at_ms);
            let session = state
                .sessions
                .get_mut(&settlement.session_id)
                .expect("run session exists");
            session.updated_at_ms = settlement.finished_at_ms;
            record_session_usage(&mut state, &settlement.session_id, &settlement.messages);
            let mut result = apply_volatile_goal_effect(&mut state, settlement.goal_effect)?;
            if let Some(report) = proxy_report {
                result.accepted_proxy_report =
                    Some(insert_volatile_proxy_report(&mut state, *report)?);
            }
            Ok(result)
        })
    }

    fn stop_goal(&self, stop: GoalStop) -> StoreFuture<'_, GoalStopResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let current = state
                .goals
                .get(&stop.session_id)
                .ok_or_else(|| conflict("Goal does not exist"))?;
            let stopped = &stop.stopped_goal;
            if current.goal_id != stop.goal_id
                || current.generation != stop.expected_generation
                || current.state != StoredGoalState::Running
                || stopped.goal_id != current.goal_id
                || stopped.session_id != current.session_id
                || stopped.objective != current.objective
                || stopped.state != StoredGoalState::Paused
                || stopped.pause_reason != Some(StoredGoalPauseReason::UserStopped)
                || stopped.generation
                    != current
                        .generation
                        .checked_add(1)
                        .ok_or_else(|| conflict("Goal generation is exhausted"))?
                || stopped.turn != current.turn
                || stopped.budget != current.budget
                || stopped.consecutive_failures != current.consecutive_failures
                || stopped.created_at_ms != current.created_at_ms
                || stopped.updated_at_ms < current.updated_at_ms
                || stopped.completed_at_ms.is_some()
            {
                return Err(conflict("Goal stop generation is stale"));
            }
            let removed_input_ids = state
                .inputs
                .values()
                .filter(|input| {
                    input.session_id == stop.session_id
                        && input.state == StoredInputState::Queued
                        && input.origin == InputOrigin::Runtime
                        && input.goal_binding.as_ref().is_some_and(|binding| {
                            binding.goal_id == stop.goal_id
                                && binding.generation == stop.expected_generation
                        })
                })
                .map(|input| input.input_id.clone())
                .collect::<Vec<_>>();
            if removed_input_ids.len() > 1 {
                return Err(conflict("Goal has multiple queued continuations"));
            }
            let active_run_ids = state
                .runs
                .values()
                .filter(|run| !run.status.is_terminal())
                .filter_map(|run| {
                    let input = state.inputs.get(&run.input_id)?;
                    (input.state == StoredInputState::Committed
                        && input.goal_binding.as_ref().is_some_and(|binding| {
                            binding.goal_id == stop.goal_id
                                && binding.generation == stop.expected_generation
                        }))
                    .then_some(run.run_id.clone())
                })
                .collect::<Vec<_>>();
            if active_run_ids.len() > 1 {
                return Err(conflict("Goal has multiple active Runs"));
            }
            for input_id in &removed_input_ids {
                state.inputs.remove(input_id);
                state.runs.retain(|_, run| &run.input_id != input_id);
            }
            let cancelling_run_id = active_run_ids.into_iter().next();
            if let Some(run_id) = cancelling_run_id.as_ref() {
                let run = state
                    .runs
                    .get_mut(run_id)
                    .ok_or_else(|| conflict("active Goal Run disappeared"))?;
                run.cancel_requested = true;
                run.status = RunStatus::Cancelling;
            }
            state
                .goals
                .insert(stop.session_id, stop.stopped_goal.clone());
            Ok(GoalStopResult {
                goal: stop.stopped_goal,
                removed_input_ids,
                cancelling_run_id,
            })
        })
    }

    fn clear_goal(&self, clear: GoalClear) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let current = state
                .goals
                .get(&clear.session_id)
                .ok_or_else(|| conflict("Goal does not exist"))?;
            if current.goal_id != clear.goal_id
                || current.generation != clear.expected_generation
                || current.state == StoredGoalState::Running
            {
                return Err(conflict("Goal cannot be cleared"));
            }
            let has_active_run = state.runs.values().any(|run| {
                !run.status.is_terminal()
                    && state.inputs.get(&run.input_id).is_some_and(|input| {
                        input
                            .goal_binding
                            .as_ref()
                            .is_some_and(|binding| binding.goal_id == clear.goal_id)
                    })
            });
            if has_active_run {
                return Err(conflict("Goal still has an active Run"));
            }
            state.goals.remove(&clear.session_id);
            Ok(())
        })
    }

    fn resume_goal_with_held_input(
        &self,
        resume: GoalHeldInputResume,
    ) -> StoreFuture<'_, GoalHeldInputResumeResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let current_goal = state
                .goals
                .get(&resume.session_id)
                .ok_or_else(|| conflict("Goal does not exist"))?;
            let goal = &resume.resumed_goal;
            if current_goal.goal_id != resume.expected_goal_id
                || current_goal.generation != resume.expected_generation
                || current_goal.state != StoredGoalState::Paused
                || goal.goal_id != current_goal.goal_id
                || goal.session_id != current_goal.session_id
                || goal.objective != current_goal.objective
                || goal.state != StoredGoalState::Running
                || goal.pause_reason.is_some()
                || goal.generation
                    != current_goal
                        .generation
                        .checked_add(1)
                        .ok_or_else(|| conflict("Goal generation is exhausted"))?
                || goal.turn
                    != current_goal
                        .turn
                        .checked_add(1)
                        .ok_or_else(|| conflict("Goal turn is exhausted"))?
                || goal.budget != current_goal.budget
                || goal.consecutive_failures != current_goal.consecutive_failures
                || goal.created_at_ms != current_goal.created_at_ms
                || goal.completed_at_ms.is_some()
            {
                return Err(conflict("held Input Goal resume is stale"));
            }
            let input = state
                .inputs
                .get(&resume.input_id)
                .ok_or_else(|| conflict("held Input does not exist"))?;
            if input.session_id != resume.session_id
                || input.state != StoredInputState::Queued
                || input.origin != InputOrigin::User
                || input.goal_binding.is_some()
                || input.user_message_id != resume.message.id
            {
                return Err(conflict("Input is not held user guidance"));
            }
            let binding = super::GoalInputBinding {
                goal_id: goal.goal_id.clone(),
                generation: goal.generation,
                turn: goal.turn,
            };
            validate_input_message(InputOrigin::User, Some(&binding), None, &resume.message)
                .map_err(|_| conflict("held Goal resume message is invalid"))?;
            let run = state
                .runs
                .values()
                .find(|run| run.input_id == resume.input_id && run.status == RunStatus::Accepted)
                .cloned()
                .ok_or_else(|| conflict("held Input has no accepted Run"))?;
            let input = state
                .inputs
                .get_mut(&resume.input_id)
                .expect("checked held Input");
            input.goal_binding = Some(binding);
            input.queued_message = Some(resume.message);
            let input = input.clone();
            state
                .goals
                .insert(resume.session_id, resume.resumed_goal.clone());
            Ok(GoalHeldInputResumeResult {
                goal: resume.resumed_goal,
                input,
                run,
            })
        })
    }

    fn load_conversation(&self, session_id: &SessionId) -> StoreFuture<'_, ConversationSnapshot> {
        let session_id = session_id.clone();
        Box::pin(async move {
            self.lock()?
                .conversations
                .get(&session_id)
                .cloned()
                .ok_or_else(|| conflict("session does not exist in runtime storage"))
        })
    }

    fn get_session_usage(&self, session_id: &SessionId) -> StoreFuture<'_, StoredSessionUsage> {
        let session_id = session_id.clone();
        Box::pin(async move {
            Ok(self
                .lock()?
                .session_usage
                .get(&session_id)
                .cloned()
                .unwrap_or_default())
        })
    }

    fn load_conversation_window(
        &self,
        request: ConversationWindowRequest,
    ) -> StoreFuture<'_, StoredConversationWindow> {
        Box::pin(async move {
            let state = self.lock()?;
            let snapshot = match &request.owner {
                ConversationOwner::MainSession { session_id } => {
                    let session = state
                        .sessions
                        .get(session_id)
                        .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
                    if session.body_generation != request.generation {
                        return Err(conflict("conversation generation changed"));
                    }
                    state
                        .conversations
                        .get(session_id)
                        .cloned()
                        .ok_or_else(|| conflict("session conversation does not exist"))?
                }
                ConversationOwner::ChildTask {
                    session_id,
                    child_task_id,
                } => {
                    let task = state
                        .child_tasks
                        .get(child_task_id)
                        .filter(|task| task.session_id == *session_id)
                        .ok_or_else(|| conflict("child task does not exist in session"))?;
                    if task.body_generation != request.generation {
                        return Err(conflict("conversation generation changed"));
                    }
                    state
                        .child_conversations
                        .get(child_task_id)
                        .cloned()
                        .ok_or_else(|| conflict("child conversation does not exist"))?
                }
            };
            Ok(conversation_window(snapshot, &request))
        })
    }

    fn load_conversation_raw_window(
        &self,
        request: ConversationRawWindowRequest,
    ) -> StoreFuture<'_, StoredConversationRawWindow> {
        Box::pin(async move {
            let state = self.lock()?;
            let snapshot = match &request.owner {
                ConversationOwner::MainSession { session_id } => {
                    let session = state
                        .sessions
                        .get(session_id)
                        .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
                    if session.body_generation != request.generation {
                        return Err(conflict("conversation generation changed"));
                    }
                    state
                        .conversations
                        .get(session_id)
                        .cloned()
                        .ok_or_else(|| conflict("session conversation does not exist"))?
                }
                ConversationOwner::ChildTask {
                    session_id,
                    child_task_id,
                } => {
                    let task = state
                        .child_tasks
                        .get(child_task_id)
                        .filter(|task| task.session_id == *session_id)
                        .ok_or_else(|| conflict("child task does not exist in session"))?;
                    if task.body_generation != request.generation {
                        return Err(conflict("conversation generation changed"));
                    }
                    state
                        .child_conversations
                        .get(child_task_id)
                        .cloned()
                        .ok_or_else(|| conflict("child conversation does not exist"))?
                }
            };
            let total = snapshot.messages.len();
            let start = request.start.min(total);
            let end = start.saturating_add(request.limit).min(total);
            Ok(StoredConversationRawWindow {
                generation: request.generation,
                start,
                end,
                total,
                conversation: ConversationSnapshot::new(snapshot.messages[start..end].to_vec()),
            })
        })
    }

    fn locate_conversation_message(
        &self,
        request: ConversationMessageLocationRequest,
    ) -> StoreFuture<'_, Option<StoredConversationMessageLocation>> {
        Box::pin(async move {
            let state = self.lock()?;
            let (generation, snapshot) = match &request.owner {
                ConversationOwner::MainSession { session_id } => {
                    let session = state
                        .sessions
                        .get(session_id)
                        .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
                    let snapshot = state
                        .conversations
                        .get(session_id)
                        .ok_or_else(|| conflict("session conversation does not exist"))?;
                    (session.body_generation, snapshot)
                }
                ConversationOwner::ChildTask {
                    session_id,
                    child_task_id,
                } => {
                    let task = state
                        .child_tasks
                        .get(child_task_id)
                        .filter(|task| task.session_id == *session_id)
                        .ok_or_else(|| conflict("child task does not exist in session"))?;
                    let snapshot = state
                        .child_conversations
                        .get(child_task_id)
                        .ok_or_else(|| conflict("child conversation does not exist"))?;
                    (task.body_generation, snapshot)
                }
            };
            snapshot
                .messages
                .iter()
                .position(|message| message_id(message) == &request.message_id)
                .map(|ordinal| {
                    let display_ordinal =
                        snapshot.messages[ordinal].is_transcript_visible().then(|| {
                            snapshot.messages[..ordinal]
                                .iter()
                                .filter(|message| message.is_transcript_visible())
                                .count()
                        });
                    Ok(StoredConversationMessageLocation {
                        generation,
                        message_ordinal: u64::try_from(ordinal)
                            .map_err(|_| conflict("conversation ordinal exceeds storage range"))?,
                        display_ordinal: display_ordinal.map(u64::try_from).transpose().map_err(
                            |_| conflict("conversation display ordinal exceeds storage range"),
                        )?,
                    })
                })
                .transpose()
        })
    }

    fn search_conversations(
        &self,
        request: ConversationSearchRequest,
    ) -> StoreFuture<'_, ConversationSearchPage> {
        Box::pin(async move {
            let query = normalize_recall_text(&request.query);
            if query.chars().count() < 3 {
                return Err(StoreError::new(
                    StoreErrorKind::InvalidInput,
                    "conversation recall query is too short",
                ));
            }
            let state = self.lock()?;
            let mut hits = Vec::new();
            for (session_id, snapshot) in &state.conversations {
                let Some(session) = state.sessions.get(session_id) else {
                    continue;
                };
                if !volatile_scope_matches(&request.scope, session) {
                    continue;
                }
                collect_volatile_hits(
                    &mut hits,
                    ConversationOwner::MainSession {
                        session_id: session_id.clone(),
                    },
                    session.body_generation,
                    session.updated_at_ms,
                    snapshot,
                    &query,
                );
                for task in state
                    .child_tasks
                    .values()
                    .filter(|task| task.session_id == *session_id)
                {
                    if let Some(child) = state.child_conversations.get(&task.child_task_id) {
                        collect_volatile_hits(
                            &mut hits,
                            ConversationOwner::ChildTask {
                                session_id: session_id.clone(),
                                child_task_id: task.child_task_id.clone(),
                            },
                            task.body_generation,
                            task.finished_at_ms.unwrap_or(task.created_at_ms),
                            child,
                            &query,
                        );
                    }
                }
            }
            hits.sort_by(|left, right| {
                right
                    .created_at_ms
                    .cmp(&left.created_at_ms)
                    .then_with(|| left.message_ordinal.cmp(&right.message_ordinal))
            });
            hits.truncate(request.limit.clamp(1, 100));
            Ok(ConversationSearchPage {
                hits,
                partial: false,
                failed_owners: Vec::new(),
            })
        })
    }

    fn set_session_archive(&self, change: ArchiveChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if change.archived {
                ensure_idle(&state, &change.session_id)?;
            }
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
            if session.role != SessionRole::Standard {
                return Err(conflict("session role cannot be archived"));
            }
            match (session.lifecycle, change.archived) {
                (StoredSessionLifecycle::Active, true) => {
                    session.lifecycle = StoredSessionLifecycle::Archived;
                    session.archived_at_ms = Some(change.changed_at_ms);
                }
                (StoredSessionLifecycle::Archived, false) => {
                    session.lifecycle = StoredSessionLifecycle::Active;
                    session.archived_at_ms = None;
                }
                _ => return Err(conflict("session lifecycle cannot be changed")),
            }
            Ok(())
        })
    }

    fn set_session_proxy(&self, change: SessionProxyChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let controller = state
                .sessions
                .get(&change.controller_session_id)
                .ok_or_else(|| conflict("controller session does not exist"))?;
            if controller.role != SessionRole::Controller
                || controller.lifecycle != StoredSessionLifecycle::Active
                || change.target_session_id == change.controller_session_id
            {
                return Err(conflict("controller session is invalid"));
            }
            let target = state
                .sessions
                .get_mut(&change.target_session_id)
                .ok_or_else(|| conflict("proxy target session does not exist"))?;
            if target.role != SessionRole::Standard
                || target.lifecycle != StoredSessionLifecycle::Active
            {
                return Err(conflict("proxy target session is invalid"));
            }
            match (&target.proxy, change.enabled) {
                (Some(current), true)
                    if current.controller_session_id == change.controller_session_id => {}
                (None, true) => {
                    target.proxy = Some(SessionProxyState {
                        controller_session_id: change.controller_session_id,
                        changed_at_ms: change.changed_at_ms,
                    });
                }
                (Some(current), false)
                    if current.controller_session_id == change.controller_session_id =>
                {
                    target.proxy = None;
                }
                (None, false) => {}
                _ => return Err(conflict("proxy target is bound to another controller")),
            }
            Ok(())
        })
    }

    fn rename_session(&self, change: SessionTitleChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.title = change.title;
            session.title_origin = SessionTitleOrigin::User;
            Ok(())
        })
    }

    fn set_session_pinned(&self, change: SessionPinnedChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.is_pinned = change.is_pinned;
            Ok(())
        })
    }

    fn set_message_feedback(&self, change: MessageFeedbackChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if !state.sessions.contains_key(&change.session_id) {
                return Err(conflict("feedback session does not exist"));
            }
            let key = (change.session_id, change.message_id);
            if let Some(feedback) = change.feedback {
                state.message_feedback.insert(key, feedback);
            } else {
                state.message_feedback.remove(&key);
            }
            Ok(())
        })
    }

    fn load_message_feedback(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, Vec<StoredMessageFeedback>> {
        let session_id = session_id.clone();
        Box::pin(async move {
            let state = self.lock()?;
            Ok(state
                .message_feedback
                .iter()
                .filter(|((owner, _), _)| owner == &session_id)
                .map(|((_, message_id), feedback)| StoredMessageFeedback {
                    message_id: message_id.clone(),
                    feedback: *feedback,
                })
                .collect())
        })
    }

    fn set_session_model(&self, change: ModelChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            ensure_idle(&state, &change.session_id)?;
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.model_key = change.model_key;
            session.reasoning_effort = change.reasoning_effort;
            Ok(())
        })
    }

    fn set_session_reasoning_effort(&self, change: ReasoningEffortChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.reasoning_effort = change.reasoning_effort;
            Ok(())
        })
    }

    fn set_session_variant(&self, change: VariantChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.current_variant = change.variant;
            Ok(())
        })
    }

    fn set_session_approval_mode(&self, change: ApprovalModeChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.approval_mode = change.approval_mode;
            Ok(())
        })
    }

    fn rewrite_from_user(&self, rewrite: ConversationRewrite) -> StoreFuture<'_, RewriteResult> {
        Box::pin(async move {
            rewrite
                .conversation
                .validate_tool_exchange_pairs()
                .map_err(|source| {
                    StoreError::with_source(
                        StoreErrorKind::InvalidInput,
                        "replacement conversation is invalid",
                        source,
                    )
                })?;
            let mut state = self.lock()?;
            ensure_idle(&state, &rewrite.session_id)?;
            if state
                .sessions
                .get(&rewrite.session_id)
                .is_none_or(|session| session.lifecycle != StoredSessionLifecycle::Active)
            {
                return Err(conflict("session is archived or missing"));
            }
            let target_order = state
                .inputs
                .values()
                .find(|input| {
                    input.session_id == rewrite.session_id
                        && input.user_message_id == rewrite.target_user_message_id
                })
                .map(|input| input.queue_order)
                .ok_or_else(|| conflict("target user message does not belong to an input"))?;
            let new_message = rewrite.input.message.clone();
            if rewrite.input.session_id != rewrite.session_id
                || rewrite.conversation.messages.last().map(message_id) != Some(&new_message.id)
            {
                return Err(StoreError::new(
                    StoreErrorKind::InvalidInput,
                    "replacement input does not match conversation",
                ));
            }
            if let Some(effect) = rewrite.goal_effect.as_ref() {
                let current = state
                    .goals
                    .get(&rewrite.session_id)
                    .ok_or_else(|| conflict("history rewrite Goal does not exist"))?;
                let goal = &effect.goal;
                if current.goal_id != effect.expected_goal_id
                    || current.generation != effect.expected_generation
                    || goal.goal_id != current.goal_id
                    || goal.session_id != current.session_id
                    || goal.objective != current.objective
                    || goal.state != StoredGoalState::Paused
                    || goal.pause_reason != Some(StoredGoalPauseReason::RecoveryRequired)
                    || goal.generation
                        != current
                            .generation
                            .checked_add(1)
                            .ok_or_else(|| conflict("Goal generation is exhausted"))?
                    || goal.turn != current.turn
                    || goal.budget != current.budget
                    || goal.consecutive_failures != current.consecutive_failures
                    || goal.created_at_ms != current.created_at_ms
                    || goal.updated_at_ms != rewrite.changed_at_ms
                    || goal.completed_at_ms.is_some()
                {
                    return Err(conflict("history rewrite Goal projection is invalid"));
                }
            }

            let removed = state
                .inputs
                .values()
                .filter(|input| {
                    input.session_id == rewrite.session_id && input.queue_order >= target_order
                })
                .map(|input| input.input_id.clone())
                .collect::<std::collections::BTreeSet<_>>();
            let retained_message_ids = rewrite
                .conversation
                .messages
                .iter()
                .map(message_id)
                .cloned()
                .collect::<BTreeSet<_>>();
            state
                .inputs
                .retain(|_, input| !removed.contains(&input.input_id));
            state.runs.retain(|_, run| !removed.contains(&run.input_id));
            state.skill_activations.retain(|_, activation| {
                activation.session_id != rewrite.session_id
                    || retained_message_ids.contains(&activation.message_id)
            });
            if let Some(effect) = rewrite.goal_effect.as_ref() {
                state
                    .goals
                    .insert(rewrite.session_id.clone(), effect.goal.clone());
            }
            state.next_queue_order += 1;
            let input = StoredInput {
                queue_order: state.next_queue_order,
                input_id: rewrite.input.input_id.clone(),
                session_id: rewrite.session_id.clone(),
                idempotency_key: rewrite.input.idempotency_key,
                agent_variant: rewrite.input.agent_variant,
                origin: rewrite.input.origin,
                goal_binding: rewrite.input.goal_binding,
                cross_session_binding: rewrite.input.cross_session_binding,
                skill_activation: rewrite.input.skill_activation,
                user_message_id: new_message.id.clone(),
                state: StoredInputState::Committed,
                queued_message: None,
                accepted_at_ms: rewrite.input.accepted_at_ms,
            };
            let run = StoredRun {
                run_id: rewrite.input.run_id,
                session_id: rewrite.session_id.clone(),
                input_id: input.input_id.clone(),
                attempt: 1,
                status: RunStatus::Accepted,
                agent_variant: rewrite.input.agent_variant,
                approval_mode: rewrite.input.approval_mode,
                reasoning_effort: None,
                cancel_requested: false,
                error: None,
                message_ids: vec![new_message.id],
                message_steps: std::collections::HashMap::new(),
                created_at_ms: rewrite.input.accepted_at_ms,
                started_at_ms: None,
                finished_at_ms: None,
            };
            state
                .conversations
                .insert(rewrite.session_id.clone(), rewrite.conversation.clone());
            let count = u64::try_from(rewrite.conversation.messages.len()).map_err(|source| {
                StoreError::with_source(
                    StoreErrorKind::Internal,
                    "conversation message count exceeds storage range",
                    source,
                )
            })?;
            let session = state
                .sessions
                .get_mut(&rewrite.session_id)
                .expect("checked session");
            session.body_generation += 1;
            let body_generation = session.body_generation;
            session.message_count = count;
            session.current_variant = rewrite.input.agent_variant;
            state.inputs.insert(input.input_id.clone(), input.clone());
            state.runs.insert(run.run_id.clone(), run.clone());
            Ok(RewriteResult {
                input,
                run,
                body_generation,
            })
        })
    }

    fn shutdown(&self) -> StoreFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
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
    let (expected_goal_id, expected_generation, goal, next_input) = match effect {
        StoredGoalSettlementEffect::Continue {
            expected_goal_id,
            expected_generation,
            goal,
            next_input,
        } => (
            expected_goal_id,
            *expected_generation,
            goal,
            Some(next_input),
        ),
        StoredGoalSettlementEffect::Transition {
            expected_goal_id,
            expected_generation,
            goal,
            ..
        } => (expected_goal_id, *expected_generation, goal, None),
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
    match (effect, next_input) {
        (StoredGoalSettlementEffect::Continue { .. }, Some(next_input)) => {
            let binding = next_input
                .goal_binding
                .as_ref()
                .ok_or_else(|| conflict("Goal continuation has no binding"))?;
            if goal.state != StoredGoalState::Running
                || goal.pause_reason.is_some()
                || goal.generation != expected_generation
                || goal.turn != current.turn.saturating_add(1)
                || next_input.origin != InputOrigin::Runtime
                || next_input.session_id != settlement.session_id
                || next_input.new_goal.is_some()
                || next_input.resumed_goal.is_some()
                || binding.goal_id != goal.goal_id
                || binding.generation != goal.generation
                || binding.turn != goal.turn
                || state.inputs.contains_key(&next_input.input_id)
                || state.runs.contains_key(&next_input.run_id)
                || state.inputs.values().any(|candidate| {
                    candidate.session_id == settlement.session_id
                        && candidate.state == StoredInputState::Queued
                        && candidate.goal_binding.is_some()
                })
            {
                return Err(conflict("Goal continuation projection is invalid"));
            }
            validate_input_message(
                next_input.origin,
                next_input.goal_binding.as_ref(),
                next_input.cross_session_binding.as_ref(),
                &next_input.message,
            )
            .map_err(|_| conflict("Goal continuation message is invalid"))?;
        }
        (StoredGoalSettlementEffect::Transition { .. }, None) => {
            if goal.state == StoredGoalState::Running
                || goal.generation != expected_generation.saturating_add(1)
                || goal.turn != current.turn
            {
                return Err(conflict("Goal terminal transition is invalid"));
            }
        }
        _ => return Err(conflict("Goal settlement effect is inconsistent")),
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
    validate_input_message(
        report.origin,
        report.goal_binding.as_ref(),
        report.cross_session_binding.as_ref(),
        &report.message,
    )
    .map_err(|_| conflict("proxy report message is invalid"))?;
    let Some(CrossSessionInputBinding::ProxyReport {
        source_session_id,
        source_run_id,
        source_goal_id,
        source_run_status,
    }) = report.cross_session_binding.as_ref()
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
            Some(StoredGoalSettlementEffect::Continue { .. })
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
        queue_order: state.next_queue_order,
        input_id: report.input_id.clone(),
        session_id: report.session_id.clone(),
        idempotency_key: report.idempotency_key,
        agent_variant: report.agent_variant,
        origin: report.origin,
        goal_binding: None,
        cross_session_binding: report.cross_session_binding,
        skill_activation: None,
        user_message_id: report.message.id.clone(),
        state: StoredInputState::Queued,
        queued_message: Some(report.message),
        accepted_at_ms: report.accepted_at_ms,
    };
    let stored_run = StoredRun {
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
        StoredGoalSettlementEffect::Continue {
            goal, next_input, ..
        } => {
            state.next_queue_order = state.next_queue_order.saturating_add(1);
            let stored_input = StoredInput {
                queue_order: state.next_queue_order,
                input_id: next_input.input_id.clone(),
                session_id: next_input.session_id.clone(),
                idempotency_key: next_input.idempotency_key,
                agent_variant: next_input.agent_variant,
                origin: next_input.origin,
                goal_binding: next_input.goal_binding,
                cross_session_binding: next_input.cross_session_binding,
                skill_activation: next_input.skill_activation,
                user_message_id: next_input.message.id.clone(),
                state: StoredInputState::Queued,
                queued_message: Some(next_input.message),
                accepted_at_ms: next_input.accepted_at_ms,
            };
            let stored_run = StoredRun {
                run_id: next_input.run_id.clone(),
                session_id: next_input.session_id,
                input_id: next_input.input_id.clone(),
                attempt: 1,
                status: RunStatus::Accepted,
                agent_variant: next_input.agent_variant,
                approval_mode: next_input.approval_mode,
                reasoning_effort: None,
                cancel_requested: false,
                error: None,
                message_ids: Vec::new(),
                message_steps: std::collections::HashMap::new(),
                created_at_ms: next_input.accepted_at_ms,
                started_at_ms: None,
                finished_at_ms: None,
            };
            state.goals.insert(goal.session_id.clone(), goal.clone());
            state
                .inputs
                .insert(stored_input.input_id.clone(), stored_input.clone());
            state
                .runs
                .insert(stored_run.run_id.clone(), stored_run.clone());
            Ok(StoredRunSettlementResult {
                goal: Some(goal),
                continuation: Some(AcceptedInput {
                    input: stored_input,
                    run: stored_run,
                    is_duplicate: false,
                }),
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
                continuation: None,
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
        || skill_parts[0].retention_key.as_deref()
            != Some(&format!("skill:{}", activation.name.as_str()))
    {
        return Err(conflict("input skill activation is inconsistent"));
    }
    Ok(())
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
    for activation in activations {
        let matches = message.parts.iter().filter(|part| {
            matches!(
                part,
                UserPart::InternalContext(part)
                    if part.kind == "skill_activation"
                        && part.retention_key.as_deref()
                            == Some(&format!("skill:{}", activation.name.as_str()))
            )
        });
        if matches.count() != 1 {
            return Err(conflict("model skill activation boundary is inconsistent"));
        }
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

#[cfg(test)]
mod tests {
    use agent_types::{PartId, TextPart, TranscriptVisibility, UserMessage, UserMessageOrigin};
    use assistant_protocol::GoalId;
    use sha2::{Digest, Sha256};

    use super::*;

    fn user_message(
        message_id: &str,
        part_id: &str,
        text: &str,
        origin: UserMessageOrigin,
        visibility: TranscriptVisibility,
    ) -> ConversationMessage {
        ConversationMessage::User(UserMessage {
            id: MessageId::new(message_id).expect("message ID"),
            origin,
            transcript_visibility: visibility,
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new(part_id).expect("part ID"),
                text: text.to_owned(),
            })],
        })
    }

    #[test]
    fn hidden_runtime_users_do_not_change_display_windows_or_volatile_recall() {
        let visible = user_message(
            "visible-user",
            "visible-user-text",
            "visible-recall-token",
            UserMessageOrigin::User,
            TranscriptVisibility::Visible,
        );
        let hidden = user_message(
            "runtime-hidden-user",
            "runtime-hidden-user-text",
            "runtime-hidden-recall-token",
            UserMessageOrigin::Runtime,
            TranscriptVisibility::Hidden,
        );
        let snapshot = ConversationSnapshot::new(vec![visible, hidden]);
        let session_id = SessionId::new("session-visible-window").expect("session ID");
        let owner = ConversationOwner::MainSession {
            session_id: session_id.clone(),
        };

        let window = conversation_window(
            snapshot.clone(),
            &ConversationWindowRequest {
                owner: owner.clone(),
                generation: 1,
                end: None,
                limit: 1,
            },
        );
        assert_eq!((window.start, window.end, window.total), (0, 1, 1));
        assert_eq!(window.conversation.messages.len(), 2);
        assert!(!window.conversation.messages[1].is_transcript_visible());

        let mut hits = Vec::new();
        collect_volatile_hits(
            &mut hits,
            owner.clone(),
            1,
            1_000,
            &snapshot,
            "runtime-hidden-recall-token",
        );
        assert!(hits.is_empty());
        collect_volatile_hits(
            &mut hits,
            owner,
            1,
            1_000,
            &snapshot,
            "visible-recall-token",
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_id.as_str(), "visible-user");
    }

    #[tokio::test]
    async fn volatile_recovery_pauses_a_running_goal_once() {
        let store = VolatileRuntimeStore::default();
        let session_id = SessionId::new("session-goal-recovery").expect("session id");
        let payload = vec![crate::StoredGoalObjectivePart::Text(TextPart {
            id: PartId::new("goal-objective-part").expect("part id"),
            text: "finish safely".to_owned(),
        })];
        let hash = format!(
            "sha256-v1:{:x}",
            Sha256::digest(serde_json::to_vec(&payload).expect("encode objective"))
        );
        store.lock().expect("state").goals.insert(
            session_id.clone(),
            crate::StoredGoal {
                goal_id: GoalId::new("goal-recovery").expect("goal id"),
                session_id,
                objective: crate::StoredGoalObjective {
                    source_message_id: MessageId::new("goal-source").expect("message id"),
                    payload,
                    payload_hash: hash,
                },
                state: StoredGoalState::Running,
                pause_reason: None,
                generation: 1,
                turn: 1,
                budget: crate::StoredGoalBudget {
                    max_runs: 20,
                    max_total_tokens: 500_000,
                    max_consecutive_failures: 3,
                    used_runs: 1,
                    used_total_tokens: 100,
                    usage_complete: true,
                },
                consecutive_failures: 0,
                created_at_ms: 1,
                updated_at_ms: 2,
                completed_at_ms: None,
            },
        );

        let first = store.load_runtime().await.expect("first recovery");
        assert_eq!(first.goals[0].state, StoredGoalState::Paused);
        assert_eq!(
            first.goals[0].pause_reason,
            Some(StoredGoalPauseReason::RecoveryRequired)
        );
        assert_eq!(first.goals[0].generation, 2);
        let second = store.load_runtime().await.expect("second recovery");
        assert_eq!(second.goals[0].generation, 2);
    }

    #[tokio::test]
    async fn volatile_skill_name_states_are_default_enabled_and_keyed_only_by_name() {
        let store = VolatileRuntimeStore::default();
        assert!(
            store
                .list_skill_name_states()
                .await
                .expect("initial states")
                .is_empty()
        );
        let name = SkillName::parse("review-pr").expect("name");
        store
            .set_skill_enabled(SkillNameStateChange {
                name: name.clone(),
                enabled: false,
                updated_at_ms: 10,
            })
            .await
            .expect("disable");
        store
            .set_skill_enabled(SkillNameStateChange {
                name,
                enabled: true,
                updated_at_ms: 20,
            })
            .await
            .expect("enable");
        assert_eq!(
            store.list_skill_name_states().await.expect("stored states"),
            vec![SkillNameState {
                name: SkillName::parse("review-pr").expect("name"),
                enabled: true,
                updated_at_ms: 20,
            }]
        );
    }
}
