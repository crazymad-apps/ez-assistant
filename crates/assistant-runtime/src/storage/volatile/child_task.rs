use super::*;

impl VolatileRuntimeStore {
    pub(super) fn create_child_task(
        &self,
        task: NewStoredChildTask,
    ) -> StoreFuture<'_, StoredChildTask> {
        Box::pin(async move {
            if task.context_window_tokens == 0 {
                return Err(conflict("child context window must be positive"));
            }
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
                context_window_tokens: Some(task.context_window_tokens),
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

    pub(super) fn start_child_task(&self, start: ChildTaskStart) -> StoreFuture<'_, ()> {
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

    pub(super) fn begin_child_tool_exchange(
        &self,
        pending: PendingChildToolExchange,
    ) -> StoreFuture<'_, ()> {
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

    pub(super) fn mark_child_tool_execution_started(
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

    pub(super) fn complete_child_tool_exchange(
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

    pub(super) fn settle_child_task(
        &self,
        settlement: StoredChildTaskSettlement,
    ) -> StoreFuture<'_, ()> {
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

    pub(super) fn request_child_task_cancellation(
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

    pub(super) fn load_child_conversation(
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
}
