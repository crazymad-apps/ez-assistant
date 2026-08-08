//! Session 归档、模型切换与历史重新输入的业务原子操作。

use std::fs;

use assistant_runtime::{
    ArchiveChange, ConversationRewrite, ModelChange, RewriteResult, StoredInput, StoredInputState,
    StoredRun,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, body_path, conflict, conversation, database_write_error,
    internal_error, recovery::ReplacementPlan, sync_directory, to_i64,
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
                 SET lifecycle = ?1, archived_at_ms = ?2, updated_at_ms = ?3
                 WHERE session_id = ?4 AND lifecycle = ?5
                   AND (?1 = 'active' OR (
                     NOT EXISTS (SELECT 1 FROM inputs WHERE session_id = ?4 AND state = 'queued')
                     AND NOT EXISTS (SELECT 1 FROM runs WHERE session_id = ?4 AND status IN ('accepted', 'running', 'cancelling'))
                     AND NOT EXISTS (SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?4)
                   ))",
                params![
                    to,
                    archived_at,
                    change.changed_at_ms,
                    change.session_id.as_str(),
                    from,
                ],
            )
            .map_err(|source| {
                database_write_error("session archive state could not be changed", source)
            })?;
        if changed != 1 {
            return Err(conflict("session lifecycle cannot be changed"));
        }
        Ok(())
    }

    pub(super) fn set_session_model(&mut self, change: ModelChange) -> StorageResult<()> {
        let changed = self
            .connection
            .execute(
                "UPDATE sessions SET model_key = ?1, updated_at_ms = ?2
                 WHERE session_id = ?3 AND lifecycle = 'active'
                   AND NOT EXISTS (SELECT 1 FROM inputs WHERE session_id = ?3 AND state = 'queued')
                   AND NOT EXISTS (SELECT 1 FROM runs WHERE session_id = ?3 AND status IN ('accepted', 'running', 'cancelling'))
                   AND NOT EXISTS (SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?3)",
                params![
                    change.model_key.as_str(),
                    change.changed_at_ms,
                    change.session_id.as_str(),
                ],
            )
            .map_err(|source| {
                database_write_error("session model could not be changed", source)
            })?;
        if changed != 1 {
            return Err(conflict("session model cannot be changed"));
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
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("conversation rewrite could not begin", source))?;
        let updated = transaction
            .execute(
                "UPDATE sessions
                 SET body_generation = ?1, message_count = ?2, updated_at_ms = ?3
                 WHERE session_id = ?4 AND body_generation = ?5 AND lifecycle = 'active'
                   AND NOT EXISTS (SELECT 1 FROM inputs WHERE session_id = ?4 AND state = 'queued')
                   AND NOT EXISTS (SELECT 1 FROM runs WHERE session_id = ?4 AND status IN ('accepted', 'running', 'cancelling'))
                   AND NOT EXISTS (SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?4)",
                params![
                    to_i64(plan.new_generation, "body generation exceeds SQLite range")?,
                    to_i64(plan.message_count, "message count exceeds SQLite range")?,
                    plan.updated_at_ms,
                    plan.session_id.as_str(),
                    to_i64(plan.previous_generation, "body generation exceeds SQLite range")?,
                ],
            )
            .map_err(|source| internal_error("conversation generation could not be switched", source))?;
        if updated != 1 {
            return Err(conflict("session is not available for history replacement"));
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
            "INSERT INTO inputs (input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms)
             VALUES (?1, ?2, ?3, ?4, 'committed', NULL, ?5)",
            params![
                rewrite.input.input_id.as_str(),
                rewrite.session_id.as_str(),
                rewrite.input.idempotency_key.as_ref().map(assistant_protocol::IdempotencyKey::as_str),
                new_message_id.as_str(),
                rewrite.input.accepted_at_ms,
            ],
        ).map_err(|source| database_write_error("replacement input could not be created", source))?;
        let queue_order = u64::try_from(transaction.last_insert_rowid())
            .map_err(|source| internal_error("queue order exceeds runtime range", source))?;
        transaction.execute(
            "INSERT INTO runs (run_id, session_id, input_id, attempt, status, cancel_requested, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms)
             VALUES (?1, ?2, ?3, 1, 'accepted', 0, NULL, NULL, ?4, NULL, NULL)",
            params![
                rewrite.input.run_id.as_str(),
                rewrite.session_id.as_str(),
                rewrite.input.input_id.as_str(),
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
            cancel_requested: false,
            error: None,
            message_ids: vec![new_message_id.clone()],
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
        Ok(RewriteResult { input, run })
    }
}
