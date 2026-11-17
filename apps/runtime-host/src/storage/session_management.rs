//! Session 归档、模型切换与历史重新输入的业务原子操作。

use std::{collections::BTreeSet, fs};

use assistant_protocol::{
    ChildTaskId, MessageFeedback, MessageId, SessionTitleGenerationTriggerSnapshot,
    SessionTitleOrigin,
};
use assistant_runtime::{
    ApprovalModeChange, ArchiveChange, ConversationRewrite, MessageFeedbackChange, ModelChange,
    ReasoningEffortChange, RewriteResult, SessionCommandCommit, SessionPinnedChange,
    SessionProxyChange, SessionTitleChange, SessionTitleGenerationCommit,
    SessionTitleGenerationCommitResult, StoredInput, StoredInputState, StoredMessageFeedback,
    StoredRun, StoredSessionCommand, StoredSessionCommandState, VariantChange,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, body_path, child_task_directory, child_tasks_directory, conflict,
    conversation, database_write_error,
    goal::apply_goal_resume,
    internal_error, invalid_data, invalid_data_with_source,
    mode::{agent_variant_value, approval_mode_value, reasoning_effort_value},
    recovery::ReplacementPlan,
    sync_directory, to_i64,
};

impl StorageEngine {
    pub(super) fn commit_session_command(
        &mut self,
        commit: SessionCommandCommit,
    ) -> StorageResult<StoredSessionCommand> {
        if commit.operation_id.trim().is_empty()
            || commit.message.origin != agent_types::UserMessageOrigin::Runtime
            || commit.message.transcript_visibility != agent_types::TranscriptVisibility::Visible
        {
            return Err(invalid_data("session command result message is invalid"));
        }
        let existing = self
            .load_session_command(&commit.session_id, &commit.input_id)?
            .ok_or_else(|| conflict("session command does not exist"))?;
        if !commit.result.matches_command(&existing.command)
            || existing.session_id != commit.session_id
            || existing.user_message_id != commit.message.id
        {
            return Err(conflict("session command identity is inconsistent"));
        }
        let current = self.load_conversation(&commit.session_id)?;
        if existing.state == StoredSessionCommandState::Committed {
            if existing.result.as_ref() == Some(&commit.result)
                && current
                    .messages
                    .iter()
                    .any(|message| matches!(message, agent_types::ConversationMessage::User(user) if user == &commit.message))
            {
                return Ok(existing);
            }
            return Err(conflict(
                "session command was already committed differently",
            ));
        }
        let mut replacement = current;
        replacement
            .messages
            .push(agent_types::ConversationMessage::User(commit.message));
        replacement
            .validate_tool_exchange_pairs()
            .map_err(|source| {
                assistant_runtime::StoreError::with_source(
                    assistant_runtime::StoreErrorKind::InvalidInput,
                    "session command result breaks conversation structure",
                    source,
                )
            })?;
        let source_generation = self
            .connection
            .query_row(
                "SELECT body_generation FROM sessions
                 WHERE session_id = ?1 AND lifecycle = 'active'
                   AND NOT EXISTS (SELECT 1 FROM runs WHERE session_id = ?1 AND status IN ('running', 'cancelling'))
                   AND NOT EXISTS (SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?1)
                   AND NOT EXISTS (SELECT 1 FROM child_tasks WHERE session_id = ?1 AND status IN ('accepted', 'running'))
                   AND NOT EXISTS (SELECT 1 FROM child_pending_tool_exchanges WHERE session_id = ?1)",
                [commit.session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|source| {
                internal_error("session command generation could not be queried", source)
            })?
            .ok_or_else(|| conflict("session command target is not active"))?;
        let source_generation = u64::try_from(source_generation)
            .map_err(|source| invalid_data_with_source("session generation is invalid", source))?;
        let mut result_generation = source_generation
            .checked_add(1)
            .ok_or_else(|| conflict("session generation is exhausted"))?;
        let session_directory = self.session_directory(&commit.session_id)?;
        while body_path(&session_directory, result_generation).exists() {
            result_generation = result_generation
                .checked_add(1)
                .ok_or_else(|| conflict("session generation is exhausted"))?;
        }
        let new_body = body_path(&session_directory, result_generation);
        let owner = assistant_protocol::ConversationOwner::MainSession {
            session_id: commit.session_id.clone(),
        };
        let mut history = self.history_for_replacement(&owner, &replacement)?;
        let mut command = existing.clone();
        command.state = StoredSessionCommandState::Committed;
        command.result = Some(commit.result.clone());
        history.commands.push(command);
        history.normalize();
        conversation::write_generation(
            &new_body,
            &owner,
            result_generation,
            &replacement,
            &history,
        )?;
        sync_directory(&session_directory)?;
        let message_count = u64::try_from(replacement.messages.len()).map_err(|source| {
            assistant_runtime::StoreError::with_source(
                assistant_runtime::StoreErrorKind::InvalidInput,
                "session command conversation is too large",
                source,
            )
        })?;
        let committed = (|| -> StorageResult<()> {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    database_write_error("session command commit could not begin", source)
                })?;
            let changed = transaction
                .execute(
                    "DELETE FROM inputs
                     WHERE input_id = ?1 AND session_id = ?2 AND input_kind = 'command'
                       AND state = 'queued' AND command_result_json IS NULL",
                    params![commit.input_id.as_str(), commit.session_id.as_str(),],
                )
                .map_err(|source| {
                    database_write_error("session command result could not be stored", source)
                })?;
            if changed != 1 {
                return Err(conflict("session command is not queued"));
            }
            let changed = transaction
                .execute(
                    "UPDATE sessions
                     SET body_generation = ?1, message_count = ?2, updated_at_ms = ?3
                     WHERE session_id = ?4 AND body_generation = ?5 AND lifecycle = 'active'",
                    params![
                        to_i64(result_generation, "session generation exceeds range")?,
                        to_i64(message_count, "session message count exceeds range")?,
                        commit.committed_at_ms,
                        commit.session_id.as_str(),
                        to_i64(source_generation, "session generation exceeds range")?,
                    ],
                )
                .map_err(|source| {
                    database_write_error("session command generation could not switch", source)
                })?;
            if changed != 1 {
                return Err(conflict("session command generation changed"));
            }
            if let assistant_runtime::StoredSessionCommandResult::ShellSwitch {
                shell,
                environment: Some(environment),
                ..
            } = &commit.result
            {
                transaction.execute(
                    "UPDATE sessions SET agent_shell_kind = ?1, agent_shell_environment_json = ?2 WHERE session_id = ?3",
                    params![super::mode::shell_kind_value(*shell), super::agent_shell::encode_environment(Some(environment))?, commit.session_id.as_str()],
                ).map_err(|source| database_write_error("session shell could not be stored", source))?;
            }
            transaction.commit().map_err(|source| {
                database_write_error("session command commit could not finish", source)
            })
        })();
        if let Err(error) = committed {
            // 提交结果不明时不能删除可能已发布的新代次；原 SQL 身份仍用于精确幂等回读。
            if self.connection.is_autocommit()
                && self.session_generation(&commit.session_id).ok() == Some(source_generation)
            {
                self.conversation_indexes.remove_under(&new_body);
                let _ = fs::remove_file(&new_body);
            }
            return Err(error);
        }
        let _ = fs::remove_file(body_path(&session_directory, source_generation));
        let _ = sync_directory(&session_directory);
        self.mark_recall_owner_dirty_now(
            &assistant_protocol::ConversationOwner::MainSession {
                session_id: commit.session_id.clone(),
            },
            result_generation,
        );
        Ok(StoredSessionCommand {
            result: Some(commit.result),
            state: StoredSessionCommandState::Committed,
            ..existing
        })
    }

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
             SET title = ?1, title_origin = 'user', automatic_title_pending = 0
             WHERE session_id = ?2 AND lifecycle = 'active'",
                params![change.title, change.session_id.as_str()],
            )
            .map_err(|source| database_write_error("session title could not be changed", source))?;
        if changed != 1 {
            return Err(conflict("session title cannot be changed"));
        }
        Ok(())
    }

    pub(super) fn disable_automatic_title(
        &mut self,
        session_id: &assistant_protocol::SessionId,
    ) -> StorageResult<()> {
        let changed = self
            .connection
            .execute(
                "UPDATE sessions SET automatic_title_pending = 0
                 WHERE session_id = ?1 AND lifecycle = 'active'",
                [session_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("automatic title eligibility could not be disabled", source)
            })?;
        if changed != 1 {
            return Err(conflict("session title generation cannot be started"));
        }
        Ok(())
    }

    pub(super) fn commit_session_title_generation(
        &mut self,
        commit: SessionTitleGenerationCommit,
    ) -> StorageResult<SessionTitleGenerationCommitResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("session title generation could not begin commit", source)
            })?;
        let current = transaction
            .query_row(
                "SELECT title, title_origin FROM sessions
                 WHERE session_id = ?1 AND lifecycle = 'active'",
                [commit.session_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|source| internal_error("session title could not be queried", source))?
            .ok_or_else(|| conflict("session title generation cannot be committed"))?;
        let current_origin = match current.1.as_str() {
            "generated" => SessionTitleOrigin::Generated,
            "user" => SessionTitleOrigin::User,
            _ => return Err(invalid_data("session title origin is invalid")),
        };
        let may_apply = match commit.trigger {
            SessionTitleGenerationTriggerSnapshot::Automatic => {
                current_origin == SessionTitleOrigin::Generated
                    && commit.expected_title.as_deref() == Some(current.0.as_str())
            }
            SessionTitleGenerationTriggerSnapshot::Manual => true,
        };
        let applied = may_apply && commit.title.is_some();
        let (title, title_origin) = if applied {
            (
                commit.title.as_deref().unwrap_or(current.0.as_str()),
                "generated",
            )
        } else {
            (current.0.as_str(), current.1.as_str())
        };
        transaction
            .execute(
                "UPDATE sessions
                 SET title = ?1, title_origin = ?2, automatic_title_pending = 0
                 WHERE session_id = ?3",
                params![title, title_origin, commit.session_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("session title generation could not be committed", source)
            })?;
        if commit.request_attempted {
            let usage = commit.usage.as_ref();
            transaction
                .execute(
                    "UPDATE session_usage SET
                       auxiliary_request_count = auxiliary_request_count + 1,
                       auxiliary_input_tokens_sum = auxiliary_input_tokens_sum + ?1,
                       auxiliary_output_tokens_sum = auxiliary_output_tokens_sum + ?2,
                       auxiliary_total_tokens_sum = auxiliary_total_tokens_sum + ?3,
                       updated_at_ms = ?4
                     WHERE session_id = ?5",
                    params![
                        to_i64(
                            usage.map_or(0, |value| value.input_tokens),
                            "auxiliary input tokens"
                        )?,
                        to_i64(
                            usage.map_or(0, |value| value.output_tokens),
                            "auxiliary output tokens"
                        )?,
                        to_i64(
                            usage.map_or(0, |value| value.total_tokens),
                            "auxiliary total tokens"
                        )?,
                        commit.completed_at_ms,
                        commit.session_id.as_str(),
                    ],
                )
                .map_err(|source| {
                    database_write_error("session auxiliary usage could not be recorded", source)
                })?;
        }
        let result = SessionTitleGenerationCommitResult {
            applied,
            title: title.to_owned(),
            title_origin: if title_origin == "generated" {
                SessionTitleOrigin::Generated
            } else {
                SessionTitleOrigin::User
            },
            automatic_title_pending: false,
        };
        transaction.commit().map_err(|source| {
            database_write_error("session title generation could not be committed", source)
        })?;
        Ok(result)
    }

    pub(super) fn set_session_pinned(&mut self, change: SessionPinnedChange) -> StorageResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| database_write_error("session pin change could not begin", e))?;
        let pinned: Option<bool> = transaction
            .query_row(
                "SELECT is_pinned FROM sessions WHERE session_id = ?1 AND lifecycle = 'active'",
                [change.session_id.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| internal_error("session pin state could not be read", e))?;
        let Some(pinned) = pinned else {
            return Err(conflict("session pinned state cannot be changed"));
        };
        // 设置同一值不重新入列；取消置顶保留组内顺序，再次置顶才放到置顶区末尾。
        if pinned != change.is_pinned {
            if change.is_pinned {
                let order = super::sidebar_order::Scope::Pins
                    .next(&transaction, super::sidebar_order::Placement::After)?;
                transaction.execute(
                    "UPDATE sessions SET is_pinned = 1, pin_order = ?2 WHERE session_id = ?1",
                    params![change.session_id.as_str(), order],
                )
            } else {
                transaction.execute(
                    "UPDATE sessions SET is_pinned = 0 WHERE session_id = ?1",
                    [change.session_id.as_str()],
                )
            }
            .map_err(|e| database_write_error("session pinned state could not be changed", e))?;
        }
        transaction
            .commit()
            .map_err(|e| database_write_error("session pin change could not be committed", e))
    }

    pub(super) fn set_message_feedback(
        &mut self,
        change: MessageFeedbackChange,
    ) -> StorageResult<()> {
        let generation = self.session_generation(&change.session_id)?;
        let path = body_path(&self.session_directory(&change.session_id)?, generation);
        self.conversation_indexes.validate_owner(
            &path,
            &assistant_protocol::ConversationOwner::MainSession {
                session_id: change.session_id.clone(),
            },
            generation,
        )?;
        let location = self.conversation_indexes.locate_message(
            &path,
            &agent_types::MessageId::new(change.message_id.as_str())
                .map_err(|_| invalid_data("feedback Message ID is invalid"))?,
        )?;
        if location.is_none() {
            return Err(conflict("feedback message does not exist"));
        }
        self.append_message_feedback(
            change.session_id,
            assistant_runtime::StoredHistoryFeedback {
                message_id: change.message_id,
                feedback: change.feedback,
                changed_at_ms: change.changed_at_ms,
            },
        )
    }

    pub(super) fn load_message_feedback(
        &mut self,
        session_id: &assistant_protocol::SessionId,
    ) -> StorageResult<Vec<StoredMessageFeedback>> {
        let facts = self
            .load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
                session_id: session_id.clone(),
            })?
            .facts;
        let mut values: Vec<_> = facts
            .feedback
            .into_iter()
            .filter_map(|change| {
                change.feedback.map(|feedback| StoredMessageFeedback {
                    message_id: change.message_id,
                    feedback,
                })
            })
            .collect();
        values.sort_by(|left, right| left.message_id.cmp(&right.message_id));
        Ok(values)
    }

    pub(super) fn legacy_message_feedback(
        &self,
        session_id: &assistant_protocol::SessionId,
    ) -> StorageResult<Vec<StoredMessageFeedback>> {
        if !self.allow_legacy_history {
            return Err(invalid_data(
                "legacy message feedback is only available during verified history upgrade",
            ));
        }
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
                "UPDATE sessions SET model_id = ?1, reasoning_effort = ?2, model_provider_instance_id = ?4, server_history_key = ?5, context_management_mode = ?6, model_source = ?7
                 WHERE session_id = ?3 AND lifecycle = 'active'
                   AND NOT EXISTS (SELECT 1 FROM inputs WHERE session_id = ?3 AND state = 'queued')
                   AND NOT EXISTS (SELECT 1 FROM runs WHERE session_id = ?3 AND status IN ('accepted', 'running', 'cancelling'))
                   AND NOT EXISTS (SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?3)",
                params![change.model_selection.as_ref().map(|selection| selection.model_id.as_str()), change.reasoning_effort.map(reasoning_effort_value), change.session_id.as_str(), change.model_selection.as_ref().map(|selection| selection.provider_instance_id.as_str()), change.server_history_key, change.context_management_mode.map(super::mode::context_management_mode_value), super::model_management::selection_source(change.model_selection.as_ref())],
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
            "UPDATE sessions SET reasoning_effort = ?1, server_history_key = ?3 WHERE session_id = ?2 AND lifecycle = 'active'",
            params![change.reasoning_effort.map(reasoning_effort_value), change.session_id.as_str(), change.server_history_key],
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

    /// 调用者持目标 Session gate；只准备未发布 generation 并原子替换历史，不运行新 attempt。
    /// 队列及未完成交换须保持空闲，失败只在确认旧指针仍权威时清理准备件，不补写历史控制。
    pub(super) fn rewrite_from_user(
        &mut self,
        rewrite: ConversationRewrite,
    ) -> StorageResult<RewriteResult> {
        let mut new_message = rewrite.input.message.clone();
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
        self.ensure_new_input_identity(
            &rewrite.session_id,
            &rewrite.input.input_id,
            Some(&rewrite.run_id),
            &new_message.id,
        )?;
        let owner = assistant_protocol::ConversationOwner::MainSession {
            session_id: rewrite.session_id.clone(),
        };
        let location = self
            .locate_conversation_message(assistant_runtime::ConversationMessageLocationRequest {
                owner: owner.clone(),
                message_id: rewrite.target_user_message_id.clone(),
            })?
            .ok_or_else(|| conflict("rewrite User message is missing"))?;
        let window =
            self.load_conversation_raw_window(assistant_runtime::ConversationRawWindowRequest {
                owner,
                generation: location.generation,
                start: usize::try_from(location.message_ordinal)
                    .map_err(|source| internal_error("rewrite ordinal exceeds range", source))?,
                limit: 1,
            })?;
        if window.generation != rewrite.expected_generation
            || u64::try_from(window.total).ok() != Some(rewrite.expected_message_count)
        {
            return Err(conflict(
                "history rewrite source generation or message count changed",
            ));
        }
        if rewrite.conversation.messages.len() != window.start.saturating_add(1)
            || rewrite.input.origin != assistant_runtime::InputOrigin::User
            || rewrite.input.cross_session.is_some()
        {
            return Err(invalid_data(
                "replacement target or input origin is invalid",
            ));
        }
        super::input_state::validate_new_input_activation(&rewrite.input)?;
        super::input_state::validate_new_input_mcp_selection(&rewrite.input)?;
        assistant_runtime::validate_input_message_with_channel_source(
            rewrite.input.origin,
            rewrite.input.goal_binding.as_ref(),
            rewrite.input.cross_session.as_ref(),
            rewrite.input.channel_source.as_ref(),
            &rewrite.input.message,
        )
        .map_err(|_| invalid_data("replacement input origin is invalid"))?;
        window
            .history
            .and_then(|facts| {
                facts.inputs.into_iter().find(|input| {
                    input.user_message_id == rewrite.target_user_message_id && !input.inherited
                })
            })
            .ok_or_else(|| conflict("target User has no local Input history"))?;
        if let Some(key) = &rewrite.input.idempotency_key
            && self.request_key_owner(&rewrite.session_id, key)?.is_some()
        {
            return Err(conflict(
                "replacement Input request key is already accepted",
            ));
        }

        let (plan, prepared) = self.begin_prepared_replacement(
            rewrite.session_id.clone(),
            rewrite.conversation.clone(),
            rewrite.changed_at_ms,
            |snapshot, generation, history| {
                let retained_children = history
                    .child_tasks
                    .iter()
                    .map(|child| child.child_task_id.clone())
                    .collect();
                ConversationRewrite::rebase_quotes(
                    &rewrite.session_id,
                    snapshot,
                    generation,
                    rewrite.expected_generation,
                    &retained_children,
                );
                if let Some(agent_types::ConversationMessage::User(user)) = snapshot.messages.last()
                {
                    new_message = user.clone();
                }
            },
        )?;
        match self.commit_rewrite(&plan, prepared, &rewrite, new_message) {
            Ok(result) => Ok(result),
            Err(error) => {
                // 只有连接已退出事务且旧指针仍可确认时才能删除准备件。提交结果不明时保留两份
                // 文件，不能把可能已发布的新代次当临时文件清掉；此处不自动补写数据库。
                if self.connection.is_autocommit()
                    && self.session_generation(&rewrite.session_id).ok()
                        == Some(plan.previous_generation)
                    && let Ok(directory) = self.session_directory(&rewrite.session_id)
                {
                    let path = body_path(&directory, plan.new_generation);
                    self.conversation_indexes.remove_under(&path);
                    let _ = fs::remove_file(path);
                }
                Err(error)
            }
        }
    }

    /// begin_replacement 已同步并核验新正文。单 worker 内比较文件目录后进入 SQLite 事务，
    /// 新输入归属随同一准备件同步；提交指针后才清理被裁掉的资源，内存与事件由 Runtime 更新。
    fn commit_rewrite(
        &mut self,
        plan: &ReplacementPlan,
        mut prepared: conversation::PreparedGeneration,
        rewrite: &ConversationRewrite,
        new_message: agent_types::UserMessage,
    ) -> StorageResult<RewriteResult> {
        let new_message_id = &new_message.id;
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
        let owner = assistant_protocol::ConversationOwner::MainSession {
            session_id: rewrite.session_id.clone(),
        };
        let old_path = body_path(&session_directory, plan.previous_generation);
        // 新目录来自完整回读生成的私有索引；旧目录来自原代次，不依赖已释放的 child／Run 行。
        let old_children = self.conversation_indexes.read_child_history(
            &old_path,
            &owner,
            plan.previous_generation,
            None,
        )?;
        let retained: BTreeSet<ChildTaskId> = prepared
            .children()
            .into_iter()
            .map(|child| child.child_task_id)
            .collect();
        for child in old_children
            .iter()
            .map(|child| &child.child_task_id)
            .chain(retained.iter())
        {
            super::filesystem::validate_child_task_component(child)?;
        }
        let removed_child_task_ids: Vec<_> = old_children
            .into_iter()
            .map(|child| child.child_task_id)
            .filter(|child| !retained.contains(child))
            .collect();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("conversation rewrite could not begin", source))?;
        let updated = transaction
            .execute(
                "UPDATE sessions
                 SET body_generation = ?1, message_count = ?2
                 WHERE session_id = ?3 AND body_generation = ?4 AND message_count = ?5 AND lifecycle = 'active'
                   AND NOT EXISTS (SELECT 1 FROM inputs WHERE session_id = ?3 AND state = 'queued')
                   AND NOT EXISTS (SELECT 1 FROM session_feedback WHERE session_id = ?3)
                   AND NOT EXISTS (SELECT 1 FROM runs WHERE session_id = ?3 AND status IN ('accepted', 'running', 'cancelling'))
                   AND NOT EXISTS (SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?3)
                   AND NOT EXISTS (SELECT 1 FROM body_appends WHERE session_id = ?3)
                   AND NOT EXISTS (SELECT 1 FROM child_tasks WHERE session_id = ?3 AND status IN ('accepted','running'))
                   AND NOT EXISTS (SELECT 1 FROM child_pending_tool_exchanges WHERE session_id = ?3)
                   AND NOT EXISTS (SELECT 1 FROM child_body_appends WHERE session_id = ?3)",
                params![
                    to_i64(plan.new_generation, "body generation exceeds SQLite range")?,
                    to_i64(plan.message_count, "message count exceeds SQLite range")?,
                    plan.session_id.as_str(),
                    to_i64(rewrite.expected_generation, "body generation exceeds SQLite range")?,
                    to_i64(rewrite.expected_message_count, "message count exceeds SQLite range")?,
                ],
            )
            .map_err(|source| internal_error("conversation generation could not be switched", source))?;
        if updated != 1 {
            return Err(conflict("session is not available for history replacement"));
        }
        let goal_binding = rewrite.input.goal_binding.as_ref();
        match (rewrite.goal_effect.as_ref(), goal_binding) {
            (Some(effect), Some(binding))
                if effect.goal.session_id == rewrite.session_id
                    && effect.goal.goal_id == effect.expected_goal_id
                    && effect.expected_generation.checked_add(1)
                        == Some(effect.goal.generation)
                    && effect.goal.goal_id == binding.goal_id
                    && effect.goal.generation == binding.generation
                    && effect.goal.turn == binding.turn
                    && effect.goal.updated_at_ms == rewrite.changed_at_ms
                    && rewrite.conversation.messages.iter().any(|message| {
                        conversation::message_id(message)
                            == &effect.goal.objective.source_message_id
                    })
                    && binding.reply_route.is_none() =>
            {
                apply_goal_resume(&transaction, &effect.goal)?;
            }
            (None, None) => {
                let has_goal = transaction
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM session_goals WHERE session_id=?1)",
                        [rewrite.session_id.as_str()],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(|source| {
                        internal_error("rewrite Goal could not be checked", source)
                    })?;
                if has_goal {
                    return Err(conflict("rewrite Goal changed before commit"));
                }
            }
            _ => return Err(invalid_data("replacement Goal binding is invalid")),
        }
        for child in &removed_child_task_ids {
            transaction.execute("DELETE FROM conversation_recall_documents WHERE session_id=?1 AND child_task_id=?2",params![rewrite.session_id.as_str(),child.as_str()])
                .map_err(|source| database_write_error("replaced child search documents could not be removed",source))?;
            transaction.execute("DELETE FROM conversation_recall_heads WHERE session_id=?1 AND child_task_id=?2",params![rewrite.session_id.as_str(),child.as_str()])
                .map_err(|source| database_write_error("replaced child search head could not be removed",source))?;
        }
        let retained_message_ids = rewrite
            .conversation
            .messages
            .iter()
            .map(conversation::message_id)
            .map(agent_types::MessageId::as_str)
            .collect::<BTreeSet<_>>();
        if !self.allow_legacy_history {
            let messages = serde_json::to_string(&retained_message_ids)
                .map_err(|e| internal_error("card retention could not be encoded", e))?;
            let children = serde_json::to_string(&retained)
                .map_err(|e| internal_error("child card retention could not be encoded", e))?;
            transaction.execute("DELETE FROM message_cards WHERE session_id=?1 AND ((child_key='' AND anchor_message_id NOT IN(SELECT value FROM json_each(?2))) OR (child_key<>'' AND child_key NOT IN(SELECT value FROM json_each(?3))))",
                params![rewrite.session_id.as_str(),messages,children]).map_err(|e|database_write_error("replaced cards could not be removed",e))?;
        }
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
        let removed_selection_ids = {
            let mut statement = transaction
                .prepare(
                    "SELECT selection_id, message_id FROM mcp_input_selections
                     WHERE session_id = ?1",
                )
                .map_err(|source| {
                    internal_error("replaced MCP selections could not be queried", source)
                })?;
            let rows = statement
                .query_map([rewrite.session_id.as_str()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|source| {
                    internal_error("replaced MCP selections could not be queried", source)
                })?;
            rows.filter_map(|row| match row {
                Ok((selection_id, message_id))
                    if !retained_message_ids.contains(message_id.as_str()) =>
                {
                    Some(Ok(selection_id))
                }
                Ok(_) => None,
                Err(source) => Some(Err(internal_error(
                    "replaced MCP selection could not be read",
                    source,
                ))),
            })
            .collect::<StorageResult<Vec<_>>>()?
        };
        for selection_id in removed_selection_ids {
            transaction
                .execute(
                    "DELETE FROM mcp_input_selections WHERE selection_id = ?1",
                    [selection_id],
                )
                .map_err(|source| {
                    database_write_error("replaced MCP selection could not be removed", source)
                })?;
        }
        transaction.execute(
            "INSERT INTO inputs (input_id, session_id, user_message_id, state, queued_message_json, accepted_at_ms, agent_variant, origin, channel_source_json, goal_id, goal_generation, goal_turn)
             VALUES (?1, ?2, ?3, 'committed', NULL, ?4, ?5, 'user', ?6, ?7, ?8, ?9)",
            params![
                rewrite.input.input_id.as_str(),
                rewrite.session_id.as_str(),
                new_message_id.as_str(),
                rewrite.input.accepted_at_ms,
                agent_variant_value(rewrite.input.agent_variant),
                channel_source_json,
                goal_binding.map(|binding| binding.goal_id.as_str()),
                goal_binding.map(|binding| to_i64(binding.generation, "Goal generation exceeds SQLite range")).transpose()?,
                goal_binding.map(|binding| i64::from(binding.turn)),
            ],
        ).map_err(|source| database_write_error("replacement input could not be created", source))?;
        let queue_order = u64::try_from(transaction.last_insert_rowid())
            .map_err(|source| internal_error("queue order exceeds runtime range", source))?;
        transaction.execute(
            "INSERT INTO runs (run_id, session_id, input_id, attempt, status, cancel_requested, approval_mode, created_at_ms, started_at_ms)
             VALUES (?1, ?2, ?3, 1, 'accepted', 0, ?4, ?5, NULL)",
            params![
                rewrite.run_id.as_str(),
                rewrite.session_id.as_str(),
                rewrite.input.input_id.as_str(),
                approval_mode_value(rewrite.approval_mode),
                rewrite.input.accepted_at_ms,
            ],
        ).map_err(|source| database_write_error("replacement run could not be created", source))?;
        let input = StoredInput {
            agent_shell_target: None,
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
            shell: None,
            run_id: rewrite.run_id.clone(),
            session_id: rewrite.session_id.clone(),
            input_id: rewrite.input.input_id.clone(),
            attempt: 1,
            status: assistant_protocol::RunStatus::Accepted,
            agent_variant: rewrite.input.agent_variant,
            approval_mode: rewrite.approval_mode,
            reasoning_effort: None,
            cancel_requested: false,
            error: None,
            message_ids: vec![new_message_id.clone()],
            message_steps: std::collections::HashMap::new(),
            created_at_ms: rewrite.input.accepted_at_ms,
            started_at_ms: None,
            finished_at_ms: None,
        };
        // Input 的真实顺序由当前事务分配后，补齐尚未发布的新 generation。崩溃或失败回滚 SQL，
        // 旧指针仍权威；调用方清理准备件。文件同步及回读成功前，不提交指针或发布 Runtime 事件。
        freeze_rewrite_input(&mut prepared, &input, &run, rewrite)?;
        transaction.commit().map_err(|source| {
            database_write_error("conversation rewrite could not be committed", source)
        })?;
        self.conversation_indexes
            .record_committed_generation(prepared);

        // SQLite 提交后新 generation 已是唯一权威。旧文件清理是 best-effort，不能把已经
        // 成功的业务提交回报成失败并让 Runtime 保留旧内存投影。
        self.conversation_indexes.remove_under(&old_path);
        if fs::remove_file(&old_path).is_ok() {
            let _ = sync_directory(&session_directory);
        }
        let child_tasks_directory = child_tasks_directory(&session_directory);
        for child_task_id in removed_child_task_ids {
            let path = child_task_directory(&session_directory, &child_task_id);
            self.conversation_indexes.remove_under(&path);
            let _ = fs::remove_dir_all(path);
        }
        let _ = sync_directory(&child_tasks_directory);
        Ok(RewriteResult {
            input,
            user_message: new_message,
            run,
            body_generation: plan.new_generation,
        })
    }
}

fn parse_feedback(value: &str) -> StorageResult<MessageFeedback> {
    match value {
        "positive" => Ok(MessageFeedback::Positive),
        "negative" => Ok(MessageFeedback::Negative),
        _ => Err(invalid_data("stored message feedback is invalid")),
    }
}

/// 只向未发布的准备件追加本次新输入事实，复用现有追加同步原语，不创建第二份提交回执。
/// 文件正文已完整回读；此处再核对新身份、顺序与消息归属，SQL 回滚时它不会成为可读历史。
fn freeze_rewrite_input(
    prepared: &mut conversation::PreparedGeneration,
    input: &StoredInput,
    run: &StoredRun,
    rewrite: &ConversationRewrite,
) -> StorageResult<()> {
    let mut input_fact = assistant_runtime::StoredInputHistory::from_input(input);
    input_fact.rewritten_from = Some(rewrite.target_user_message_id.clone());
    let skill_activations = rewrite
        .input
        .skill_activation
        .clone()
        .into_iter()
        .map(|mut activation| {
            activation.run_id = Some(run.run_id.clone());
            activation
        })
        .collect();
    let expected = assistant_runtime::StoredHistoryFacts {
        inputs: vec![input_fact],
        skill_activations,
        mcp_selections: rewrite.input.mcp_selection.clone().into_iter().collect(),
        runs: vec![assistant_runtime::StoredRunHistory::from_run(
            run,
            Some(input.user_message_id.clone()),
        )],
        message_origins: vec![assistant_runtime::StoredMessageOrigin {
            message_id: input.user_message_id.clone(),
            run_id: Some(run.run_id.clone()),
            input_id: Some(input.input_id.clone()),
            attempt: Some(run.attempt),
            step: None,
            inherited: false,
        }],
        ..Default::default()
    };
    prepared.append_history(&format!("rewrite:{}", run.run_id), expected)
}
