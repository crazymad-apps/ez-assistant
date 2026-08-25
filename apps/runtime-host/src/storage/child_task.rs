//! 子任务关系、独立 Conversation 生命周期与结构化投影。
//!
//! 子任务不是隐藏 Session/Run：SQLite 只保存父子关系和状态，正文继续使用与主会话相同的
//! `ConversationMessage` JSONL 编码。所有 SQLite 与文件操作仍由单一 Host storage worker 串行拥有。

use std::{collections::HashSet, fs, path::PathBuf};

use agent_model::SystemPromptSnapshot;
use agent_types::{ConversationMessage, MessageId};
use assistant_protocol::{
    ChildTaskId, ChildTaskStatus, RunId, RuntimeErrorInfo, SessionId, ToolCallId,
};
use assistant_runtime::{
    ChildTaskStart, NewStoredChildTask, StoredChildTask, StoredChildTaskSettlement,
    StoredConversationState,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult,
    append_effect::{AppendPurpose, ConversationStorageTarget, apply_purpose},
    child_body_path, child_task_directory, child_tasks_directory, conflict, conversation,
    create_new_private_file, database_write_error, internal_error, invalid_data,
    invalid_data_with_source,
    mode::{agent_variant_value, parse_agent_variant, parse_child_task_status},
    non_negative_u64, positive_u64,
    run_projection::parse_error_code,
    sync_directory,
};
use crate::config_source::prepare_private_directory;

impl StorageEngine {
    pub(super) fn create_child_task(
        &mut self,
        task: NewStoredChildTask,
    ) -> StorageResult<StoredChildTask> {
        super::filesystem::validate_session_component(&task.session_id)?;
        super::filesystem::validate_child_task_component(&task.child_task_id)?;
        self.ensure_parent_ownership(&task.session_id, &task.parent_run_id)?;

        // 先完成所有纯内存编码，避免在可预见的序列化失败前创建任何磁盘资源。
        let prompt_json = serde_json::to_string(&task.system_prompt).map_err(|source| {
            internal_error("child task system prompt could not be encoded", source)
        })?;

        let session_directory = self.session_directory(&task.session_id)?;
        let tasks_directory = child_tasks_directory(&session_directory);
        prepare_private_directory(&tasks_directory).map_err(|source| {
            internal_error("child task directory could not be prepared", source)
        })?;
        let task_directory = child_task_directory(&session_directory, &task.child_task_id);
        fs::create_dir(&task_directory).map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                assistant_runtime::StoreError::with_source(
                    assistant_runtime::StoreErrorKind::Conflict,
                    "child task directory already exists",
                    source,
                )
            } else {
                internal_error("child task directory could not be created", source)
            }
        })?;
        if let Err(source) = prepare_private_directory(&task_directory) {
            let _ = fs::remove_dir(&task_directory);
            return Err(internal_error(
                "child task directory could not be prepared",
                source,
            ));
        }
        let body_path = child_body_path(&task_directory, 1);
        if let Err(error) = create_new_private_file(&body_path) {
            let _ = fs::remove_dir(&task_directory);
            return Err(error);
        }
        if let Err(error) =
            sync_directory(&task_directory).and_then(|()| sync_directory(&tasks_directory))
        {
            let _ = fs::remove_file(&body_path);
            let _ = fs::remove_dir(&task_directory);
            return Err(error);
        }
        let persisted = self.connection.execute(
            "INSERT INTO child_tasks (
                child_task_id, session_id, parent_run_id, parent_tool_call_id, title,
                system_prompt_json, agent_variant, status, cancel_requested, body_generation,
                message_count, final_message_id, error_code, error_message, created_at_ms,
                started_at_ms, finished_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'accepted', 0, 1, 0, NULL, NULL, NULL,
                       ?8, NULL, NULL)",
            params![
                task.child_task_id.as_str(),
                task.session_id.as_str(),
                task.parent_run_id.as_str(),
                task.parent_tool_call_id.as_str(),
                task.title,
                prompt_json,
                agent_variant_value(task.agent_variant),
                task.created_at_ms,
            ],
        );
        if let Err(source) = persisted {
            let _ = fs::remove_file(&body_path);
            let _ = fs::remove_dir(&task_directory);
            return Err(database_write_error(
                "child task could not be created",
                source,
            ));
        }

        let recall_owner = assistant_protocol::ConversationOwner::ChildTask {
            session_id: task.session_id.clone(),
            child_task_id: task.child_task_id.clone(),
        };
        let _ = self.initialize_recall_owner(&recall_owner, 1, task.created_at_ms);

        Ok(StoredChildTask {
            child_task_id: task.child_task_id,
            session_id: task.session_id,
            parent_run_id: task.parent_run_id,
            parent_tool_call_id: task.parent_tool_call_id,
            title: task.title,
            system_prompt: task.system_prompt,
            agent_variant: task.agent_variant,
            status: ChildTaskStatus::Accepted,
            cancel_requested: false,
            body_generation: 1,
            message_count: 0,
            final_message_id: None,
            error: None,
            created_at_ms: task.created_at_ms,
            started_at_ms: None,
            finished_at_ms: None,
            conversation_state: StoredConversationState::Available,
        })
    }

    pub(super) fn start_child_task(&mut self, start: ChildTaskStart) -> StorageResult<()> {
        self.ensure_child_status(
            &start.session_id,
            &start.child_task_id,
            ChildTaskStatus::Accepted,
        )?;
        self.append_child_messages(super::recovery::ChildAppendRequest {
            operation_id: start.operation_id,
            child_task_id: start.child_task_id,
            session_id: start.session_id,
            messages: vec![ConversationMessage::User(start.message)],
            message_step: None,
            created_at_ms: start.started_at_ms,
            purpose: AppendPurpose::ChildStart,
        })
    }

    pub(super) fn settle_child_task(
        &mut self,
        settlement: StoredChildTaskSettlement,
    ) -> StorageResult<()> {
        if !settlement.status.is_terminal() {
            return Err(assistant_runtime::StoreError::new(
                assistant_runtime::StoreErrorKind::InvalidInput,
                "child task settlement status is not terminal",
            ));
        }
        let current_status =
            self.child_status(&settlement.session_id, &settlement.child_task_id)?;
        if !matches!(
            current_status,
            ChildTaskStatus::Accepted | ChildTaskStatus::Running
        ) {
            return Err(conflict("child task is not in a settleable state"));
        }
        if current_status == ChildTaskStatus::Accepted && !settlement.messages.is_empty() {
            return Err(conflict(
                "accepted child task cannot append terminal messages",
            ));
        }
        let pending_count = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM child_pending_tool_exchanges WHERE child_task_id = ?1",
                [settlement.child_task_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| {
                internal_error("child pending tool exchange could not be queried", source)
            })?;
        if pending_count != 0 {
            return Err(conflict("child task has a pending tool exchange"));
        }

        self.validate_final_message(
            &settlement.session_id,
            &settlement.child_task_id,
            &settlement.messages,
            settlement.final_message_id.as_ref(),
        )?;

        let purpose = AppendPurpose::ChildSettlement {
            status: settlement.status,
            cancel_requested: settlement.cancel_requested,
            error: settlement.error,
            final_message_id: settlement.final_message_id,
        };
        if settlement.messages.is_empty() {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    internal_error("child task settlement could not begin", source)
                })?;
            apply_purpose(
                &transaction,
                &purpose,
                &ConversationStorageTarget::ChildTask {
                    child_task_id: settlement.child_task_id.clone(),
                    session_id: settlement.session_id.clone(),
                },
                settlement.finished_at_ms,
            )?;
            return transaction.commit().map_err(|source| {
                database_write_error("child task settlement could not be committed", source)
            });
        }
        self.append_child_messages(super::recovery::ChildAppendRequest {
            operation_id: settlement.operation_id,
            child_task_id: settlement.child_task_id,
            session_id: settlement.session_id,
            messages: settlement.messages,
            message_step: None,
            created_at_ms: settlement.finished_at_ms,
            purpose,
        })
    }

    pub(super) fn load_child_conversation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StorageResult<agent_types::ConversationSnapshot> {
        super::filesystem::validate_session_component(session_id)?;
        super::filesystem::validate_child_task_component(child_task_id)?;
        if self
            .unavailable_child_tasks
            .contains(child_task_id.as_str())
        {
            return Err(invalid_data("child task conversation is unavailable"));
        }
        let generation = self.child_generation(session_id, child_task_id)?;
        conversation::read(&self.child_body(session_id, child_task_id, generation)?)
    }

    pub(super) fn load_child_tasks(&self) -> StorageResult<Vec<StoredChildTask>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT child_task_id, session_id, parent_run_id, parent_tool_call_id, title,
                        system_prompt_json, agent_variant, status, cancel_requested,
                        body_generation, message_count, final_message_id, error_code,
                        error_message, created_at_ms, started_at_ms, finished_at_ms
                 FROM child_tasks ORDER BY created_at_ms, child_task_id",
            )
            .map_err(|source| internal_error("child tasks could not be queried", source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, Option<i64>>(15)?,
                    row.get::<_, Option<i64>>(16)?,
                ))
            })
            .map_err(|source| internal_error("child tasks could not be read", source))?;

        let mut tasks = Vec::new();
        for row in rows {
            let row =
                row.map_err(|source| internal_error("child task row could not be read", source))?;
            if !matches!(row.8, 0 | 1) {
                return Err(invalid_data(
                    "stored child task cancellation flag is invalid",
                ));
            }
            let child_task_id = ChildTaskId::new(row.0).map_err(|source| {
                invalid_data_with_source("stored child task id is invalid", source)
            })?;
            super::filesystem::validate_child_task_component(&child_task_id).map_err(|_| {
                invalid_data("stored child task id cannot be used as a path component")
            })?;
            let error = match (row.12, row.13) {
                (None, None) => None,
                (Some(code), Some(message)) => {
                    Some(RuntimeErrorInfo::new(parse_error_code(&code)?, message))
                }
                _ => return Err(invalid_data("stored child task error is incomplete")),
            };
            tasks.push(StoredChildTask {
                conversation_state: if self
                    .unavailable_child_tasks
                    .contains(child_task_id.as_str())
                {
                    StoredConversationState::Unavailable
                } else {
                    StoredConversationState::Available
                },
                child_task_id,
                session_id: SessionId::new(row.1).map_err(|source| {
                    invalid_data_with_source("stored child task session id is invalid", source)
                })?,
                parent_run_id: RunId::new(row.2).map_err(|source| {
                    invalid_data_with_source("stored child task parent run id is invalid", source)
                })?,
                parent_tool_call_id: ToolCallId::new(row.3).map_err(|source| {
                    invalid_data_with_source(
                        "stored child task parent tool call id is invalid",
                        source,
                    )
                })?,
                title: row.4,
                system_prompt: serde_json::from_str::<SystemPromptSnapshot>(&row.5).map_err(
                    |source| {
                        invalid_data_with_source(
                            "stored child task system prompt is invalid",
                            source,
                        )
                    },
                )?,
                agent_variant: parse_agent_variant(&row.6)?,
                status: parse_child_task_status(&row.7)?,
                cancel_requested: row.8 == 1,
                body_generation: positive_u64(row.9, "stored child body generation is invalid")?,
                message_count: non_negative_u64(row.10, "stored child message count is invalid")?,
                final_message_id: row.11.map(MessageId::new).transpose().map_err(|source| {
                    invalid_data_with_source("stored child final message id is invalid", source)
                })?,
                error,
                created_at_ms: row.14,
                started_at_ms: row.15,
                finished_at_ms: row.16,
            });
        }
        Ok(tasks)
    }

    pub(super) fn recover_child_storage(&mut self) -> StorageResult<HashSet<String>> {
        let mut unavailable = self.recover_child_body_appends()?;
        unavailable.extend(self.recover_pending_child_tool_exchanges()?);
        Ok(unavailable)
    }

    pub(super) fn request_child_task_cancellation(
        &mut self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StorageResult<StoredChildTask> {
        self.connection
            .execute(
                "UPDATE child_tasks SET cancel_requested = 1
                 WHERE child_task_id = ?1 AND session_id = ?2
                   AND status IN ('accepted', 'running')",
                params![child_task_id.as_str(), session_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("child task cancellation could not be recorded", source)
            })?;
        self.load_child_tasks()?
            .into_iter()
            .find(|task| task.child_task_id == *child_task_id && task.session_id == *session_id)
            .ok_or_else(|| conflict("child task does not exist in runtime storage"))
    }

    pub(super) fn interrupt_nonterminal_child_tasks(&mut self) -> StorageResult<()> {
        self.connection
            .execute(
                "UPDATE child_tasks
                 SET status = 'interrupted', finished_at_ms = ?1
                 WHERE status IN ('accepted', 'running')",
                [super::run_state::system_time_ms()?],
            )
            .map_err(|source| {
                database_write_error("non-terminal child tasks could not be interrupted", source)
            })?;
        Ok(())
    }

    fn ensure_parent_ownership(&self, session_id: &SessionId, run_id: &RunId) -> StorageResult<()> {
        let parent_session = self
            .connection
            .query_row(
                "SELECT session_id FROM runs WHERE run_id = ?1",
                [run_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| internal_error("child task parent run could not be queried", source))?
            .ok_or_else(|| conflict("child task parent run does not exist"))?;
        if parent_session != session_id.as_str() {
            return Err(conflict("child task parent belongs to a different session"));
        }
        Ok(())
    }

    pub(super) fn ensure_child_status(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
        expected: ChildTaskStatus,
    ) -> StorageResult<()> {
        let status = self
            .connection
            .query_row(
                "SELECT status FROM child_tasks WHERE child_task_id = ?1 AND session_id = ?2",
                params![child_task_id.as_str(), session_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| internal_error("child task could not be queried", source))?
            .ok_or_else(|| conflict("child task does not exist in session"))?;
        if parse_child_task_status(&status)? != expected {
            return Err(conflict("child task is not in the required state"));
        }
        Ok(())
    }

    fn child_status(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StorageResult<ChildTaskStatus> {
        let status = self
            .connection
            .query_row(
                "SELECT status FROM child_tasks WHERE child_task_id = ?1 AND session_id = ?2",
                params![child_task_id.as_str(), session_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| internal_error("child task could not be queried", source))?
            .ok_or_else(|| conflict("child task does not exist in session"))?;
        parse_child_task_status(&status)
    }

    pub(super) fn child_generation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StorageResult<u64> {
        let generation = self.connection.query_row(
            "SELECT body_generation FROM child_tasks WHERE child_task_id = ?1 AND session_id = ?2",
            params![child_task_id.as_str(), session_id.as_str()],
            |row| row.get::<_, i64>(0),
        ).optional().map_err(|source| internal_error("child body generation could not be queried", source))?
            .ok_or_else(|| conflict("child task does not exist in session"))?;
        positive_u64(generation, "stored child body generation is invalid")
    }

    pub(super) fn child_body(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
        generation: u64,
    ) -> StorageResult<PathBuf> {
        let session_directory = self.session_directory(session_id)?;
        Ok(child_body_path(
            &child_task_directory(&session_directory, child_task_id),
            generation,
        ))
    }

    fn validate_final_message(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
        appended: &[ConversationMessage],
        final_message_id: Option<&MessageId>,
    ) -> StorageResult<()> {
        let Some(final_message_id) = final_message_id else {
            return Ok(());
        };
        let current = self.load_child_conversation(session_id, child_task_id)?;
        if current
            .messages
            .iter()
            .chain(appended)
            .all(|message| conversation::message_id(message) != final_message_id)
        {
            return Err(assistant_runtime::StoreError::new(
                assistant_runtime::StoreErrorKind::InvalidInput,
                "child task final message does not exist in its conversation",
            ));
        }
        Ok(())
    }
}
