//! staged append 的持久业务意图及其 SQLite 提交效果。

use assistant_protocol::{RunId, RunStatus, RuntimeErrorInfo, SessionId};
use rusqlite::{Transaction, params};
use serde::{Deserialize, Serialize};

use super::{
    StorageResult, conflict, internal_error, invalid_data, invalid_data_with_source,
    run_projection::{error_code_value, run_status_value},
};

/// Conversation 文件写入成功后，必须在同一 SQLite 事务应用的业务转换。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum AppendPurpose {
    /// 只追加规范消息，不改变 Input/Run 状态。
    Messages,
    /// 首次 User Message 已写入，提交 Input 并启动 Run。
    UserMessage,
    /// 完整工具批次已写入；清除对应的临时 pending 事实。
    ToolExchange { receipt_id: String },
    /// 最终消息已写入，结算 Run 终态。
    RunSettlement {
        status: RunStatus,
        cancel_requested: bool,
        error: Option<RuntimeErrorInfo>,
    },
}

pub(super) fn encode_purpose(purpose: &AppendPurpose) -> StorageResult<String> {
    if matches!(purpose, AppendPurpose::Messages) {
        return Ok("messages".to_owned());
    }
    serde_json::to_string(purpose)
        .map_err(|source| internal_error("staged append purpose could not be encoded", source))
}

pub(super) fn decode_purpose(value: &str) -> StorageResult<AppendPurpose> {
    if value == "messages" {
        return Ok(AppendPurpose::Messages);
    }
    serde_json::from_str(value)
        .map_err(|source| invalid_data_with_source("staged append purpose is invalid", source))
}

pub(super) fn apply_purpose(
    transaction: &Transaction<'_>,
    purpose: &AppendPurpose,
    run_id: &RunId,
    session_id: &SessionId,
    created_at_ms: i64,
) -> StorageResult<()> {
    match purpose {
        AppendPurpose::Messages => Ok(()),
        AppendPurpose::UserMessage => {
            apply_user_message_start(transaction, run_id, session_id, created_at_ms)
        }
        AppendPurpose::ToolExchange { receipt_id } => {
            let deleted = transaction
                .execute(
                    "DELETE FROM pending_tool_exchanges
                     WHERE receipt_id = ?1 AND run_id = ?2 AND session_id = ?3 AND state = 'ready'",
                    params![receipt_id, run_id.as_str(), session_id.as_str()],
                )
                .map_err(|source| {
                    internal_error("pending tool exchange could not be cleared", source)
                })?;
            if deleted != 1 {
                return Err(invalid_data(
                    "pending tool exchange finalization is inconsistent",
                ));
            }
            Ok(())
        }
        AppendPurpose::RunSettlement { .. } => {
            apply_run_settlement(transaction, run_id, session_id, purpose, created_at_ms)
        }
    }
}

fn apply_user_message_start(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    session_id: &SessionId,
    created_at_ms: i64,
) -> StorageResult<()> {
    let input_updated = transaction
        .execute(
            "UPDATE inputs
             SET state = 'committed', queued_message_json = NULL
             WHERE input_id = (SELECT input_id FROM runs WHERE run_id = ?1)
               AND session_id = ?2 AND state = 'queued'",
            params![run_id.as_str(), session_id.as_str()],
        )
        .map_err(|source| internal_error("run input could not be committed", source))?;
    let run_updated = transaction
        .execute(
            "UPDATE runs
             SET status = 'running', started_at_ms = ?1
             WHERE run_id = ?2 AND session_id = ?3 AND status = 'accepted'",
            params![created_at_ms, run_id.as_str(), session_id.as_str()],
        )
        .map_err(|source| internal_error("run could not be started", source))?;
    if input_updated != 1 || run_updated != 1 {
        return Err(invalid_data("staged run start state is inconsistent"));
    }
    Ok(())
}

pub(super) fn apply_run_settlement(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    session_id: &SessionId,
    purpose: &AppendPurpose,
    finished_at_ms: i64,
) -> StorageResult<()> {
    let AppendPurpose::RunSettlement {
        status,
        cancel_requested,
        error,
    } = purpose
    else {
        return Err(invalid_data("run settlement purpose is invalid"));
    };
    let updated = transaction
        .execute(
            "UPDATE runs
             SET status = ?1, cancel_requested = ?2, error_code = ?3,
                 error_message = ?4, finished_at_ms = ?5
             WHERE run_id = ?6 AND session_id = ?7
               AND status IN ('accepted', 'running', 'cancelling')",
            params![
                run_status_value(*status),
                i64::from(*cancel_requested),
                error.as_ref().map(|error| error_code_value(error.code)),
                error.as_ref().map(|error| error.message.as_str()),
                finished_at_ms,
                run_id.as_str(),
                session_id.as_str(),
            ],
        )
        .map_err(|source| internal_error("run could not be settled", source))?;
    if updated != 1 {
        return Err(conflict("run is not in a settleable state"));
    }
    Ok(())
}
