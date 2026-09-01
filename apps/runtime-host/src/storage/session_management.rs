//! Session 归档、模型切换与历史重新输入的业务原子操作。

use std::{collections::BTreeSet, fs};

use assistant_protocol::{ChildTaskId, MessageFeedback, MessageId};
use assistant_runtime::{
    ApprovalModeChange, ArchiveChange, ConversationRewrite, MessageFeedbackChange, ModelChange,
    ReasoningEffortChange, RewriteResult, SessionPinnedChange, SessionProxyChange,
    SessionTitleChange, StoredInput, StoredInputState, StoredMessageFeedback, StoredRun,
    VariantChange,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, body_path, child_task_directory, child_tasks_directory, conflict,
    conversation, database_write_error,
    goal::apply_goal_rewrite_pause,
    internal_error, invalid_data,
    mode::{agent_variant_value, approval_mode_value, reasoning_effort_value},
    recovery::ReplacementPlan,
    sync_directory, to_i64,
};

impl StorageEngine {
    pub(super) fn set_session_archive(&mut self, change: ArchiveChange) -> StorageResult<()> {
        let (from, to, archived_at) = if change.archived {
            ("active", "archived", Some(change.changed_at_ms))
        } else {
            ("archived", "active", None)
        };
        let changed = self
            .connection
            .execute(
                "UPDATE sessions
                 SET lifecycle = ?1, archived_at_ms = ?2
                 WHERE session_id = ?3 AND lifecycle = ?4 AND role = 'standard'
                   AND (?1 = 'active' OR (
                     NOT EXISTS (SELECT 1 FROM inputs WHERE session_id = ?3 AND state = 'queued')
                     AND NOT EXISTS (SELECT 1 FROM runs WHERE session_id = ?3 AND status IN ('accepted', 'running', 'cancelling'))
                     AND NOT EXISTS (SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?3)
                   ))",
                params![to, archived_at, change.session_id.as_str(), from],
            )
            .map_err(|source| {
                database_write_error("session archive state could not be changed", source)
            })?;
        if changed != 1 {
            return Err(conflict("session lifecycle cannot be changed"));
        }
        Ok(())
    }

    pub(super) fn set_session_proxy(&mut self, change: SessionProxyChange) -> StorageResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("session proxy transaction could not be started", source)
            })?;
        let controller_role = transaction
            .query_row(
                "SELECT role FROM sessions WHERE session_id = ?1 AND lifecycle = 'active'",
                [change.controller_session_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| internal_error("controller session could not be queried", source))?;
        if controller_role.as_deref() != Some("controller")
            || change.controller_session_id == change.target_session_id
        {
            return Err(conflict("controller session is invalid"));
        }
        let target = transaction
            .query_row(
                "SELECT role, proxy_controller_session_id
                 FROM sessions WHERE session_id = ?1 AND lifecycle = 'active'",
                [change.target_session_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(|source| internal_error("proxy target session could not be queried", source))?
            .ok_or_else(|| conflict("proxy target session does not exist"))?;
        if target.0 != "standard" {
            return Err(conflict("proxy target session is invalid"));
        }
        match (target.1.as_deref(), change.enabled) {
            (Some(current), true) if current == change.controller_session_id.as_str() => {}
            (None, true) => {
                transaction
                    .execute(
                        "UPDATE sessions
                         SET proxy_controller_session_id = ?1, proxy_changed_at_ms = ?2
                         WHERE session_id = ?3",
                        params![
                            change.controller_session_id.as_str(),
                            change.changed_at_ms,
                            change.target_session_id.as_str(),
                        ],
                    )
                    .map_err(|source| {
                        database_write_error("session proxy could not be enabled", source)
                    })?;
            }
            (Some(current), false) if current == change.controller_session_id.as_str() => {
                transaction
                    .execute(
                        "UPDATE sessions
                         SET proxy_controller_session_id = NULL, proxy_changed_at_ms = NULL
                         WHERE session_id = ?1",
                        [change.target_session_id.as_str()],
                    )
                    .map_err(|source| {
                        database_write_error("session proxy could not be disabled", source)
                    })?;
            }
            (None, false) => {}
            _ => return Err(conflict("proxy target is bound to another controller")),
        }
        transaction.commit().map_err(|source| {
            database_write_error("session proxy transaction could not be committed", source)
        })?;
        Ok(())
    }

    pub(super) fn rename_session(&mut self, change: SessionTitleChange) -> StorageResult<()> {
        let changed = self
            .connection
            .execute(
                "UPDATE sessions
             SET title = ?1, title_origin = 'user'
             WHERE session_id = ?2 AND lifecycle = 'active'",
                params![change.title, change.session_id.as_str()],
            )
            .map_err(|source| database_write_error("session title could not be changed", source))?;
        if changed != 1 {
            return Err(conflict("session title cannot be changed"));
        }
        Ok(())
    }

    pub(super) fn set_session_pinned(&mut self, change: SessionPinnedChange) -> StorageResult<()> {
        let changed = self
            .connection
            .execute(
                "UPDATE sessions SET is_pinned = ?1
             WHERE session_id = ?2 AND lifecycle = 'active'",
                params![i64::from(change.is_pinned), change.session_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("session pinned state could not be changed", source)
            })?;
        if changed != 1 {
            return Err(conflict("session pinned state cannot be changed"));
        }
        Ok(())
    }

    pub(super) fn set_message_feedback(
        &mut self,
        change: MessageFeedbackChange,
    ) -> StorageResult<()> {
        let exists = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
                [change.session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| internal_error("feedback session could not be queried", source))?;
        if exists != 1 {
            return Err(conflict("feedback session does not exist"));
        }
        if let Some(feedback) = change.feedback {
            self.connection
                .execute(
                    "INSERT INTO message_feedback (session_id, message_id, feedback, changed_at_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(session_id, message_id) DO UPDATE SET
                    feedback = excluded.feedback, changed_at_ms = excluded.changed_at_ms",
                    params![
                        change.session_id.as_str(),
                        change.message_id.as_str(),
                        feedback_value(feedback),
                        change.changed_at_ms,
                    ],
                )
                .map_err(|source| {
                    database_write_error("message feedback could not be saved", source)
                })?;
        } else {
            self.connection
                .execute(
                    "DELETE FROM message_feedback WHERE session_id = ?1 AND message_id = ?2",
                    params![change.session_id.as_str(), change.message_id.as_str()],
                )
                .map_err(|source| {
                    database_write_error("message feedback could not be cleared", source)
                })?;
        }
        Ok(())
    }

    pub(super) fn load_message_feedback(
        &self,
        session_id: &assistant_protocol::SessionId,
    ) -> StorageResult<Vec<StoredMessageFeedback>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT message_id, feedback FROM message_feedback
             WHERE session_id = ?1 ORDER BY message_id",
            )
            .map_err(|source| internal_error("message feedback could not be queried", source))?;
        let rows = statement
            .query_map([session_id.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|source| internal_error("message feedback could not be read", source))?;
        rows.map(|row| {
            let (message_id, feedback) = row.map_err(|source| {
                internal_error("message feedback row could not be read", source)
            })?;
            Ok(StoredMessageFeedback {
                message_id: MessageId::new(message_id)
                    .map_err(|_| invalid_data("stored feedback message id is invalid"))?,
                feedback: parse_feedback(&feedback)?,
            })
        })
        .collect()
    }

    pub(super) fn set_session_model(&mut self, change: ModelChange) -> StorageResult<()> {
        let changed = self
            .connection
            .execute(
                "UPDATE sessions SET model_key = ?1, reasoning_effort = ?2
                 WHERE session_id = ?3 AND lifecycle = 'active'
                   AND NOT EXISTS (SELECT 1 FROM inputs WHERE session_id = ?3 AND state = 'queued')
                   AND NOT EXISTS (SELECT 1 FROM runs WHERE session_id = ?3 AND status IN ('accepted', 'running', 'cancelling'))
                   AND NOT EXISTS (SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?3)",
                params![change.model_key.as_str(), change.reasoning_effort.map(reasoning_effort_value), change.session_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("session model could not be changed", source)
            })?;
        if changed != 1 {
            return Err(conflict("session model cannot be changed"));
        }
        Ok(())
    }

    pub(super) fn set_session_reasoning_effort(
        &mut self,
        change: ReasoningEffortChange,
    ) -> StorageResult<()> {
        let changed = self.connection.execute(
            "UPDATE sessions SET reasoning_effort = ?1 WHERE session_id = ?2 AND lifecycle = 'active'",
            params![change.reasoning_effort.map(reasoning_effort_value), change.session_id.as_str()],
        ).map_err(|source| database_write_error("session reasoning effort could not be changed", source))?;
        if changed != 1 {
            return Err(conflict("session reasoning effort cannot be changed"));
        }
        Ok(())
    }

    pub(super) fn set_session_variant(&mut self, change: VariantChange) -> StorageResult<()> {
        let changed = self.connection.execute(
            "UPDATE sessions SET current_variant = ?1 WHERE session_id = ?2 AND lifecycle = 'active'",
            params![agent_variant_value(change.variant), change.session_id.as_str()],
        ).map_err(|source| database_write_error("session variant could not be changed", source))?;
        if changed != 1 {
            return Err(conflict("session variant cannot be changed"));
        }
        Ok(())
    }

    pub(super) fn set_session_approval_mode(
        &mut self,
        change: ApprovalModeChange,
    ) -> StorageResult<()> {
        let changed = self.connection.execute(
            "UPDATE sessions SET approval_mode = ?1 WHERE session_id = ?2 AND lifecycle = 'active'",
            params![approval_mode_value(change.approval_mode), change.session_id.as_str()],
        ).map_err(|source| database_write_error("session approval mode could not be changed", source))?;
        if changed != 1 {
            return Err(conflict("session approval mode cannot be changed"));
        }
        Ok(())
    }

    pub(super) fn rewrite_from_user(
        &mut self,
        rewrite: ConversationRewrite,
    ) -> StorageResult<RewriteResult> {
        let new_message = rewrite.input.message.clone();
        if rewrite.input.session_id != rewrite.session_id
            || rewrite
                .conversation
                .messages
                .last()
                .map(conversation::message_id)
                != Some(&new_message.id)
        {
            return Err(assistant_runtime::StoreError::new(
                assistant_runtime::StoreErrorKind::InvalidInput,
                "replacement input does not match conversation",
            ));
        }
        let target_order = self
            .connection
            .query_row(
                "SELECT queue_order FROM inputs
                 WHERE session_id = ?1 AND user_message_id = ?2 AND state = 'committed'",
                params![
                    rewrite.session_id.as_str(),
                    rewrite.target_user_message_id.as_str()
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|source| internal_error("rewrite target could not be queried", source))?
            .ok_or_else(|| conflict("target user message does not belong to an input"))?;

        let plan = self.begin_replacement(
            rewrite.session_id.clone(),
            rewrite.conversation.clone(),
            rewrite.changed_at_ms,
        )?;
        match self.commit_rewrite(&plan, &rewrite, target_order, &new_message.id) {
            Ok(result) => Ok(result),
            Err(error) => {
                if let Ok(directory) = self.session_directory(&rewrite.session_id) {
                    let _ = fs::remove_file(body_path(&directory, plan.new_generation));
                }
                Err(error)
            }
        }
    }

    fn commit_rewrite(
        &mut self,
        plan: &ReplacementPlan,
        rewrite: &ConversationRewrite,
        target_order: i64,
        new_message_id: &agent_types::MessageId,
    ) -> StorageResult<RewriteResult> {
        let session_directory = self.session_directory(&rewrite.session_id)?;
        let channel_source_json = rewrite
            .input
            .channel_source
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|source| {
                internal_error("replacement channel source could not be encoded", source)
            })?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("conversation rewrite could not begin", source))?;
        let updated = transaction
            .execute(
                "UPDATE sessions
                 SET body_generation = ?1, message_count = ?2, current_variant = ?3
                 WHERE session_id = ?4 AND body_generation = ?5 AND lifecycle = 'active'
                   AND NOT EXISTS (SELECT 1 FROM inputs WHERE session_id = ?4 AND state = 'queued')
                   AND NOT EXISTS (SELECT 1 FROM runs WHERE session_id = ?4 AND status IN ('accepted', 'running', 'cancelling'))
                   AND NOT EXISTS (SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?4)",
                params![
                    to_i64(plan.new_generation, "body generation exceeds SQLite range")?,
                    to_i64(plan.message_count, "message count exceeds SQLite range")?,
                    agent_variant_value(rewrite.input.agent_variant),
                    plan.session_id.as_str(),
                    to_i64(plan.previous_generation, "body generation exceeds SQLite range")?,
                ],
            )
            .map_err(|source| internal_error("conversation generation could not be switched", source))?;
        if updated != 1 {
            return Err(conflict("session is not available for history replacement"));
        }
        if let Some(effect) = rewrite.goal_effect.as_ref() {
            apply_goal_rewrite_pause(&transaction, effect, rewrite.changed_at_ms)?;
        }
        let removed_child_task_ids = {
            let mut statement = transaction
                .prepare(
                    "SELECT child_tasks.child_task_id
                     FROM child_tasks
                     JOIN runs ON runs.run_id = child_tasks.parent_run_id
                     JOIN inputs ON inputs.input_id = runs.input_id
                     WHERE inputs.session_id = ?1 AND inputs.queue_order >= ?2",
                )
                .map_err(|source| {
                    internal_error("replaced child tasks could not be queried", source)
                })?;
            let rows = statement
                .query_map(params![rewrite.session_id.as_str(), target_order], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|source| {
                    internal_error("replaced child tasks could not be queried", source)
                })?;
            rows.map(|row| {
                let value = row.map_err(|source| {
                    internal_error("replaced child task id could not be read", source)
                })?;
                ChildTaskId::new(value)
                    .map_err(|_| invalid_data("replaced child task id is invalid"))
            })
            .collect::<StorageResult<Vec<_>>>()?
        };
        let retained_message_ids = rewrite
            .conversation
            .messages
            .iter()
            .map(conversation::message_id)
            .map(agent_types::MessageId::as_str)
            .collect::<BTreeSet<_>>();
        let removed_activation_ids = {
            let mut statement = transaction
                .prepare(
                    "SELECT activation_id, message_id FROM skill_activations
                     WHERE session_id = ?1 AND owner_kind = 'session'",
                )
                .map_err(|source| {
                    internal_error("replaced skill activations could not be queried", source)
                })?;
            let rows = statement
                .query_map([rewrite.session_id.as_str()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|source| {
                    internal_error("replaced skill activations could not be queried", source)
                })?;
            rows.filter_map(|row| match row {
                Ok((activation_id, message_id))
                    if !retained_message_ids.contains(message_id.as_str()) =>
                {
                    Some(Ok(activation_id))
                }
                Ok(_) => None,
                Err(source) => Some(Err(internal_error(
                    "replaced skill activation could not be read",
                    source,
                ))),
            })
            .collect::<StorageResult<Vec<_>>>()?
        };
        for activation_id in removed_activation_ids {
            transaction
                .execute(
                    "DELETE FROM skill_activations WHERE activation_id = ?1",
                    [activation_id],
                )
                .map_err(|source| {
                    database_write_error("replaced skill activation could not be removed", source)
                })?;
        }
        transaction
            .execute(
                "DELETE FROM inputs WHERE session_id = ?1 AND queue_order >= ?2",
                params![rewrite.session_id.as_str(), target_order],
            )
            .map_err(|source| {
                database_write_error("replaced inputs could not be removed", source)
            })?;
        transaction.execute(
            "INSERT INTO inputs (input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms, agent_variant, origin, channel_source_json)
             VALUES (?1, ?2, ?3, ?4, 'committed', NULL, ?5, ?6, 'user', ?7)",
            params![
                rewrite.input.input_id.as_str(),
                rewrite.session_id.as_str(),
                rewrite.input.idempotency_key.as_ref().map(assistant_protocol::IdempotencyKey::as_str),
                new_message_id.as_str(),
                rewrite.input.accepted_at_ms,
                agent_variant_value(rewrite.input.agent_variant),
                channel_source_json,
            ],
        ).map_err(|source| database_write_error("replacement input could not be created", source))?;
        let queue_order = u64::try_from(transaction.last_insert_rowid())
            .map_err(|source| internal_error("queue order exceeds runtime range", source))?;
        transaction.execute(
            "INSERT INTO runs (run_id, session_id, input_id, attempt, status, cancel_requested, approval_mode, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms)
             VALUES (?1, ?2, ?3, 1, 'accepted', 0, ?4, NULL, NULL, ?5, NULL, NULL)",
            params![
                rewrite.input.run_id.as_str(),
                rewrite.session_id.as_str(),
                rewrite.input.input_id.as_str(),
                approval_mode_value(rewrite.input.approval_mode),
                rewrite.input.accepted_at_ms,
            ],
        ).map_err(|source| database_write_error("replacement run could not be created", source))?;
        transaction
            .execute(
                "INSERT INTO run_message_refs (run_id, message_id) VALUES (?1, ?2)",
                params![rewrite.input.run_id.as_str(), new_message_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("replacement message reference could not be created", source)
            })?;
        let input = StoredInput {
            queue_order,
            input_id: rewrite.input.input_id.clone(),
            session_id: rewrite.session_id.clone(),
            idempotency_key: rewrite.input.idempotency_key.clone(),
            agent_variant: rewrite.input.agent_variant,
            origin: rewrite.input.origin,
            goal_binding: rewrite.input.goal_binding.clone(),
            cross_session: rewrite.input.cross_session.clone(),
            channel_source: rewrite.input.channel_source.clone(),
            skill_activation: None,
            user_message_id: new_message_id.clone(),
            state: StoredInputState::Committed,
            queued_message: None,
            accepted_at_ms: rewrite.input.accepted_at_ms,
        };
        let run = StoredRun {
            run_id: rewrite.input.run_id.clone(),
            session_id: rewrite.session_id.clone(),
            input_id: rewrite.input.input_id.clone(),
            attempt: 1,
            status: assistant_protocol::RunStatus::Accepted,
            agent_variant: rewrite.input.agent_variant,
            approval_mode: rewrite.input.approval_mode,
            reasoning_effort: None,
            cancel_requested: false,
            error: None,
            message_ids: vec![new_message_id.clone()],
            message_steps: std::collections::HashMap::new(),
            created_at_ms: rewrite.input.accepted_at_ms,
            started_at_ms: None,
            finished_at_ms: None,
        };
        transaction.commit().map_err(|source| {
            database_write_error("conversation rewrite could not be committed", source)
        })?;

        // SQLite 提交后新 generation 已是唯一权威。旧文件清理是 best-effort，不能把已经
        // 成功的业务提交回报成失败并让 Runtime 保留旧内存投影。
        if fs::remove_file(body_path(&session_directory, plan.previous_generation)).is_ok() {
            let _ = sync_directory(&session_directory);
        }
        let child_tasks_directory = child_tasks_directory(&session_directory);
        for child_task_id in removed_child_task_ids {
            let _ = fs::remove_dir_all(child_task_directory(&session_directory, &child_task_id));
        }
        let _ = sync_directory(&child_tasks_directory);
        Ok(RewriteResult {
            input,
            run,
            body_generation: plan.new_generation,
        })
    }
}

fn feedback_value(feedback: MessageFeedback) -> &'static str {
    match feedback {
        MessageFeedback::Positive => "positive",
        MessageFeedback::Negative => "negative",
    }
}

fn parse_feedback(value: &str) -> StorageResult<MessageFeedback> {
    match value {
        "positive" => Ok(MessageFeedback::Positive),
        "negative" => Ok(MessageFeedback::Negative),
        _ => Err(invalid_data("stored message feedback is invalid")),
    }
}
