use super::*;

impl VolatileRuntimeStore {
    pub(super) fn create_session(
        &self,
        session: NewStoredSession,
    ) -> StoreFuture<'_, StoredSession> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if state.sessions.contains_key(&session.session_id) {
                return Err(conflict("session already exists in runtime storage"));
            }
            let stored = StoredSession {
                session_id: session.session_id.clone(),
                agent_shell_kind: session.agent_shell_kind,
                agent_shell_environment: session.agent_shell_environment.clone(),
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

    pub(super) fn materialize_session(
        &self,
        materialization: NewStoredSessionMaterialization,
    ) -> StoreFuture<'_, StoredSessionMaterialization> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let key = materialization
                .session
                .materialization_key
                .as_ref()
                .ok_or_else(|| conflict("materialization key is required"))?;
            if let Some(existing) = state
                .sessions
                .values()
                .find(|session| session.materialization_key.as_ref() == Some(key))
                .cloned()
            {
                let input = state
                    .inputs
                    .values()
                    .find(|input| input.session_id == existing.session_id)
                    .cloned()
                    .ok_or_else(|| conflict("materialized input is missing"))?;
                let run = state
                    .runs
                    .values()
                    .find(|run| run.input_id == input.input_id && run.attempt == 1)
                    .cloned()
                    .ok_or_else(|| conflict("materialized run is missing"))?;
                let attachments = state
                    .attachments
                    .values()
                    .filter(|attachment| attachment.session_id == existing.session_id)
                    .cloned()
                    .collect::<Vec<_>>();
                let persisted_message = input.queued_message.clone().or_else(|| {
                    state
                        .conversations
                        .get(&existing.session_id)
                        .and_then(|conversation| {
                            conversation
                                .messages
                                .iter()
                                .find_map(|message| match message {
                                    ConversationMessage::User(user)
                                        if user.id == input.user_message_id =>
                                    {
                                        Some(user.clone())
                                    }
                                    _ => None,
                                })
                        })
                });
                let existing_selection = state
                    .mcp_input_selections
                    .values()
                    .find(|selection| selection.input_id.as_ref() == Some(&input.input_id));
                let selection_matches = existing_selection
                    .map(|selection| (&selection.server_key, selection.display_name.as_str()))
                    == materialization
                        .input
                        .mcp_selection
                        .as_ref()
                        .map(|selection| (&selection.server_key, selection.display_name.as_str()));
                if !selection_matches
                    || !materialization_semantically_matches(
                        &existing,
                        &attachments,
                        &input,
                        persisted_message.as_ref(),
                        &materialization,
                    )
                {
                    return Err(conflict(
                        "materialization key was reused with different content",
                    ));
                }
                return Ok(StoredSessionMaterialization {
                    goal: state.goals.get(&existing.session_id).cloned(),
                    session: existing,
                    attachments,
                    accepted: AcceptedInput {
                        input,
                        run,
                        is_duplicate: true,
                    },
                });
            }
            if state
                .sessions
                .contains_key(&materialization.session.session_id)
                || state.inputs.contains_key(&materialization.input.input_id)
                || state.runs.contains_key(&materialization.input.run_id)
                || materialization.input.session_id != materialization.session.session_id
            {
                return Err(conflict("materialization identities conflict"));
            }
            validate_input_message_with_channel_source(
                materialization.input.origin,
                materialization.input.goal_binding.as_ref(),
                materialization.input.cross_session.as_ref(),
                materialization.input.channel_source.as_ref(),
                &materialization.input.message,
            )
            .map_err(|_| conflict("materialized input message is invalid"))?;
            validate_volatile_input_activation(&state, &materialization.input)?;
            if let Some(selection) = materialization.input.mcp_selection.as_ref()
                && (materialization.input.origin != InputOrigin::User
                    || selection.session_id != materialization.input.session_id
                    || selection.input_id.as_ref() != Some(&materialization.input.input_id)
                    || selection.message_id != materialization.input.message.id)
            {
                return Err(conflict("materialized MCP selection is inconsistent"));
            }
            if materialization.input.resumed_goal.is_some() {
                return Err(conflict("new session cannot resume a Goal"));
            }
            if materialization.input.goal_binding.is_some()
                != materialization.input.new_goal.is_some()
            {
                return Err(conflict(
                    "materialized Goal binding does not match Goal creation",
                ));
            }
            if let Some(goal) = materialization.input.new_goal.as_ref() {
                let binding = materialization
                    .input
                    .goal_binding
                    .as_ref()
                    .ok_or_else(|| conflict("materialized Goal has no binding"))?;
                if goal.session_id != materialization.session.session_id
                    || goal.goal_id != binding.goal_id
                    || goal.generation != binding.generation
                    || goal.turn != binding.turn
                    || goal.objective.source_message_id != materialization.input.message.id
                {
                    return Err(conflict("materialized Goal is inconsistent"));
                }
            }
            let session = stored_session(materialization.session);
            let mut attachments = Vec::with_capacity(materialization.attachments.len());
            for upload in materialization.attachments {
                if upload.session_id != session.session_id
                    || state.attachments.contains_key(&upload.attachment_id)
                {
                    return Err(conflict("materialized attachment is invalid"));
                }
                let agent_readable_path = super::super::attachment_stable_view_path(
                    std::path::Path::new(&session.environment.session_attachment_directory),
                    &upload.attachment_id,
                    &upload.original_name,
                )
                .to_string_lossy()
                .into_owned();
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
                attachments.push(stored);
            }
            let input = materialization.input;
            let mcp_selection = input.mcp_selection.clone();
            let queue_order = state.next_queue_order.saturating_add(1);
            let stored_input = StoredInput {
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
                input_id: input.input_id,
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
            if let Some(goal) = input.new_goal {
                state.goals.insert(goal.session_id.clone(), goal);
            }
            if let Some(activation) = input.skill_activation {
                state
                    .skill_activations
                    .insert(activation.activation_id.clone(), activation);
            }
            state.next_queue_order = queue_order;
            state.conversations.insert(
                session.session_id.clone(),
                ConversationSnapshot::new(Vec::new()),
            );
            state
                .session_usage
                .insert(session.session_id.clone(), StoredSessionUsage::default());
            state
                .sessions
                .insert(session.session_id.clone(), session.clone());
            for attachment in &attachments {
                state
                    .attachments
                    .insert(attachment.attachment_id.clone(), attachment.clone());
            }
            state
                .inputs
                .insert(stored_input.input_id.clone(), stored_input.clone());
            if let Some(selection) = mcp_selection {
                state
                    .mcp_input_selections
                    .insert(selection.selection_id.clone(), selection);
            }
            state.runs.insert(run.run_id.clone(), run.clone());
            Ok(StoredSessionMaterialization {
                goal: state.goals.get(&stored_input.session_id).cloned(),
                session,
                attachments,
                accepted: AcceptedInput {
                    input: stored_input,
                    run,
                    is_duplicate: false,
                },
            })
        })
    }

    pub(super) fn fork_session(&self, fork: SessionFork) -> StoreFuture<'_, StoredSessionFork> {
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
                    || !activation.has_valid_definition_identity()
                {
                    return Err(conflict("fork skill activation is invalid"));
                }
            }
            let mut selection_ids = BTreeSet::new();
            for selection in &fork.mcp_selections {
                let source_selection = state.mcp_input_selections.values().find(|candidate| {
                    candidate.session_id == fork.source_session_id
                        && candidate.message_id == selection.message_id
                });
                if !selection_ids.insert(selection.selection_id.clone())
                    || state
                        .mcp_input_selections
                        .contains_key(&selection.selection_id)
                    || selection.session_id != fork.session.session_id
                    || selection.input_id.is_some()
                    || !message_ids.contains(selection.message_id.as_str())
                    || !source_selection.is_some_and(|source| {
                        source.server_key == selection.server_key
                            && source.display_name == selection.display_name
                            && source.created_at_ms == selection.created_at_ms
                    })
                {
                    return Err(conflict("fork MCP selection is invalid"));
                }
            }
            let mut command_ids = BTreeSet::new();
            let mut command_message_ids = BTreeSet::new();
            for copied in &fork.session_commands {
                let command = &copied.command;
                let source = state.session_commands.get(&copied.source_input_id);
                if !command_ids.insert(command.input_id.clone())
                    || !command_message_ids.insert(command.user_message_id.clone())
                    || state.inputs.contains_key(&command.input_id)
                    || state.session_commands.contains_key(&command.input_id)
                    || command.session_id != fork.session.session_id
                    || command.idempotency_key.is_some()
                    || command.state != StoredSessionCommandState::Committed
                    || !message_ids.contains(command.user_message_id.as_str())
                    || !source.is_some_and(|source| {
                        source.session_id == fork.source_session_id
                            && source.state == StoredSessionCommandState::Committed
                            && source.user_message_id != command.user_message_id
                            && source.command == command.command
                            && source.result == command.result
                            && source.agent_variant == command.agent_variant
                            && source.accepted_at_ms == command.accepted_at_ms
                    })
                {
                    return Err(conflict("fork session command is invalid"));
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
                    "{VOLATILE_ROOT}/sessions/{}/attachments/{}/file",
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
                agent_shell_kind: fork.session.agent_shell_kind,
                agent_shell_environment: fork.session.agent_shell_environment.clone(),
                title: fork.session.title,
                title_origin: fork.session.title_origin,
                model_selection: fork.session.model_selection,
                reasoning_effort: fork.session.reasoning_effort,
                system_prompt: fork.session.system_prompt,

                environment: fork.session.environment,
                lifecycle: StoredSessionLifecycle::Active,
                current_variant: fork.session.current_variant,
                approval_mode: fork.session.approval_mode,
                role: fork.session.role,
                materialization_key: fork.session.materialization_key,
                automatic_title_pending: fork.session.automatic_title_pending,
                proxy: None,
                pc_output_hosting: None,
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
            for selection in &fork.mcp_selections {
                state
                    .mcp_input_selections
                    .insert(selection.selection_id.clone(), selection.clone());
            }
            let session_commands = fork
                .session_commands
                .into_iter()
                .map(|copied| copied.command)
                .collect::<Vec<_>>();
            for command in &session_commands {
                state
                    .session_commands
                    .insert(command.input_id.clone(), command.clone());
            }
            Ok(StoredSessionFork {
                session: stored,
                conversation,
                attachments,
                skill_activations: fork.skill_activations,
                mcp_selections: fork.mcp_selections,
                session_commands,
                work_plan,
                goal,
            })
        })
    }

    pub(super) fn inspect_session_deletion(
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

    pub(super) fn delete_session(&self, deletion: SessionDeletion) -> StoreFuture<'_, ()> {
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
            state
                .session_commands
                .retain(|_, command| command.session_id != deletion.session_id);
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
                .mcp_input_selections
                .retain(|_, selection| selection.session_id != deletion.session_id);
            state
                .pending_tool_exchanges
                .retain(|_, exchange| exchange.session_id != deletion.session_id);
            state
                .pending_child_tool_exchanges
                .retain(|_, exchange| exchange.session_id != deletion.session_id);
            Ok(())
        })
    }
}
