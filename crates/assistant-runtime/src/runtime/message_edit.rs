//! 可靠用户输入的编辑准备、完整重建与裁尾提交。

use std::collections::BTreeSet;

use agent_types::{
    ConversationMessage, ConversationSnapshot, TranscriptVisibility, UserMessageOrigin, UserPart,
};
use assistant_protocol::{
    ConversationOwner, GetUserMessageEditRequest, GetUserMessageEditResult, MessageId,
    ObservedSnapshot, ReenterFromUserMessageRequest, ReenterFromUserMessageResult, RuntimeEvent,
    UserMessageEditBlockedReason as Blocked, UserMessageEditPreparation, UserMessageMcpEdit,
    UserMessageSkillEdit, UserMessageSnapshot,
};

use super::{
    AssistantRuntime,
    goal::prepare_goal_resume,
    model::{
        RewriteCompilationInput, RunAuthorizationInput, RunCompilationResources, compile_run_agent,
    },
    now_ms,
    quote::insert_quotes,
    session_management::conversation_message_id,
};
use crate::{
    AcceptedInput, ConversationMessageLocationRequest, ConversationRawWindowRequest,
    ConversationRewrite, InputChannelSource, InputOrigin, NewStoredInput, RewriteGoalEffect,
    RuntimeError, RuntimeResult, SkillActivationOwner, SkillActivationTrigger, SkillCatalog,
    SkillName, StoredInputHistory, StoredMcpSelection, StoredSkillActivation,
    goal::{GoalControl, GoalRunBinding, GoalState},
    internal_boundary::{
        InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
    },
    run::{RunRecord, allocate_input_id, allocate_part_id, allocate_run_id, create_user_message},
    session::{InputRecord, SessionController},
};

struct EditableInput {
    ordinal: usize,
    generation: u64,
    message_count: u64,
    input: StoredInputHistory,
    message: UserMessageSnapshot,
    skill: Option<(StoredSkillActivation, String)>,
    mcp: Option<StoredMcpSelection>,
    goal: Option<GoalControl>,
}

impl AssistantRuntime {
    pub async fn get_user_message_edit(
        &self,
        request: GetUserMessageEditRequest,
    ) -> RuntimeResult<GetUserMessageEditResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id).await?;
        let _mutation = session.mutation().await;
        for _ in 0..4 {
            let observation = self
                .event_sender
                .read_session_view(session.id(), session.environment().workspace_id.clone());
            let value = match self.editable_input(&session, &request.message_id).await? {
                Ok(input) => UserMessageEditPreparation::Ready {
                    message: Box::new(input.message),
                    variant: input.input.agent_variant,
                    generation: input.generation,
                    message_count: input.message_count,
                },
                Err(reason) => UserMessageEditPreparation::Blocked { reason },
            };
            if let Some((observed_sequence, ())) = observation.capture(|| Ok(()))? {
                return Ok(GetUserMessageEditResult {
                    snapshot: ObservedSnapshot {
                        observed_sequence,
                        observed_at_ms: now_ms().ok(),
                        value,
                    },
                });
            }
        }
        Err(RuntimeError::SnapshotBusy)
    }

    pub async fn reenter_from_user_message(
        &self,
        request: ReenterFromUserMessageRequest,
    ) -> RuntimeResult<ReenterFromUserMessageResult> {
        let _operation = self.operation_gate.read().await;
        let _binding = self.model_binding_gate.read().await;
        self.ensure_running()?;
        let session = self.prepared_session(&request.session_id).await?;
        let _mutation = session.mutation().await;
        let target = agent_message_id(&request.message_id)?;
        if let Some(accepted) = self
            .find_message_acceptance(&session, &request.idempotency_key)
            .await?
        {
            return self.edit_receipt(&session, &target, &accepted).await;
        }
        session.ensure_active()?;
        session.ensure_healthy()?;
        let (generation, message_count) = {
            let state = session.lock_state()?;
            (state.body_generation, state.message_count)
        };
        if generation != request.expected_generation
            || message_count != request.expected_message_count
        {
            return Err(RuntimeError::SnapshotStale);
        }
        let original = self
            .editable_input(&session, &request.message_id)
            .await?
            .map_err(|reason| match reason {
                Blocked::SessionNotIdle => RuntimeError::SessionNotIdle {
                    session_id: session.id().clone(),
                },
                Blocked::SessionArchived => RuntimeError::SessionArchived {
                    session_id: session.id().clone(),
                },
                _ => RuntimeError::UserMessageNotEditable,
            })?;
        let owner = owner(&session);
        let history = self
            .store
            .load_conversation_history(owner.clone())
            .await
            .map_err(|source| RuntimeError::from_store("load edit history", source))?;
        let current = self
            .store
            .load_conversation(session.id())
            .await
            .map_err(|source| RuntimeError::from_store("load edit prefix", source))?;
        if history.generation != generation
            || current.messages.len() as u64 != message_count
            || !matches!(current.messages.get(original.ordinal), Some(ConversationMessage::User(user)) if user.id == target)
        {
            return Err(RuntimeError::SnapshotStale);
        }

        let retained_quotes = request
            .quotes
            .iter()
            .map(|quote| {
                original
                    .message
                    .quotes
                    .iter()
                    .find(|old| old.quote_id == quote.quote_id)
                    .unwrap_or(quote)
                    .clone()
            })
            .collect::<Vec<_>>();
        if request.message.trim().is_empty()
            && request.attachment_ids.is_empty()
            && retained_quotes.is_empty()
        {
            return Err(RuntimeError::InvalidRequest {
                reason: "edited input must contain text, an attachment, or a quote",
            });
        }
        // 已有引文沿可靠原件冻结。只对新选择查询来源，随后统一分配新的 Part 身份。
        super::quote::validate_quotes(&retained_quotes)?;
        let mut quotes = Vec::with_capacity(retained_quotes.len());
        for mut quote in retained_quotes {
            if !original
                .message
                .quotes
                .iter()
                .any(|old| old.quote_id == quote.quote_id)
            {
                quote = self
                    .normalize_quotes(session.id(), std::slice::from_ref(&quote))
                    .await?
                    .pop()
                    .ok_or(RuntimeError::UserMessageNotEditable)?;
            }
            quote.quote_id = assistant_protocol::PartId::new(allocate_part_id()?.as_str())
                .map_err(|_| RuntimeError::UserMessageNotEditable)?;
            quotes.push(quote);
        }

        let files = self.resolve_file_references(session.id(), &request.attachment_ids)?;
        let mut message = create_user_message(request.message, files, request.variant)?;
        insert_quotes(&mut message, &quotes)?;
        let (input_id, run_id, approval_mode, shell, shell_kind, plan) = {
            let state = session.lock_state()?;
            crate::run::ensure_message_id_available(&mut message.id, &state, &history.facts)?;
            (
                allocate_input_id(&state, &history.facts)?,
                allocate_run_id(&state, &history.facts)?,
                state.approval_mode,
                state.agent_shell_environment.clone(),
                state.agent_shell_kind,
                state.work_plan.clone(),
            )
        };
        let at = now_ms()?;
        let goal_effect = original
            .goal
            .as_ref()
            .map(|goal| {
                let resumed = prepare_goal_resume(goal, &mut message, at)?;
                Ok::<_, RuntimeError>(RewriteGoalEffect {
                    expected_goal_id: goal.id.clone(),
                    expected_generation: goal.generation,
                    goal: resumed.to_stored(session.id().clone()),
                })
            })
            .transpose()?;
        let catalog = self.current_skill_catalog(&session).await?;
        let mut input = NewStoredInput {
            feedback_answer: None,
            agent_shell_target: None,
            input_id: input_id.clone(),
            session_id: session.id().clone(),
            idempotency_key: Some(request.idempotency_key.clone()),
            agent_variant: request.variant,
            origin: InputOrigin::User,
            goal_binding: goal_effect.as_ref().map(|effect| crate::GoalInputBinding {
                goal_id: effect.goal.goal_id.clone(),
                generation: effect.goal.generation,
                turn: effect.goal.turn,
                reply_route: None,
            }),
            cross_session: None,
            channel_source: Some(InputChannelSource::desktop_text()),
            skill_activation: None,
            mcp_selection: None,
            message,
            new_goal: None,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: at,
        };
        self.prepare_edit_selections(
            &session,
            &original,
            request.skill_edit,
            request.mcp_edit,
            &catalog,
            &mut input,
        )?;
        input.message =
            crate::work_plan::inject_claimed_context(Some(input.message), plan.as_ref())?
                .ok_or(RuntimeError::UserMessageNotEditable)?;
        let mut prefix = current.messages;
        let removed = prefix.split_off(original.ordinal);
        let retained: BTreeSet<_> = prefix
            .iter()
            .map(conversation_message_id)
            .cloned()
            .collect();
        let removed: BTreeSet<_> = removed
            .iter()
            .map(conversation_message_id)
            .cloned()
            .collect();
        prefix.push(ConversationMessage::User(input.message.clone()));
        let mut replacement = ConversationSnapshot::new(prefix);
        crate::normalize_context_usage_adjustments(&mut replacement);
        replacement
            .validate_tool_exchange_pairs()
            .map_err(|_| RuntimeError::InvalidRequest {
                reason: "replacement would split a tool exchange",
            })?;
        let activations = history
            .facts
            .skill_activations
            .iter()
            .filter(|activation| retained.contains(&activation.message_id))
            .chain(input.skill_activation.iter())
            .cloned()
            .collect();
        let shell = if shell.is_some() {
            shell
        } else {
            let factory = self.run_tool_factory.clone();
            tokio::task::spawn_blocking(move || factory.freeze_shell(shell_kind))
                .await
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "edit shell discovery",
                })?
                .map_err(|source| RuntimeError::RunToolsBuildFailed { source })?
        };
        let config = self.config_registry.snapshot()?;
        let prepared = compile_run_agent(
            session.clone(),
            &config,
            &self.config_registry,
            RunCompilationResources {
                background: self.background_services(),
                plugin: self.plugin.clone(),
                attachments: self.attachments.clone(),
                shell,
                skill_catalog: catalog,
                skill_package_source: self.skill_package_source.clone(),
                model_factory: self.model_factory.clone(),
                context_window: self.context_window.clone(),
                run_tool_factory: self.run_tool_factory.as_ref(),
                child_task_workspace_factory: self.child_task_workspace_factory.clone(),
                child_tasks: self.child_tasks.clone(),
                store: self.store.clone(),
                recall_reference_codec: self.recall_reference_codec.clone(),
                controller_tools: self.controller_tool_coordinator(),
                output_dispatcher: self.output_dispatcher.clone(),
                mcp_registry: self.mcp_service.registry.clone(),
                mcp_management: self.mcp_management(),
                schedule_runtime: self.scheduler.runtime(),
                mcp_image_materializer: self.mcp_service.image_materializer.clone(),
            },
            RunAuthorizationInput {
                input_id: input_id.clone(),
                permission_coordinator: self.permission_coordinator.clone(),
                approval_registry: self.approval_registry.clone(),
                variant: request.variant,
                run_id: run_id.clone(),
                cancellation: self.root_cancellation.child_token(),
                events: self.event_sender.clone(),
                resume_paused_goal: goal_effect.is_some(),
                goal_binding: input.goal_binding.as_ref().map(|binding| GoalRunBinding {
                    goal_id: binding.goal_id.clone(),
                    generation: binding.generation,
                    run_id: run_id.clone(),
                }),
                rewrite_input: Some(RewriteCompilationInput {
                    activations,
                    mcp_selection: input.mcp_selection.clone(),
                }),
                input_origin: InputOrigin::User,
                cross_session: None,
            },
            None,
        )
        .await?;
        prepared.ensure_models_current(&self.config_registry)?;
        let user_message_id = protocol_message_id(&input.message.id)?;
        let resumed_goal = goal_effect
            .as_ref()
            .map(|effect| GoalControl::try_from(effect.goal.clone()))
            .transpose()
            .map_err(|_| RuntimeError::UserMessageNotEditable)?;
        let mut skill = input.skill_activation.clone();
        if let Some(activation) = &mut skill {
            activation.run_id = Some(run_id.clone());
        }
        let mcp = input.mcp_selection.clone();
        let rewritten = self
            .store
            .rewrite_from_user(ConversationRewrite {
                run_id: run_id.clone(),
                approval_mode,
                session_id: session.id().clone(),
                target_user_message_id: target.clone(),
                expected_generation: generation,
                expected_message_count: message_count,
                conversation: replacement.clone(),
                input,
                goal_effect,
                changed_at_ms: at,
            })
            .await;
        let rewritten = match rewritten {
            Ok(rewritten) => rewritten,
            Err(source) if source.kind() == crate::StoreErrorKind::Conflict => {
                return Err(RuntimeError::SnapshotStale);
            }
            Err(source) => {
                // 事务应答丢失时先从可靠历史确认。已提交而内存未安装时封闭本会话，保留回执供重启恢复。
                let result = self
                    .find_message_acceptance(&session, &request.idempotency_key)
                    .await;
                if let Ok(Some(accepted)) = result {
                    session.lock_state()?.is_faulted = true;
                    return self.edit_receipt(&session, &target, &accepted).await;
                }
                if result.is_err() {
                    session.lock_state()?.is_faulted = true;
                }
                return Err(RuntimeError::from_store(
                    "rewrite conversation history",
                    source,
                ));
            }
        };

        let run = RunRecord::accepted(&rewritten.run, vec![rewritten.user_message.id.clone()]);
        let run_snapshot = run.snapshot();
        let generation = rewritten.body_generation;
        let installed =
            (|| {
                self.child_tasks.remove_session(session.id())?;
                let last = replacement
                    .messages
                    .last_mut()
                    .ok_or(RuntimeError::UserMessageNotEditable)?;
                *last = ConversationMessage::User(rewritten.user_message);
                // 裁尾已可靠提交，按冷恢复的同一规则重建窗口及净记忆／能力状态。
                // 不能无条件使用本地摘要投影，否则原生压缩点会丢失，后续自动和手动压缩均无法核对历史。
                let mut window = crate::ExecutionWindow::from_snapshot(replacement.clone())
                    .map_err(|_| RuntimeError::InternalStateUnavailable {
                        component: "edited conversation journal",
                    })?;
                window.conversation = crate::session::context_for_mode(
                    &window.conversation,
                    session.lock_state()?.context_management_mode,
                );
                let journal = crate::journal::InMemoryJournal::from_window(window);
                let persisted_message_count = journal.message_count();
                let removed_inputs: BTreeSet<_> = history
                    .facts
                    .inputs
                    .iter()
                    .filter(|fact| removed.contains(&fact.user_message_id))
                    .map(|fact| fact.input_id.clone())
                    .collect();
                let removed_runs: BTreeSet<_> = history
                    .facts
                    .runs
                    .iter()
                    .filter(|fact| {
                        fact.input_id
                            .as_ref()
                            .is_some_and(|id| removed_inputs.contains(id))
                    })
                    .map(|fact| fact.run_id.clone())
                    .collect();
                let mut state = session.lock_state()?;
                state.runs.retain(|id, _| !removed_runs.contains(id));
                state.inputs.retain(|id, _| !removed_inputs.contains(id));
                state
                    .skill_activations
                    .retain(|activation| retained.contains(&activation.message_id));
                state.skill_activations.extend(skill);
                state
                    .mcp_selections
                    .retain(|selection| retained.contains(&selection.message_id));
                state.mcp_selections.extend(mcp);
                state
                    .commands
                    .retain(|_, command| retained.contains(&command.user_message_id));
                state
                    .feedback
                    .retain(|_, request| !removed_runs.contains(&request.run_id));
                state.context_measurements.clear();
                state.goal = resumed_goal.clone();
                state.queue_paused_by_user = false;
                state.resume_required = false;
                state.queue_revision = state.queue_revision.saturating_add(1);
                state.message_count = replacement.messages.len() as u64;
                state.persisted_message_count = persisted_message_count;
                state.body_generation = generation;
                state.journal = Some(journal);
                state.runs.insert(run_id.clone(), run);
                if resumed_goal.is_some() {
                    state.goal_inputs.push_back(input_id.clone());
                } else {
                    state.queue_item_ids.push_back(input_id.clone());
                }
                state.inputs.insert(
                    input_id.clone(),
                    InputRecord {
                        stored: rewritten.input,
                        first_run_id: Some(run_id.clone()),
                        latest_run_id: Some(run_id.clone()),
                    },
                );
                Ok::<_, RuntimeError>(state.queue_revision)
            })();
        let execution_error = match installed {
            Ok(revision) => {
                self.publish(RuntimeEvent::ConversationCommitted { owner, generation });
                self.publish(RuntimeEvent::QueueChanged {
                    session_id: session.id().clone(),
                    revision,
                });
                if let Some(goal) = resumed_goal {
                    self.publish(RuntimeEvent::GoalChanged {
                        session_id: session.id().clone(),
                        goal_id: goal.id,
                        generation: goal.generation,
                    });
                }
                self.publish(RuntimeEvent::RunAccepted {
                    created_at_ms: run_snapshot.created_at_ms,
                    session_id: session.id().clone(),
                    run_id,
                });
                drop(_mutation);
                self.wake_queue(session.clone()).err()
            }
            Err(error) => Some(error),
        };
        if execution_error.is_some()
            && let Ok(mut state) = session.lock_state()
        {
            state.is_faulted = true;
        }
        Ok(ReenterFromUserMessageResult {
            input_id,
            run: run_snapshot,
            user_message_id,
            generation,
            execution_error: execution_error.map(|error| error.to_protocol_info()),
        })
    }

    /// 保持 Session mutation。仅读取一个可靠正文窗口，避免为打开编辑加载整个会话。
    async fn editable_input(
        &self,
        session: &SessionController,
        message_id: &MessageId,
    ) -> RuntimeResult<Result<EditableInput, Blocked>> {
        if let Err(error) = session.ensure_active() {
            return match error {
                RuntimeError::SessionArchived { .. } => Ok(Err(Blocked::SessionArchived)),
                _ => Err(error),
            };
        }
        session.ensure_healthy()?;
        if let Err(error) = session.ensure_idle() {
            return match error {
                RuntimeError::SessionNotIdle { .. }
                | RuntimeError::SessionCompactionInProgress { .. } => {
                    Ok(Err(Blocked::SessionNotIdle))
                }
                _ => Err(error),
            };
        }
        let (generation, message_count, goal) = {
            let state = session.lock_state()?;
            (
                state.body_generation,
                state.message_count,
                state.goal.clone(),
            )
        };
        if goal
            .as_ref()
            .is_some_and(|goal| !matches!(goal.state, GoalState::Paused(_)))
            || self.child_tasks.active_count_for_session(session.id())? != 0
            || !self.approval_queue(session.id())?.items.is_empty()
        {
            return Ok(Err(Blocked::SessionNotIdle));
        }
        let owner = owner(session);
        let id = agent_message_id(message_id)?;
        let Some(location) = self.edit_location(&owner, &id).await? else {
            return Ok(Err(Blocked::MessageNotFound));
        };
        let ordinal = usize::try_from(location.message_ordinal)
            .map_err(|_| RuntimeError::UserMessageNotEditable)?;
        let window = self
            .store
            .load_conversation_raw_window(ConversationRawWindowRequest {
                owner: owner.clone(),
                generation: location.generation,
                start: ordinal,
                limit: 1,
            })
            .await
            .map_err(|source| RuntimeError::from_store("load editable user input", source))?;
        if generation != window.generation || message_count != window.total as u64 {
            return Err(RuntimeError::SnapshotStale);
        }
        let [ConversationMessage::User(raw)] = window.conversation.messages.as_slice() else {
            return Ok(Err(Blocked::NotLocalUserInput));
        };
        let Some(history) = window.history.as_ref() else {
            return Ok(Err(Blocked::InputUnavailable));
        };
        let Some(input) = history
            .inputs
            .iter()
            .find(|input| input.user_message_id == id)
        else {
            return Ok(Err(Blocked::NotLocalUserInput));
        };
        if raw.id != id
            || raw.origin != UserMessageOrigin::User
            || raw.transcript_visibility != TranscriptVisibility::Visible
            || input.inherited
            || input.origin != InputOrigin::User
            || input.cross_session.is_some()
        {
            return Ok(Err(Blocked::NotLocalUserInput));
        }
        if let Some(goal) = &goal
            && self
                .edit_location(&owner, &goal.objective.source_message_id)
                .await?
                .is_none_or(|objective| {
                    objective.generation != generation
                        || location.message_ordinal <= objective.message_ordinal
                })
        {
            return Ok(Err(Blocked::GoalObjectiveBoundary));
        }
        let skills = history
            .skill_activations
            .iter()
            .filter(|entry| entry.message_id == id)
            .collect::<Vec<_>>();
        let skill_parts = raw
            .parts
            .iter()
            .filter_map(|part| match part {
                UserPart::InternalContext(part) if part.kind == "skill_activation" => Some(part),
                _ => None,
            })
            .collect::<Vec<_>>();
        let skill = match (skills.as_slice(), skill_parts.as_slice()) {
            ([], []) => None,
            ([skill], [part]) if skill.input_id.as_ref() == Some(&input.input_id)
                && skill.owner == SkillActivationOwner::Session(session.id().clone())
                && skill.trigger == SkillActivationTrigger::User && skill.has_valid_definition_identity()
                && part.text.starts_with(&format!(
                    "SKILL_ACTIVATION_V1\ntrigger: user\nname: {}\ncatalog_revision: {}\ndefinition_digest: {}\n",
                    skill.name, skill.catalog_revision, skill.definition_digest,
                )) => Some(((*skill).clone(), part.text.clone())),
            _ => return Ok(Err(Blocked::InputUnavailable)),
        };
        let selections = history
            .mcp_selections
            .iter()
            .filter(|entry| entry.message_id == id)
            .collect::<Vec<_>>();
        let mcp = match selections.as_slice() {
            [] => None,
            [entry] if entry.input_id.as_ref() == Some(&input.input_id) => Some((*entry).clone()),
            _ => return Ok(Err(Blocked::InputUnavailable)),
        };
        let Some(message) =
            self.project_edit_user_message(session.id(), &window.conversation, history)?
        else {
            return Ok(Err(Blocked::InputUnavailable));
        };
        let attachment_count = raw
            .parts
            .iter()
            .map(|part| match part {
                UserPart::FileReferences(part) => part.files.len(),
                _ => 0,
            })
            .sum::<usize>();
        let quote_count = raw
            .parts
            .iter()
            .filter(|part| matches!(part, UserPart::QuotedText(_)))
            .count();
        if message.attachment_ids.len() != attachment_count || message.quotes.len() != quote_count {
            return Ok(Err(Blocked::InputUnavailable));
        }
        Ok(Ok(EditableInput {
            ordinal,
            generation,
            message_count,
            input: input.clone(),
            message,
            skill,
            mcp,
            goal,
        }))
    }

    async fn edit_location(
        &self,
        owner: &ConversationOwner,
        message_id: &agent_types::MessageId,
    ) -> RuntimeResult<Option<crate::StoredConversationMessageLocation>> {
        self.store
            .locate_conversation_message(ConversationMessageLocationRequest {
                owner: owner.clone(),
                message_id: message_id.clone(),
            })
            .await
            .map_err(|source| RuntimeError::from_store("locate edit input", source))
    }

    /// 已发布键先查当前可靠文件，原编辑目标在成功裁尾后已不存在。
    async fn edit_receipt(
        &self,
        session: &SessionController,
        target: &agent_types::MessageId,
        accepted: &AcceptedInput,
    ) -> RuntimeResult<ReenterFromUserMessageResult> {
        let id = match accepted {
            AcceptedInput::Control { input, .. } => &input.user_message_id,
            AcceptedInput::Historical { input, .. } => &input.user_message_id,
        };
        let owner = owner(session);
        let location = self
            .edit_location(&owner, id)
            .await?
            .ok_or(RuntimeError::UserMessageNotEditable)?;
        let window = self
            .store
            .load_conversation_raw_window(ConversationRawWindowRequest {
                owner,
                generation: location.generation,
                start: usize::try_from(location.message_ordinal)
                    .map_err(|_| RuntimeError::UserMessageNotEditable)?,
                limit: 1,
            })
            .await
            .map_err(|source| RuntimeError::from_store("verify edit receipt", source))?;
        if !window.history.as_ref().is_some_and(|facts| {
            facts.inputs.iter().any(|input| {
                input.input_id == *accepted.input_id()
                    && !input.inherited
                    && input.rewritten_from.as_ref() == Some(target)
            })
        }) {
            return Err(RuntimeError::InvalidRequest {
                reason: "request key belongs to another input",
            });
        }
        let run = self
            .project_accepted_run(session, accepted)
            .await?
            .ok_or(RuntimeError::UserMessageNotEditable)?;
        let execution_error = {
            let mut state = session.lock_state()?;
            if state.body_generation != window.generation {
                state.is_faulted = true;
            }
            state.is_faulted.then(|| {
                RuntimeError::InternalStateUnavailable {
                    component: "edited session execution requires recovery",
                }
                .to_protocol_info()
            })
        };
        Ok(ReenterFromUserMessageResult {
            input_id: accepted.input_id().clone(),
            run,
            user_message_id: protocol_message_id(id)?,
            generation: window.generation,
            execution_error,
        })
    }

    fn prepare_edit_selections(
        &self,
        session: &SessionController,
        original: &EditableInput,
        skill_edit: UserMessageSkillEdit,
        mcp_edit: UserMessageMcpEdit,
        catalog: &SkillCatalog,
        input: &mut NewStoredInput,
    ) -> RuntimeResult<()> {
        let skill = match skill_edit {
            UserMessageSkillEdit::Clear => None,
            UserMessageSkillEdit::Retain => original.skill.as_ref().map(|(old, text)| {
                (
                    old.name.clone(),
                    old.catalog_revision.clone(),
                    old.definition_digest.clone(),
                    text.clone(),
                )
            }),
            UserMessageSkillEdit::Select { name } => {
                let name = SkillName::parse(name).map_err(|_| RuntimeError::SkillNameInvalid)?;
                let definition = catalog.user_definition(&name).map_err(|error| {
                    use crate::SkillActivationResolveError::*;
                    match error {
                        CatalogUnavailable => RuntimeError::SkillCatalogUnavailable {
                            session_id: session.id().clone(),
                        },
                        NotFound => RuntimeError::SkillNotFound {
                            session_id: session.id().clone(),
                        },
                        NotUserInvocable => RuntimeError::SkillNotUserInvocable {
                            session_id: session.id().clone(),
                        },
                    }
                })?;
                Some((
                    name,
                    catalog.revision.clone(),
                    definition.definition_digest.clone(),
                    crate::skill::render_user_activation(&catalog.revision, definition),
                ))
            }
        };
        if let Some((name, catalog_revision, definition_digest, text)) = skill {
            InternalBoundaryCoordinator::append(
                &mut input.message,
                InternalBoundaryRequest {
                    source: InternalBoundarySource::SkillActivation,
                    text,
                },
            )?;
            input.skill_activation = Some(StoredSkillActivation {
                activation_id: new_identity("skill-activation")?,
                session_id: session.id().clone(),
                owner: SkillActivationOwner::Session(session.id().clone()),
                run_id: None,
                input_id: Some(input.input_id.clone()),
                message_id: input.message.id.clone(),
                name,
                catalog_revision,
                definition_digest,
                trigger: SkillActivationTrigger::User,
                created_at_ms: input.accepted_at_ms,
            });
        }
        let mcp = match mcp_edit {
            UserMessageMcpEdit::Clear => None,
            UserMessageMcpEdit::Retain => original
                .mcp
                .as_ref()
                .map(|old| (old.server_key.clone(), Some(old.display_name.clone()))),
            UserMessageMcpEdit::Select { server_key } => Some((server_key, None)),
        };
        if let Some((server_key, frozen_name)) = mcp {
            let server = self
                .mcp_service
                .registry
                .catalog_server(&server_key)?
                .ok_or(RuntimeError::McpServerUnavailable)?;
            if !server.tools.iter().any(|tool| {
                self.permission_coordinator
                    .mcp_tool_is_explicitly_denied(
                        &session.permission_scopes(),
                        input.agent_variant,
                        &server_key,
                        &tool.name,
                    )
                    .is_ok_and(|denied| !denied)
            }) {
                return Err(RuntimeError::McpServerUnavailable);
            }
            input.mcp_selection = Some(StoredMcpSelection {
                selection_id: new_identity("mcp-selection")?,
                session_id: session.id().clone(),
                input_id: Some(input.input_id.clone()),
                message_id: input.message.id.clone(),
                server_key,
                display_name: frozen_name.unwrap_or(server.display_name),
                created_at_ms: input.accepted_at_ms,
            });
        }
        Ok(())
    }
}

fn new_identity(prefix: &'static str) -> RuntimeResult<String> {
    crate::id::generate(prefix)
        .map_err(|_| RuntimeError::InternalStateUnavailable { component: prefix })
}

fn owner(session: &SessionController) -> ConversationOwner {
    ConversationOwner::MainSession {
        session_id: session.id().clone(),
    }
}

fn agent_message_id(id: &MessageId) -> RuntimeResult<agent_types::MessageId> {
    agent_types::MessageId::new(id.as_str()).map_err(|_| RuntimeError::UserMessageNotEditable)
}

fn protocol_message_id(id: &agent_types::MessageId) -> RuntimeResult<MessageId> {
    MessageId::new(id.as_str()).map_err(|_| RuntimeError::UserMessageNotEditable)
}
