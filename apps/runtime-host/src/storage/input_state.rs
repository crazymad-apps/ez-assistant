//! Input 准入、幂等命中、排队取消与恢复投影。
//!
//! 本模块负责把一次合法输入及其首个 Run 原子地写入 Runtime Store，并维护输入队列、
//! Goal 绑定、跨会话来源和技能激活等稳定事实。它不执行 Agent Run；执行调度由 Runtime
//! 在事务提交后根据这里返回的投影继续处理。
//!
//! 所有依赖当前会话、设备或跨会话代理状态的准入判断，都必须与对应写入共享同一事务，
//! 避免检查通过后权威状态已经发生变化。

use agent_types::MessageId;
use assistant_protocol::{
    GoalId, IdempotencyKey, InputId, McpServerKey, RunId, RunStatus, SessionCommand, SessionId,
};
use assistant_runtime::{
    AcceptedInput, AcceptedStoredSessionCommand, CrossSessionInputBinding,
    CrossSessionInputEnvelope, GoalHeldInputResume, GoalHeldInputResumeResult, GoalInputBinding,
    InputChannelSource, InputOrigin, NewStoredInput, NewStoredSessionCommand, ReplyRoute,
    SkillActivationOwner, SkillActivationTrigger, StoredInput, StoredInputState,
    StoredMcpSelection, StoredSessionCommand, StoredSessionCommandState, StoredSkillActivation,
    validate_input_message, validate_input_message_with_channel_source,
};
use rusqlite::Transaction;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, conflict, database_write_error,
    goal::{apply_goal_resume, insert_new_goal},
    internal_error, invalid_data, invalid_data_with_source,
    mode::{agent_variant_value, parse_agent_variant},
    skill::insert_skill_activation,
};

impl StorageEngine {
    /// 正常新接纳须同时避开本 owner 的可靠历史；幂等命中在调用前已返回。
    /// 队列／活动身份仍由原 SQL 约束保护，不扫描其他会话，也不复制文件事实。
    pub(super) fn ensure_new_input_identity(
        &mut self,
        session: &SessionId,
        input: &InputId,
        run: Option<&RunId>,
        message: &MessageId,
    ) -> StorageResult<()> {
        if self.allow_legacy_history {
            return Ok(());
        }
        // clear/reenter 保留的首次物化身份是防重绑墓碑；当前 generation 可能已没有原消息。
        // 这里只核对最小身份，不读取或恢复原 Input，更不能让新消息/命令占用这个 ID。
        let reserved: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id=?1 AND materialization_input_id=?2)",
            params![session.as_str(), input.as_str()], |row| row.get(0),
        ).map_err(|source| internal_error("original materialization identity could not be checked", source))?;
        if reserved {
            return Err(conflict(
                "new Input identity collides with original materialization identity",
            ));
        }
        let owner = assistant_protocol::ConversationOwner::MainSession {
            session_id: session.clone(),
        };
        self.ensure_no_pending_history(&owner)?;
        if self.unavailable_sessions.contains(session.as_str()) {
            return Err(invalid_data("input target history is unavailable"));
        }
        let generation = self.session_generation(session)?;
        let path = super::body_path(&self.session_directory(session)?, generation);
        if self
            .conversation_indexes
            .input_identity_conflicts(&path, &owner, generation, input, run, message)?
        {
            return Err(conflict(
                "new Input identity collides with retained owner history",
            ));
        }
        Ok(())
    }

    /// 幂等键只定位本 Session 的原接纳身份，不为一次重试解码其他队列载荷。
    fn load_input_for_key(
        &mut self,
        session: &SessionId,
        key: &IdempotencyKey,
    ) -> StorageResult<Option<StoredInput>> {
        let id: Option<String> = self.connection.query_row(
            "SELECT input_id FROM inputs WHERE session_id=?1 AND idempotency_key=?2 AND input_kind='message'",
            params![session.as_str(), key.as_str()], |row| row.get(0),
        ).optional().map_err(|source| internal_error("accepted Input identity could not be read", source))?;
        id.map(|id| {
            let id = InputId::new(id).map_err(|source| {
                invalid_data_with_source("accepted Input ID is invalid", source)
            })?;
            self.load_input_for_id(session, &id)?
                .ok_or_else(|| invalid_data("accepted Input is missing"))
        })
        .transpose()
    }

    /// 固定首次 attempt；SQL 只读取未完成控制，终态只能从当前 owner 的可靠文件取得。
    /// 此处仅返回只读幂等结果，不创建 Run/队列，不以终态 SQL 或猜测值补齐缺失历史。
    pub(super) fn existing_input_acceptance(
        &mut self,
        input: StoredInput,
    ) -> StorageResult<AcceptedInput> {
        let run_id: Option<String> = self.connection.query_row(
            "SELECT run_id FROM runs WHERE session_id=?1 AND input_id=?2 AND attempt=1 AND status IN ('accepted','running','cancelling')",
            params![input.session_id.as_str(),input.input_id.as_str()], |row| row.get(0),
        ).optional().map_err(|source| internal_error("accepted Run control could not be located",source))?;
        if let Some(run_id) = run_id {
            let id = RunId::new(run_id)
                .map_err(|source| invalid_data_with_source("accepted Run ID is invalid", source))?;
            let run = self
                .load_run_control(&input.session_id, &id)?
                .ok_or_else(|| invalid_data("accepted Run control is missing"))?;
            return Ok(AcceptedInput::Control {
                input: Box::new(input),
                run: Some(run),
                is_duplicate: true,
            });
        }
        if input.state != StoredInputState::Committed {
            return Ok(AcceptedInput::Control {
                input: Box::new(input),
                run: None,
                is_duplicate: true,
            });
        }
        let history = self
            .load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
                session_id: input.session_id.clone(),
            })?
            .facts;
        let fact = history
            .inputs
            .into_iter()
            .find(|fact| {
                fact.input_id == input.input_id
                    && !fact.inherited
                    && fact.user_message_id == input.user_message_id
            })
            .ok_or_else(|| invalid_data("accepted Input history is missing"))?;
        let run = history
            .runs
            .into_iter()
            .find(|run| {
                run.input_id.as_ref() == Some(&input.input_id)
                    && run.attempt == 1
                    && !run.inherited
                    && !run.partial
                    && run.status.is_some_and(|status| status.is_terminal())
            })
            .ok_or_else(|| invalid_data("accepted first Run history is missing"))?;
        Ok(AcceptedInput::Historical {
            session_id: input.session_id,
            input: fact,
            run,
        })
    }

    /// 将一条暂存的用户指导绑定到下一代 Goal，并恢复对应 Goal。
    ///
    /// Goal 代际推进、Input 绑定和排队消息更新必须原子完成；失败时仍保留原来的暂存状态。
    pub(super) fn resume_goal_with_held_input(
        &mut self,
        resume: GoalHeldInputResume,
    ) -> StorageResult<GoalHeldInputResumeResult> {
        let binding = GoalInputBinding {
            goal_id: resume.resumed_goal.goal_id.clone(),
            generation: resume.resumed_goal.generation,
            turn: resume.resumed_goal.turn,
            reply_route: None,
        };
        // 只接受目标一致、generation 恰好递增一代的恢复，防止过期调用跨代绑定 Input。
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
        // WHERE 条件同时确认这是未绑定 Goal 的排队用户输入，避免覆盖已被调度器消费的状态。
        let changed = transaction
            .execute(
                "UPDATE inputs
                 SET goal_id = ?1, goal_generation = ?2, goal_turn = ?3,
                     queued_message_json = ?4
                 WHERE input_id = ?5 AND session_id = ?6 AND state = 'queued'
                   AND origin = 'user' AND goal_id IS NULL AND goal_generation IS NULL
                   AND goal_turn IS NULL AND user_message_id = ?7
                   AND NOT EXISTS (SELECT 1 FROM runs WHERE runs.input_id = inputs.input_id)",
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
            .load_input_for_id(&resume.session_id, &resume.input_id)?
            .ok_or_else(|| invalid_data("resumed held Input is missing"))?;
        let AcceptedInput::Control { input, run, .. } = self.existing_input_acceptance(input)?
        else {
            return Err(invalid_data("resumed held Input has no execution control"));
        };
        if run.is_some() {
            return Err(invalid_data("resumed held Input Run is not accepted"));
        }
        Ok(GoalHeldInputResumeResult {
            goal: resume.resumed_goal,
            input: *input,
            run,
        })
    }

    /// 将一条未绑定 Goal 的排队用户输入提升到当前队列首位。
    pub(super) fn prioritize_queued_input(
        &mut self,
        change: assistant_runtime::QueuePriorityChange,
    ) -> StorageResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("queue priority update could not begin", source))?;
        // 先整体后移，再把目标写为 0，可在一次事务内保留其余输入之间的相对顺序。
        transaction
            .execute(
                "UPDATE inputs
                 SET priority_order = COALESCE(priority_order, queue_order) + 1
                 WHERE session_id = ?1 AND state = 'queued' AND goal_id IS NULL
                   AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.input_id=inputs.input_id)
                   AND ((input_kind = 'message' AND origin = 'user')
                        OR input_kind = 'command')",
                [change.session_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("queued input priorities could not be shifted", source)
            })?;
        let changed = transaction
            .execute(
                "UPDATE inputs SET priority_order = ?1
                 WHERE session_id = ?2 AND input_id = ?3 AND state = 'queued' AND goal_id IS NULL
                   AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.input_id=inputs.input_id)
                   AND ((input_kind = 'message' AND origin = 'user')
                        OR input_kind = 'command')",
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

    /// 校验并暂存新 Input 与相关选择；实际领取才创建首次 Run。
    ///
    /// 幂等键以 Session 为作用域；重复请求返回首次接纳的 Input，不产生第二次写入。
    pub(super) fn accept_input(&mut self, input: NewStoredInput) -> StorageResult<AcceptedInput> {
        // 幂等命中必须发生在任何插入之前，并固定返回首个 Run，保持调用方重试结果稳定。
        if let Some(key) = input.idempotency_key.as_ref()
            && let Some(existing) = self.load_input_for_key(&input.session_id, key)?
        {
            return self.existing_input_acceptance(existing);
        }
        self.ensure_new_input_identity(
            &input.session_id,
            &input.input_id,
            None,
            &input.message.id,
        )?;
        let queue_order = if input.feedback_answer.is_some() && !self.allow_legacy_history {
            // 预检只验证同一 SQL 效果并回滚；未提交正文不进入文件，文件只冻结回答事实。
            let preflight = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    internal_error("feedback input preflight could not begin", source)
                })?;
            apply_input_acceptance(&preflight, &input, false)?;
            preflight.rollback().map_err(|source| {
                internal_error("feedback input preflight could not roll back", source)
            })?;
            let operation_id = format!("feedback-input:{}", input.input_id.as_str());
            self.stage_target_append(
                operation_id.clone(),
                super::append_effect::ConversationStorageTarget::SessionMetadata {
                    session_id: input.session_id.clone(),
                },
                Vec::new(),
                None,
                input.accepted_at_ms,
                super::append_effect::AppendPurpose::FeedbackInput {
                    input: Box::new(input.clone()),
                },
            )?;
            self.complete_staged_append(&operation_id)?;
            self.load_input_for_id(&input.session_id, &input.input_id)?
                .ok_or_else(|| invalid_data("accepted feedback Input is missing"))?
                .queue_order
        } else {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| internal_error("input acceptance could not begin", source))?;
            let queue_order =
                apply_input_acceptance(&transaction, &input, self.allow_legacy_history)?;
            transaction.commit().map_err(|source| {
                database_write_error("input acceptance could not be committed", source)
            })?;
            queue_order
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
            skill_activation: input.skill_activation,
            user_message_id: input.message.id.clone(),
            state: StoredInputState::Queued,
            queued_message: Some(input.message),
            accepted_at_ms: input.accepted_at_ms,
        };
        Ok(AcceptedInput::Control {
            input: Box::new(stored),
            run: None,
            is_duplicate: false,
        })
    }

    /// 取消仍可由用户控制的排队 Input。
    ///
    /// 已绑定 Goal、跨会话投递和带渠道来源的 Runtime Input 不允许通过该入口删除。
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
                   AND input_kind = 'message' AND goal_id IS NULL
                   AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.input_id=inputs.input_id)
                   AND (origin = 'user' OR (
                       origin = 'runtime'
                       AND cross_session_json IS NULL
                       AND channel_source_json IS NULL
                   ))",
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

    pub(super) fn accept_session_command(
        &mut self,
        command: NewStoredSessionCommand,
    ) -> StorageResult<AcceptedStoredSessionCommand> {
        if let Some(key) = command.idempotency_key.as_ref() {
            let identity: Option<(String, String)> = self.connection.query_row(
                "SELECT input_kind,input_id FROM inputs WHERE session_id=?1 AND idempotency_key=?2",
                params![command.session_id.as_str(), key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional().map_err(|source| internal_error("input idempotency identity could not be read", source))?;
            if let Some((kind, raw)) = identity {
                if kind == "message" {
                    return Err(conflict(
                        "session command idempotency key belongs to a message input",
                    ));
                }
                if kind != "command" {
                    return Err(invalid_data("stored input kind is invalid"));
                }
                let id = InputId::new(raw).map_err(|source| {
                    invalid_data_with_source("command Input ID is invalid", source)
                })?;
                let existing = self
                    .load_session_command(&command.session_id, &id)?
                    .ok_or_else(|| invalid_data("accepted command identity is missing"))?;
                if existing.command != command.command {
                    return Err(conflict(
                        "session command idempotency key was reused with different content",
                    ));
                }
                return Ok(AcceptedStoredSessionCommand {
                    command: existing,
                    is_duplicate: true,
                });
            }
        }

        self.ensure_new_input_identity(
            &command.session_id,
            &command.input_id,
            None,
            &command.user_message_id,
        )?;
        let command_json = serde_json::to_string(&command.command)
            .map_err(|source| internal_error("session command could not be encoded", source))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("session command acceptance could not begin", source)
            })?;
        let target_valid = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions
                 WHERE session_id = ?1 AND lifecycle = 'active')",
                [command.session_id.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|source| {
                internal_error("session command target could not be queried", source)
            })?;
        if !target_valid {
            return Err(conflict("session command target is not active"));
        }
        let priority_order = transaction
            .query_row(
                "SELECT COALESCE(MAX(COALESCE(priority_order, queue_order)), -1) + 1
                 FROM inputs WHERE session_id = ?1",
                [command.session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| internal_error("next command priority could not be read", source))?;
        transaction
            .execute(
                "INSERT INTO inputs (
                    priority_order, input_id, session_id, idempotency_key, user_message_id,
                    state, accepted_at_ms, agent_variant, origin, input_kind, command_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, 'runtime', 'command', ?8)",
                params![
                    priority_order,
                    command.input_id.as_str(),
                    command.session_id.as_str(),
                    command.idempotency_key.as_ref().map(IdempotencyKey::as_str),
                    command.user_message_id.as_str(),
                    command.accepted_at_ms,
                    agent_variant_value(command.agent_variant),
                    command_json,
                ],
            )
            .map_err(|source| {
                database_write_error("session command could not be accepted", source)
            })?;
        transaction
            .execute(
                "UPDATE sessions SET updated_at_ms = ?1 WHERE session_id = ?2",
                params![command.accepted_at_ms, command.session_id.as_str()],
            )
            .map_err(|source| {
                database_write_error(
                    "session command acceptance time could not be stored",
                    source,
                )
            })?;
        transaction.commit().map_err(|source| {
            database_write_error("session command acceptance could not be committed", source)
        })?;
        Ok(AcceptedStoredSessionCommand {
            command: StoredSessionCommand {
                queue_order: u64::try_from(priority_order).map_err(|source| {
                    internal_error("session command queue order exceeds range", source)
                })?,
                input_id: command.input_id,
                session_id: command.session_id,
                idempotency_key: command.idempotency_key,
                user_message_id: command.user_message_id,
                agent_variant: command.agent_variant,
                command: command.command,
                result: None,
                state: StoredSessionCommandState::Queued,
                accepted_at_ms: command.accepted_at_ms,
            },
            is_duplicate: false,
        })
    }

    pub(super) fn load_session_commands(&self) -> StorageResult<Vec<StoredSessionCommand>> {
        self.query_session_commands(None, None)
    }

    pub(super) fn load_session_commands_scoped(
        &self,
        session_id: &SessionId,
    ) -> StorageResult<Vec<StoredSessionCommand>> {
        self.query_session_commands(Some(session_id), None)
    }

    /// 只定位原接纳身份；未完成命令读取 SQL，完成值回读当前 owner 文件，不据此恢复队列。
    pub(super) fn load_session_command(
        &mut self,
        session_id: &SessionId,
        input_id: &InputId,
    ) -> StorageResult<Option<StoredSessionCommand>> {
        let identity: Option<(String, Option<String>)> = self.connection.query_row(
            "SELECT state,idempotency_key FROM inputs WHERE session_id=?1 AND input_id=?2 AND input_kind='command'",
            params![session_id.as_str(), input_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(|source| internal_error("command identity could not be read", source))?;
        let Some((state, key)) = identity else {
            return Ok(None);
        };
        if self.allow_legacy_history || state == "queued" {
            return Ok(self
                .query_session_commands(Some(session_id), Some(input_id))?
                .into_iter()
                .next());
        }
        if state != "committed" {
            return Err(invalid_data("command identity has invalid state"));
        }
        let facts = self
            .load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
                session_id: session_id.clone(),
            })?
            .facts;
        let command = facts
            .commands
            .into_iter()
            .find(|command| &command.input_id == input_id)
            .ok_or_else(|| invalid_data("committed command file history is missing"))?;
        if command.state != StoredSessionCommandState::Committed
            || &command.session_id != session_id
            || command.idempotency_key.as_ref().map(IdempotencyKey::as_str) != key.as_deref()
            || !command
                .result
                .as_ref()
                .is_some_and(|result| result.matches_command(&command.command))
        {
            return Err(invalid_data(
                "command file history differs from accepted identity",
            ));
        }
        Ok(Some(command))
    }

    // 全量恢复与精确幂等查询共享行解码/Run 归属校验，但在线请求只读取目标唯一键。
    fn query_session_commands(
        &self,
        session_id: Option<&SessionId>,
        input_id: Option<&InputId>,
    ) -> StorageResult<Vec<StoredSessionCommand>> {
        let predicate = if input_id.is_some() {
            "AND session_id = ?1 AND input_id = ?2"
        } else if session_id.is_some() {
            "AND session_id = ?1"
        } else {
            ""
        };
        let control_only = if self.allow_legacy_history {
            ""
        } else {
            "AND state='queued'"
        };
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT COALESCE(priority_order, queue_order), input_id, session_id,
                        idempotency_key, user_message_id, agent_variant, command_json,
                        command_result_json, state, accepted_at_ms
                 FROM inputs WHERE input_kind = 'command' {predicate} {control_only}
                 ORDER BY COALESCE(priority_order, queue_order), queue_order"
            ))
            .map_err(|source| internal_error("session commands could not be queried", source))?;
        let rows = statement
            .query_map(
                rusqlite::params_from_iter(
                    session_id
                        .map(SessionId::as_str)
                        .into_iter()
                        .chain(input_id.map(InputId::as_str)),
                ),
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, i64>(9)?,
                    ))
                },
            )
            .map_err(|source| internal_error("session commands could not be read", source))?;
        rows.map(|row| {
            let (
                queue_order,
                input_id,
                session_id,
                idempotency_key,
                user_message_id,
                agent_variant,
                command_json,
                result_json,
                state,
                accepted_at_ms,
            ) = row.map_err(|source| {
                internal_error("session command row could not be read", source)
            })?;
            let state = match state.as_str() {
                "queued" if result_json.is_none() => StoredSessionCommandState::Queued,
                "committed" if result_json.is_some() => StoredSessionCommandState::Committed,
                _ => return Err(invalid_data("stored session command state is invalid")),
            };
            let command =
                serde_json::from_str::<SessionCommand>(&command_json).map_err(|source| {
                    invalid_data_with_source("stored session command is invalid", source)
                })?;
            let result = result_json
                .map(|value| {
                    serde_json::from_str(&value).map_err(|source| {
                        invalid_data_with_source("stored session command result is invalid", source)
                    })
                })
                .transpose()?;
            let parsed_input_id = InputId::new(input_id).map_err(|source| {
                invalid_data_with_source("stored session command input id is invalid", source)
            })?;
            let run_count = self
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM runs WHERE input_id = ?1",
                    [parsed_input_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|source| {
                    internal_error("session command run count could not be read", source)
                })?;
            if run_count != 0 {
                return Err(invalid_data(
                    "stored session command unexpectedly owns a Run",
                ));
            }
            Ok(StoredSessionCommand {
                queue_order: u64::try_from(queue_order).map_err(|source| {
                    invalid_data_with_source("stored command queue order is invalid", source)
                })?,
                input_id: parsed_input_id,
                session_id: SessionId::new(session_id).map_err(|source| {
                    invalid_data_with_source("stored command session id is invalid", source)
                })?,
                idempotency_key: idempotency_key
                    .map(IdempotencyKey::new)
                    .transpose()
                    .map_err(|source| {
                        invalid_data_with_source(
                            "stored command idempotency key is invalid",
                            source,
                        )
                    })?,
                user_message_id: MessageId::new(user_message_id).map_err(|source| {
                    invalid_data_with_source("stored command message id is invalid", source)
                })?,
                agent_variant: parse_agent_variant(&agent_variant)?,
                command,
                result,
                state,
                accepted_at_ms,
            })
        })
        .collect()
    }

    /// 从稳定存储重建 Input 队列投影，并严格校验组合字段是否自洽。
    pub(super) fn load_inputs(&mut self) -> StorageResult<Vec<StoredInput>> {
        self.load_inputs_scoped(None)
    }

    pub(super) fn load_inputs_scoped(
        &mut self,
        session_id: Option<&assistant_protocol::SessionId>,
    ) -> StorageResult<Vec<StoredInput>> {
        self.load_inputs_filtered(session_id, None)
    }

    /// 恢复一个已核对的待处理 Input；不得为处理它解码其他会话或整条队列的正文。
    pub(super) fn load_input_for_id(
        &mut self,
        session_id: &assistant_protocol::SessionId,
        input_id: &InputId,
    ) -> StorageResult<Option<StoredInput>> {
        Ok(self
            .load_inputs_filtered(Some(session_id), Some(input_id))?
            .pop())
    }

    fn load_inputs_filtered(
        &mut self,
        session_id: Option<&assistant_protocol::SessionId>,
        input_id: Option<&InputId>,
    ) -> StorageResult<Vec<StoredInput>> {
        let predicate = if session_id.is_some() {
            "session_id = ?1"
        } else {
            "?1 IS NULL"
        };
        let mut statement = self.connection.prepare(&format!("SELECT COALESCE(priority_order, queue_order), input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms, agent_variant, origin, goal_id, goal_generation, goal_turn, goal_reply_route_json, skill_activation_json, cross_session_json, channel_source_json, agent_shell_target, EXISTS(SELECT 1 FROM runs r WHERE r.input_id=inputs.input_id AND r.session_id=inputs.session_id AND r.status IN ('accepted','running','cancelling')) FROM inputs WHERE input_kind = 'message' AND {predicate} AND (?2 IS NULL OR input_id=?2) ORDER BY COALESCE(priority_order, queue_order), queue_order")).map_err(|source| internal_error("runtime inputs could not be queried", source))?;
        let rows = statement
            .query_map(
                params![
                    session_id.map(assistant_protocol::SessionId::as_str),
                    input_id.map(InputId::as_str)
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<i64>>(11)?,
                        row.get::<_, Option<i64>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, Option<String>>(14)?,
                        row.get::<_, Option<String>>(15)?,
                        row.get::<_, Option<String>>(16)?,
                        row.get::<_, Option<String>>(17)?,
                        row.get::<_, bool>(18)?,
                    ))
                },
            )
            .map_err(|source| internal_error("runtime inputs could not be read", source))?;
        let rows = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| internal_error("runtime input rows could not be read", source))?;
        drop(statement);
        let input_id_filter_is_unscoped = input_id.is_none();
        let mut inputs = Vec::with_capacity(rows.len());
        for row in rows {
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
                goal_reply_route_json,
                skill_activation_json,
                cross_session_json,
                channel_source_json,
                agent_shell_target,
                has_control,
            ) = row;
            if !self.allow_legacy_history && state == "committed" && !has_control {
                if input_id.is_empty() {
                    return Err(invalid_data("stored Input identity is empty"));
                }
                if self.unavailable_sessions.contains(&session_id) && input_id_filter_is_unscoped {
                    continue;
                }
                let session = SessionId::new(session_id).map_err(|source| {
                    invalid_data_with_source("stored Input Session ID is invalid", source)
                })?;
                let id = InputId::new(input_id).map_err(|source| {
                    invalid_data_with_source("stored Input ID is invalid", source)
                })?;
                let fact = self.committed_input_history(&session, &id)?;
                if fact.inherited
                    || fact.queue_order
                        != u64::try_from(queue_order).map_err(|source| {
                            invalid_data_with_source("stored Input order is invalid", source)
                        })?
                {
                    return Err(invalid_data("Input identity does not match local history"));
                }
                inputs.push(StoredInput {
                    input_id: id,
                    session_id: session,
                    idempotency_key: key.map(IdempotencyKey::new).transpose().map_err(
                        |source| {
                            invalid_data_with_source("stored idempotency key is invalid", source)
                        },
                    )?,
                    queue_order: fact.queue_order,
                    agent_variant: fact.agent_variant,
                    origin: fact.origin,
                    goal_binding: fact.goal_binding,
                    cross_session: fact.cross_session,
                    channel_source: fact.channel_source,
                    agent_shell_target: fact.agent_shell_target,
                    skill_activation: None,
                    user_message_id: fact.user_message_id,
                    state: StoredInputState::Committed,
                    queued_message: None,
                    accepted_at_ms: fact.accepted_at_ms,
                });
                continue;
            }
            let message_id = message_id
                .ok_or_else(|| invalid_data("queued Input message identity is missing"))?;
            let accepted_at_ms =
                accepted_at_ms.ok_or_else(|| invalid_data("Input acceptance time is missing"))?;
            let agent_variant =
                agent_variant.ok_or_else(|| invalid_data("Input variant is missing"))?;
            let origin = origin.ok_or_else(|| invalid_data("Input origin is missing"))?;
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
            // 排队 Input 必须保留待消费消息；提交后的 Input 必须已经释放该临时载荷。
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
            let skill_activation_json = skill_activation_json
                .filter(|_| self.allow_legacy_history || state == StoredInputState::Queued);
            let skill_activation: Option<StoredSkillActivation> = skill_activation_json
                .map(|json| {
                    serde_json::from_str(&json).map_err(|source| {
                        invalid_data_with_source("stored input skill activation is invalid", source)
                    })
                })
                .transpose()?;
            let cross_session: Option<CrossSessionInputEnvelope> = cross_session_json
                .map(|json| {
                    serde_json::from_str(&json).map_err(|source| {
                        invalid_data_with_source(
                            "stored cross-session input binding is invalid",
                            source,
                        )
                    })
                })
                .transpose()?;
            // 旧版本的用户输入没有 channel_source，按原有 Desktop 文本语义恢复以兼容存量数据。
            let channel_source = match channel_source_json {
                Some(json) => Some(serde_json::from_str::<InputChannelSource>(&json).map_err(
                    |source| {
                        invalid_data_with_source("stored input channel source is invalid", source)
                    },
                )?),
                None if origin == InputOrigin::User => Some(InputChannelSource::Desktop(
                    assistant_runtime::DesktopInputSource {
                        modality: assistant_runtime::InputModality::Text,
                        requested_output: assistant_runtime::OutputPreference::Text,
                    },
                )),
                None => None,
            };
            let goal_reply_route = goal_reply_route_json
                .map(|json| {
                    serde_json::from_str::<ReplyRoute>(&json).map_err(|source| {
                        invalid_data_with_source("stored Goal reply route is invalid", source)
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
            let goal_binding = match (goal_id, goal_generation, goal_turn, goal_reply_route) {
                (None, None, None, None) => None,
                (Some(goal_id), Some(generation), Some(turn), reply_route) => {
                    Some(GoalInputBinding {
                        goal_id: GoalId::new(goal_id).map_err(|source| {
                            invalid_data_with_source("stored Goal input id is invalid", source)
                        })?,
                        generation: u64::try_from(generation).map_err(|source| {
                            invalid_data_with_source(
                                "stored Goal input generation is invalid",
                                source,
                            )
                        })?,
                        turn: u32::try_from(turn).map_err(|source| {
                            invalid_data_with_source("stored Goal input turn is invalid", source)
                        })?,
                        reply_route,
                    })
                }
                _ => return Err(invalid_data("stored Goal input binding is incomplete")),
            };
            if let Some(message) = queued_message.as_ref() {
                validate_input_message_with_channel_source(
                    origin,
                    goal_binding.as_ref(),
                    cross_session.as_ref(),
                    channel_source.as_ref(),
                    message,
                )
                .map_err(|_| {
                    invalid_data("stored input message origin or Goal binding is invalid")
                })?;
            } else if origin == InputOrigin::Runtime
                && goal_binding.is_none()
                && cross_session.is_none()
            {
                return Err(invalid_data("stored Runtime input has no binding"));
            }
            inputs.push(StoredInput {
                agent_shell_target: super::mode::parse_shell_kind(agent_shell_target)?,
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
                cross_session,
                channel_source,
                skill_activation,
                user_message_id: parsed_message_id,
                state,
                queued_message,
                accepted_at_ms,
            });
        }
        Ok(inputs)
    }

    /// 幂等身份只定位一条本地 Input；事实按现有位置索引读取，不解码其他历史或排队载荷。
    pub(super) fn committed_input_history(
        &mut self,
        session: &SessionId,
        input_id: &InputId,
    ) -> StorageResult<assistant_runtime::StoredInputHistory> {
        let owner = assistant_protocol::ConversationOwner::MainSession {
            session_id: session.clone(),
        };
        self.ensure_no_pending_history(&owner)?;
        if self.unavailable_sessions.contains(session.as_str()) {
            return Err(invalid_data("committed Input history is unavailable"));
        }
        let generation = self.session_generation(session)?;
        let path = super::body_path(&self.session_directory(session)?, generation);
        self.conversation_indexes
            .read_input_history(&path, &owner, generation, input_id)?
            .ok_or_else(|| invalid_data("committed Input history is missing"))
    }

    /// 正常库只读待消费 MCP 选择；已消费的标签由所属 owner 文件装配。
    pub(super) fn load_mcp_input_selections(&self) -> StorageResult<Vec<StoredMcpSelection>> {
        self.load_mcp_input_selections_scoped(None)
    }

    pub(super) fn load_mcp_input_selections_scoped(
        &self,
        session_id: Option<&assistant_protocol::SessionId>,
    ) -> StorageResult<Vec<StoredMcpSelection>> {
        let predicate = if session_id.is_some() {
            "session_id = ?1"
        } else {
            "?1 IS NULL"
        };
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT selection_id, session_id, input_id, message_id, server_key,
                        display_name, created_at_ms
                 FROM mcp_input_selections
                 WHERE ({predicate}) AND (?2 OR EXISTS(
                    SELECT 1 FROM inputs i WHERE i.input_id=mcp_input_selections.input_id
                    AND i.session_id=mcp_input_selections.session_id AND i.state='queued'))
                 ORDER BY session_id, created_at_ms, selection_id"
            ))
            .map_err(|source| {
                internal_error("MCP input selections could not be queried", source)
            })?;
        let rows = statement
            .query_map(
                params![
                    session_id.map(assistant_protocol::SessionId::as_str),
                    self.allow_legacy_history
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .map_err(|source| internal_error("MCP input selections could not be read", source))?;
        rows.map(|row| {
            let (
                selection_id,
                session_id,
                input_id,
                message_id,
                server_key,
                display_name,
                created_at_ms,
            ) = row.map_err(|source| {
                internal_error("MCP input selection row could not be read", source)
            })?;
            if selection_id.trim().is_empty()
                || display_name.trim().is_empty()
                || display_name.len() > 128
            {
                return Err(invalid_data("stored MCP input selection is invalid"));
            }
            Ok(StoredMcpSelection {
                selection_id,
                session_id: SessionId::new(session_id).map_err(|source| {
                    invalid_data_with_source("stored MCP selection session id is invalid", source)
                })?,
                input_id: input_id.map(InputId::new).transpose().map_err(|source| {
                    invalid_data_with_source("stored MCP selection input id is invalid", source)
                })?,
                message_id: MessageId::new(message_id).map_err(|source| {
                    invalid_data_with_source("stored MCP selection message id is invalid", source)
                })?,
                server_key: McpServerKey::new(server_key).map_err(|source| {
                    invalid_data_with_source("stored MCP selection server key is invalid", source)
                })?,
                display_name,
                created_at_ms,
            })
        })
        .collect()
    }
}

fn validate_new_input_mcp_selection(input: &NewStoredInput) -> StorageResult<()> {
    let Some(selection) = input.mcp_selection.as_ref() else {
        return Ok(());
    };
    if input.origin != InputOrigin::User
        || selection.session_id != input.session_id
        || selection.input_id.as_ref() != Some(&input.input_id)
        || selection.message_id != input.message.id
        || selection.display_name.trim().is_empty()
        || selection.display_name.len() > 128
    {
        return Err(invalid_data("MCP input selection is inconsistent"));
    }
    Ok(())
}

/// 校验新 Input 携带的技能激活是否属于本次创建的 Run。
pub(super) fn validate_new_input_activation(input: &NewStoredInput) -> StorageResult<()> {
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
        .is_some_and(|activation| activation.run_id.is_some())
    {
        return Err(invalid_data("input skill activation Run is invalid"));
    }
    Ok(())
}

/// 校验技能激活稳定记录与其 Session、Input、Message 及内部上下文标记一一对应。
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
        // 消息不能仅凭一个内部上下文片段伪造未落库、无所有者的技能激活。
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
        || message.is_some() && skill_parts.len() != 1
    {
        return Err(invalid_data("input skill activation is inconsistent"));
    }
    Ok(())
}

pub(super) fn input_origin_value(origin: InputOrigin) -> &'static str {
    match origin {
        InputOrigin::User => "user",
        InputOrigin::Runtime => "runtime",
    }
}

/// 在来源 Run 的结算事务中向 Controller 插入代理报告及其首个 Run。
///
/// 该函数接收调用方已有事务，以保证来源状态校验、来源 Run 结算和报告入队不会产生中间态。
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
        input.cross_session.as_ref(),
        &input.message,
    )
    .map_err(|_| invalid_data("proxy report message is invalid"))?;
    let Some(CrossSessionInputBinding::ProxyReport {
        source_session_id: bound_session_id,
        source_run_id: bound_run_id,
        source_goal_id,
        source_run_status,
        ..
    }) = input
        .cross_session
        .as_ref()
        .map(|envelope| &envelope.binding)
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
    // 报告只属于仍由目标 Controller 托管、且没有其他排队工作的来源会话。
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
            .cross_session
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
                cross_session_json
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
    let queue_order = u64::try_from(priority_order).map_err(|source| {
        internal_error("proxy report queue order exceeds storage range", source)
    })?;
    Ok(AcceptedInput::Control {
        input: Box::new(StoredInput {
            agent_shell_target: None,
            queue_order,
            input_id: input.input_id.clone(),
            session_id: input.session_id.clone(),
            idempotency_key: input.idempotency_key.clone(),
            agent_variant: input.agent_variant,
            origin: input.origin,
            goal_binding: None,
            cross_session: input.cross_session.clone(),
            channel_source: None,
            skill_activation: None,
            user_message_id: input.message.id.clone(),
            state: StoredInputState::Queued,
            queued_message: Some(input.message.clone()),
            accepted_at_ms: input.accepted_at_ms,
        }),
        run: None,
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

/// 调用方持有唯一 Store worker，所有来源授权、Goal CAS、队列顺序与 Input 写入
/// 共用传入事务；此函数不提交、不发布事件、不写文件，也不执行模型或工具。
pub(super) fn apply_input_acceptance(
    transaction: &Transaction<'_>,
    input: &NewStoredInput,
    legacy_feedback: bool,
) -> StorageResult<u64> {
    validate_input_message_with_channel_source(
        input.origin,
        input.goal_binding.as_ref(),
        input.cross_session.as_ref(),
        input.channel_source.as_ref(),
        &input.message,
    )
    .map_err(|_| invalid_data("input message origin or Goal binding is invalid"))?;
    validate_new_input_activation(input)?;
    validate_new_input_mcp_selection(input)?;
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
        .map_err(|source| internal_error("input skill activation could not be encoded", source))?;
    let cross_session_json = input
        .cross_session
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|source| {
            internal_error("cross-session input binding could not be encoded", source)
        })?;
    let channel_source_json = input
        .channel_source
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|source| internal_error("input channel source could not be encoded", source))?;
    let goal_reply_route_json = input
        .goal_binding
        .as_ref()
        .and_then(|binding| binding.reply_route.as_ref())
        .map(serde_json::to_string)
        .transpose()
        .map_err(|source| internal_error("Goal reply route could not be encoded", source))?;
    if let Some(request) = &input.feedback_answer {
        if request.session_id != input.session_id
            || request.mode != assistant_protocol::FeedbackMode::EndTurn
            || request.status != assistant_protocol::FeedbackStatus::Answered
        {
            return Err(conflict("feedback input ownership mismatch"));
        }
        super::feedback::apply_feedback_mutation(
            transaction,
            &assistant_runtime::FeedbackMutation {
                request: request.clone(),
                expected_status: Some(assistant_protocol::FeedbackStatus::Pending),
            },
            legacy_feedback,
        )?;
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
            // Controller 投递只在来源 Run 仍执行、目标仍受其代理且目标队列为空时接纳。
            let starts_goal = input.new_goal.is_some();
            if input.origin != InputOrigin::Runtime
                || input.goal_binding.is_some() != starts_goal
                || input.skill_activation.is_some()
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
            // 报告必须和来源 Run 结算共用事务，不能从普通输入入口独立插入。
            return Err(invalid_data(
                "proxy reports must be accepted through run settlement",
            ));
        }
        None if input.origin == InputOrigin::User => {
            // 设备身份与目标会话状态属于准入条件，必须在接纳事务中重新读取权威数据。
            if let Some(InputChannelSource::Device(source)) = input.channel_source.as_ref() {
                let target_is_active = transaction
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sessions
                             WHERE session_id = ?1 AND lifecycle = 'active')",
                        [input.session_id.as_str()],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(|source| {
                        internal_error("device input target could not be queried", source)
                    })?;
                let device_is_paired = transaction
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM devices
                             WHERE device_id = ?1 AND lifecycle = 'paired')",
                        [source.device_id.as_str()],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(|source| {
                        internal_error("device input source could not be queried", source)
                    })?;
                if !target_is_active || !device_is_paired {
                    return Err(conflict("device input source is not authorized"));
                }
            }
            // 用户直接输入意味着接管标准会话：解除代理，并丢弃尚未消费的 Controller 投递。
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
                           AND json_extract(cross_session_json, '$.binding.kind') = 'controller_delivery'",
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
    // 无 Goal、跨会话和渠道来源的 Runtime Input 当前用于轮内播报提醒，需要优先执行。
    let priority_order = if input.feedback_answer.is_some()
        || (input.origin == InputOrigin::Runtime
            && input.goal_binding.is_none()
            && input.cross_session.is_none()
            && input.channel_source.is_none())
    {
        transaction
            .execute(
                "UPDATE inputs
                     SET priority_order = COALESCE(priority_order, queue_order) + 1
                     WHERE session_id = ?1 AND state = 'queued'",
                [input.session_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("speech reminder priority could not be reserved", source)
            })?;
        0
    } else {
        transaction
            .query_row(
                "SELECT COALESCE(MAX(COALESCE(priority_order, queue_order)), -1) + 1
                     FROM inputs WHERE session_id = ?1",
                [input.session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| internal_error("next queue priority could not be read", source))?
    };
    // Goal 创建/恢复、Input、首个 Run、技能激活和 Session 投影共同构成一次接纳提交。
    if let Some(goal) = input.new_goal.as_ref() {
        insert_new_goal(transaction, goal)?;
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
        apply_goal_resume(transaction, goal)?;
    }
    transaction.execute("INSERT INTO inputs (priority_order, input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms, agent_variant, origin, goal_id, goal_generation, goal_turn, goal_reply_route_json, skill_activation_json, cross_session_json, channel_source_json, agent_shell_target) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)", params![priority_order, input.input_id.as_str(), input.session_id.as_str(), input.idempotency_key.as_ref().map(IdempotencyKey::as_str), input.message.id.as_str(), message_json, input.accepted_at_ms, agent_variant_value(input.agent_variant), input_origin_value(input.origin), input.goal_binding.as_ref().map(|binding| binding.goal_id.as_str()), input.goal_binding.as_ref().map(|binding| i64::try_from(binding.generation)).transpose().map_err(|source| internal_error("Goal input generation exceeds storage range", source))?, input.goal_binding.as_ref().map(|binding| i64::from(binding.turn)), goal_reply_route_json, skill_activation_json, cross_session_json, channel_source_json, input.agent_shell_target.map(super::mode::shell_kind_value)]).map_err(|source| database_write_error("input could not be accepted", source))?;
    if let Some(selection) = input.mcp_selection.as_ref() {
        transaction
            .execute(
                "INSERT INTO mcp_input_selections (
                         selection_id, session_id, input_id, message_id,
                         server_key, display_name, created_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    selection.selection_id.as_str(),
                    selection.session_id.as_str(),
                    selection.input_id.as_ref().map(InputId::as_str),
                    selection.message_id.as_str(),
                    selection.server_key.as_str(),
                    selection.display_name.as_str(),
                    selection.created_at_ms,
                ],
            )
            .map_err(|source| {
                database_write_error("MCP input selection could not be accepted", source)
            })?;
    }
    let queue_order = u64::try_from(priority_order)
        .map_err(|source| internal_error("queue order exceeds storage range", source))?;
    if let Some(activation) = input.skill_activation.as_ref() {
        insert_skill_activation(transaction, activation)?;
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
        .map_err(|source| database_write_error("session variant could not be updated", source))?;
    if changed != 1 {
        return Err(conflict("input session is not active"));
    }
    Ok(queue_order)
}
