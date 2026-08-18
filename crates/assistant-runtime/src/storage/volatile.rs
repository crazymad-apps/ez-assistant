//! 仅供无本地 Host 的嵌入式调用与 Runtime 单元测试使用的易失 Store。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, MessageId, UserPart,
};
use assistant_protocol::{
    AttachmentId, ChildTaskId, ChildTaskStatus, ConversationOwner, InputId, MessageFeedback,
    MessageId as ProtocolMessageId, RunId, RunStatus, SessionId, SessionTitleOrigin, WorkspaceId,
};

use super::{
    AcceptedInput, ApprovalModeChange, ArchiveChange, ChildTaskStart, ChildToolExecutionStart,
    CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementTarget, ConversationMessageLocationRequest, ConversationRawWindowRequest,
    ConversationRewrite, ConversationSearchHit, ConversationSearchPage, ConversationSearchRequest,
    ConversationSearchScope, ConversationWindowRequest, MessageFeedbackChange, ModelChange,
    NewAttachmentUpload, NewStoredChildTask, NewStoredInput, NewStoredRunAttempt, NewStoredSession,
    NewWorkspaceRegistration, PendingChildToolExchange, PendingToolExchange, QueuePriorityChange,
    RecoveredRuntime, RewriteResult, RuntimeStore, SessionDeletion, SessionFork,
    SessionPinnedChange, SessionTitleChange, StoreError, StoreErrorKind, StoreFuture,
    StoredAttachment, StoredAttachmentState, StoredChildTask, StoredChildTaskSettlement,
    StoredConversationMessageLocation, StoredConversationRawWindow, StoredConversationState,
    StoredConversationWindow, StoredInput, StoredInputState, StoredMessageFeedback, StoredRun,
    StoredRunSettlement, StoredSession, StoredSessionFork, StoredSessionLifecycle, StoredWorkspace,
    StoredWorkspaceLifecycle, ToolExecutionStart, UserMessageCommit, VariantChange,
    WorkspaceRemoval,
};
use crate::{
    MemoryContextSnapshot, PersonaMutation, PersonaSnapshot, PinnedMemoryMutation,
    PinnedMemoryMutationResult, StoredPinnedMemory,
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
    persona: PersonaSnapshot,
    pinned_collection_revision: u64,
    pinned_memories: BTreeMap<String, StoredPinnedMemory>,
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
            if session.title_origin == SessionTitleOrigin::Generated
                && let Some(title) = input.generated_title
            {
                session.title = title;
            }
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

    fn prioritize_queued_input(&self, change: QueuePriorityChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let mut ordered = state
                .inputs
                .values()
                .filter(|input| {
                    input.session_id == change.session_id && input.state == StoredInputState::Queued
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
                title_origin: session.title_origin,
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
                is_pinned: false,
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
            if source.body_generation != fork.source_generation {
                return Err(conflict("fork source generation changed"));
            }
            if state.sessions.contains_key(&fork.session.session_id) {
                return Err(conflict("fork session already exists"));
            }
            let mut new_attachment_ids = BTreeSet::new();
            for reference in &fork.attachments {
                if !new_attachment_ids.insert(reference.attachment_id.clone())
                    || state.attachments.contains_key(&reference.attachment_id)
                {
                    return Err(conflict("fork attachment already exists"));
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
                system_prompt: fork.session.system_prompt,
                environment: fork.session.environment,
                lifecycle: StoredSessionLifecycle::Active,
                current_variant: fork.session.current_variant,
                approval_mode: fork.session.approval_mode,
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
            Ok(StoredSessionFork {
                session: stored,
                conversation,
                attachments,
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
                .pending_tool_exchanges
                .retain(|_, exchange| exchange.session_id != deletion.session_id);
            state
                .pending_child_tool_exchanges
                .retain(|_, exchange| exchange.session_id != deletion.session_id);
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
            let session = state
                .sessions
                .get_mut(&settlement.session_id)
                .expect("run session exists");
            session.updated_at_ms = settlement.finished_at_ms;
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
                    let display_ordinal = matches!(
                        snapshot.messages[ordinal],
                        ConversationMessage::User(_) | ConversationMessage::Assistant(_)
                    )
                    .then(|| {
                        snapshot.messages[..ordinal]
                            .iter()
                            .filter(|message| {
                                matches!(
                                    message,
                                    ConversationMessage::User(_)
                                        | ConversationMessage::Assistant(_)
                                )
                            })
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

fn conversation_window(
    snapshot: ConversationSnapshot,
    request: &ConversationWindowRequest,
) -> StoredConversationWindow {
    let display_indices = snapshot
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            matches!(
                message,
                ConversationMessage::User(_) | ConversationMessage::Assistant(_)
            )
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
                        UserPart::Injected(_) => {}
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
