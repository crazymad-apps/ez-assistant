//! 问答仍属于同一 Session 存储聚合；终态 CAS 与 Input 接纳可共享 SQLite 事务。
use super::{StorageEngine, StorageResult, conflict, internal_error};
use assistant_protocol::{FeedbackRequestSnapshot, FeedbackStatus, SessionId};
use assistant_runtime::FeedbackMutation;
use rusqlite::{Connection, OptionalExtension, params};

impl StorageEngine {
    pub(super) fn load_feedback(
        &self,
        session_id: &SessionId,
    ) -> StorageResult<Vec<FeedbackRequestSnapshot>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT snapshot_json FROM session_feedback WHERE session_id=?1 ORDER BY rowid",
            )
            .map_err(|e| internal_error("feedback query failed", e))?;
        let rows = statement
            .query_map([session_id.as_str()], |row| row.get::<_, String>(0))
            .map_err(|e| internal_error("feedback query failed", e))?;
        rows.map(|row| {
            let json = row.map_err(|e| internal_error("feedback read failed", e))?;
            serde_json::from_str(&json).map_err(|e| internal_error("feedback data invalid", e))
        })
        .collect()
    }
    pub(super) fn mutate_feedback(&mut self, mutation: FeedbackMutation) -> StorageResult<()> {
        let transaction = self
            .connection
            .transaction()
            .map_err(|e| internal_error("feedback transaction failed", e))?;
        apply_feedback_mutation(&transaction, &mutation)?;
        transaction
            .commit()
            .map_err(|e| internal_error("feedback commit failed", e))
    }
}

/// 调用方持有 Session gate；检查和提交处于同一事务，旧回答不能覆盖取消或另一次回答。
pub(super) fn apply_feedback_mutation(
    connection: &Connection,
    mutation: &FeedbackMutation,
) -> StorageResult<()> {
    let request = &mutation.request;
    let owns_run: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE run_id=?1 AND session_id=?2)",
            params![request.run_id.as_str(), request.session_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| internal_error("feedback owner lookup failed", error))?;
    if !owns_run {
        return Err(conflict("feedback Run does not belong to Session"));
    }
    let current: Option<String> = connection
        .query_row(
            "SELECT snapshot_json FROM session_feedback WHERE request_id=?1",
            params![request.request_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| internal_error("feedback read failed", e))?;
    if let Some(json) = current {
        let current: FeedbackRequestSnapshot =
            serde_json::from_str(&json).map_err(|e| internal_error("feedback data invalid", e))?;
        if current == *request {
            return Ok(());
        }
        if mutation.expected_status != Some(FeedbackStatus::Pending)
            || current.status != FeedbackStatus::Pending
            || request.status == FeedbackStatus::Pending
            || current.run_id != request.run_id
            || current.session_id != request.session_id
            || current.tool_call_id != request.tool_call_id
            || current.mode != request.mode
            || current.questions != request.questions
        {
            return Err(conflict("feedback already handled or changed"));
        }
    } else if mutation.expected_status.is_some() || request.status != FeedbackStatus::Pending {
        return Err(conflict("feedback request does not exist"));
    }
    let json =
        serde_json::to_string(request).map_err(|e| internal_error("feedback encode failed", e))?;
    connection.execute("INSERT INTO session_feedback(request_id,session_id,run_id,snapshot_json) VALUES(?1,?2,?3,?4) ON CONFLICT(request_id) DO UPDATE SET snapshot_json=excluded.snapshot_json", params![request.request_id, request.session_id.as_str(), request.run_id.as_str(), json]).map_err(|e| internal_error("feedback write failed", e))?;
    Ok(())
}
