//! Run attempt 创建、领取、终态结算和启动中断收敛。

use std::time::{SystemTime, UNIX_EPOCH};

use agent_types::ConversationMessage;
use assistant_protocol::{InputId, RunStatus};
use assistant_runtime::{NewStoredRunAttempt, StoredRun, StoredRunSettlement, UserMessageCommit};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult,
    append_effect::{AppendPurpose, apply_run_settlement},
    conflict, database_write_error, internal_error, invalid_data_with_source,
    mode::{approval_mode_value, parse_agent_variant},
    recovery::AppendRequest,
};

impl StorageEngine {
    /// 为最新的 Failed/Interrupted Run 创建同一 Input 的下一次执行尝试。
    pub(super) fn create_run_attempt(
        &mut self,
        attempt: NewStoredRunAttempt,
    ) -> StorageResult<StoredRun> {
        let (input_id, source_attempt, source_status): (String, i64, String) = self
            .connection
            .query_row(
                "SELECT input_id, attempt, status FROM runs WHERE run_id = ?1 AND session_id = ?2",
                params![attempt.source_run_id.as_str(), attempt.session_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|source| internal_error("source run could not be queried", source))?
            .ok_or_else(|| conflict("source run does not exist"))?;
        if !matches!(source_status.as_str(), "failed" | "interrupted") {
            return Err(conflict("run is not retryable"));
        }
        let next: i64 = self
            .connection
            .query_row(
                "SELECT COALESCE(MAX(attempt), 0) + 1 FROM runs WHERE input_id = ?1",
                [&input_id],
                |row| row.get(0),
            )
            .map_err(|source| internal_error("run attempt could not be allocated", source))?;
        if next != source_attempt + 1 {
            return Err(conflict("only the latest run can be retried"));
        }
        let agent_variant = self
            .connection
            .query_row(
                "SELECT agent_variant FROM inputs WHERE input_id = ?1",
                [&input_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|source| internal_error("run input variant could not be queried", source))?;
        let agent_variant = parse_agent_variant(&agent_variant)?;
        self.connection.execute("INSERT INTO runs (run_id, session_id, input_id, attempt, status, cancel_requested, approval_mode, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms) VALUES (?1, ?2, ?3, ?4, 'accepted', 0, ?5, NULL, NULL, ?6, NULL, NULL)", params![attempt.run_id.as_str(), attempt.session_id.as_str(), input_id, next, approval_mode_value(attempt.approval_mode), attempt.created_at_ms]).map_err(|source| database_write_error("run attempt could not be created", source))?;
        Ok(StoredRun {
            run_id: attempt.run_id,
            session_id: attempt.session_id,
            input_id: InputId::new(input_id)
                .map_err(|source| invalid_data_with_source("stored input id is invalid", source))?,
            attempt: u32::try_from(next)
                .map_err(|source| internal_error("run attempt exceeds runtime range", source))?,
            status: RunStatus::Accepted,
            agent_variant,
            approval_mode: attempt.approval_mode,
            cancel_requested: false,
            error: None,
            message_ids: Vec::new(),
            created_at_ms: attempt.created_at_ms,
            started_at_ms: None,
            finished_at_ms: None,
        })
    }

    /// 首次领取提交 User Message；后续 attempt 只把新的 Run 转为 Running。
    pub(super) fn commit_user_message(&mut self, commit: UserMessageCommit) -> StorageResult<()> {
        if commit.message.is_none() {
            let changed = self.connection.execute("UPDATE runs SET status = 'running', started_at_ms = ?1 WHERE run_id = ?2 AND session_id = ?3 AND input_id = ?4 AND status = 'accepted' AND EXISTS (SELECT 1 FROM inputs WHERE input_id = ?4 AND state = 'committed')", params![commit.created_at_ms, commit.run_id.as_str(), commit.session_id.as_str(), commit.input_id.as_str()]).map_err(|source| database_write_error("run could not be started", source))?;
            if changed != 1 {
                return Err(conflict("run cannot be started"));
            }
            return Ok(());
        }
        let message = commit.message.expect("checked message");

        let operation_id = commit.operation_id.clone();
        self.stage_append_for(
            AppendRequest {
                operation_id,
                session_id: commit.session_id,
                run_id: commit.run_id,
                messages: vec![ConversationMessage::User(message)],
                created_at_ms: commit.created_at_ms,
            },
            AppendPurpose::UserMessage,
        )?;
        self.complete_staged_append(&commit.operation_id)
    }

    /// 将 Run 与本次新增的完整规范消息作为一个可恢复操作提交。
    pub(super) fn settle_run(&mut self, settlement: StoredRunSettlement) -> StorageResult<()> {
        if !settlement.status.is_terminal() {
            return Err(assistant_runtime::StoreError::new(
                assistant_runtime::StoreErrorKind::InvalidInput,
                "run settlement status is not terminal",
            ));
        }
        let pending_count: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM pending_tool_exchanges WHERE run_id = ?1",
                [settlement.run_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|source| {
                internal_error("run pending tool exchange could not be queried", source)
            })?;
        if pending_count != 0 {
            return Err(conflict("run has a pending tool exchange"));
        }
        let purpose = AppendPurpose::RunSettlement {
            status: settlement.status,
            cancel_requested: settlement.cancel_requested,
            error: settlement.error,
        };
        if settlement.messages.is_empty() {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| internal_error("run settlement could not begin", source))?;
            apply_run_settlement(
                &transaction,
                &settlement.run_id,
                &settlement.session_id,
                &purpose,
                settlement.finished_at_ms,
            )?;
            return transaction.commit().map_err(|source| {
                database_write_error("run settlement could not be committed", source)
            });
        }

        let operation_id = settlement.operation_id.clone();
        self.stage_append_for(
            AppendRequest {
                operation_id,
                session_id: settlement.session_id,
                run_id: settlement.run_id,
                messages: settlement.messages,
                created_at_ms: settlement.finished_at_ms,
            },
            purpose,
        )?;
        self.complete_staged_append(&settlement.operation_id)
    }

    /// 重启时只中断已经领取或已经提交 User Message 的未终结 Run。
    pub(super) fn interrupt_nonterminal_runs(&mut self) -> StorageResult<()> {
        self.connection
            .execute(
                "UPDATE runs
                 SET status = 'interrupted', finished_at_ms = ?1
                 WHERE status IN ('running', 'cancelling')
                    OR (status = 'accepted' AND EXISTS (
                        SELECT 1 FROM inputs WHERE inputs.input_id = runs.input_id AND inputs.state = 'committed'
                    ))",
                [system_time_ms()?],
            )
            .map_err(|source| {
                database_write_error("non-terminal runs could not be interrupted", source)
            })?;
        Ok(())
    }
}

pub(super) fn system_time_ms() -> StorageResult<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|source| internal_error("system clock is before the Unix epoch", source))?;
    i64::try_from(duration.as_millis())
        .map_err(|source| internal_error("system clock exceeds SQLite range", source))
}
