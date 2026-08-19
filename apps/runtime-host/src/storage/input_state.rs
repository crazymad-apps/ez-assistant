//! Input 准入、幂等命中、排队取消与恢复投影。

use agent_types::MessageId;
use assistant_protocol::{IdempotencyKey, InputId, RunStatus, SessionId};
use assistant_runtime::{AcceptedInput, NewStoredInput, StoredInput, StoredInputState, StoredRun};
use rusqlite::{TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, conflict, database_write_error, internal_error, invalid_data,
    invalid_data_with_source,
    mode::{agent_variant_value, approval_mode_value, parse_agent_variant},
};

impl StorageEngine {
    pub(super) fn prioritize_queued_input(
        &mut self,
        change: assistant_runtime::QueuePriorityChange,
    ) -> StorageResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("queue priority update could not begin", source))?;
        transaction
            .execute(
                "UPDATE inputs
                 SET priority_order = COALESCE(priority_order, queue_order) + 1
                 WHERE session_id = ?1 AND state = 'queued'",
                [change.session_id.as_str()],
            )
            .map_err(|source| {
                database_write_error("queued input priorities could not be shifted", source)
            })?;
        let changed = transaction
            .execute(
                "UPDATE inputs SET priority_order = ?1 WHERE session_id = ?2 AND input_id = ?3 AND state = 'queued'",
                params![0_i64, change.session_id.as_str(), change.input_id.as_str()],
            )
            .map_err(|source| database_write_error("queued input priority could not be updated", source))?;
        if changed != 1 {
            return Err(conflict("input is not queued"));
        }
        transaction.commit().map_err(|source| {
            database_write_error("queue priority update could not be committed", source)
        })?;
        Ok(())
    }

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
        let priority_order = transaction
            .query_row(
                "SELECT COALESCE(MAX(COALESCE(priority_order, queue_order)), -1) + 1
                 FROM inputs WHERE session_id = ?1",
                [input.session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| internal_error("next queue priority could not be read", source))?;
        transaction.execute("INSERT INTO inputs (priority_order, input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms, agent_variant) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, ?8)", params![priority_order, input.input_id.as_str(), input.session_id.as_str(), input.idempotency_key.as_ref().map(IdempotencyKey::as_str), input.message.id.as_str(), message_json, input.accepted_at_ms, agent_variant_value(input.agent_variant)]).map_err(|source| database_write_error("input could not be accepted", source))?;
        let queue_order = u64::try_from(priority_order)
            .map_err(|source| internal_error("queue order exceeds storage range", source))?;
        transaction.execute("INSERT INTO runs (run_id, session_id, input_id, attempt, status, cancel_requested, approval_mode, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms) VALUES (?1, ?2, ?3, 1, 'accepted', 0, ?4, NULL, NULL, ?5, NULL, NULL)", params![input.run_id.as_str(), input.session_id.as_str(), input.input_id.as_str(), approval_mode_value(input.approval_mode), input.accepted_at_ms]).map_err(|source| database_write_error("run could not be accepted", source))?;
        let changed = transaction
            .execute(
                "UPDATE sessions
             SET current_variant = ?1,
                 title = CASE
                     WHEN title_origin = 'generated' AND ?3 IS NOT NULL THEN ?3
                     ELSE title
                 END
             WHERE session_id = ?2 AND lifecycle = 'active'",
                params![
                    agent_variant_value(input.agent_variant),
                    input.session_id.as_str(),
                    input.generated_title.as_deref(),
                ],
            )
            .map_err(|source| {
                database_write_error("session variant could not be updated", source)
            })?;
        if changed != 1 {
            return Err(conflict("input session is not active"));
        }
        transaction.commit().map_err(|source| {
            database_write_error("input acceptance could not be committed", source)
        })?;
        let stored = StoredInput {
            queue_order,
            input_id: input.input_id.clone(),
            session_id: input.session_id.clone(),
            idempotency_key: input.idempotency_key,
            agent_variant: input.agent_variant,
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
            agent_variant: input.agent_variant,
            approval_mode: input.approval_mode,
            reasoning_effort: None,
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
        let mut statement = self.connection.prepare("SELECT COALESCE(priority_order, queue_order), input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms, agent_variant FROM inputs ORDER BY COALESCE(priority_order, queue_order), queue_order").map_err(|source| internal_error("runtime inputs could not be queried", source))?;
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
                    row.get::<_, String>(8)?,
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
                agent_variant,
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
                agent_variant: parse_agent_variant(&agent_variant)?,
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
