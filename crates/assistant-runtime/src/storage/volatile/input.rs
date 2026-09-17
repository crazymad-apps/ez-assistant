use super::*;

impl VolatileRuntimeStore {
    pub(super) fn accept_input(&self, input: NewStoredInput) -> StoreFuture<'_, AcceptedInput> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if input.idempotency_key.as_ref().is_some_and(|key| {
                state.session_commands.values().any(|command| {
                    command.session_id == input.session_id
                        && command.idempotency_key.as_ref() == Some(key)
                })
            }) {
                return Err(conflict(
                    "message idempotency key belongs to a session command",
                ));
            }
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
            validate_input_message_with_channel_source(
                input.origin,
                input.goal_binding.as_ref(),
                input.cross_session.as_ref(),
                input.channel_source.as_ref(),
                &input.message,
            )
            .map_err(|_| conflict("input message origin or Goal binding is invalid"))?;
            if let Some(crate::InputChannelSource::Device(source)) = input.channel_source.as_ref() {
                let paired = state
                    .devices
                    .get(&source.device_id)
                    .is_some_and(|device| device.lifecycle == crate::DeviceLifecycle::Paired);
                let target_is_active = state
                    .sessions
                    .get(&input.session_id)
                    .is_some_and(|session| session.lifecycle == StoredSessionLifecycle::Active);
                if !paired || !target_is_active {
                    return Err(conflict("device input source is not authorized"));
                }
            }
            validate_volatile_input_activation(&state, &input)?;
            if let Some(selection) = input.mcp_selection.as_ref()
                && (input.origin != InputOrigin::User
                    || selection.session_id != input.session_id
                    || selection.input_id.as_ref() != Some(&input.input_id)
                    || selection.message_id != input.message.id
                    || selection.display_name.trim().is_empty()
                    || selection.display_name.len() > 128)
            {
                return Err(conflict("MCP input selection is inconsistent"));
            }
            if input.new_goal.is_some() && input.resumed_goal.is_some() {
                return Err(conflict("input cannot start and resume a Goal together"));
            }
            if let Some(goal) = input.new_goal.as_ref() {
                let binding = input
                    .goal_binding
                    .as_ref()
                    .ok_or_else(|| conflict("new Goal input has no Goal binding"))?;
                let valid_origin = input.origin == InputOrigin::User
                    || (input.origin == InputOrigin::Runtime
                        && input.cross_session.as_ref().is_some_and(|envelope| {
                            matches!(
                                envelope.binding,
                                CrossSessionInputBinding::ControllerDelivery { .. }
                            )
                        }));
                if !valid_origin
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
            match input
                .cross_session
                .as_ref()
                .map(|envelope| &envelope.binding)
            {
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
                    let starts_goal = input.new_goal.is_some();
                    if input.origin != InputOrigin::Runtime
                        || input.goal_binding.is_some() != starts_goal
                        || input.skill_activation.is_some()
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
                                && candidate.cross_session.as_ref().is_some_and(|envelope| {
                                    matches!(
                                        envelope.binding,
                                        CrossSessionInputBinding::ControllerDelivery { .. }
                                    )
                                })
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
                    state.mcp_input_selections.retain(|_, selection| {
                        !selection
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
            let queue_order = if input.origin == InputOrigin::Runtime
                && input.goal_binding.is_none()
                && input.cross_session.is_none()
                && input.channel_source.is_none()
            {
                for queued in state.inputs.values_mut().filter(|candidate| {
                    candidate.session_id == input.session_id
                        && candidate.state == StoredInputState::Queued
                }) {
                    queued.queue_order = queued.queue_order.saturating_add(1);
                }
                0
            } else {
                state.next_queue_order += 1;
                state.next_queue_order
            };
            let stored = StoredInput {
                agent_shell_target: input.agent_shell_target,
                queue_order,
                input_id: input.input_id.clone(),
                session_id: input.session_id.clone(),
                idempotency_key: input.idempotency_key,
                agent_variant: input.agent_variant,
                origin: input.origin,
                goal_binding: input.goal_binding,
                cross_session: input.cross_session,
                channel_source: input.channel_source,
                skill_activation: input.skill_activation.clone(),
                user_message_id: input.message.id.clone(),
                state: StoredInputState::Queued,
                queued_message: Some(input.message),
                accepted_at_ms: input.accepted_at_ms,
            };
            let run = StoredRun {
                shell: None,
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
            if let Some(selection) = input.mcp_selection {
                state
                    .mcp_input_selections
                    .insert(selection.selection_id.clone(), selection);
            }
            Ok(AcceptedInput {
                input: stored,
                run,
                is_duplicate: false,
            })
        })
    }

    pub(super) fn accept_session_command(
        &self,
        command: NewStoredSessionCommand,
    ) -> StoreFuture<'_, AcceptedStoredSessionCommand> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if let Some(key) = command.idempotency_key.as_ref() {
                if state.inputs.values().any(|input| {
                    input.session_id == command.session_id
                        && input.idempotency_key.as_ref() == Some(key)
                }) {
                    return Err(conflict(
                        "session command idempotency key belongs to a message input",
                    ));
                }
                if let Some(existing) = state.session_commands.values().find(|candidate| {
                    candidate.session_id == command.session_id
                        && candidate.idempotency_key.as_ref() == Some(key)
                }) {
                    if existing.command != command.command {
                        return Err(conflict(
                            "session command idempotency key was reused with different content",
                        ));
                    }
                    return Ok(AcceptedStoredSessionCommand {
                        command: existing.clone(),
                        is_duplicate: true,
                    });
                }
            }
            let session = state
                .sessions
                .get(&command.session_id)
                .ok_or_else(|| conflict("session command target does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active
                || state.inputs.contains_key(&command.input_id)
                || state.session_commands.contains_key(&command.input_id)
                || state
                    .inputs
                    .values()
                    .any(|input| input.user_message_id == command.user_message_id)
                || state
                    .session_commands
                    .values()
                    .any(|existing| existing.user_message_id == command.user_message_id)
                || state.conversations.values().any(|conversation| {
                    conversation
                        .messages
                        .iter()
                        .any(|message| message_id(message) == &command.user_message_id)
                })
            {
                return Err(conflict("session command cannot be accepted"));
            }
            state.next_queue_order = state.next_queue_order.saturating_add(1);
            let stored = StoredSessionCommand {
                queue_order: state.next_queue_order,
                input_id: command.input_id.clone(),
                session_id: command.session_id,
                idempotency_key: command.idempotency_key,
                user_message_id: command.user_message_id,
                agent_variant: command.agent_variant,
                command: command.command,
                result: None,
                state: StoredSessionCommandState::Queued,
                accepted_at_ms: command.accepted_at_ms,
            };
            state
                .session_commands
                .insert(stored.input_id.clone(), stored.clone());
            state
                .sessions
                .get_mut(&stored.session_id)
                .expect("checked session")
                .updated_at_ms = stored.accepted_at_ms;
            Ok(AcceptedStoredSessionCommand {
                command: stored,
                is_duplicate: false,
            })
        })
    }

    pub(super) fn cancel_queued_input(
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
                || !(input.origin == InputOrigin::User
                    || input.origin == InputOrigin::Runtime
                        && input.cross_session.is_none()
                        && input.channel_source.is_none())
                || input.goal_binding.is_some()
            {
                return Err(conflict("input is not queued"));
            }
            state.inputs.remove(&input_id);
            state.runs.retain(|_, run| run.input_id != input_id);
            state
                .skill_activations
                .retain(|_, activation| activation.input_id.as_ref() != Some(&input_id));
            state
                .mcp_input_selections
                .retain(|_, selection| selection.input_id.as_ref() != Some(&input_id));
            Ok(())
        })
    }

    pub(super) fn prioritize_queued_input(
        &self,
        change: QueuePriorityChange,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let mut ordered = state
                .inputs
                .values()
                .filter(|input| {
                    input.session_id == change.session_id
                        && input.state == StoredInputState::Queued
                        && (input.origin == InputOrigin::User
                            || input.origin == InputOrigin::Runtime
                                && input.cross_session.is_none()
                                && input.channel_source.is_none())
                        && input.goal_binding.is_none()
                })
                .map(|input| (input.queue_order, input.input_id.clone()))
                .collect::<Vec<_>>();
            ordered.extend(
                state
                    .session_commands
                    .values()
                    .filter(|command| {
                        command.session_id == change.session_id
                            && command.state == StoredSessionCommandState::Queued
                    })
                    .map(|command| (command.queue_order, command.input_id.clone())),
            );
            ordered.sort_by_key(|(queue_order, _)| *queue_order);
            let position = ordered
                .iter()
                .position(|(_, input_id)| input_id == &change.input_id)
                .ok_or_else(|| conflict("input is not queued"))?;
            let selected = ordered.remove(position);
            ordered.insert(0, selected);
            for (queue_order, (_, input_id)) in ordered.into_iter().enumerate() {
                let queue_order = u64::try_from(queue_order)
                    .map_err(|_| conflict("queue order exceeds storage range"))?;
                if let Some(input) = state.inputs.get_mut(&input_id) {
                    input.queue_order = queue_order;
                } else if let Some(command) = state.session_commands.get_mut(&input_id) {
                    command.queue_order = queue_order;
                } else {
                    return Err(conflict("queued item disappeared"));
                }
            }
            Ok(())
        })
    }

    pub(super) fn create_run_attempt(
        &self,
        attempt: NewStoredRunAttempt,
    ) -> StoreFuture<'_, StoredRun> {
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
                shell: None,
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
}
