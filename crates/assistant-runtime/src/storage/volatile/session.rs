use super::*;

impl State {
    fn materialized_input(&self, session_id: &SessionId) -> Result<StoredInput, StoreError> {
        let id = self
            .materialization_inputs
            .get(session_id)
            .ok_or_else(|| conflict("original materialization Input identity is unavailable"))?;
        self.input_projection(session_id, id)
    }
}

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
                context_management_mode: None,
                server_history_key: None,
                custom_agent: session.custom_agent,
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
                let input = state.materialized_input(&existing.session_id)?;
                let accepted = state.existing_input_acceptance(input.clone())?;
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
                let selections = state.mcp_selection_projections(Some(&existing.session_id));
                let existing_selection = selections
                    .iter()
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
                    accepted,
                });
            }
            if state
                .sessions
                .contains_key(&materialization.session.session_id)
                || state.inputs.contains_key(&materialization.input.input_id)
                || materialization.input.session_id != materialization.session.session_id
            {
                return Err(conflict("materialization identities conflict"));
            }
            state.ensure_new_input_identity(
                &materialization.input.session_id,
                &materialization.input.input_id,
                None,
                &materialization.input.message.id,
            )?;
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
                    || state.contains_mcp_selection(
                        &materialization.input.session_id,
                        &selection.selection_id,
                    )
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
            state
                .materialization_inputs
                .insert(session.session_id.clone(), stored_input.input_id.clone());
            for attachment in &attachments {
                state
                    .attachments
                    .insert(attachment.attachment_id.clone(), attachment.clone());
            }
            state
                .inputs
                .insert(stored_input.input_id.clone(), stored_input.clone());
            state.input_identities.insert(
                stored_input.input_id.clone(),
                InputIdentity::from_input(&stored_input),
            );
            if let Some(selection) = mcp_selection {
                state
                    .mcp_input_selections
                    .insert(selection.selection_id.clone(), selection);
            }
            Ok(StoredSessionMaterialization {
                goal: state.goals.get(&stored_input.session_id).cloned(),
                session,
                attachments,
                accepted: AcceptedInput::Control {
                    input: Box::new(stored_input),
                    run: None,
                    is_duplicate: false,
                },
            })
        })
    }

    pub(super) fn get_session_materialization(
        &self,
        key: &assistant_protocol::IdempotencyKey,
    ) -> StoreFuture<'_, Option<StoredSessionMaterialization>> {
        let key = key.clone();
        Box::pin(async move {
            let state = self.lock()?;
            let Some(session) = state
                .sessions
                .values()
                .find(|session| session.materialization_key.as_ref() == Some(&key))
                .cloned()
            else {
                return Ok(None);
            };
            let input = state.materialized_input(&session.session_id)?;
            let accepted = state.existing_input_acceptance(input.clone())?;
            let mut available = state
                .attachments
                .values()
                .filter(|attachment| attachment.session_id == session.session_id)
                .cloned()
                .collect::<Vec<_>>();
            let persisted_message = input.queued_message.as_ref().or_else(|| {
                state
                    .conversations
                    .get(&session.session_id)
                    .and_then(|conversation| {
                        conversation
                            .messages
                            .iter()
                            .find_map(|message| match message {
                                ConversationMessage::User(user)
                                    if user.id == input.user_message_id =>
                                {
                                    Some(user)
                                }
                                _ => None,
                            })
                    })
            });
            let mut attachments = Vec::new();
            if let Some(message) = persisted_message {
                for file in message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        UserPart::FileReferences(files) => Some(&files.files),
                        _ => None,
                    })
                    .flatten()
                {
                    if let Some(index) = available
                        .iter()
                        .position(|attachment| attachment.agent_readable_path == file.readable_path)
                    {
                        attachments.push(available.remove(index));
                    }
                }
            }
            Ok(Some(StoredSessionMaterialization {
                goal: state.goals.get(&session.session_id).cloned(),
                session,
                attachments,
                accepted,
            }))
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
                    || state.contains_skill_activation(
                        &fork.session.session_id,
                        &activation.owner,
                        &activation.activation_id,
                    )
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
                let source_selection =
                    state
                        .histories
                        .get(&fork.source_session_id)
                        .and_then(|history| {
                            history
                                .mcp_selections
                                .iter()
                                .find(|candidate| candidate.message_id == selection.message_id)
                        });
                if !selection_ids.insert(selection.selection_id.clone())
                    || state
                        .contains_mcp_selection(&fork.session.session_id, &selection.selection_id)
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
                let source = state
                    .histories
                    .get(&fork.source_session_id)
                    .and_then(|history| {
                        history
                            .commands
                            .iter()
                            .find(|command| command.input_id == copied.source_input_id)
                    });
                if !command_ids.insert(command.input_id.clone())
                    || !command_message_ids.insert(command.user_message_id.clone())
                    || state.inputs.contains_key(&command.input_id)
                    || state.input_identities.contains_key(&command.input_id)
                    || state.command_identities.contains_key(&command.input_id)
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
            // 同一独占提交中从源事实裁切，不从已释放的 Run 控制重建继承历史。
            let mut inherited = state
                .histories
                .get(&fork.source_session_id)
                .cloned()
                .unwrap_or_default()
                .fork_lineage(
                    &fork.session.session_id,
                    &fork.conversation,
                    &fork.session_commands,
                )?;
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
            // 所有 child 准备成功后才安装目标；正文缺失只保留明确不可用的目录，
            // 不把来源 owner 的单键正文借给目标，也不创建 child 执行控制。
            let mut children = Vec::new();
            for child in &mut inherited.child_tasks {
                if child.body_unavailable {
                    continue;
                }
                let source_key = (fork.source_session_id.clone(), child.child_task_id.clone());
                let (Some(body), Some(source_facts)) = (
                    state.child_conversations.get(&source_key),
                    state.child_histories.get(&source_key),
                ) else {
                    child.body_unavailable = true;
                    continue;
                };
                let source_binding = state
                    .child_history(&fork.source_session_id, &child.child_task_id)
                    .ok_or_else(|| conflict("fork child source description is missing"))?;
                if source_binding.parent_run_id != child.parent_run_id
                    || source_binding.parent_tool_call_id != child.parent_tool_call_id
                    || source_binding.status != child.status
                {
                    return Err(conflict("fork child source binding changed"));
                }
                let mut body = body.clone();
                let mut facts = source_facts.fork_lineage(&fork.session.session_id, &body, &[])?;
                if rewrite_file_reference_paths(&mut body, &path_rewrites).is_err() {
                    child.body_unavailable = true;
                    continue;
                }
                facts.child_tasks = vec![child.clone()];
                facts.skill_activations = source_facts
                    .skill_activations
                    .iter()
                    .cloned()
                    .map(|mut activation| {
                        activation.session_id = fork.session.session_id.clone();
                        activation.run_id = None;
                        activation.input_id = None;
                        activation
                    })
                    .collect();
                children.push((child.child_task_id.clone(), body, facts));
            }
            let message_count = u64::try_from(conversation.messages.len())
                .map_err(|_| conflict("fork conversation is too large"))?;
            let stored = StoredSession {
                context_management_mode: None,
                server_history_key: None,
                custom_agent: fork.session.custom_agent,
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
            state.histories.insert(stored.session_id.clone(), inherited);
            for (id, body, facts) in children {
                let key = (stored.session_id.clone(), id);
                state.child_conversations.insert(key.clone(), body);
                state.child_histories.insert(key, facts);
            }
            // Fork 只继承可靠选择；目标不产生可消费的选择控制或输入身份。
            let history = state
                .histories
                .entry(stored.session_id.clone())
                .or_default();
            history
                .skill_activations
                .extend(fork.skill_activations.iter().cloned());
            history
                .mcp_selections
                .extend(fork.mcp_selections.iter().cloned());
            let session_commands = fork
                .session_commands
                .into_iter()
                .map(|copied| copied.command)
                .collect::<Vec<_>>();
            state
                .histories
                .entry(stored.session_id.clone())
                .or_default()
                .commands
                .extend(session_commands.iter().cloned());
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
                run_count: count_u64(state.session_run_ids(&session_id).len())?,
                child_task_count: count_u64(state.session_child_ids(&session_id).len())?,
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
                run_count: count_u64(state.session_run_ids(&deletion.session_id).len())?,
                child_task_count: count_u64(state.session_child_ids(&deletion.session_id).len())?,
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
            state
                .feedback
                .retain(|_, request| request.session_id != deletion.session_id);
            state.sessions.remove(&deletion.session_id);
            state.materialization_inputs.remove(&deletion.session_id);
            state.conversations.remove(&deletion.session_id);
            state.histories.remove(&deletion.session_id);
            state
                .input_identities
                .retain(|_, identity| identity.session_id != deletion.session_id);
            state
                .child_histories
                .retain(|(owner, _), _| owner != &deletion.session_id);
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
            state
                .command_identities
                .retain(|_, identity| identity.session_id != deletion.session_id);
            state.runs.retain(|id, _| !run_ids.contains(id));
            state
                .child_tasks
                .retain(|_, task| task.session_id != deletion.session_id);
            state
                .child_conversations
                .retain(|(owner, _), _| owner != &deletion.session_id);
            state
                .attachments
                .retain(|_, attachment| attachment.session_id != deletion.session_id);
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
