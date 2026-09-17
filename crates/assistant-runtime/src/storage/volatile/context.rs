use super::*;

impl VolatileRuntimeStore {
    pub(super) fn replace_context(
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
                    let product_history = state
                        .conversations
                        .get(&session_id)
                        .ok_or_else(|| conflict("session conversation does not exist"))?;
                    let merged = super::super::merge_context_replacement_with_product_history(
                        product_history,
                        &replacement.conversation,
                    )?;
                    let message_count = u64::try_from(merged.messages.len()).map_err(|_| {
                        StoreError::new(
                            StoreErrorKind::InvalidInput,
                            "replacement conversation is too large",
                        )
                    })?;
                    state.conversations.insert(session_id.clone(), merged);
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
                        product_message_count: message_count,
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
                    let product_history = state
                        .child_conversations
                        .get(&child_task_id)
                        .ok_or_else(|| conflict("child conversation does not exist"))?;
                    let merged = super::super::merge_context_replacement_with_product_history(
                        product_history,
                        &replacement.conversation,
                    )?;
                    let message_count = u64::try_from(merged.messages.len()).map_err(|_| {
                        StoreError::new(
                            StoreErrorKind::InvalidInput,
                            "replacement conversation is too large",
                        )
                    })?;
                    state
                        .child_conversations
                        .insert(child_task_id.clone(), merged);
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
                        product_message_count: message_count,
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
                    let product_history = state
                        .conversations
                        .get(&session_id)
                        .ok_or_else(|| conflict("session conversation does not exist"))?;
                    let merged = super::super::merge_context_replacement_with_product_history(
                        product_history,
                        &replacement.conversation,
                    )?;
                    let message_count = u64::try_from(merged.messages.len()).map_err(|_| {
                        StoreError::new(
                            StoreErrorKind::InvalidInput,
                            "replacement conversation is too large",
                        )
                    })?;
                    state.conversations.insert(session_id.clone(), merged);
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
                        product_message_count: message_count,
                    }
                }
            };
            if let Some((session_id, messages)) = committed_main {
                record_session_usage(&mut state, &session_id, &messages);
            }
            Ok(result)
        })
    }

    pub(super) fn commit_user_message(&self, commit: UserMessageCommit) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let target = if commit.message.is_some() {
                state
                    .inputs
                    .get(&commit.input_id)
                    .and_then(|input| input.agent_shell_target)
            } else {
                None
            };
            if let Some(target) = target {
                if commit.shell.as_ref().map(|shell| shell.kind) != Some(target) {
                    return Err(conflict(
                        "shell switch snapshot does not match the queued target",
                    ));
                }
                if !state.sessions.contains_key(&commit.session_id) {
                    return Err(conflict("shell switch session does not exist"));
                }
            }
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
            run.shell = commit.shell.clone();
            run.started_at_ms = Some(commit.created_at_ms);
            if let Some(message) = message.as_ref() {
                run.message_ids.push(message_id(message).clone());
            }
            state
                .sessions
                .get_mut(&commit.session_id)
                .expect("checked session")
                .agent_shell_environment = commit.shell;
            if let Some(target) = target {
                state
                    .sessions
                    .get_mut(&commit.session_id)
                    .expect("checked session")
                    .agent_shell_kind = Some(target);
            }
            Ok(())
        })
    }

    pub(super) fn commit_session_command(
        &self,
        commit: SessionCommandCommit,
    ) -> StoreFuture<'_, StoredSessionCommand> {
        Box::pin(async move {
            if commit.operation_id.trim().is_empty()
                || commit.message.origin != agent_types::UserMessageOrigin::Runtime
                || commit.message.transcript_visibility
                    != agent_types::TranscriptVisibility::Visible
            {
                return Err(conflict("session command result message is invalid"));
            }
            let mut state = self.lock()?;
            let existing = state
                .session_commands
                .get(&commit.input_id)
                .cloned()
                .ok_or_else(|| conflict("session command does not exist"))?;
            if !commit.result.matches_command(&existing.command)
                || existing.session_id != commit.session_id
                || existing.user_message_id != commit.message.id
            {
                return Err(conflict("session command belongs to another session"));
            }
            if existing.state == StoredSessionCommandState::Committed {
                if existing.result.as_ref() == Some(&commit.result)
                    && state
                        .conversations
                        .get(&commit.session_id)
                        .is_some_and(|conversation| {
                            conversation
                                .messages
                                .iter()
                                .any(|message| matches!(message, ConversationMessage::User(user) if user == &commit.message))
                        })
                {
                    return Ok(existing);
                }
                return Err(conflict(
                    "session command was already committed differently",
                ));
            }
            if state.runs.values().any(|run| {
                run.session_id == commit.session_id
                    && matches!(run.status, RunStatus::Running | RunStatus::Cancelling)
            }) || state
                .pending_tool_exchanges
                .values()
                .any(|exchange| exchange.session_id == commit.session_id)
                || state
                    .child_tasks
                    .values()
                    .any(|task| task.session_id == commit.session_id && !task.status.is_terminal())
                || state
                    .pending_child_tool_exchanges
                    .values()
                    .any(|exchange| exchange.session_id == commit.session_id)
                || state.goals.contains_key(&commit.session_id)
            {
                return Err(conflict("session command target is blocked"));
            }
            append(
                &mut state,
                &commit.session_id,
                &[ConversationMessage::User(commit.message)],
            )?;
            let message_count = state
                .conversations
                .get(&commit.session_id)
                .map(|conversation| conversation.messages.len())
                .ok_or_else(|| conflict("session conversation does not exist"))?;
            let session = state
                .sessions
                .get_mut(&commit.session_id)
                .ok_or_else(|| conflict("session does not exist"))?;
            session.body_generation = session
                .body_generation
                .checked_add(1)
                .ok_or_else(|| conflict("session generation is exhausted"))?;
            if let crate::StoredSessionCommandResult::ShellSwitch {
                shell,
                environment: Some(environment),
                ..
            } = &commit.result
            {
                session.agent_shell_kind = Some(*shell);
                session.agent_shell_environment = Some(environment.as_ref().clone());
            }
            session.message_count = u64::try_from(message_count)
                .map_err(|_| conflict("session message count exceeds storage range"))?;
            session.updated_at_ms = commit.committed_at_ms;

            let stored = state
                .session_commands
                .get_mut(&commit.input_id)
                .expect("checked command");
            stored.state = StoredSessionCommandState::Committed;
            stored.result = Some(commit.result);
            Ok(stored.clone())
        })
    }
}
