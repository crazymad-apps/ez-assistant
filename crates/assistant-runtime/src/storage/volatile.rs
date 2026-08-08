//! 仅供无本地 Host 的嵌入式调用与 Runtime 单元测试使用的易失 Store。

use std::{collections::BTreeMap, sync::Mutex};

use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, MessageId,
};
use assistant_protocol::{InputId, RunId, RunStatus, SessionId};

use super::{
    AcceptedInput, ArchiveChange, CompletedToolExchange, ConversationRewrite, ModelChange,
    NewStoredInput, NewStoredRunAttempt, NewStoredSession, PendingToolExchange, RecoveredRuntime,
    RewriteResult, RuntimeStore, StoreError, StoreErrorKind, StoreFuture, StoredConversationState,
    StoredInput, StoredInputState, StoredRun, StoredRunSettlement, StoredSession,
    StoredSessionLifecycle, UserMessageCommit,
};

struct VolatilePendingExchange {
    session_id: SessionId,
    run_id: RunId,
    assistant: AssistantMessage,
}

#[derive(Default)]
struct State {
    sessions: BTreeMap<SessionId, StoredSession>,
    conversations: BTreeMap<SessionId, ConversationSnapshot>,
    inputs: BTreeMap<InputId, StoredInput>,
    runs: BTreeMap<RunId, StoredRun>,
    pending_tool_exchanges: BTreeMap<String, VolatilePendingExchange>,
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
                sessions: state.sessions.values().cloned().collect(),
                inputs: state.inputs.values().cloned().collect(),
                runs: state.runs.values().cloned().collect(),
            })
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
                cancel_requested: false,
                error: None,
                message_ids: Vec::new(),
                created_at_ms: input.accepted_at_ms,
                started_at_ms: None,
                finished_at_ms: None,
            };
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
                lifecycle: StoredSessionLifecycle::Active,
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
                },
            );
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
