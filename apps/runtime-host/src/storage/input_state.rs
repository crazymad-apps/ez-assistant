//! Input 准入、幂等命中、排队取消与恢复投影。

use agent_types::MessageId;
use assistant_protocol::{GoalId, IdempotencyKey, InputId, RunStatus, SessionId};
use assistant_runtime::{
    AcceptedInput, CrossSessionInputBinding, GoalHeldInputResume, GoalHeldInputResumeResult,
    GoalInputBinding, InputOrigin, NewStoredInput, SkillActivationOwner, SkillActivationTrigger,
    StoredInput, StoredInputState, StoredRun, StoredSkillActivation, validate_input_message,
};
use rusqlite::Transaction;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, conflict, database_write_error,
    goal::{apply_goal_resume, insert_new_goal},
    internal_error, invalid_data, invalid_data_with_source,
    mode::{agent_variant_value, approval_mode_value, parse_agent_variant},
    skill::insert_skill_activation,
};

impl StorageEngine {
    pub(super) fn resume_goal_with_held_input(
        &mut self,
        resume: GoalHeldInputResume,
    ) -> StorageResult<GoalHeldInputResumeResult> {
        let binding = GoalInputBinding {
            goal_id: resume.resumed_goal.goal_id.clone(),
            generation: resume.resumed_goal.generation,
            turn: resume.resumed_goal.turn,
        };
        if resume.expected_goal_id != resume.resumed_goal.goal_id
            || resume.resumed_goal.generation
                != resume
                    .expected_generation
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("Goal generation is exhausted"))?
            || resume.message.id.as_str().is_empty()
        {
            return Err(invalid_data("held Input Goal resume is invalid"));
        }
        validate_input_message(InputOrigin::User, Some(&binding), None, &resume.message)
            .map_err(|_| invalid_data("held Goal resume message is invalid"))?;
        let message_json = serde_json::to_string(&resume.message).map_err(|source| {
            internal_error("held Goal resume message could not be encoded", source)
        })?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("held Input Goal resume could not begin", source))?;
        apply_goal_resume(&transaction, &resume.resumed_goal)?;
        let changed = transaction
            .execute(
                "UPDATE inputs
                 SET goal_id = ?1, goal_generation = ?2, goal_turn = ?3,
                     queued_message_json = ?4
                 WHERE input_id = ?5 AND session_id = ?6 AND state = 'queued'
                   AND origin = 'user' AND goal_id IS NULL AND goal_generation IS NULL
                   AND goal_turn IS NULL AND user_message_id = ?7
                   AND EXISTS (
                       SELECT 1 FROM runs WHERE runs.input_id = inputs.input_id
                         AND runs.status = 'accepted'
                   )",
                params![
                    binding.goal_id.as_str(),
                    i64::try_from(binding.generation).map_err(|source| internal_error(
                        "Goal generation exceeds storage range",
                        source
                    ))?,
                    i64::from(binding.turn),
                    message_json,
                    resume.input_id.as_str(),
                    resume.session_id.as_str(),
                    resume.message.id.as_str(),
                ],
            )
            .map_err(|source| {
                database_write_error("held Input could not be bound to Goal", source)
            })?;
        if changed != 1 {
            return Err(conflict("Input is not held user guidance"));
        }
        transaction.commit().map_err(|source| {
            database_write_error("held Input Goal resume could not commit", source)
        })?;
        let input = self
            .load_inputs()?
            .into_iter()
            .find(|input| input.input_id == resume.input_id)
            .ok_or_else(|| invalid_data("resumed held Input is missing"))?;
        let run = self
            .load_runs()?
            .into_iter()
            .find(|run| run.input_id == resume.input_id && run.status == RunStatus::Accepted)
            .ok_or_else(|| invalid_data("resumed held Input Run is missing"))?;
        Ok(GoalHeldInputResumeResult {
            goal: resume.resumed_goal,
            input,
            run,
        })
    }

    pub(super) fn prioritize_queued_input(
        &mut self,
        change: assistant_runtime::QueuePriorityChange,
    ) -> StorageResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("queue priority update could not begin", source))?;
        transaction
            .execute(
                "UPDATE inputs
                 SET priority_order = COALESCE(priority_order, queue_order) + 1
                 WHERE session_id = ?1 AND state = 'queued'
                   AND origin = 'user' AND goal_id IS NULL",
                [change.session_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("queued input priorities could not be shifted", source)
            })?;
        let changed = transaction
            .execute(
                "UPDATE inputs SET priority_order = ?1
                 WHERE session_id = ?2 AND input_id = ?3 AND state = 'queued'
                   AND origin = 'user' AND goal_id IS NULL",
                params![0_i64, change.session_id.as_str(), change.input_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("queued input priority could not be updated", source)
            })?;
        if changed != 1 {
            return Err(conflict("input is not queued"));
        }
        transaction.commit().map_err(|source| {
            database_write_error("queue priority update could not be committed", source)
        })?;
        Ok(())
    }

    pub(super) fn accept_input(&mut self, input: NewStoredInput) -> StorageResult<AcceptedInput> {
        if let Some(key) = input.idempotency_key.as_ref()
            && let Some(existing) = self.load_inputs()?.into_iter().find(|candidate| {
                candidate.session_id == input.session_id
                    && candidate.idempotency_key.as_ref() == Some(key)
            })
        {
            let run = self
                .load_runs()?
                .into_iter()
                .find(|run| run.input_id == existing.input_id && run.attempt == 1)
                .ok_or_else(|| invalid_data("accepted input has no first run"))?;
            return Ok(AcceptedInput {
                input: existing,
                run,
                is_duplicate: true,
            });
        }
        validate_input_message(
            input.origin,
            input.goal_binding.as_ref(),
            input.cross_session_binding.as_ref(),
            &input.message,
        )
        .map_err(|_| invalid_data("input message origin or Goal binding is invalid"))?;
        validate_new_input_activation(&input)?;
        if input.new_goal.is_some() && input.resumed_goal.is_some() {
            return Err(invalid_data(
                "input cannot start and resume a Goal together",
            ));
        }
        if let Some(goal) = input.new_goal.as_ref() {
            let binding = input
                .goal_binding
                .as_ref()
                .ok_or_else(|| invalid_data("new Goal input has no Goal binding"))?;
            if input.origin != InputOrigin::User
                || goal.session_id != input.session_id
                || goal.goal_id != binding.goal_id
                || goal.generation != binding.generation
                || goal.turn != binding.turn
                || goal.objective.source_message_id != input.message.id
            {
                return Err(invalid_data("new Goal does not match its first input"));
            }
        }
        let message_json = serde_json::to_string(&input.message)
            .map_err(|source| internal_error("queued user message could not be encoded", source))?;
        let skill_activation_json = input
            .skill_activation
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|source| {
                internal_error("input skill activation could not be encoded", source)
            })?;
        let cross_session_binding_json = input
            .cross_session_binding
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|source| {
                internal_error("cross-session input binding could not be encoded", source)
            })?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("input acceptance could not begin", source))?;
        match input.cross_session_binding.as_ref() {
            Some(CrossSessionInputBinding::ControllerDelivery {
                controller_session_id,
                controller_run_id,
                ..
            }) => {
                if input.origin != InputOrigin::Runtime
                    || input.goal_binding.is_some()
                    || input.skill_activation.is_some()
                    || input.new_goal.is_some()
                    || input.resumed_goal.is_some()
                    || input.generated_title.is_some()
                    || input.idempotency_key.is_none()
                {
                    return Err(invalid_data("controller delivery input shape is invalid"));
                }
                let source = transaction
                    .query_row(
                        "SELECT sessions.role, sessions.lifecycle, runs.status
                         FROM sessions JOIN runs ON runs.session_id = sessions.session_id
                         WHERE sessions.session_id = ?1 AND runs.run_id = ?2",
                        params![controller_session_id.as_str(), controller_run_id.as_str()],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(|source| {
                        internal_error("controller delivery source could not be queried", source)
                    })?;
                let target = transaction
                    .query_row(
                        "SELECT role, lifecycle, proxy_controller_session_id
                         FROM sessions WHERE session_id = ?1",
                        [input.session_id.as_str()],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, Option<String>>(2)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(|source| {
                        internal_error("controller delivery target could not be queried", source)
                    })?;
                let queue_exists = transaction
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM inputs WHERE session_id = ?1 AND state = 'queued')",
                        [input.session_id.as_str()],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(|source| {
                        internal_error("controller delivery queue could not be queried", source)
                    })?;
                if !matches!(source, Some((role, lifecycle, status))
                    if role == "controller"
                        && lifecycle == "active"
                        && matches!(status.as_str(), "running" | "cancelling"))
                    || !matches!(target, Some((role, lifecycle, proxy))
                        if role == "standard"
                            && lifecycle == "active"
                            && proxy.as_deref() == Some(controller_session_id.as_str()))
                    || queue_exists
                {
                    return Err(conflict("controller delivery is not currently accepted"));
                }
            }
            Some(CrossSessionInputBinding::ProxyReport { .. }) => {
                return Err(invalid_data(
                    "proxy reports must be accepted through run settlement",
                ));
            }
            None if input.origin == InputOrigin::User => {
                transaction
                    .execute(
                        "UPDATE sessions
                         SET proxy_controller_session_id = NULL, proxy_changed_at_ms = NULL
                         WHERE session_id = ?1 AND role = 'standard' AND lifecycle = 'active'",
                        [input.session_id.as_str()],
                    )
                    .map_err(|source| {
                        database_write_error("session takeover could not clear proxy", source)
                    })?;
                transaction
                    .execute(
                        "DELETE FROM inputs
                         WHERE session_id = ?1 AND state = 'queued' AND origin = 'runtime'
                           AND json_extract(cross_session_binding_json, '$.kind') = 'controller_delivery'",
                        [input.session_id.as_str()],
                    )
                    .map_err(|source| {
                        database_write_error(
                            "session takeover could not remove queued deliveries",
                            source,
                        )
                    })?;
            }
            None => {}
        }
        let priority_order = transaction
            .query_row(
                "SELECT COALESCE(MAX(COALESCE(priority_order, queue_order)), -1) + 1
                 FROM inputs WHERE session_id = ?1",
                [input.session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| internal_error("next queue priority could not be read", source))?;
        if let Some(goal) = input.new_goal.as_ref() {
            insert_new_goal(&transaction, goal)?;
        }
        if let Some(goal) = input.resumed_goal.as_ref() {
            let binding = input
                .goal_binding
                .as_ref()
                .ok_or_else(|| invalid_data("resumed Goal input has no Goal binding"))?;
            if goal.session_id != input.session_id
                || goal.goal_id != binding.goal_id
                || goal.generation != binding.generation
                || goal.turn != binding.turn
                || (input.origin == InputOrigin::Runtime
                    && (input.idempotency_key.is_some()
                        || input.generated_title.is_some()
                        || input.new_goal.is_some()))
            {
                return Err(invalid_data("resumed Goal does not match its input"));
            }
            apply_goal_resume(&transaction, goal)?;
        }
        transaction.execute("INSERT INTO inputs (priority_order, input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms, agent_variant, origin, goal_id, goal_generation, goal_turn, skill_activation_json, cross_session_binding_json) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)", params![priority_order, input.input_id.as_str(), input.session_id.as_str(), input.idempotency_key.as_ref().map(IdempotencyKey::as_str), input.message.id.as_str(), message_json, input.accepted_at_ms, agent_variant_value(input.agent_variant), input_origin_value(input.origin), input.goal_binding.as_ref().map(|binding| binding.goal_id.as_str()), input.goal_binding.as_ref().map(|binding| i64::try_from(binding.generation)).transpose().map_err(|source| internal_error("Goal input generation exceeds storage range", source))?, input.goal_binding.as_ref().map(|binding| i64::from(binding.turn)), skill_activation_json, cross_session_binding_json]).map_err(|source| database_write_error("input could not be accepted", source))?;
        let queue_order = u64::try_from(priority_order)
            .map_err(|source| internal_error("queue order exceeds storage range", source))?;
        transaction.execute("INSERT INTO runs (run_id, session_id, input_id, attempt, status, cancel_requested, approval_mode, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms) VALUES (?1, ?2, ?3, 1, 'accepted', 0, ?4, NULL, NULL, ?5, NULL, NULL)", params![input.run_id.as_str(), input.session_id.as_str(), input.input_id.as_str(), approval_mode_value(input.approval_mode), input.accepted_at_ms]).map_err(|source| database_write_error("run could not be accepted", source))?;
        if let Some(activation) = input.skill_activation.as_ref() {
            insert_skill_activation(&transaction, activation)?;
        }
        let changed = transaction
            .execute(
                "UPDATE sessions
             SET current_variant = ?1,
                 title = CASE
                     WHEN title_origin = 'generated' AND ?3 IS NOT NULL THEN ?3
                     ELSE title
                 END
             WHERE session_id = ?2 AND lifecycle = 'active'",
                params![
                    agent_variant_value(input.agent_variant),
                    input.session_id.as_str(),
                    input.generated_title.as_deref(),
                ],
            )
            .map_err(|source| {
                database_write_error("session variant could not be updated", source)
            })?;
        if changed != 1 {
            return Err(conflict("input session is not active"));
        }
        transaction.commit().map_err(|source| {
            database_write_error("input acceptance could not be committed", source)
        })?;
        let stored = StoredInput {
            queue_order,
            input_id: input.input_id.clone(),
            session_id: input.session_id.clone(),
            idempotency_key: input.idempotency_key,
            agent_variant: input.agent_variant,
            origin: input.origin,
            goal_binding: input.goal_binding,
            cross_session_binding: input.cross_session_binding,
            skill_activation: input.skill_activation,
            user_message_id: input.message.id.clone(),
            state: StoredInputState::Queued,
            queued_message: Some(input.message),
            accepted_at_ms: input.accepted_at_ms,
        };
        let run = StoredRun {
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
        Ok(AcceptedInput {
            input: stored,
            run,
            is_duplicate: false,
        })
    }

    pub(super) fn cancel_queued_input(
        &mut self,
        session_id: &SessionId,
        input_id: &InputId,
    ) -> StorageResult<()> {
        let changed = self
            .connection
            .execute(
                "DELETE FROM inputs
                 WHERE input_id = ?1 AND session_id = ?2 AND state = 'queued'
                   AND origin = 'user' AND goal_id IS NULL",
                params![input_id.as_str(), session_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("queued input could not be cancelled", source)
            })?;
        if changed != 1 {
            return Err(conflict("input is not queued"));
        }
        Ok(())
    }

    pub(super) fn load_inputs(&self) -> StorageResult<Vec<StoredInput>> {
        let mut statement = self.connection.prepare("SELECT COALESCE(priority_order, queue_order), input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms, agent_variant, origin, goal_id, goal_generation, goal_turn, skill_activation_json, cross_session_binding_json FROM inputs ORDER BY COALESCE(priority_order, queue_order), queue_order").map_err(|source| internal_error("runtime inputs could not be queried", source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                ))
            })
            .map_err(|source| internal_error("runtime inputs could not be read", source))?;
        rows.map(|row| {
            let (
                queue_order,
                input_id,
                session_id,
                key,
                message_id,
                state,
                message_json,
                accepted_at_ms,
                agent_variant,
                origin,
                goal_id,
                goal_generation,
                goal_turn,
                skill_activation_json,
                cross_session_binding_json,
            ) = row
                .map_err(|source| internal_error("runtime input row could not be read", source))?;
            let state = match state.as_str() {
                "queued" => StoredInputState::Queued,
                "committed" => StoredInputState::Committed,
                _ => return Err(invalid_data("stored input state is invalid")),
            };
            let queued_message = message_json
                .map(|json| {
                    serde_json::from_str(&json).map_err(|source| {
                        invalid_data_with_source("stored queued message is invalid", source)
                    })
                })
                .transpose()?;
            if (state == StoredInputState::Queued) != queued_message.is_some() {
                return Err(invalid_data("stored queued message state is inconsistent"));
            }
            let origin = parse_input_origin(&origin)?;
            let parsed_input_id = InputId::new(input_id)
                .map_err(|source| invalid_data_with_source("stored input id is invalid", source))?;
            let parsed_session_id = SessionId::new(session_id).map_err(|source| {
                invalid_data_with_source("stored input session id is invalid", source)
            })?;
            let parsed_message_id = MessageId::new(message_id).map_err(|source| {
                invalid_data_with_source("stored user message id is invalid", source)
            })?;
            let skill_activation: Option<StoredSkillActivation> = skill_activation_json
                .map(|json| {
                    serde_json::from_str(&json).map_err(|source| {
                        invalid_data_with_source("stored input skill activation is invalid", source)
                    })
                })
                .transpose()?;
            let cross_session_binding: Option<CrossSessionInputBinding> =
                cross_session_binding_json
                    .map(|json| {
                        serde_json::from_str(&json).map_err(|source| {
                            invalid_data_with_source(
                                "stored cross-session input binding is invalid",
                                source,
                            )
                        })
                    })
                    .transpose()?;
            validate_stored_input_activation(
                origin,
                &parsed_session_id,
                &parsed_input_id,
                &parsed_message_id,
                queued_message.as_ref(),
                skill_activation.as_ref(),
            )?;
            let goal_binding = match (goal_id, goal_generation, goal_turn) {
                (None, None, None) => None,
                (Some(goal_id), Some(generation), Some(turn)) => Some(GoalInputBinding {
                    goal_id: GoalId::new(goal_id).map_err(|source| {
                        invalid_data_with_source("stored Goal input id is invalid", source)
                    })?,
                    generation: u64::try_from(generation).map_err(|source| {
                        invalid_data_with_source("stored Goal input generation is invalid", source)
                    })?,
                    turn: u32::try_from(turn).map_err(|source| {
                        invalid_data_with_source("stored Goal input turn is invalid", source)
                    })?,
                }),
                _ => return Err(invalid_data("stored Goal input binding is incomplete")),
            };
            if let Some(message) = queued_message.as_ref() {
                validate_input_message(
                    origin,
                    goal_binding.as_ref(),
                    cross_session_binding.as_ref(),
                    message,
                )
                .map_err(|_| {
                    invalid_data("stored input message origin or Goal binding is invalid")
                })?;
            } else if origin == InputOrigin::Runtime
                && goal_binding.is_none()
                && cross_session_binding.is_none()
            {
                return Err(invalid_data("stored Runtime input has no binding"));
            }
            Ok(StoredInput {
                queue_order: u64::try_from(queue_order).map_err(|source| {
                    invalid_data_with_source("stored queue order is invalid", source)
                })?,
                input_id: parsed_input_id,
                session_id: parsed_session_id,
                idempotency_key: key.map(IdempotencyKey::new).transpose().map_err(|source| {
                    invalid_data_with_source("stored idempotency key is invalid", source)
                })?,
                agent_variant: parse_agent_variant(&agent_variant)?,
                origin,
                goal_binding,
                cross_session_binding,
                skill_activation,
                user_message_id: parsed_message_id,
                state,
                queued_message,
                accepted_at_ms,
            })
        })
        .collect()
    }
}

fn validate_new_input_activation(input: &NewStoredInput) -> StorageResult<()> {
    validate_stored_input_activation(
        input.origin,
        &input.session_id,
        &input.input_id,
        &input.message.id,
        Some(&input.message),
        input.skill_activation.as_ref(),
    )?;
    if input
        .skill_activation
        .as_ref()
        .is_some_and(|activation| activation.run_id.as_ref() != Some(&input.run_id))
    {
        return Err(invalid_data("input skill activation Run is invalid"));
    }
    Ok(())
}

fn validate_stored_input_activation(
    origin: InputOrigin,
    session_id: &SessionId,
    input_id: &InputId,
    message_id: &MessageId,
    message: Option<&agent_types::UserMessage>,
    activation: Option<&StoredSkillActivation>,
) -> StorageResult<()> {
    let skill_parts = message
        .into_iter()
        .flat_map(|message| &message.parts)
        .filter_map(|part| match part {
            agent_types::UserPart::InternalContext(part) if part.kind == "skill_activation" => {
                Some(part)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let Some(activation) = activation else {
        if !skill_parts.is_empty() {
            return Err(invalid_data(
                "input has an unbound skill activation context",
            ));
        }
        return Ok(());
    };
    let owner_matches = matches!(
        &activation.owner,
        SkillActivationOwner::Session(owner_session_id) if owner_session_id == session_id
    );
    if origin != InputOrigin::User
        || activation.trigger != SkillActivationTrigger::User
        || &activation.session_id != session_id
        || activation.input_id.as_ref() != Some(input_id)
        || &activation.message_id != message_id
        || !owner_matches
        || message.is_some()
            && (skill_parts.len() != 1
                || skill_parts[0].retention_key.as_deref()
                    != Some(&format!("skill:{}", activation.name.as_str())))
    {
        return Err(invalid_data("input skill activation is inconsistent"));
    }
    Ok(())
}

fn input_origin_value(origin: InputOrigin) -> &'static str {
    match origin {
        InputOrigin::User => "user",
        InputOrigin::Runtime => "runtime",
    }
}

pub(super) fn insert_goal_continuation(
    transaction: &Transaction<'_>,
    input: &NewStoredInput,
) -> StorageResult<AcceptedInput> {
    validate_input_message(
        input.origin,
        input.goal_binding.as_ref(),
        input.cross_session_binding.as_ref(),
        &input.message,
    )
    .map_err(|_| invalid_data("Goal continuation message is invalid"))?;
    if input.origin != InputOrigin::Runtime
        || input.goal_binding.is_none()
        || input.new_goal.is_some()
        || input.resumed_goal.is_some()
        || input.skill_activation.is_some()
        || input.idempotency_key.is_some()
        || input.generated_title.is_some()
    {
        return Err(invalid_data("Goal continuation input shape is invalid"));
    }
    let message_json = serde_json::to_string(&input.message).map_err(|source| {
        internal_error("Goal continuation message could not be encoded", source)
    })?;
    let priority_order = transaction
        .query_row(
            "SELECT COALESCE(MAX(COALESCE(priority_order, queue_order)), -1) + 1
             FROM inputs WHERE session_id = ?1",
            [input.session_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|source| internal_error("next Goal queue order could not be read", source))?;
    let binding = input.goal_binding.as_ref().expect("binding checked");
    transaction
        .execute(
            "INSERT INTO inputs (
                priority_order, input_id, session_id, idempotency_key, user_message_id,
                state, queued_message_json, accepted_at_ms, agent_variant, origin,
                goal_id, goal_generation, goal_turn
             ) VALUES (?1, ?2, ?3, NULL, ?4, 'queued', ?5, ?6, ?7, 'runtime', ?8, ?9, ?10)",
            params![
                priority_order,
                input.input_id.as_str(),
                input.session_id.as_str(),
                input.message.id.as_str(),
                message_json,
                input.accepted_at_ms,
                agent_variant_value(input.agent_variant),
                binding.goal_id.as_str(),
                i64::try_from(binding.generation).map_err(|source| internal_error(
                    "Goal continuation generation exceeds storage range",
                    source
                ))?,
                i64::from(binding.turn),
            ],
        )
        .map_err(|source| {
            database_write_error("Goal continuation could not be inserted", source)
        })?;
    transaction
        .execute(
            "INSERT INTO runs (
                run_id, session_id, input_id, attempt, status, cancel_requested,
                approval_mode, error_code, error_message, created_at_ms,
                started_at_ms, finished_at_ms
             ) VALUES (?1, ?2, ?3, 1, 'accepted', 0, ?4, NULL, NULL, ?5, NULL, NULL)",
            params![
                input.run_id.as_str(),
                input.session_id.as_str(),
                input.input_id.as_str(),
                approval_mode_value(input.approval_mode),
                input.accepted_at_ms,
            ],
        )
        .map_err(|source| {
            database_write_error("Goal continuation Run could not be inserted", source)
        })?;
    let queue_order = u64::try_from(priority_order)
        .map_err(|source| internal_error("Goal queue order exceeds storage range", source))?;
    let stored_input = StoredInput {
        queue_order,
        input_id: input.input_id.clone(),
        session_id: input.session_id.clone(),
        idempotency_key: None,
        agent_variant: input.agent_variant,
        origin: input.origin,
        goal_binding: input.goal_binding.clone(),
        cross_session_binding: None,
        skill_activation: None,
        user_message_id: input.message.id.clone(),
        state: StoredInputState::Queued,
        queued_message: Some(input.message.clone()),
        accepted_at_ms: input.accepted_at_ms,
    };
    let stored_run = StoredRun {
        run_id: input.run_id.clone(),
        session_id: input.session_id.clone(),
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
    Ok(AcceptedInput {
        input: stored_input,
        run: stored_run,
        is_duplicate: false,
    })
}

pub(super) fn insert_proxy_report(
    transaction: &Transaction<'_>,
    source_session_id: &SessionId,
    source_run_id: &assistant_protocol::RunId,
    source_status: RunStatus,
    input: &NewStoredInput,
) -> StorageResult<AcceptedInput> {
    validate_input_message(
        input.origin,
        input.goal_binding.as_ref(),
        input.cross_session_binding.as_ref(),
        &input.message,
    )
    .map_err(|_| invalid_data("proxy report message is invalid"))?;
    let Some(CrossSessionInputBinding::ProxyReport {
        source_session_id: bound_session_id,
        source_run_id: bound_run_id,
        source_goal_id,
        source_run_status,
    }) = input.cross_session_binding.as_ref()
    else {
        return Err(invalid_data("proxy report binding is missing"));
    };
    if input.origin != InputOrigin::Runtime
        || input.goal_binding.is_some()
        || input.skill_activation.is_some()
        || input.new_goal.is_some()
        || input.resumed_goal.is_some()
        || input.generated_title.is_some()
        || input.idempotency_key.is_none()
        || bound_session_id != source_session_id
        || bound_run_id != source_run_id
        || *source_run_status != source_status
        || input.accepted_at_ms <= 0
    {
        return Err(invalid_data("proxy report input shape is invalid"));
    }
    let source = transaction
        .query_row(
            "SELECT sessions.role, sessions.lifecycle,
                    sessions.proxy_controller_session_id, runs.status, inputs.goal_id,
                    EXISTS(SELECT 1 FROM inputs queued
                           WHERE queued.session_id = sessions.session_id
                             AND queued.state = 'queued'
                             AND queued.input_id != inputs.input_id)
             FROM sessions JOIN runs ON runs.session_id = sessions.session_id
             JOIN inputs ON inputs.input_id = runs.input_id
             WHERE sessions.session_id = ?1 AND runs.run_id = ?2",
            params![source_session_id.as_str(), source_run_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, bool>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|source| internal_error("proxy report source could not be queried", source))?;
    let target_valid = transaction
        .query_row(
            "SELECT role = 'controller' AND lifecycle = 'active'
             FROM sessions WHERE session_id = ?1",
            [input.session_id.as_str()],
            |row| row.get::<_, bool>(0),
        )
        .optional()
        .map_err(|source| internal_error("proxy report target could not be queried", source))?
        .unwrap_or(false);
    if !matches!(source, Some((role, lifecycle, proxy, status, goal_id, false))
        if role == "standard"
            && lifecycle == "active"
            && proxy.as_deref() == Some(input.session_id.as_str())
            && status == super::run_projection::run_status_value(source_status)
            && goal_id.as_deref() == source_goal_id.as_ref().map(GoalId::as_str))
        || !target_valid
    {
        return Err(conflict("proxy report is not currently accepted"));
    }

    let message_json = serde_json::to_string(&input.message)
        .map_err(|source| internal_error("proxy report message could not be encoded", source))?;
    let binding_json = serde_json::to_string(
        input
            .cross_session_binding
            .as_ref()
            .expect("proxy report binding checked"),
    )
    .map_err(|source| internal_error("proxy report binding could not be encoded", source))?;
    let priority_order = transaction
        .query_row(
            "SELECT COALESCE(MAX(COALESCE(priority_order, queue_order)), -1) + 1
             FROM inputs WHERE session_id = ?1",
            [input.session_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|source| {
            internal_error("next proxy report queue order could not be read", source)
        })?;
    transaction
        .execute(
            "INSERT INTO inputs (
                priority_order, input_id, session_id, idempotency_key, user_message_id,
                state, queued_message_json, accepted_at_ms, agent_variant, origin,
                cross_session_binding_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, ?8, 'runtime', ?9)",
            params![
                priority_order,
                input.input_id.as_str(),
                input.session_id.as_str(),
                input.idempotency_key.as_ref().map(IdempotencyKey::as_str),
                input.message.id.as_str(),
                message_json,
                input.accepted_at_ms,
                agent_variant_value(input.agent_variant),
                binding_json,
            ],
        )
        .map_err(|source| database_write_error("proxy report could not be inserted", source))?;
    transaction
        .execute(
            "INSERT INTO runs (
                run_id, session_id, input_id, attempt, status, cancel_requested,
                approval_mode, error_code, error_message, created_at_ms,
                started_at_ms, finished_at_ms
             ) VALUES (?1, ?2, ?3, 1, 'accepted', 0, ?4, NULL, NULL, ?5, NULL, NULL)",
            params![
                input.run_id.as_str(),
                input.session_id.as_str(),
                input.input_id.as_str(),
                approval_mode_value(input.approval_mode),
                input.accepted_at_ms,
            ],
        )
        .map_err(|source| database_write_error("proxy report Run could not be inserted", source))?;
    let queue_order = u64::try_from(priority_order).map_err(|source| {
        internal_error("proxy report queue order exceeds storage range", source)
    })?;
    Ok(AcceptedInput {
        input: StoredInput {
            queue_order,
            input_id: input.input_id.clone(),
            session_id: input.session_id.clone(),
            idempotency_key: input.idempotency_key.clone(),
            agent_variant: input.agent_variant,
            origin: input.origin,
            goal_binding: None,
            cross_session_binding: input.cross_session_binding.clone(),
            skill_activation: None,
            user_message_id: input.message.id.clone(),
            state: StoredInputState::Queued,
            queued_message: Some(input.message.clone()),
            accepted_at_ms: input.accepted_at_ms,
        },
        run: StoredRun {
            run_id: input.run_id.clone(),
            session_id: input.session_id.clone(),
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
        },
        is_duplicate: false,
    })
}

fn parse_input_origin(value: &str) -> StorageResult<InputOrigin> {
    match value {
        "user" => Ok(InputOrigin::User),
        "runtime" => Ok(InputOrigin::Runtime),
        _ => Err(invalid_data("stored input origin is invalid")),
    }
}
