use super::*;

impl State {
    pub(super) fn conversation_history(
        &self,
        owner: ConversationOwner,
    ) -> Result<super::super::StoredConversationHistory, StoreError> {
        let state = self;
        let mut facts;
        let generation = match owner {
            ConversationOwner::MainSession { session_id } => {
                let session = state
                    .sessions
                    .get(&session_id)
                    .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
                facts = state
                    .histories
                    .get(&session_id)
                    .cloned()
                    .unwrap_or_default();
                session.body_generation
            }
            ConversationOwner::ChildTask {
                session_id,
                child_task_id,
            } => {
                let task = state
                    .child_history(&session_id, &child_task_id)
                    .ok_or_else(|| conflict("child history does not exist in session"))?;
                if task.body_unavailable {
                    return Err(StoreError::new(
                        StoreErrorKind::ResourceUnavailable,
                        "child body is unavailable",
                    ));
                }
                facts = state
                    .child_histories
                    .get(&(session_id, child_task_id))
                    .cloned()
                    .ok_or_else(|| conflict("child history facts are missing"))?;
                task.body_generation
            }
        };
        facts.normalize();
        Ok(super::super::StoredConversationHistory { generation, facts })
    }
}

impl VolatileRuntimeStore {
    pub(super) fn load_conversation_history(
        &self,
        owner: ConversationOwner,
    ) -> StoreFuture<'_, super::super::StoredConversationHistory> {
        Box::pin(async move { self.lock()?.conversation_history(owner) })
    }
    pub(super) fn load_conversation(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, ConversationSnapshot> {
        let session_id = session_id.clone();
        Box::pin(async move {
            self.lock()?
                .conversations
                .get(&session_id)
                .cloned()
                .ok_or_else(|| conflict("session does not exist in runtime storage"))
        })
    }

    pub(super) fn get_session_usage(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, StoredSessionUsage> {
        let session_id = session_id.clone();
        Box::pin(async move {
            Ok(self
                .lock()?
                .session_usage
                .get(&session_id)
                .cloned()
                .unwrap_or_default())
        })
    }

    pub(super) fn load_conversation_window(
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
                        .child_history(session_id, child_task_id)
                        .ok_or_else(|| conflict("child task does not exist in session"))?;
                    if task.body_generation != request.generation {
                        return Err(conflict("conversation generation changed"));
                    }
                    state
                        .child_conversations
                        .get(&(session_id.clone(), child_task_id.clone()))
                        .cloned()
                        .ok_or_else(|| conflict("child conversation does not exist"))?
                }
            };
            let mut window = conversation_window(snapshot, &request);
            window.history = Some(state.conversation_history(request.owner)?.facts);
            Ok(window)
        })
    }

    pub(super) fn load_conversation_raw_window(
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
                        .child_history(session_id, child_task_id)
                        .ok_or_else(|| conflict("child task does not exist in session"))?;
                    if task.body_generation != request.generation {
                        return Err(conflict("conversation generation changed"));
                    }
                    state
                        .child_conversations
                        .get(&(session_id.clone(), child_task_id.clone()))
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
                history: Some(state.conversation_history(request.owner)?.facts),
            })
        })
    }

    pub(super) fn locate_conversation_message(
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
                        .child_history(session_id, child_task_id)
                        .ok_or_else(|| conflict("child task does not exist in session"))?;
                    let snapshot = state
                        .child_conversations
                        .get(&(session_id.clone(), child_task_id.clone()))
                        .ok_or_else(|| conflict("child conversation does not exist"))?;
                    (task.body_generation, snapshot)
                }
            };
            snapshot
                .messages
                .iter()
                .position(|message| message_id(message) == &request.message_id)
                .map(|ordinal| {
                    let display_ordinal =
                        snapshot.messages[ordinal].is_transcript_visible().then(|| {
                            snapshot.messages[..ordinal]
                                .iter()
                                .filter(|message| message.is_transcript_visible())
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

    pub(super) fn search_conversations(
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
                for (_, task) in state.recovered_child_history(Some(session_id)) {
                    if let Some(child) = state
                        .child_conversations
                        .get(&(session_id.clone(), task.child_task_id.clone()))
                    {
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
            for hit in &mut hits {
                if let ConversationOwner::ChildTask {
                    session_id,
                    child_task_id,
                } = &hit.owner
                {
                    hit.child_task_title = state
                        .child_history(session_id, child_task_id)
                        .map(|task| task.title.clone());
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

    pub(super) fn set_message_feedback(
        &self,
        change: MessageFeedbackChange,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if !state.sessions.contains_key(&change.session_id) {
                return Err(conflict("feedback session does not exist"));
            }
            let history = state.histories.entry(change.session_id).or_default();
            history.feedback.push(crate::StoredHistoryFeedback {
                message_id: change.message_id,
                feedback: change.feedback,
                changed_at_ms: change.changed_at_ms,
            });
            history.normalize();
            Ok(())
        })
    }

    pub(super) fn load_message_feedback(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, Vec<StoredMessageFeedback>> {
        let session_id = session_id.clone();
        Box::pin(async move {
            let state = self.lock()?;
            Ok(state
                .histories
                .get(&session_id)
                .into_iter()
                .flat_map(|history| &history.feedback)
                .filter_map(|feedback| {
                    Some(StoredMessageFeedback {
                        message_id: feedback.message_id.clone(),
                        feedback: feedback.feedback?,
                    })
                })
                .collect())
        })
    }

    pub(super) fn rewrite_from_user(
        &self,
        rewrite: ConversationRewrite,
    ) -> StoreFuture<'_, RewriteResult> {
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
            state.ensure_new_input_identity(
                &rewrite.session_id,
                &rewrite.input.input_id,
                Some(&rewrite.run_id),
                &rewrite.input.message.id,
            )?;
            if state
                .sessions
                .get(&rewrite.session_id)
                .is_none_or(|session| session.lifecycle != StoredSessionLifecycle::Active)
            {
                return Err(conflict("session is archived or missing"));
            }
            let target_order = state
                .histories
                .get(&rewrite.session_id)
                .into_iter()
                .flat_map(|history| &history.inputs)
                .find(|input| {
                    !input.inherited && input.user_message_id == rewrite.target_user_message_id
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
            if let Some(effect) = rewrite.goal_effect.as_ref() {
                let current = state
                    .goals
                    .get(&rewrite.session_id)
                    .ok_or_else(|| conflict("history rewrite Goal does not exist"))?;
                let goal = &effect.goal;
                if current.goal_id != effect.expected_goal_id
                    || current.generation != effect.expected_generation
                    || goal.goal_id != current.goal_id
                    || goal.session_id != current.session_id
                    || goal.objective != current.objective
                    || goal.state != StoredGoalState::Paused
                    || goal.pause_reason != Some(StoredGoalPauseReason::RecoveryRequired)
                    || goal.generation
                        != current
                            .generation
                            .checked_add(1)
                            .ok_or_else(|| conflict("Goal generation is exhausted"))?
                    || goal.turn != current.turn
                    || goal.budget != current.budget
                    || goal.consecutive_failures != current.consecutive_failures
                    || goal.created_at_ms != current.created_at_ms
                    || goal.updated_at_ms != rewrite.changed_at_ms
                    || goal.completed_at_ms.is_some()
                {
                    return Err(conflict("history rewrite Goal projection is invalid"));
                }
            }

            let mut removed = state
                .inputs
                .values()
                .filter(|input| {
                    input.session_id == rewrite.session_id && input.queue_order >= target_order
                })
                .map(|input| input.input_id.clone())
                .collect::<std::collections::BTreeSet<_>>();
            removed.extend(
                state
                    .histories
                    .get(&rewrite.session_id)
                    .into_iter()
                    .flat_map(|history| &history.inputs)
                    .filter(|input| !input.inherited && input.queue_order >= target_order)
                    .map(|input| input.input_id.clone()),
            );
            let retained_message_ids = rewrite
                .conversation
                .messages
                .iter()
                .map(message_id)
                .cloned()
                .collect::<BTreeSet<_>>();
            state.retain_rewritten_history(&rewrite.session_id, &removed, &retained_message_ids);
            state
                .inputs
                .retain(|_, input| !removed.contains(&input.input_id));
            state.input_identities.retain(|id, _| !removed.contains(id));
            state.runs.retain(|_, run| !removed.contains(&run.input_id));
            // EndTurn 的父 Run 控制可能早已释放；只按被改写 owner 的可靠历史裁尾。
            // 其他 Session 的待回答控制不参与这次历史替换。
            let retained_runs = state
                .histories
                .get(&rewrite.session_id)
                .into_iter()
                .flat_map(|history| history.runs.iter())
                .map(|run| run.run_id.clone())
                .collect::<BTreeSet<_>>();
            state.feedback.retain(|_, request| {
                request.session_id != rewrite.session_id || retained_runs.contains(&request.run_id)
            });
            state.skill_activations.retain(|_, activation| {
                activation.session_id != rewrite.session_id
                    || retained_message_ids.contains(&activation.message_id)
            });
            state.mcp_input_selections.retain(|_, selection| {
                selection.session_id != rewrite.session_id
                    || retained_message_ids.contains(&selection.message_id)
            });
            state.session_commands.retain(|_, command| {
                command.session_id != rewrite.session_id
                    || retained_message_ids.contains(&command.user_message_id)
            });
            let retained_commands = state
                .histories
                .get(&rewrite.session_id)
                .into_iter()
                .flat_map(|history| history.commands.iter())
                .filter(|command| retained_message_ids.contains(&command.user_message_id))
                .map(|command| command.input_id.clone())
                .collect::<BTreeSet<_>>();
            state.command_identities.retain(|id, identity| {
                identity.session_id != rewrite.session_id || retained_commands.contains(id)
            });
            if let Some(effect) = rewrite.goal_effect.as_ref() {
                state
                    .goals
                    .insert(rewrite.session_id.clone(), effect.goal.clone());
            }
            state.next_queue_order += 1;
            let input = StoredInput {
                agent_shell_target: None,
                queue_order: state.next_queue_order,
                input_id: rewrite.input.input_id.clone(),
                session_id: rewrite.session_id.clone(),
                idempotency_key: rewrite.input.idempotency_key,
                agent_variant: rewrite.input.agent_variant,
                origin: rewrite.input.origin,
                goal_binding: rewrite.input.goal_binding,
                cross_session: rewrite.input.cross_session,
                channel_source: rewrite.input.channel_source,
                skill_activation: rewrite.input.skill_activation,
                user_message_id: new_message.id.clone(),
                state: StoredInputState::Committed,
                queued_message: None,
                accepted_at_ms: rewrite.input.accepted_at_ms,
            };
            let run = StoredRun {
                shell: None,
                run_id: rewrite.run_id,
                session_id: rewrite.session_id.clone(),
                input_id: input.input_id.clone(),
                attempt: 1,
                status: RunStatus::Accepted,
                agent_variant: rewrite.input.agent_variant,
                approval_mode: rewrite.approval_mode,
                reasoning_effort: None,
                cancel_requested: false,
                error: None,
                message_ids: vec![new_message.id],
                message_steps: std::collections::HashMap::new(),
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
            state
                .input_identities
                .insert(input.input_id.clone(), InputIdentity::from_input(&input));
            state.runs.insert(run.run_id.clone(), run.clone());
            state.record_run_history(&run.run_id)?;
            Ok(RewriteResult {
                input,
                run,
                body_generation,
            })
        })
    }
}
