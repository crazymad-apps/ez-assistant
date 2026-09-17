use super::*;

impl VolatileRuntimeStore {
    pub(super) fn begin_tool_exchange(&self, pending: PendingToolExchange) -> StoreFuture<'_, ()> {
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

    pub(super) fn mark_tool_execution_started(
        &self,
        start: ToolExecutionStart,
    ) -> StoreFuture<'_, ()> {
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

    pub(super) fn complete_tool_exchange(
        &self,
        completed: CompletedToolExchange,
    ) -> StoreFuture<'_, ()> {
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

    pub(super) fn settle_run(
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
            let (input_id, run_status) = state
                .runs
                .get(&settlement.run_id)
                .map(|run| (run.input_id.clone(), run.status))
                .ok_or_else(|| conflict("run does not exist in runtime storage"))?;
            if state.runs[&settlement.run_id].session_id != settlement.session_id
                || !matches!(
                    run_status,
                    RunStatus::Accepted | RunStatus::Running | RunStatus::Cancelling
                )
            {
                return Err(conflict("run is not settleable"));
            }
            let input = state
                .inputs
                .get(&input_id)
                .ok_or_else(|| conflict("run input does not exist in runtime storage"))?;
            if input.session_id != settlement.session_id {
                return Err(conflict("run input belongs to a different session"));
            }
            let input_was_queued = input.state == StoredInputState::Queued;
            let mut messages = settlement.messages.clone();
            match input.state {
                StoredInputState::Queued => {
                    let message = settlement.queued_user_message.as_ref().ok_or_else(|| {
                        conflict("queued run input requires its original user message")
                    })?;
                    if input.user_message_id != message.id
                        || input.queued_message.as_ref() != Some(message)
                    {
                        return Err(conflict("queued run input message does not match"));
                    }
                    messages.insert(0, ConversationMessage::User(message.clone()));
                }
                StoredInputState::Committed => {
                    if settlement.queued_user_message.is_some() || input.queued_message.is_some() {
                        return Err(conflict("committed run input settlement is inconsistent"));
                    }
                }
            }
            let proxy_report = settlement.proxy_report.clone();
            append(&mut state, &settlement.session_id, &messages)?;
            if input_was_queued {
                let input = state
                    .inputs
                    .get_mut(&input_id)
                    .expect("run input existence checked before append");
                input.state = StoredInputState::Committed;
                input.queued_message = None;
            }
            let run = state
                .runs
                .get_mut(&settlement.run_id)
                .expect("run existence checked before append");
            run.message_ids
                .extend(messages.iter().map(message_id).cloned());
            if let Some(step) = settlement.message_step {
                for message in &messages {
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
            record_session_usage(&mut state, &settlement.session_id, &messages);
            let mut result = apply_volatile_goal_effect(&mut state, settlement.goal_effect)?;
            if let Some(report) = proxy_report {
                result.accepted_proxy_report =
                    Some(insert_volatile_proxy_report(&mut state, *report)?);
            }
            Ok(result)
        })
    }

    pub(super) fn reconcile_terminal_run_input(
        &self,
        reconciliation: StoredTerminalRunInputReconciliation,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let run = state
                .runs
                .get(&reconciliation.run_id)
                .ok_or_else(|| conflict("terminal run does not exist in runtime storage"))?;
            if run.session_id != reconciliation.session_id
                || run.input_id != reconciliation.input_id
                || !run.status.is_terminal()
            {
                return Err(conflict(
                    "terminal run input reconciliation does not match the run",
                ));
            }
            let input = state
                .inputs
                .get(&reconciliation.input_id)
                .ok_or_else(|| conflict("terminal run input does not exist in runtime storage"))?;
            if input.session_id != reconciliation.session_id
                || input.user_message_id != reconciliation.user_message.id
            {
                return Err(conflict(
                    "terminal run input reconciliation is inconsistent",
                ));
            }
            if input.state == StoredInputState::Committed {
                return if input.queued_message.is_none() {
                    Ok(())
                } else {
                    Err(conflict(
                        "committed terminal run input still has a queued message",
                    ))
                };
            }
            if input.queued_message.as_ref() != Some(&reconciliation.user_message) {
                return Err(conflict("terminal run queued input message does not match"));
            }
            let message = ConversationMessage::User(reconciliation.user_message);
            append(
                &mut state,
                &reconciliation.session_id,
                std::slice::from_ref(&message),
            )?;
            let input = state
                .inputs
                .get_mut(&reconciliation.input_id)
                .expect("terminal run input existence checked before append");
            input.state = StoredInputState::Committed;
            input.queued_message = None;
            state
                .runs
                .get_mut(&reconciliation.run_id)
                .expect("terminal run existence checked before append")
                .message_ids
                .push(message_id(&message).clone());
            Ok(())
        })
    }

    pub(super) fn commit_run_continuation(
        &self,
        continuation: StoredRunContinuation,
    ) -> StoreFuture<'_, StoredRunContinuationResult> {
        Box::pin(async move {
            if continuation.messages.is_empty() {
                return Err(StoreError::new(
                    StoreErrorKind::InvalidInput,
                    "run continuation has no messages",
                ));
            }
            let mut state = self.lock()?;
            if state
                .pending_tool_exchanges
                .values()
                .any(|pending| pending.run_id == continuation.run_id)
            {
                return Err(conflict("run has a pending tool exchange"));
            }
            let validation = StoredRunSettlement {
                operation_id: continuation.operation_id.clone(),
                run_id: continuation.run_id.clone(),
                session_id: continuation.session_id.clone(),
                status: RunStatus::Completed,
                cancel_requested: false,
                error: None,
                queued_user_message: None,
                messages: continuation.messages.clone(),
                message_step: Some(continuation.message_step),
                goal_effect: continuation.goal_effect.clone(),
                proxy_report: None,
                finished_at_ms: continuation.committed_at_ms,
            };
            validate_volatile_goal_effect(&state, &validation)?;
            let run = state
                .runs
                .get(&continuation.run_id)
                .ok_or_else(|| conflict("run does not exist in runtime storage"))?;
            if run.session_id != continuation.session_id || run.status != RunStatus::Running {
                return Err(conflict("run is not active"));
            }
            append(&mut state, &continuation.session_id, &continuation.messages)?;
            let run = state
                .runs
                .get_mut(&continuation.run_id)
                .expect("run existence checked before append");
            for message in &continuation.messages {
                let id = message_id(message).clone();
                run.message_ids.push(id.clone());
                run.message_steps.insert(id, continuation.message_step);
            }
            state
                .sessions
                .get_mut(&continuation.session_id)
                .expect("run session exists")
                .updated_at_ms = continuation.committed_at_ms;
            record_session_usage(&mut state, &continuation.session_id, &continuation.messages);
            let result = apply_volatile_goal_effect(&mut state, continuation.goal_effect)?;
            Ok(StoredRunContinuationResult {
                goal: result.goal,
                resume_required: result.resume_required,
            })
        })
    }
}
