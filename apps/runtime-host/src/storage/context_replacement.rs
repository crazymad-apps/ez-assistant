//! 活动父 Run/子任务的上下文压缩 generation 切换。

use std::fs;

use assistant_protocol::ConversationOwner;
use assistant_runtime::{ContextReplacement, ContextReplacementResult, ContextReplacementTarget};
use rusqlite::{TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, body_path, child_body_path, child_task_directory, conflict,
    conversation, database_write_error, internal_error,
    session_history::ensure_session_history_idle, sync_directory, to_i64,
};

impl StorageEngine {
    pub(super) fn replace_context(
        &mut self,
        replacement: ContextReplacement,
    ) -> StorageResult<ContextReplacementResult> {
        match replacement.target {
            ContextReplacementTarget::Run { session_id, run_id } => {
                let active = self
                    .connection
                    .query_row(
                        "SELECT COUNT(*) FROM runs
                     WHERE run_id = ?1 AND session_id = ?2 AND status = 'running'
                       AND NOT EXISTS (
                           SELECT 1 FROM pending_tool_exchanges
                           WHERE run_id = ?1 AND session_id = ?2
                       )",
                        params![run_id.as_str(), session_id.as_str()],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|source| {
                        internal_error("active run context could not be checked", source)
                    })?;
                if active != 1 {
                    return Err(conflict("run context is not replaceable"));
                }
                let plan = self.begin_replacement(
                    session_id,
                    replacement.conversation,
                    replacement.changed_at_ms,
                )?;
                if let Err(error) = self.commit_replacement(&plan) {
                    if let Ok(directory) = self.session_directory(&plan.session_id) {
                        let _ = fs::remove_file(body_path(&directory, plan.new_generation));
                    }
                    return Err(error);
                }
                Ok(ContextReplacementResult {
                    source_generation: plan.previous_generation,
                    result_generation: plan.new_generation,
                })
            }
            ContextReplacementTarget::ChildTask {
                session_id,
                child_task_id,
            } => self.replace_child_context(
                session_id,
                child_task_id,
                replacement.conversation,
                replacement.changed_at_ms,
            ),
            ContextReplacementTarget::IdleSession {
                session_id,
                expected_generation,
                operation_id,
                compacted_message_count,
                retained_message_count,
            } => self.replace_idle_session_context(
                session_id,
                expected_generation,
                operation_id,
                compacted_message_count,
                retained_message_count,
                replacement.conversation,
                replacement.changed_at_ms,
            ),
        }
    }

    fn replace_child_context(
        &mut self,
        session_id: assistant_protocol::SessionId,
        child_task_id: assistant_protocol::ChildTaskId,
        snapshot: agent_types::ConversationSnapshot,
        changed_at_ms: i64,
    ) -> StorageResult<ContextReplacementResult> {
        let payload = conversation::encode_messages(&snapshot.messages)?;
        conversation::decode(std::io::BufReader::new(payload.as_slice()))?;
        let previous_generation = self.child_generation(&session_id, &child_task_id)?;
        let task_directory =
            child_task_directory(&self.session_directory(&session_id)?, &child_task_id);
        let mut new_generation = previous_generation
            .checked_add(1)
            .ok_or_else(|| conflict("child conversation generation is exhausted"))?;
        while child_body_path(&task_directory, new_generation).exists() {
            new_generation = new_generation
                .checked_add(1)
                .ok_or_else(|| conflict("child conversation generation is exhausted"))?;
        }
        let new_path = child_body_path(&task_directory, new_generation);
        conversation::write_replacement(&new_path, &payload)?;
        sync_directory(&task_directory)?;

        let message_count = u64::try_from(snapshot.messages.len()).map_err(|source| {
            assistant_runtime::StoreError::with_source(
                assistant_runtime::StoreErrorKind::InvalidInput,
                "replacement child conversation is too large",
                source,
            )
        })?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("child context replacement could not begin", source)
            })?;
        let updated = transaction
            .execute(
                "UPDATE child_tasks
             SET body_generation = ?1, message_count = ?2
             WHERE child_task_id = ?3 AND session_id = ?4 AND body_generation = ?5
               AND status = 'running'
               AND NOT EXISTS (
                   SELECT 1 FROM child_pending_tool_exchanges
                   WHERE child_task_id = ?3
               )",
                params![
                    to_i64(new_generation, "child body generation exceeds SQLite range")?,
                    to_i64(message_count, "child message count exceeds SQLite range")?,
                    child_task_id.as_str(),
                    session_id.as_str(),
                    to_i64(
                        previous_generation,
                        "child body generation exceeds SQLite range"
                    )?,
                ],
            )
            .map_err(|source| {
                database_write_error("child context could not be switched", source)
            })?;
        if updated != 1 {
            let _ = fs::remove_file(&new_path);
            return Err(conflict("child context is not replaceable"));
        }
        super::usage::record_usage_messages(
            &transaction,
            &ConversationOwner::ChildTask {
                session_id: session_id.clone(),
                child_task_id: child_task_id.clone(),
            },
            None,
            &snapshot.messages,
            changed_at_ms,
            false,
        )?;
        transaction.commit().map_err(|source| {
            database_write_error("child context replacement could not be committed", source)
        })?;

        self.mark_recall_owner_dirty_now(
            &ConversationOwner::ChildTask {
                session_id: session_id.clone(),
                child_task_id: child_task_id.clone(),
            },
            new_generation,
        );

        // SQLite 已指向新 generation；旧文件只做 best-effort 清理。
        if fs::remove_file(child_body_path(&task_directory, previous_generation)).is_ok() {
            let _ = sync_directory(&task_directory);
        }
        Ok(ContextReplacementResult {
            source_generation: previous_generation,
            result_generation: new_generation,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn replace_idle_session_context(
        &mut self,
        session_id: assistant_protocol::SessionId,
        expected_generation: u64,
        operation_id: assistant_protocol::IdempotencyKey,
        compacted_message_count: u64,
        retained_message_count: u64,
        snapshot: agent_types::ConversationSnapshot,
        changed_at_ms: i64,
    ) -> StorageResult<ContextReplacementResult> {
        let plan = self.begin_replacement(session_id.clone(), snapshot, changed_at_ms)?;
        if plan.previous_generation != expected_generation {
            let _ = fs::remove_file(body_path(
                &self.session_directory(&session_id)?,
                plan.new_generation,
            ));
            return Err(conflict("compact session generation changed"));
        }
        let result = self.commit_idle_session_replacement(
            &plan,
            &operation_id,
            compacted_message_count,
            retained_message_count,
        );
        if result.is_err()
            && let Ok(directory) = self.session_directory(&session_id)
        {
            let _ = fs::remove_file(body_path(&directory, plan.new_generation));
        }
        result.map(|()| ContextReplacementResult {
            source_generation: plan.previous_generation,
            result_generation: plan.new_generation,
        })
    }

    fn commit_idle_session_replacement(
        &mut self,
        plan: &super::recovery::ReplacementPlan,
        operation_id: &assistant_protocol::IdempotencyKey,
        compacted_message_count: u64,
        retained_message_count: u64,
    ) -> StorageResult<()> {
        let session_directory = self.session_directory(&plan.session_id)?;
        let new_path = body_path(&session_directory, plan.new_generation);
        let replacement = conversation::read(&new_path)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("idle context replacement could not begin", source))?;
        ensure_session_history_idle(&transaction, &plan.session_id)?;
        let updated = transaction
            .execute(
                "UPDATE sessions
                 SET body_generation = ?1, message_count = ?2
                 WHERE session_id = ?3 AND body_generation = ?4 AND lifecycle = 'active'",
                params![
                    to_i64(plan.new_generation, "body generation exceeds SQLite range")?,
                    to_i64(plan.message_count, "message count exceeds SQLite range")?,
                    plan.session_id.as_str(),
                    to_i64(
                        plan.previous_generation,
                        "body generation exceeds SQLite range"
                    )?,
                ],
            )
            .map_err(|source| database_write_error("idle context could not be switched", source))?;
        if updated != 1 {
            return Err(conflict("idle session context is not replaceable"));
        }
        let receipt_updated = transaction
            .execute(
                "UPDATE session_history_operations
                 SET state = 'completed', result_generation = ?2,
                     compacted_message_count = ?3, retained_message_count = ?4,
                     finished_at_ms = ?5
                 WHERE operation_id = ?1 AND session_id = ?6 AND kind = 'compact'
                   AND state = 'preparing' AND source_generation = ?7",
                params![
                    operation_id.as_str(),
                    to_i64(
                        plan.new_generation,
                        "compact result generation exceeds range"
                    )?,
                    to_i64(
                        compacted_message_count,
                        "compacted message count exceeds range"
                    )?,
                    to_i64(
                        retained_message_count,
                        "retained message count exceeds range"
                    )?,
                    plan.changed_at_ms,
                    plan.session_id.as_str(),
                    to_i64(
                        plan.previous_generation,
                        "compact source generation exceeds range"
                    )?,
                ],
            )
            .map_err(|source| database_write_error("compact receipt could not commit", source))?;
        if receipt_updated != 1 {
            return Err(conflict("compact receipt is not preparing"));
        }
        super::usage::record_usage_messages(
            &transaction,
            &ConversationOwner::MainSession {
                session_id: plan.session_id.clone(),
            },
            None,
            &replacement.messages,
            plan.changed_at_ms,
            false,
        )?;
        transaction.commit().map_err(|source| {
            database_write_error("idle context replacement could not commit", source)
        })?;
        self.mark_recall_owner_dirty_now(
            &ConversationOwner::MainSession {
                session_id: plan.session_id.clone(),
            },
            plan.new_generation,
        );
        if fs::remove_file(body_path(&session_directory, plan.previous_generation)).is_ok() {
            sync_directory(&session_directory)?;
        }
        Ok(())
    }
}
