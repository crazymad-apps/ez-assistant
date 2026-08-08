//! 跨 SQLite 与 Conversation 文件的 staged append 和 generation 切换。

use std::{collections::HashSet, fs, io::BufReader};

use agent_types::{ConversationMessage, ConversationSnapshot};
use assistant_protocol::{RunId, SessionId};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult,
    append_effect::{AppendPurpose, apply_purpose, decode_purpose, encode_purpose},
    body_path, conflict, conversation, database_write_error, internal_error, invalid_data,
    non_negative_u64, sync_directory, to_i64,
};

pub(super) struct AppendRequest {
    pub operation_id: String,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub messages: Vec<ConversationMessage>,
    pub created_at_ms: i64,
}

pub(super) struct ReplacementPlan {
    pub session_id: SessionId,
    pub previous_generation: u64,
    pub new_generation: u64,
    pub message_count: u64,
    pub updated_at_ms: i64,
}

pub(super) struct StagedAppend {
    pub(super) operation_id: String,
    pub(super) session_id: SessionId,
    pub(super) run_id: RunId,
    pub(super) body_generation: u64,
    pub(super) base_byte_length: u64,
    pub(super) payload: Vec<u8>,
    pub(super) message_count_delta: u64,
    pub(super) created_at_ms: i64,
    pub(super) purpose: AppendPurpose,
}

impl StorageEngine {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "generic staged append is exercised by recovery tests"
        )
    )]
    pub(super) fn append_messages(&mut self, request: AppendRequest) -> StorageResult<()> {
        let operation_id = request.operation_id.clone();
        self.stage_append(request)?;
        self.complete_staged_append(&operation_id)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "interruption tests stop after the staging boundary"
        )
    )]
    pub(super) fn stage_append(&mut self, request: AppendRequest) -> StorageResult<()> {
        self.stage_append_for(request, AppendPurpose::Messages)
    }

    pub(super) fn stage_append_for(
        &mut self,
        request: AppendRequest,
        purpose: AppendPurpose,
    ) -> StorageResult<()> {
        if request.operation_id.trim().is_empty() || request.messages.is_empty() {
            return Err(assistant_runtime::StoreError::new(
                assistant_runtime::StoreErrorKind::InvalidInput,
                "staged conversation append is invalid",
            ));
        }
        let current = self.load_conversation(&request.session_id)?;
        conversation::validate_candidate(&current, &request.messages)?;
        let payload = conversation::encode_messages(&request.messages)?;
        let generation = self.session_generation(&request.session_id)?;
        let path = body_path(&self.session_directory(&request.session_id)?, generation);
        let base_byte_length = fs::metadata(path)
            .map_err(|source| internal_error("conversation metadata could not be read", source))?
            .len();
        let message_count_delta = u64::try_from(request.messages.len()).map_err(|source| {
            assistant_runtime::StoreError::with_source(
                assistant_runtime::StoreErrorKind::InvalidInput,
                "conversation message batch is too large",
                source,
            )
        })?;

        let kind = encode_purpose(&purpose)?;
        self.connection
            .execute(
                "INSERT INTO body_appends (
                    operation_id, session_id, run_id, body_generation, base_byte_length,
                    kind, payload, message_count_delta, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    request.operation_id,
                    request.session_id.as_str(),
                    request.run_id.as_str(),
                    to_i64(generation, "body generation exceeds SQLite range")?,
                    to_i64(base_byte_length, "conversation file exceeds SQLite range")?,
                    kind,
                    payload,
                    to_i64(message_count_delta, "message count exceeds SQLite range")?,
                    request.created_at_ms,
                ],
            )
            .map_err(|source| {
                database_write_error("conversation append could not be staged", source)
            })?;
        Ok(())
    }

    pub(super) fn write_staged_append(&self, operation_id: &str) -> StorageResult<()> {
        let staged = self.staged_append(operation_id)?;
        self.verify_staged_generation(&staged)?;
        let path = body_path(
            &self.session_directory(&staged.session_id)?,
            staged.body_generation,
        );
        conversation::reconcile_append(&path, staged.base_byte_length, &staged.payload)
    }

    pub(super) fn finalize_staged_append(&mut self, operation_id: &str) -> StorageResult<()> {
        let staged = self.staged_append(operation_id)?;
        self.verify_staged_generation(&staged)?;
        if staged.operation_id != operation_id {
            return Err(invalid_data("staged append identity is inconsistent"));
        }
        let batch = conversation::decode(BufReader::new(staged.payload.as_slice()))?;
        let actual_delta = u64::try_from(batch.messages.len()).map_err(|source| {
            assistant_runtime::StoreError::with_source(
                assistant_runtime::StoreErrorKind::InvalidData,
                "staged append message count is invalid",
                source,
            )
        })?;
        if actual_delta != staged.message_count_delta {
            return Err(invalid_data(
                "staged append message count does not match payload",
            ));
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("conversation append could not begin finalization", source)
            })?;
        let updated = transaction
            .execute(
                "UPDATE sessions
                 SET message_count = message_count + ?1, updated_at_ms = ?2
                 WHERE session_id = ?3 AND body_generation = ?4",
                params![
                    to_i64(
                        staged.message_count_delta,
                        "message count exceeds SQLite range"
                    )?,
                    staged.created_at_ms,
                    staged.session_id.as_str(),
                    to_i64(
                        staged.body_generation,
                        "body generation exceeds SQLite range"
                    )?,
                ],
            )
            .map_err(|source| {
                internal_error("conversation append metadata could not be updated", source)
            })?;
        if updated != 1 {
            return Err(invalid_data(
                "staged append generation is no longer authoritative",
            ));
        }
        for message in &batch.messages {
            transaction
                .execute(
                    "INSERT INTO run_message_refs (run_id, message_id) VALUES (?1, ?2)",
                    params![
                        staged.run_id.as_str(),
                        conversation::message_id(message).as_str()
                    ],
                )
                .map_err(|source| {
                    internal_error("run message reference could not be recorded", source)
                })?;
        }
        apply_purpose(
            &transaction,
            &staged.purpose,
            &staged.run_id,
            &staged.session_id,
            staged.created_at_ms,
        )?;
        let deleted = transaction
            .execute(
                "DELETE FROM body_appends WHERE operation_id = ?1",
                [operation_id],
            )
            .map_err(|source| internal_error("staged append could not be cleared", source))?;
        if deleted != 1 {
            return Err(invalid_data(
                "staged append disappeared during finalization",
            ));
        }
        transaction
            .commit()
            .map_err(|source| internal_error("conversation append could not be finalized", source))
    }

    pub(super) fn complete_staged_append(&mut self, operation_id: &str) -> StorageResult<()> {
        self.write_staged_append(operation_id)?;
        self.finalize_staged_append(operation_id)
    }

    pub(super) fn begin_replacement(
        &mut self,
        session_id: SessionId,
        snapshot: ConversationSnapshot,
        updated_at_ms: i64,
    ) -> StorageResult<ReplacementPlan> {
        // Round-trip through the same reader used after restart. This checks duplicate IDs, Tool
        // pairing and the exact bytes before a new generation can become authoritative.
        let payload = conversation::encode_messages(&snapshot.messages)?;
        conversation::decode(BufReader::new(payload.as_slice()))?;
        let previous_generation = self.session_generation(&session_id)?;
        let session_directory = self.session_directory(&session_id)?;
        let mut new_generation = previous_generation
            .checked_add(1)
            .ok_or_else(|| conflict("conversation generation is exhausted"))?;
        while body_path(&session_directory, new_generation).exists() {
            new_generation = new_generation
                .checked_add(1)
                .ok_or_else(|| conflict("conversation generation is exhausted"))?;
        }
        let new_path = body_path(&session_directory, new_generation);
        conversation::write_replacement(&new_path, &payload)?;
        sync_directory(&session_directory)?;

        Ok(ReplacementPlan {
            session_id,
            previous_generation,
            new_generation,
            message_count: u64::try_from(snapshot.messages.len()).map_err(|source| {
                assistant_runtime::StoreError::with_source(
                    assistant_runtime::StoreErrorKind::InvalidInput,
                    "replacement conversation is too large",
                    source,
                )
            })?,
            updated_at_ms,
        })
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "history replacement is connected in M4")
    )]
    pub(super) fn commit_replacement(&mut self, plan: &ReplacementPlan) -> StorageResult<()> {
        let session_directory = self.session_directory(&plan.session_id)?;
        let new_path = body_path(&session_directory, plan.new_generation);
        conversation::read(&new_path)?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("conversation generation switch could not begin", source)
            })?;
        let updated = transaction
            .execute(
                "UPDATE sessions
                 SET body_generation = ?1, message_count = ?2, updated_at_ms = ?3
                 WHERE session_id = ?4 AND body_generation = ?5",
                params![
                    to_i64(plan.new_generation, "body generation exceeds SQLite range")?,
                    to_i64(plan.message_count, "message count exceeds SQLite range")?,
                    plan.updated_at_ms,
                    plan.session_id.as_str(),
                    to_i64(
                        plan.previous_generation,
                        "body generation exceeds SQLite range"
                    )?,
                ],
            )
            .map_err(|source| {
                internal_error("conversation generation could not be switched", source)
            })?;
        if updated != 1 {
            return Err(conflict(
                "conversation generation changed before replacement commit",
            ));
        }
        transaction.commit().map_err(|source| {
            internal_error(
                "conversation generation switch could not be committed",
                source,
            )
        })?;

        // 旧 generation 已不再是权威正文；删除失败只留下不可见孤立文件，不回滚已提交切换。
        let old_path = body_path(&session_directory, plan.previous_generation);
        if fs::remove_file(old_path).is_ok() {
            sync_directory(&session_directory)?;
        }
        Ok(())
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "staged operation count is a recovery test assertion"
        )
    )]
    pub(super) fn staged_append_count(&self) -> StorageResult<u64> {
        let count = self
            .connection
            .query_row("SELECT COUNT(*) FROM body_appends", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|source| internal_error("staged append count could not be read", source))?;
        non_negative_u64(count, "staged append count is invalid")
    }

    pub(super) fn recover_body_appends(&mut self) -> StorageResult<HashSet<String>> {
        let pending = {
            let mut statement = self
                .connection
                .prepare("SELECT operation_id, session_id FROM body_appends ORDER BY created_at_ms, operation_id")
                .map_err(|source| internal_error("staged appends could not be queried", source))?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|source| internal_error("staged appends could not be read", source))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|source| internal_error("staged append row could not be read", source))?
        };

        let mut unavailable = HashSet::new();
        for (operation_id, session_id) in pending {
            if self.complete_staged_append(&operation_id).is_err() {
                unavailable.insert(session_id);
            }
        }
        Ok(unavailable)
    }

    fn staged_append(&self, operation_id: &str) -> StorageResult<StagedAppend> {
        let row = self
            .connection
            .query_row(
                "SELECT operation_id, session_id, run_id, body_generation, base_byte_length,
                        kind, payload, message_count_delta, created_at_ms
                 FROM body_appends WHERE operation_id = ?1",
                [operation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Vec<u8>>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| internal_error("staged append could not be queried", source))?
            .ok_or_else(|| conflict("staged append does not exist"))?;
        let purpose = decode_purpose(&row.5)?;
        Ok(StagedAppend {
            operation_id: row.0,
            session_id: SessionId::new(row.1).map_err(|source| {
                super::invalid_data_with_source("staged append session id is invalid", source)
            })?,
            run_id: RunId::new(row.2).map_err(|source| {
                super::invalid_data_with_source("staged append run id is invalid", source)
            })?,
            body_generation: super::positive_u64(row.3, "staged append generation is invalid")?,
            base_byte_length: non_negative_u64(row.4, "staged append base length is invalid")?,
            payload: row.6,
            message_count_delta: super::positive_u64(
                row.7,
                "staged append message count is invalid",
            )?,
            created_at_ms: row.8,
            purpose,
        })
    }

    fn verify_staged_generation(&self, staged: &StagedAppend) -> StorageResult<()> {
        if self.session_generation(&staged.session_id)? != staged.body_generation {
            return Err(invalid_data(
                "staged append targets a non-authoritative generation",
            ));
        }
        Ok(())
    }
}
