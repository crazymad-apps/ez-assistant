//! 仅供无本地 Host 的嵌入式调用与 Runtime 单元测试使用的易失 Store。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, MessageId,
};
use assistant_protocol::{
    AttachmentId, ChildTaskId, ChildTaskStatus, InputId, RunId, RunStatus, SessionId, WorkspaceId,
};

use super::{
    AcceptedInput, ApprovalModeChange, ArchiveChange, ChildTaskStart, ChildToolExecutionStart,
    CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementTarget, ConversationRewrite, ModelChange, NewAttachmentUpload,
    NewStoredChildTask, NewStoredInput, NewStoredRunAttempt, NewStoredSession,
    NewWorkspaceRegistration, PendingChildToolExchange, PendingToolExchange, RecoveredRuntime,
    RewriteResult, RuntimeStore, StoreError, StoreErrorKind, StoreFuture, StoredAttachment,
    StoredAttachmentState, StoredChildTask, StoredChildTaskSettlement, StoredConversationState,
    StoredInput, StoredInputState, StoredRun, StoredRunSettlement, StoredSession,
    StoredSessionLifecycle, StoredWorkspace, StoredWorkspaceLifecycle, ToolExecutionStart,
    UserMessageCommit, VariantChange, WorkspaceRemoval,
};

struct VolatilePendingExchange {
    session_id: SessionId,
    run_id: RunId,
    assistant: AssistantMessage,
    started_calls: BTreeSet<String>,
}

struct VolatileChildPendingExchange {
    child_task_id: ChildTaskId,
    session_id: SessionId,
    assistant: AssistantMessage,
    started_calls: BTreeSet<String>,
}

#[derive(Default)]
struct State {
    workspaces: BTreeMap<WorkspaceId, StoredWorkspace>,
    attachments: BTreeMap<AttachmentId, StoredAttachment>,
    sessions: BTreeMap<SessionId, StoredSession>,
    conversations: BTreeMap<SessionId, ConversationSnapshot>,
    inputs: BTreeMap<InputId, StoredInput>,
    runs: BTreeMap<RunId, StoredRun>,
    child_tasks: BTreeMap<ChildTaskId, StoredChildTask>,
    child_conversations: BTreeMap<ChildTaskId, ConversationSnapshot>,
    pending_tool_exchanges: BTreeMap<String, VolatilePendingExchange>,
    pending_child_tool_exchanges: BTreeMap<String, VolatileChildPendingExchange>,
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
            let state = self.lock()?;
            Ok(RecoveredRuntime {
                workspaces: state.workspaces.values().cloned().collect(),
                attachments: state.attachments.values().cloned().collect(),
                sessions: state.sessions.values().cloned().collect(),
                inputs: state.inputs.values().cloned().collect(),
                runs: state.runs.values().cloned().collect(),
                child_tasks: state.child_tasks.values().cloned().collect(),
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
            state.next_queue_order += 1;
            let stored = StoredInput {
                queue_order: state.next_queue_order,
                input_id: input.input_id.clone(),
                session_id: input.session_id.clone(),
                idempotency_key: input.idempotency_key,
                agent_variant: input.agent_variant,
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
                cancel_requested: false,
                error: None,
                message_ids: Vec::new(),
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
            session.updated_at_ms = stored.accepted_at_ms;
            state.inputs.insert(input.input_id, stored.clone());
            state.runs.insert(run.run_id.clone(), run.clone());
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
            if input.session_id != session_id || input.state != StoredInputState::Queued {
                return Err(conflict("input is not queued"));
            }
            state.inputs.remove(&input_id);
            state.runs.retain(|_, run| run.input_id != input_id);
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
                cancel_requested: false,
                error: None,
                message_ids: Vec::new(),
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
                model_key: session.model_key,
                system_prompt: session.system_prompt,
                environment: session.environment,
                lifecycle: StoredSessionLifecycle::Active,
                current_variant: session.current_variant,
                approval_mode: session.approval_mode,
                body_generation: 1,
                message_count: 0,
                created_at_ms: session.created_at_ms,
                updated_at_ms: session.created_at_ms,
                archived_at_ms: None,
                conversation_state: StoredConversationState::Available,
            };
            state
                .conversations
                .insert(session.session_id, ConversationSnapshot::new(Vec::new()));
            state
                .sessions
                .insert(stored.session_id.clone(), stored.clone());
            Ok(stored)
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
            append_child(&mut state, &completed.child_task_id, &messages)?;
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

    fn replace_context(&self, replacement: ContextReplacement) -> StoreFuture<'_, ()> {
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
            let mut state = self.lock()?;
            match replacement.target {
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
                    session.updated_at_ms = replacement.changed_at_ms;
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
                }
            }
            Ok(())
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
            if pending.session_id != completed.session_id || pending.run_id != completed.run_id {
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
            append(&mut state, &completed.session_id, &messages)?;
            let run = state
                .runs
                .get_mut(&completed.run_id)
                .ok_or_else(|| conflict("run does not exist in runtime storage"))?;
            run.message_ids
                .extend(messages.iter().map(message_id).cloned());
            state
                .pending_tool_exchanges
                .remove(completed.receipt.as_str());
            Ok(())
        })
    }

    fn settle_run(&self, settlement: StoredRunSettlement) -> StoreFuture<'_, ()> {
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
            run.status = settlement.status;
            run.cancel_requested = settlement.cancel_requested;
            run.error = settlement.error;
            run.finished_at_ms = Some(settlement.finished_at_ms);
            Ok(())
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
            session.updated_at_ms = change.changed_at_ms;
            Ok(())
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
            session.updated_at_ms = change.changed_at_ms;
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
            session.updated_at_ms = change.changed_at_ms;
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
            session.updated_at_ms = change.changed_at_ms;
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

            let removed = state
                .inputs
                .values()
                .filter(|input| {
                    input.session_id == rewrite.session_id && input.queue_order >= target_order
                })
                .map(|input| input.input_id.clone())
                .collect::<std::collections::BTreeSet<_>>();
            state
                .inputs
                .retain(|_, input| !removed.contains(&input.input_id));
            state.runs.retain(|_, run| !removed.contains(&run.input_id));
            state.next_queue_order += 1;
            let input = StoredInput {
                queue_order: state.next_queue_order,
                input_id: rewrite.input.input_id.clone(),
                session_id: rewrite.session_id.clone(),
                idempotency_key: rewrite.input.idempotency_key,
                agent_variant: rewrite.input.agent_variant,
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
                cancel_requested: false,
                error: None,
                message_ids: vec![new_message.id],
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
            session.message_count = count;
            session.current_variant = rewrite.input.agent_variant;
            session.updated_at_ms = rewrite.changed_at_ms;
            state.inputs.insert(input.input_id.clone(), input.clone());
            state.runs.insert(run.run_id.clone(), run.clone());
            Ok(RewriteResult { input, run })
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
    {
        return Err(conflict("session is not idle"));
    }
    Ok(())
}

fn conflict(message: &'static str) -> StoreError {
    StoreError::new(StoreErrorKind::Conflict, message)
}
