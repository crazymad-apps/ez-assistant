//! Input 准入、幂等命中、排队取消与恢复投影。

use agent_types::MessageId;
use assistant_protocol::{IdempotencyKey, InputId, RunStatus, SessionId};
use assistant_runtime::{AcceptedInput, NewStoredInput, StoredInput, StoredInputState, StoredRun};
use rusqlite::{TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, conflict, database_write_error, internal_error, invalid_data,
    invalid_data_with_source,
};

impl StorageEngine {
    pub(super) fn accept_input(&mut self, input: NewStoredInput) -> StorageResult<AcceptedInput> {
        if let Some(key) = input.idempotency_key.as_ref()
            && let Some(existing) = self.load_inputs()?.into_iter().find(|candidate| {
                candidate.session_id == input.session_id
                    && candidate.idempotency_key.as_ref() == Some(key)
            })
        {
            let run = self
                .load_runs()?
                .into_iter()
                .find(|run| run.input_id == existing.input_id && run.attempt == 1)
                .ok_or_else(|| invalid_data("accepted input has no first run"))?;
            return Ok(AcceptedInput {
                input: existing,
                run,
                is_duplicate: true,
            });
        }
        let message_json = serde_json::to_string(&input.message)
            .map_err(|source| internal_error("queued user message could not be encoded", source))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("input acceptance could not begin", source))?;
        transaction.execute("INSERT INTO inputs (input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms) VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6)", params![input.input_id.as_str(), input.session_id.as_str(), input.idempotency_key.as_ref().map(IdempotencyKey::as_str), input.message.id.as_str(), message_json, input.accepted_at_ms]).map_err(|source| database_write_error("input could not be accepted", source))?;
        let queue_order = u64::try_from(transaction.last_insert_rowid())
            .map_err(|source| internal_error("queue order exceeds storage range", source))?;
        transaction.execute("INSERT INTO runs (run_id, session_id, input_id, attempt, status, cancel_requested, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms) VALUES (?1, ?2, ?3, 1, 'accepted', 0, NULL, NULL, ?4, NULL, NULL)", params![input.run_id.as_str(), input.session_id.as_str(), input.input_id.as_str(), input.accepted_at_ms]).map_err(|source| database_write_error("run could not be accepted", source))?;
        transaction.commit().map_err(|source| {
            database_write_error("input acceptance could not be committed", source)
        })?;
        let stored = StoredInput {
            queue_order,
            input_id: input.input_id.clone(),
            session_id: input.session_id.clone(),
            idempotency_key: input.idempotency_key,
            user_message_id: input.message.id.clone(),
            state: StoredInputState::Queued,
            queued_message: Some(input.message),
            accepted_at_ms: input.accepted_at_ms,
        };
        let run = StoredRun {
            run_id: input.run_id,
            session_id: input.session_id,
            input_id: input.input_id,
            attempt: 1,
            status: RunStatus::Accepted,
            cancel_requested: false,
            error: None,
            message_ids: Vec::new(),
            created_at_ms: input.accepted_at_ms,
            started_at_ms: None,
            finished_at_ms: None,
        };
        Ok(AcceptedInput {
            input: stored,
            run,
            is_duplicate: false,
        })
    }

    pub(super) fn cancel_queued_input(
        &mut self,
        session_id: &SessionId,
        input_id: &InputId,
    ) -> StorageResult<()> {
        let changed = self
            .connection
            .execute(
                "DELETE FROM inputs WHERE input_id = ?1 AND session_id = ?2 AND state = 'queued'",
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

    pub(super) fn load_inputs(&self) -> StorageResult<Vec<StoredInput>> {
        let mut statement = self.connection.prepare("SELECT queue_order, input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms FROM inputs ORDER BY queue_order").map_err(|source| internal_error("runtime inputs could not be queried", source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })
            .map_err(|source| internal_error("runtime inputs could not be read", source))?;
        rows.map(|row| {
            let (
                queue_order,
                input_id,
                session_id,
                key,
                message_id,
                state,
                message_json,
                accepted_at_ms,
            ) = row
                .map_err(|source| internal_error("runtime input row could not be read", source))?;
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
            if (state == StoredInputState::Queued) != queued_message.is_some() {
                return Err(invalid_data("stored queued message state is inconsistent"));
            }
            Ok(StoredInput {
                queue_order: u64::try_from(queue_order).map_err(|source| {
                    invalid_data_with_source("stored queue order is invalid", source)
                })?,
                input_id: InputId::new(input_id).map_err(|source| {
                    invalid_data_with_source("stored input id is invalid", source)
                })?,
                session_id: SessionId::new(session_id).map_err(|source| {
                    invalid_data_with_source("stored input session id is invalid", source)
                })?,
                idempotency_key: key.map(IdempotencyKey::new).transpose().map_err(|source| {
                    invalid_data_with_source("stored idempotency key is invalid", source)
                })?,
                user_message_id: MessageId::new(message_id).map_err(|source| {
                    invalid_data_with_source("stored user message id is invalid", source)
                })?,
                state,
                queued_message,
                accepted_at_ms,
            })
        })
        .collect()
    }
}
