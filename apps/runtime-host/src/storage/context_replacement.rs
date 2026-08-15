//! 活动父 Run/子任务的上下文压缩 generation 切换。

use std::fs;

use assistant_runtime::{ContextReplacement, ContextReplacementTarget};
use rusqlite::{TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, body_path, child_body_path, child_task_directory, conflict,
    conversation, database_write_error, internal_error, sync_directory, to_i64,
};

impl StorageEngine {
    pub(super) fn replace_context(&mut self, replacement: ContextReplacement) -> StorageResult<()> {
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
                let plan = self.begin_replacement(session_id, replacement.conversation)?;
                if let Err(error) = self.commit_replacement(&plan) {
                    if let Ok(directory) = self.session_directory(&plan.session_id) {
                        let _ = fs::remove_file(body_path(&directory, plan.new_generation));
                    }
                    return Err(error);
                }
                Ok(())
            }
            ContextReplacementTarget::ChildTask {
                session_id,
                child_task_id,
            } => self.replace_child_context(session_id, child_task_id, replacement.conversation),
        }
    }

    fn replace_child_context(
        &mut self,
        session_id: assistant_protocol::SessionId,
        child_task_id: assistant_protocol::ChildTaskId,
        snapshot: agent_types::ConversationSnapshot,
    ) -> StorageResult<()> {
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
        transaction.commit().map_err(|source| {
            database_write_error("child context replacement could not be committed", source)
        })?;

        // SQLite 已指向新 generation；旧文件只做 best-effort 清理。
        if fs::remove_file(child_body_path(&task_directory, previous_generation)).is_ok() {
            let _ = sync_directory(&task_directory);
        }
        Ok(())
    }
}
