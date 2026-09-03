//! Session 历史清空的 generation 切换、精确物理清理与崩溃恢复。

use std::{collections::BTreeSet, fs, io};

use assistant_protocol::{
    ChildTaskId, CompactSessionOutcome, SessionHistoryCleanupStatus, SessionId,
};
use assistant_runtime::{
    SessionHistoryClear, SessionHistoryClearResult, SessionHistoryCompactionFinish,
    SessionHistoryCompactionFinishKind, SessionHistoryCompactionPreparation,
    SessionHistoryCompactionPreparationResult, SessionRole, StoreError, StoreErrorKind,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, body_path, conflict, create_new_private_file,
    database_write_error, internal_error, invalid_data, non_negative_u64, positive_u64,
    sync_directory, to_i64,
};
use crate::config_source::prepare_private_directory;

#[derive(Clone)]
struct HistoryOperation {
    session_id: String,
    kind: String,
    state: String,
    source_generation: u64,
    result_generation: Option<u64>,
    compacted_message_count: Option<u64>,
    retained_message_count: Option<u64>,
}

impl StorageEngine {
    /// 以 operation ID 幂等地把目标 Session 切换到新的空 Conversation generation。
    ///
    /// 新文件先 fsync，SQLite 再于单一 Immediate transaction 中删除旧历史投影并切换
    /// generation。事务提交后的文件清理失败只形成 `Pending`，不会回退权威空历史。
    pub(super) fn clear_session_history(
        &mut self,
        clear: SessionHistoryClear,
    ) -> StorageResult<SessionHistoryClearResult> {
        if let Some(operation) = self.history_operation(clear.operation_id.as_str())? {
            self.validate_clear_retry(&clear, &operation)?;
            return match operation.state.as_str() {
                "completed" => self.clear_result(
                    &clear.session_id,
                    operation.source_generation,
                    operation
                        .result_generation
                        .ok_or_else(|| invalid_data("completed clear generation is missing"))?,
                    SessionHistoryCleanupStatus::Completed,
                ),
                "cleanup_pending" => {
                    let result_generation = operation
                        .result_generation
                        .ok_or_else(|| invalid_data("pending clear generation is missing"))?;
                    let cleanup_status = self.finish_clear_cleanup(
                        clear.operation_id.as_str(),
                        &clear.session_id,
                        result_generation,
                        clear.changed_at_ms,
                    );
                    self.clear_result(
                        &clear.session_id,
                        operation.source_generation,
                        result_generation,
                        cleanup_status,
                    )
                }
                "interrupted" => {
                    self.connection
                        .execute(
                            "DELETE FROM session_history_operations WHERE operation_id = ?1",
                            [clear.operation_id.as_str()],
                        )
                        .map_err(|source| {
                            database_write_error(
                                "interrupted clear receipt could not be reset",
                                source,
                            )
                        })?;
                    self.begin_clear_session_history(clear)
                }
                "preparing" => Err(conflict("session history clear is already preparing")),
                _ => Err(conflict(
                    "session history operation has a different outcome",
                )),
            };
        }
        self.begin_clear_session_history(clear)
    }

    fn begin_clear_session_history(
        &mut self,
        clear: SessionHistoryClear,
    ) -> StorageResult<SessionHistoryClearResult> {
        clear.skill_catalog.validate_structure().map_err(|source| {
            StoreError::with_source(
                StoreErrorKind::InvalidInput,
                "clear skill catalog is invalid",
                source,
            )
        })?;
        let stored_environment = self.load_session_environment(&clear.session_id)?;
        if stored_environment != clear.environment {
            return Err(conflict("clear session environment changed"));
        }
        let result_generation = clear
            .expected_generation
            .checked_add(1)
            .ok_or_else(|| conflict("clear session generation exhausted"))?;
        let source_generation = to_i64(
            clear.expected_generation,
            "clear source generation exceeds storage range",
        )?;
        let result_generation_sql = to_i64(
            result_generation,
            "clear result generation exceeds storage range",
        )?;
        let prompt_json = serde_json::to_string(&clear.system_prompt)
            .map_err(|source| internal_error("clear system prompt could not be encoded", source))?;
        let skill_catalog_json = serde_json::to_string(&clear.skill_catalog)
            .map_err(|source| internal_error("clear skill catalog could not be encoded", source))?;

        {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    database_write_error("clear preparation transaction could not begin", source)
                })?;
            let (lifecycle, role, generation) = transaction
                .query_row(
                    "SELECT lifecycle, role, body_generation FROM sessions WHERE session_id = ?1",
                    [clear.session_id.as_str()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|source| internal_error("clear session could not be queried", source))?
                .ok_or_else(|| conflict("clear session does not exist"))?;
            if lifecycle != "active"
                || role != role_value(clear.expected_role)
                || generation != source_generation
            {
                return Err(conflict("clear session snapshot changed"));
            }
            ensure_session_history_idle(&transaction, &clear.session_id)?;
            transaction
                .execute(
                    "INSERT INTO session_history_operations (
                        operation_id, session_id, kind, state, source_generation,
                        result_generation, created_at_ms, finished_at_ms
                     ) VALUES (?1, ?2, 'clear', 'preparing', ?3, ?4, ?5, NULL)",
                    params![
                        clear.operation_id.as_str(),
                        clear.session_id.as_str(),
                        source_generation,
                        result_generation_sql,
                        clear.changed_at_ms,
                    ],
                )
                .map_err(|source| {
                    database_write_error("clear preparation receipt could not be created", source)
                })?;
            transaction.commit().map_err(|source| {
                database_write_error("clear preparation transaction could not commit", source)
            })?;
        }

        let session_directory = self.session_directory(&clear.session_id)?;
        let result_body = body_path(&session_directory, result_generation);
        if let Err(error) =
            create_new_private_file(&result_body).and_then(|_| sync_directory(&session_directory))
        {
            let _ = self.mark_clear_interrupted(clear.operation_id.as_str(), clear.changed_at_ms);
            return Err(error);
        }

        let switched = (|| -> StorageResult<()> {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    database_write_error("clear switch transaction could not begin", source)
                })?;
            let (lifecycle, role, generation) = transaction
                .query_row(
                    "SELECT lifecycle, role, body_generation
                     FROM sessions WHERE session_id = ?1",
                    [clear.session_id.as_str()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|source| internal_error("clear session could not be queried", source))?
                .ok_or_else(|| conflict("clear session does not exist"))?;
            if lifecycle != "active"
                || role != role_value(clear.expected_role)
                || generation != source_generation
            {
                return Err(conflict("clear session snapshot changed"));
            }
            ensure_session_history_idle(&transaction, &clear.session_id)?;
            transaction
                .execute(
                    "UPDATE sessions
                     SET system_prompt_json = ?1, skill_catalog_json = ?2,
                         body_generation = ?3, message_count = 0, updated_at_ms = ?4,
                         automatic_title_pending = 0,
                         proxy_controller_session_id = CASE WHEN role = 'standard' THEN NULL
                                                            ELSE proxy_controller_session_id END,
                         proxy_changed_at_ms = CASE WHEN role = 'standard' THEN NULL
                                                    ELSE proxy_changed_at_ms END
                     WHERE session_id = ?5",
                    params![
                        prompt_json,
                        skill_catalog_json,
                        result_generation_sql,
                        clear.changed_at_ms,
                        clear.session_id.as_str(),
                    ],
                )
                .map_err(|source| {
                    database_write_error("clear session could not be switched", source)
                })?;
            for statement in [
                "DELETE FROM message_feedback WHERE session_id = ?1",
                "DELETE FROM session_work_plans WHERE session_id = ?1",
                "DELETE FROM work_plan_completion_receipts WHERE session_id = ?1",
                "DELETE FROM session_goals WHERE session_id = ?1",
                "DELETE FROM skill_activations WHERE session_id = ?1",
                "DELETE FROM mcp_input_selections WHERE session_id = ?1",
                "DELETE FROM model_request_records WHERE session_id = ?1",
                "DELETE FROM conversation_recall_documents WHERE session_id = ?1",
                "DELETE FROM conversation_recall_heads WHERE session_id = ?1",
                "DELETE FROM inputs WHERE session_id = ?1",
                "DELETE FROM session_usage WHERE session_id = ?1",
            ] {
                transaction
                    .execute(statement, [clear.session_id.as_str()])
                    .map_err(|source| {
                        database_write_error(
                            "clear history projection could not be deleted",
                            source,
                        )
                    })?;
            }
            transaction
                .execute(
                    "DELETE FROM session_history_operations
                     WHERE session_id = ?1 AND operation_id != ?2",
                    params![clear.session_id.as_str(), clear.operation_id.as_str()],
                )
                .map_err(|source| {
                    database_write_error("old history receipts could not be deleted", source)
                })?;
            transaction
                .execute(
                    "INSERT INTO session_usage (session_id, backfilled, updated_at_ms)
                     VALUES (?1, 1, ?2)",
                    params![clear.session_id.as_str(), clear.changed_at_ms],
                )
                .map_err(|source| {
                    database_write_error("clear session usage could not be reset", source)
                })?;
            transaction
                .execute(
                    "UPDATE session_history_operations
                     SET state = 'cleanup_pending', finished_at_ms = NULL
                     WHERE operation_id = ?1 AND state = 'preparing'",
                    [clear.operation_id.as_str()],
                )
                .map_err(|source| {
                    database_write_error("clear cleanup receipt could not be updated", source)
                })?;
            transaction.commit().map_err(|source| {
                database_write_error("clear switch transaction could not commit", source)
            })?;
            Ok(())
        })();
        if let Err(error) = switched {
            let _ = fs::remove_file(&result_body);
            let _ = sync_directory(&session_directory);
            let _ = self.mark_clear_interrupted(clear.operation_id.as_str(), clear.changed_at_ms);
            return Err(error);
        }

        self.unavailable_sessions.remove(clear.session_id.as_str());
        self.conversation_indexes.remove_under(&session_directory);
        let cleanup_status = self.finish_clear_cleanup(
            clear.operation_id.as_str(),
            &clear.session_id,
            result_generation,
            clear.changed_at_ms,
        );
        self.clear_result(
            &clear.session_id,
            clear.expected_generation,
            result_generation,
            cleanup_status,
        )
    }

    pub(super) fn prepare_session_compaction(
        &mut self,
        preparation: SessionHistoryCompactionPreparation,
    ) -> StorageResult<SessionHistoryCompactionPreparationResult> {
        if let Some(operation) = self.history_operation(preparation.operation_id.as_str())? {
            self.validate_compaction_identity(&preparation, &operation)?;
            return match operation.state.as_str() {
                "completed" => Ok(SessionHistoryCompactionPreparationResult::Completed(
                    CompactSessionOutcome::Compacted {
                        source_generation: operation.source_generation,
                        result_generation: operation.result_generation.ok_or_else(|| {
                            invalid_data("completed compact generation is missing")
                        })?,
                        compacted_message_count: operation.compacted_message_count.ok_or_else(
                            || invalid_data("completed compact message count is missing"),
                        )?,
                        retained_message_count: operation.retained_message_count.ok_or_else(
                            || invalid_data("completed compact retained count is missing"),
                        )?,
                    },
                )),
                "no_op" => Ok(SessionHistoryCompactionPreparationResult::Completed(
                    CompactSessionOutcome::NoOp,
                )),
                "cancelled" => Ok(SessionHistoryCompactionPreparationResult::Completed(
                    CompactSessionOutcome::Cancelled,
                )),
                "interrupted" => {
                    self.connection
                        .execute(
                            "DELETE FROM session_history_operations WHERE operation_id = ?1",
                            [preparation.operation_id.as_str()],
                        )
                        .map_err(|source| {
                            database_write_error(
                                "interrupted compact receipt could not be reset",
                                source,
                            )
                        })?;
                    self.begin_session_compaction(preparation)
                }
                "preparing" => Err(conflict("session compaction is already preparing")),
                _ => Err(conflict(
                    "session history operation has a different outcome",
                )),
            };
        }
        self.begin_session_compaction(preparation)
    }

    fn begin_session_compaction(
        &mut self,
        preparation: SessionHistoryCompactionPreparation,
    ) -> StorageResult<SessionHistoryCompactionPreparationResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("compact preparation transaction could not begin", source)
            })?;
        let (lifecycle, generation) = transaction
            .query_row(
                "SELECT lifecycle, body_generation FROM sessions WHERE session_id = ?1",
                [preparation.session_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(|source| internal_error("compact session could not be queried", source))?
            .ok_or_else(|| conflict("compact session does not exist"))?;
        if lifecycle != "active"
            || generation
                != to_i64(
                    preparation.expected_generation,
                    "compact source generation exceeds storage range",
                )?
        {
            return Err(conflict("compact session snapshot changed"));
        }
        ensure_session_history_idle(&transaction, &preparation.session_id)?;
        transaction
            .execute(
                "INSERT INTO session_history_operations (
                    operation_id, session_id, kind, state, source_generation,
                    result_generation, compacted_message_count, retained_message_count,
                    created_at_ms, finished_at_ms
                 ) VALUES (?1, ?2, 'compact', 'preparing', ?3, NULL, NULL, NULL, ?4, NULL)",
                params![
                    preparation.operation_id.as_str(),
                    preparation.session_id.as_str(),
                    to_i64(
                        preparation.expected_generation,
                        "compact source generation exceeds storage range"
                    )?,
                    preparation.created_at_ms,
                ],
            )
            .map_err(|source| {
                database_write_error("compact preparation receipt could not be created", source)
            })?;
        transaction.commit().map_err(|source| {
            database_write_error("compact preparation transaction could not commit", source)
        })?;
        Ok(SessionHistoryCompactionPreparationResult::Prepared)
    }

    pub(super) fn finish_session_compaction(
        &mut self,
        finish: SessionHistoryCompactionFinish,
    ) -> StorageResult<()> {
        let state = match finish.kind {
            SessionHistoryCompactionFinishKind::NoOp => "no_op",
            SessionHistoryCompactionFinishKind::Cancelled => "cancelled",
            SessionHistoryCompactionFinishKind::Interrupted => "interrupted",
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("compact finish transaction could not begin", source)
            })?;
        let operation = transaction
            .query_row(
                "SELECT session_id, kind, state, source_generation
                 FROM session_history_operations WHERE operation_id = ?1",
                [finish.operation_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| internal_error("compact receipt could not be queried", source))?
            .ok_or_else(|| conflict("compact receipt does not exist"))?;
        if operation.0 != finish.session_id.as_str()
            || operation.1 != "compact"
            || positive_u64(operation.3, "compact source generation is invalid")?
                != finish.expected_generation
        {
            return Err(conflict("session history operation identity was reused"));
        }
        if operation.2 == state {
            return Ok(());
        }
        if operation.2 != "preparing" {
            return Err(conflict("compact receipt already has a different outcome"));
        }
        let generation = transaction
            .query_row(
                "SELECT body_generation FROM sessions WHERE session_id = ?1",
                [finish.session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| internal_error("compact generation could not be queried", source))?;
        if positive_u64(generation, "compact generation is invalid")? != finish.expected_generation
        {
            return Err(conflict("compact generation changed before finish"));
        }
        ensure_session_history_idle(&transaction, &finish.session_id)?;
        transaction
            .execute(
                "UPDATE session_history_operations
                 SET state = ?2, finished_at_ms = ?3
                 WHERE operation_id = ?1 AND state = 'preparing'",
                params![finish.operation_id.as_str(), state, finish.finished_at_ms],
            )
            .map_err(|source| database_write_error("compact receipt could not finish", source))?;
        transaction.commit().map_err(|source| {
            database_write_error("compact finish transaction could not commit", source)
        })
    }

    /// 在读取任何 Session/append 投影前收敛 clear 的文件阶段。
    pub(super) fn recover_session_history_operations(&mut self) -> StorageResult<BTreeSet<String>> {
        let operations = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT operation_id, session_id, kind, state, source_generation,
                            result_generation, compacted_message_count, retained_message_count
                     FROM session_history_operations
                     WHERE state IN ('preparing', 'cleanup_pending')
                     ORDER BY created_at_ms, operation_id",
                )
                .map_err(|source| {
                    internal_error("session history operations could not be queried", source)
                })?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                    ))
                })
                .map_err(|source| {
                    internal_error("session history operations could not be read", source)
                })?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|source| {
                    internal_error("session history operation row could not be read", source)
                })?
                .into_iter()
                .map(
                    |(
                        operation_id,
                        session_id,
                        kind,
                        state,
                        source,
                        result,
                        compacted,
                        retained,
                    )| {
                        Ok((
                            operation_id,
                            HistoryOperation {
                                session_id,
                                kind,
                                state,
                                source_generation: positive_u64(
                                    source,
                                    "stored history source generation is invalid",
                                )?,
                                result_generation: result
                                    .map(|value| {
                                        positive_u64(
                                            value,
                                            "stored history result generation is invalid",
                                        )
                                    })
                                    .transpose()?,
                                compacted_message_count: compacted
                                    .map(|value| {
                                        non_negative_u64(
                                            value,
                                            "stored compact message count is invalid",
                                        )
                                    })
                                    .transpose()?,
                                retained_message_count: retained
                                    .map(|value| {
                                        non_negative_u64(
                                            value,
                                            "stored compact retained count is invalid",
                                        )
                                    })
                                    .transpose()?,
                            },
                        ))
                    },
                )
                .collect::<StorageResult<Vec<_>>>()?
        };
        let mut pending_clear_sessions = BTreeSet::new();
        for (operation_id, operation) in operations {
            let session_id = SessionId::new(operation.session_id.clone())
                .map_err(|_| invalid_data("stored history session id is invalid"))?;
            let current_generation = self.session_generation(&session_id)?;
            match (operation.kind.as_str(), operation.state.as_str()) {
                ("clear", "preparing") => {
                    if current_generation != operation.source_generation {
                        return Err(invalid_data(
                            "preparing clear does not match the authoritative generation",
                        ));
                    }
                    if let Some(result_generation) = operation.result_generation {
                        let candidate =
                            body_path(&self.session_directory(&session_id)?, result_generation);
                        match fs::symlink_metadata(&candidate) {
                            Ok(metadata) if metadata.file_type().is_file() => {
                                fs::remove_file(&candidate).map_err(|source| {
                                    internal_error(
                                        "interrupted clear file could not be removed",
                                        source,
                                    )
                                })?;
                                sync_directory(candidate.parent().ok_or_else(|| {
                                    invalid_data("clear file parent is missing")
                                })?)?;
                            }
                            Ok(_) => {
                                return Err(invalid_data(
                                    "interrupted clear path is not a regular file",
                                ));
                            }
                            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                            Err(source) => {
                                return Err(internal_error(
                                    "interrupted clear file could not be inspected",
                                    source,
                                ));
                            }
                        }
                    }
                    self.mark_clear_interrupted(&operation_id, 0)?;
                }
                ("clear", "cleanup_pending") => {
                    let result_generation = operation.result_generation.ok_or_else(|| {
                        invalid_data("pending clear result generation is missing")
                    })?;
                    if current_generation != result_generation {
                        return Err(invalid_data(
                            "pending clear does not match the authoritative generation",
                        ));
                    }
                    let cleanup_status =
                        self.finish_clear_cleanup(&operation_id, &session_id, result_generation, 0);
                    if cleanup_status == SessionHistoryCleanupStatus::Pending {
                        pending_clear_sessions.insert(session_id.as_str().to_owned());
                    }
                }
                (_, "preparing") => {
                    self.mark_clear_interrupted(&operation_id, 0)?;
                }
                _ => {}
            }
        }
        Ok(pending_clear_sessions)
    }

    fn finish_clear_cleanup(
        &mut self,
        operation_id: &str,
        session_id: &SessionId,
        result_generation: u64,
        finished_at_ms: i64,
    ) -> SessionHistoryCleanupStatus {
        if self
            .cleanup_cleared_session_files(session_id, result_generation)
            .is_err()
        {
            return SessionHistoryCleanupStatus::Pending;
        }
        if self
            .connection
            .execute(
                "UPDATE session_history_operations
                 SET state = 'completed', finished_at_ms = ?2
                 WHERE operation_id = ?1 AND state = 'cleanup_pending'",
                params![operation_id, finished_at_ms],
            )
            .is_err()
        {
            return SessionHistoryCleanupStatus::Pending;
        }
        SessionHistoryCleanupStatus::Completed
    }

    fn cleanup_cleared_session_files(
        &mut self,
        session_id: &SessionId,
        result_generation: u64,
    ) -> StorageResult<()> {
        let session_directory = self.session_directory(session_id)?;
        let mut old_bodies = Vec::new();
        let mut child_files = Vec::new();
        let mut child_directories = Vec::new();
        let mut tool_images = Vec::new();

        for entry in fs::read_dir(&session_directory)
            .map_err(|source| internal_error("clear session directory could not be read", source))?
        {
            let entry = entry.map_err(|source| {
                internal_error("clear session directory entry could not be read", source)
            })?;
            let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
                return Err(invalid_data("clear session entry name is invalid"));
            };
            let Some(raw_generation) = file_name
                .strip_prefix("conversation.")
                .and_then(|value| value.strip_suffix(".jsonl"))
            else {
                continue;
            };
            let generation = raw_generation
                .parse::<u64>()
                .ok()
                .filter(|generation| *generation > 0)
                .ok_or_else(|| invalid_data("clear conversation file name is invalid"))?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(|source| {
                internal_error("clear conversation metadata could not be read", source)
            })?;
            if !metadata.file_type().is_file() {
                return Err(invalid_data(
                    "clear conversation path is not a regular file",
                ));
            }
            if generation != result_generation {
                old_bodies.push(entry.path());
            }
        }
        let authoritative_body = body_path(&session_directory, result_generation);
        if !fs::symlink_metadata(&authoritative_body)
            .map(|metadata| metadata.file_type().is_file())
            .unwrap_or(false)
        {
            return Err(invalid_data(
                "clear authoritative conversation file is missing",
            ));
        }

        let children_root = session_directory.join("child-tasks");
        if children_root.exists() {
            if !fs::symlink_metadata(&children_root)
                .map_err(|source| {
                    internal_error("clear child directory metadata could not be read", source)
                })?
                .file_type()
                .is_dir()
            {
                return Err(invalid_data("clear child path is not a directory"));
            }
            for child in fs::read_dir(&children_root).map_err(|source| {
                internal_error("clear child directory could not be read", source)
            })? {
                let child = child.map_err(|source| {
                    internal_error("clear child entry could not be read", source)
                })?;
                let child_name = child
                    .file_name()
                    .into_string()
                    .map_err(|_| invalid_data("clear child id is invalid"))?;
                let child_id = ChildTaskId::new(child_name)
                    .map_err(|_| invalid_data("clear child id is invalid"))?;
                super::filesystem::validate_child_task_component(&child_id)?;
                if !fs::symlink_metadata(child.path())
                    .map_err(|source| {
                        internal_error("clear child metadata could not be read", source)
                    })?
                    .file_type()
                    .is_dir()
                {
                    return Err(invalid_data("clear child path is not a directory"));
                }
                for body in fs::read_dir(child.path()).map_err(|source| {
                    internal_error("clear child body directory could not be read", source)
                })? {
                    let body = body.map_err(|source| {
                        internal_error("clear child body entry could not be read", source)
                    })?;
                    let body_name = body
                        .file_name()
                        .into_string()
                        .map_err(|_| invalid_data("clear child body name is invalid"))?;
                    if body_name
                        .strip_prefix("body-")
                        .and_then(|value| value.strip_suffix(".jsonl"))
                        .and_then(|value| value.parse::<u64>().ok())
                        .filter(|generation| *generation > 0)
                        .is_none()
                    {
                        return Err(invalid_data("clear child body name is invalid"));
                    }
                    if !fs::symlink_metadata(body.path())
                        .map_err(|source| {
                            internal_error("clear child body metadata could not be read", source)
                        })?
                        .file_type()
                        .is_file()
                    {
                        return Err(invalid_data("clear child body is not a regular file"));
                    }
                    child_files.push(body.path());
                }
                child_directories.push(child.path());
            }
        }

        let tool_image_directory = session_directory.join("tool-images");
        for image in fs::read_dir(&tool_image_directory).map_err(|source| {
            internal_error("clear tool image directory could not be read", source)
        })? {
            let image = image.map_err(|source| {
                internal_error("clear tool image entry could not be read", source)
            })?;
            let file_name = image
                .file_name()
                .into_string()
                .map_err(|_| invalid_data("clear tool image name is invalid"))?;
            if !file_name.ends_with(".part")
                && super::tool_images::reference_from_file_name(&file_name).is_none()
            {
                return Err(invalid_data("clear tool image name is invalid"));
            }
            if !fs::symlink_metadata(image.path())
                .map_err(|source| {
                    internal_error("clear tool image metadata could not be read", source)
                })?
                .file_type()
                .is_file()
            {
                return Err(invalid_data("clear tool image is not a regular file"));
            }
            tool_images.push(image.path());
        }

        for path in old_bodies {
            fs::remove_file(path).map_err(|source| {
                internal_error("old clear conversation could not be removed", source)
            })?;
        }
        for path in child_files {
            fs::remove_file(path).map_err(|source| {
                internal_error("clear child body could not be removed", source)
            })?;
        }
        for path in child_directories {
            fs::remove_dir(path).map_err(|source| {
                internal_error("clear child directory could not be removed", source)
            })?;
        }
        if children_root.exists() {
            fs::remove_dir(&children_root).map_err(|source| {
                internal_error("clear child root could not be removed", source)
            })?;
            prepare_private_directory(&children_root).map_err(|source| {
                internal_error("clear child root could not be recreated", source)
            })?;
            sync_directory(&children_root)?;
        }
        for path in tool_images {
            fs::remove_file(path).map_err(|source| {
                internal_error("clear tool image could not be removed", source)
            })?;
        }
        sync_directory(&tool_image_directory)?;
        sync_directory(&session_directory)?;
        self.conversation_indexes.remove_under(&session_directory);
        Ok(())
    }

    fn history_operation(&self, operation_id: &str) -> StorageResult<Option<HistoryOperation>> {
        self.connection
            .query_row(
                "SELECT session_id, kind, state, source_generation, result_generation,
                        compacted_message_count, retained_message_count
                 FROM session_history_operations WHERE operation_id = ?1",
                [operation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| internal_error("history operation could not be queried", source))?
            .map(|row| {
                Ok(HistoryOperation {
                    session_id: row.0,
                    kind: row.1,
                    state: row.2,
                    source_generation: positive_u64(
                        row.3,
                        "stored history source generation is invalid",
                    )?,
                    result_generation: row
                        .4
                        .map(|value| {
                            positive_u64(value, "stored history result generation is invalid")
                        })
                        .transpose()?,
                    compacted_message_count: row
                        .5
                        .map(|value| {
                            non_negative_u64(value, "stored compact message count is invalid")
                        })
                        .transpose()?,
                    retained_message_count: row
                        .6
                        .map(|value| {
                            non_negative_u64(value, "stored compact retained count is invalid")
                        })
                        .transpose()?,
                })
            })
            .transpose()
    }

    fn validate_clear_retry(
        &self,
        clear: &SessionHistoryClear,
        operation: &HistoryOperation,
    ) -> StorageResult<()> {
        if operation.kind != "clear"
            || operation.session_id != clear.session_id.as_str()
            || operation.source_generation != clear.expected_generation
        {
            return Err(conflict("session history operation identity was reused"));
        }
        Ok(())
    }

    fn validate_compaction_identity(
        &self,
        preparation: &SessionHistoryCompactionPreparation,
        operation: &HistoryOperation,
    ) -> StorageResult<()> {
        if operation.kind != "compact"
            || operation.session_id != preparation.session_id.as_str()
            || operation.source_generation != preparation.expected_generation
        {
            return Err(conflict("session history operation identity was reused"));
        }
        Ok(())
    }

    fn clear_result(
        &self,
        session_id: &SessionId,
        source_generation: u64,
        result_generation: u64,
        cleanup_status: SessionHistoryCleanupStatus,
    ) -> StorageResult<SessionHistoryClearResult> {
        let session = self
            .load_sessions()?
            .into_iter()
            .find(|session| &session.session_id == session_id)
            .ok_or_else(|| conflict("clear session does not exist"))?;
        if session.body_generation != result_generation {
            return Err(conflict("clear result generation changed"));
        }
        Ok(SessionHistoryClearResult {
            session,
            source_generation,
            result_generation,
            cleanup_status,
        })
    }

    fn mark_clear_interrupted(&self, operation_id: &str, finished_at_ms: i64) -> StorageResult<()> {
        self.connection
            .execute(
                "UPDATE session_history_operations
                 SET state = 'interrupted', finished_at_ms = ?2
                 WHERE operation_id = ?1 AND state = 'preparing'",
                params![operation_id, finished_at_ms],
            )
            .map_err(|source| {
                database_write_error("clear receipt could not be interrupted", source)
            })?;
        Ok(())
    }
}

pub(super) fn ensure_session_history_idle(
    transaction: &Transaction<'_>,
    session_id: &SessionId,
) -> StorageResult<()> {
    let busy = transaction
        .query_row(
            "SELECT
                EXISTS(SELECT 1 FROM inputs WHERE session_id = ?1 AND state = 'queued')
                OR EXISTS(SELECT 1 FROM runs WHERE session_id = ?1
                          AND status NOT IN ('completed', 'failed', 'cancelled', 'interrupted'))
                OR EXISTS(SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?1)
                OR EXISTS(SELECT 1 FROM body_appends WHERE session_id = ?1)
                OR EXISTS(SELECT 1 FROM child_tasks WHERE session_id = ?1
                          AND status NOT IN ('completed', 'failed', 'cancelled', 'interrupted'))
                OR EXISTS(SELECT 1 FROM child_pending_tool_exchanges WHERE session_id = ?1)
                OR EXISTS(SELECT 1 FROM child_body_appends WHERE session_id = ?1)",
            [session_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|source| internal_error("session idle state could not be queried", source))?;
    if busy != 0 {
        return Err(conflict(
            "session history operation requires an idle session",
        ));
    }
    Ok(())
}

fn role_value(role: SessionRole) -> &'static str {
    match role {
        SessionRole::Standard => "standard",
        SessionRole::Controller => "controller",
    }
}
