//! 子任务关系、独立 Conversation 生命周期与结构化投影。
//!
//! 子任务不是隐藏 Session/Run：执行控制归 SQLite，可靠历史归独立 Conversation 文件。
//! 所有 SQLite 与文件操作仍由单一 Host storage worker 串行拥有。

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
    database_write_error, internal_error, invalid_data, invalid_data_with_source,
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
        // 继承正文可能明确不可用，没有目录不能推导为身份未占用。
        if self
            .load_child_history_catalog(&task.session_id)?
            .iter()
            .any(|child| child.child_task_id == task.child_task_id)
        {
            return Err(conflict(
                "child history identity already exists in this session",
            ));
        }
        let context_window = i64::try_from(task.context_window_tokens)
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                invalid_data("child context window must be a positive SQLite integer")
            })?;

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
        if let Err(error) = conversation::create_empty(
            &body_path,
            &assistant_protocol::ConversationOwner::ChildTask {
                session_id: task.session_id.clone(),
                child_task_id: task.child_task_id.clone(),
            },
            1,
        ) {
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
                message_count, created_at_ms,
                started_at_ms, context_window_tokens, context_management_mode
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'accepted', 0, 1, 0,
                       ?8, NULL, ?9, ?10)",
            params![
                task.child_task_id.as_str(),
                task.session_id.as_str(),
                task.parent_run_id.as_str(),
                task.parent_tool_call_id.as_str(),
                task.title,
                prompt_json,
                agent_variant_value(task.agent_variant),
                task.created_at_ms,
                context_window,
                super::mode::context_management_mode_value(task.context_management_mode),
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
            context_management_mode: task.context_management_mode,
            context_window_tokens: Some(task.context_window_tokens),
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
            parent_exchange_receipt: if self.allow_legacy_history {
                None
            } else {
                self.pending_child_receipt(&settlement.session_id, &settlement.child_task_id)?
            },
        };
        let preflight = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("child settlement preflight could not begin", source)
            })?;
        apply_purpose(
            &preflight,
            &purpose,
            &ConversationStorageTarget::ChildTask {
                child_task_id: settlement.child_task_id.clone(),
                session_id: settlement.session_id.clone(),
            },
            settlement.finished_at_ms,
            self.allow_legacy_history,
        )?;
        preflight.rollback().map_err(|source| {
            internal_error("child settlement preflight could not roll back", source)
        })?;
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
        &mut self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StorageResult<agent_types::ConversationSnapshot> {
        self.ensure_no_pending_history(&assistant_protocol::ConversationOwner::ChildTask {
            session_id: session_id.clone(),
            child_task_id: child_task_id.clone(),
        })?;
        super::filesystem::validate_session_component(session_id)?;
        super::filesystem::validate_child_task_component(child_task_id)?;
        if self.unavailable_child_tasks.contains(&(
            session_id.as_str().to_owned(),
            child_task_id.as_str().to_owned(),
        )) {
            return Err(invalid_data("child task conversation is unavailable"));
        }
        let generation = self.child_history_generation(session_id, child_task_id)?;
        self.read_owner_document(
            &self.child_body(session_id, child_task_id, generation)?,
            &assistant_protocol::ConversationOwner::ChildTask {
                session_id: session_id.clone(),
                child_task_id: child_task_id.clone(),
            },
            generation,
        )
        .map(|document| document.conversation)
    }

    #[cfg(test)]
    pub(super) fn load_child_tasks(&self) -> StorageResult<Vec<StoredChildTask>> {
        self.load_child_tasks_scoped(None)
    }

    pub(super) fn load_child_tasks_scoped(
        &self,
        session_id: Option<&assistant_protocol::SessionId>,
    ) -> StorageResult<Vec<StoredChildTask>> {
        self.load_child_tasks_filtered(session_id, None, !self.allow_legacy_history)
    }

    pub(super) fn load_unfinished_child_tasks_scoped(
        &self,
        session_id: Option<&SessionId>,
    ) -> StorageResult<Vec<StoredChildTask>> {
        self.load_child_tasks_filtered(session_id, None, true)
    }

    pub(super) fn load_child_task_control(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StorageResult<Option<StoredChildTask>> {
        Ok(self
            .load_child_tasks_filtered(
                Some(session_id),
                Some(child_task_id),
                !self.allow_legacy_history,
            )?
            .pop())
    }

    fn load_child_tasks_filtered(
        &self,
        session_id: Option<&SessionId>,
        child_task_id: Option<&ChildTaskId>,
        unfinished_only: bool,
    ) -> StorageResult<Vec<StoredChildTask>> {
        let predicate = if session_id.is_some() {
            "session_id = ?1"
        } else {
            "?1 IS NULL"
        };
        let terminal = if self.allow_legacy_history {
            "final_message_id, error_code, error_message"
        } else {
            "NULL, NULL, NULL"
        };
        let finish = if self.allow_legacy_history {
            "finished_at_ms"
        } else {
            "NULL"
        };
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT child_task_id, session_id, parent_run_id, parent_tool_call_id, title,
                        system_prompt_json, agent_variant, status, cancel_requested,
                        body_generation, message_count, {terminal},
                        created_at_ms, started_at_ms, {finish}, context_window_tokens, context_management_mode
                 FROM child_tasks WHERE {predicate} AND (?2 IS NULL OR child_task_id=?2)
                    AND (?3=0 OR status IN ('accepted','running')) ORDER BY created_at_ms, child_task_id"
            ))
            .map_err(|source| internal_error("child tasks could not be queried", source))?;
        let rows = statement
            .query_map(
                params![
                    session_id.map(assistant_protocol::SessionId::as_str),
                    child_task_id.map(ChildTaskId::as_str),
                    unfinished_only
                ],
                |row| {
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
                        row.get::<_, Option<i64>>(17)?,
                        row.get::<_, String>(18)?,
                    ))
                },
            )
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
                context_management_mode: super::mode::parse_context_management_mode(&row.18)?,
                context_window_tokens: row
                    .17
                    .map(|value| positive_u64(value, "stored child context window is invalid"))
                    .transpose()?,
                conversation_state: if self
                    .unavailable_child_tasks
                    .contains(&(row.1.clone(), child_task_id.as_str().to_owned()))
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

    pub(super) fn recover_child_storage(&mut self) -> StorageResult<HashSet<(String, String)>> {
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
        self.load_child_task_control(session_id, child_task_id)?
            .ok_or_else(|| conflict("child task does not exist in runtime storage"))
    }

    pub(super) fn interrupt_nonterminal_child_tasks(&mut self) -> StorageResult<()> {
        self.interrupt_nonterminal_child_tasks_scoped(None)
    }
    pub(super) fn interrupt_nonterminal_child_tasks_scoped(
        &mut self,
        scope: Option<&assistant_protocol::SessionId>,
    ) -> StorageResult<()> {
        let tasks = self.load_unfinished_child_tasks_scoped(scope)?;
        let finished_at_ms = super::run_state::system_time_ms()?;
        for task in tasks {
            if task.conversation_state == StoredConversationState::Unavailable {
                continue;
            }
            let owner = assistant_protocol::ConversationOwner::ChildTask {
                session_id: task.session_id.clone(),
                child_task_id: task.child_task_id.clone(),
            };
            let path =
                self.child_body(&task.session_id, &task.child_task_id, task.body_generation)?;
            if self.allow_legacy_history
                && !conversation::has_history_header(&path, &owner, task.body_generation)?
            {
                // 仅限完成备份后的旧格式恢复；下一步整体迁出把该终态冻结到新文件。
                // 正常产品没有无头零消息格式，也不能据此保留 SQL 历史兜底。
                let changed = self
                    .connection
                    .execute(
                        "UPDATE child_tasks SET status='interrupted',finished_at_ms=?1
                     WHERE child_task_id=?2 AND session_id=?3 AND status IN ('accepted','running')",
                        params![
                            finished_at_ms,
                            task.child_task_id.as_str(),
                            task.session_id.as_str()
                        ],
                    )
                    .map_err(|source| {
                        database_write_error("legacy child recovery could not be recorded", source)
                    })?;
                if changed != 1 {
                    return Err(conflict("legacy child recovery status changed"));
                }
            } else {
                // child pending 已在调用前闭合；文件 fsync 和 SQL 效果共同完成后才恢复父结果。
                // 回执保留原结束时刻；再次启动先恢复回执，不重复制造中断记录。
                self.settle_child_task(StoredChildTaskSettlement {
                    operation_id: format!("recover-child:{}", task.child_task_id.as_str()),
                    child_task_id: task.child_task_id,
                    session_id: task.session_id,
                    status: ChildTaskStatus::Interrupted,
                    cancel_requested: task.cancel_requested,
                    error: task.error,
                    messages: Vec::new(),
                    final_message_id: task.final_message_id,
                    finished_at_ms,
                })?;
            }
        }
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
                "SELECT status FROM child_tasks WHERE child_task_id = ?1 AND session_id = ?2
                 AND (?3 OR status IN ('accepted','running'))",
                params![
                    child_task_id.as_str(),
                    session_id.as_str(),
                    self.allow_legacy_history
                ],
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
            "SELECT body_generation FROM child_tasks WHERE child_task_id = ?1 AND session_id = ?2
             AND (?3 OR status IN ('accepted','running'))",
            params![child_task_id.as_str(), session_id.as_str(),self.allow_legacy_history],
            |row| row.get::<_, i64>(0),
        ).optional().map_err(|source| internal_error("child body generation could not be queried", source))?
            .ok_or_else(|| conflict("child task does not exist in session"))?;
        positive_u64(generation, "stored child body generation is invalid")
    }

    /// 阅读定位可以使用父文件中的只读 child 绑定；执行提交仍只使用 `child_generation`
    /// 核对真实控制行，绝不据历史重建任务。控制尚未释放时仅读取它的 owner／文件指针。
    pub(super) fn child_history_generation(
        &mut self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StorageResult<u64> {
        let generation = self
            .connection
            .query_row(
                "SELECT body_generation FROM child_tasks WHERE child_task_id=?1 AND session_id=?2
                 AND (?3 OR status IN ('accepted','running'))",
                params![
                    child_task_id.as_str(),
                    session_id.as_str(),
                    self.allow_legacy_history
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|source| {
                internal_error("child history location could not be checked", source)
            })?;
        if let Some(generation) = generation {
            return positive_u64(generation, "stored child body generation is invalid");
        }
        let child = self
            .load_child_history_entries(session_id, Some(child_task_id))?
            .into_iter()
            .find(|task| &task.child_task_id == child_task_id)
            .ok_or_else(|| conflict("child history does not belong to this session"))?;
        if child.body_unavailable {
            return Err(assistant_runtime::StoreError::new(
                assistant_runtime::StoreErrorKind::ResourceUnavailable,
                "inherited child body is unavailable",
            ));
        }
        Ok(child.body_generation)
    }

    /// 主文件目录与尚未闭合父交换的临时位置提供阅读绑定；事实始终读取可靠文件。
    /// 先拒绝未完成尾帧，不查询终态控制、不建立执行任务，也不发布 Runtime 事件。
    pub(super) fn load_child_history_catalog(
        &mut self,
        session_id: &SessionId,
    ) -> StorageResult<Vec<assistant_runtime::StoredChildTaskHistory>> {
        self.load_child_history_entries(session_id, None)
    }

    /// Store worker 内只读已提交目录；定位单个 child 时只解码其最后事实帧，
    /// 避免每次正文／用量读取重读全部 sibling。owner、generation、pending 校验保持。
    fn load_child_history_entries(
        &mut self,
        session_id: &SessionId,
        child_task_id: Option<&ChildTaskId>,
    ) -> StorageResult<Vec<assistant_runtime::StoredChildTaskHistory>> {
        let owner = assistant_protocol::ConversationOwner::MainSession {
            session_id: session_id.clone(),
        };
        self.ensure_no_pending_history(&owner)?;
        super::filesystem::validate_session_component(session_id)?;
        if self.unavailable_sessions.contains(session_id.as_str()) {
            return Err(invalid_data("session conversation is unavailable"));
        }
        let generation = self.session_generation(session_id)?;
        let path = super::body_path(&self.session_directory(session_id)?, generation);
        let mut children = self.conversation_indexes.read_child_history(
            &path,
            &owner,
            generation,
            child_task_id,
        )?;
        if !self.allow_legacy_history {
            for child in self
                .pending_child_catalog(session_id)?
                .into_iter()
                .filter(|child| child_task_id.is_none_or(|id| id == &child.child_task_id))
            {
                if let Some(existing) = children
                    .iter()
                    .find(|entry| entry.child_task_id == child.child_task_id)
                {
                    if existing != &child {
                        return Err(invalid_data(
                            "pending child conflicts with reliable parent history",
                        ));
                    }
                } else {
                    children.push(child);
                }
            }
        }
        Ok(children)
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
        &mut self,
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
