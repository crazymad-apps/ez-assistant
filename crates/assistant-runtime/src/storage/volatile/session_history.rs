use super::*;

impl VolatileRuntimeStore {
    pub(super) fn clear_session_history(
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
            cleared.environment = clear.environment;
            cleared.body_generation = result_generation;
            cleared.message_count = 0;
            cleared.updated_at_ms = clear.changed_at_ms;
            cleared.conversation_state = StoredConversationState::Available;
            cleared.automatic_title_pending = false;
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
                .session_commands
                .retain(|_, command| command.session_id != clear.session_id);
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
                .mcp_input_selections
                .retain(|_, selection| selection.session_id != clear.session_id);
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

    pub(super) fn prepare_session_compaction(
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

    pub(super) fn finish_session_compaction(
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
}
