//! 未回答问题由 SQL 控制；可靠终态只落会话文件，回答与下一条队列输入共用原回执。
use std::collections::BTreeMap;

use super::{
    StorageEngine, StorageResult,
    append_effect::{AppendPurpose, ConversationStorageTarget},
    conflict, internal_error, invalid_data,
};
use assistant_protocol::{ConversationOwner, FeedbackRequestSnapshot, FeedbackStatus, SessionId};
use assistant_runtime::FeedbackMutation;
use rusqlite::{Connection, OptionalExtension, params};

impl StorageEngine {
    /// 只有已备份的旧源可读取 SQL 终态；正常文件尾帧未完成时不发布回答。
    pub(super) fn load_feedback(
        &mut self,
        session_id: &SessionId,
    ) -> StorageResult<Vec<FeedbackRequestSnapshot>> {
        let controls = self.load_feedback_controls(session_id)?;
        if self.allow_legacy_history {
            return Ok(controls);
        }
        let facts = self
            .load_conversation_history(ConversationOwner::MainSession {
                session_id: session_id.clone(),
            })?
            .facts;
        let mut requests = BTreeMap::new();
        for request in facts.questions {
            if &request.session_id != session_id || request.status == FeedbackStatus::Pending {
                return Err(invalid_data(
                    "question history has invalid owner or pending state",
                ));
            }
            requests.insert(request.request_id.clone(), request);
        }
        for control in controls {
            if requests
                .insert(control.request_id.clone(), control)
                .is_some()
            {
                return Err(invalid_data(
                    "question has both historical and unfinished state",
                ));
            }
        }
        Ok(requests.into_values().collect())
    }

    /// 只供当前提交/恢复收集必要控制；正常路径不以旧终态 SQL 作为历史兜底。
    pub(super) fn load_feedback_controls(
        &self,
        session_id: &SessionId,
    ) -> StorageResult<Vec<FeedbackRequestSnapshot>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT request_id,run_id,snapshot_json FROM session_feedback WHERE session_id=?1
             AND (?2 OR json_extract(snapshot_json,'$.status')='pending') ORDER BY rowid",
            )
            .map_err(|source| internal_error("feedback query failed", source))?;
        let rows = statement
            .query_map(
                params![session_id.as_str(), self.allow_legacy_history],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|source| internal_error("feedback query failed", source))?;
        rows.map(|row| {
            let (id, run, json) =
                row.map_err(|source| internal_error("feedback read failed", source))?;
            decode_control(&id, session_id.as_str(), &run, &json)
        })
        .collect()
    }

    pub(super) fn mutate_feedback(&mut self, mutation: FeedbackMutation) -> StorageResult<()> {
        if self.allow_legacy_history {
            let transaction = self
                .connection
                .transaction()
                .map_err(|source| internal_error("feedback transaction failed", source))?;
            apply_feedback_mutation(&transaction, &mutation, self.allow_legacy_history)?;
            return transaction
                .commit()
                .map_err(|source| internal_error("feedback commit failed", source));
        }
        let session_id = mutation.request.session_id.clone();
        if let Some(existing) = self
            .load_feedback(&session_id)?
            .into_iter()
            .find(|request| request.request_id == mutation.request.request_id)
            && existing.status != FeedbackStatus::Pending
        {
            return if existing == mutation.request {
                Ok(())
            } else {
                Err(conflict("feedback already handled or changed"))
            };
        }
        if mutation.request.status == FeedbackStatus::Pending {
            let transaction = self
                .connection
                .transaction()
                .map_err(|source| internal_error("feedback transaction failed", source))?;
            apply_feedback_mutation(&transaction, &mutation, false)?;
            return transaction
                .commit()
                .map_err(|source| internal_error("feedback commit failed", source));
        }
        let preflight = self
            .connection
            .transaction()
            .map_err(|source| internal_error("feedback preflight failed", source))?;
        apply_feedback_mutation(&preflight, &mutation, false)?;
        preflight
            .rollback()
            .map_err(|source| internal_error("feedback preflight rollback failed", source))?;
        let operation_id = format!(
            "question-feedback:{}:{:?}",
            mutation.request.request_id, mutation.request.status
        );
        self.stage_target_append(
            operation_id.clone(),
            ConversationStorageTarget::SessionMetadata { session_id },
            Vec::new(),
            None,
            super::run_state::system_time_ms()?,
            AppendPurpose::QuestionFeedback {
                request: mutation.request,
                expected_status: mutation.expected_status,
            },
        )?;
        self.complete_staged_append(&operation_id)
    }
}

fn decode_control(
    id: &str,
    session: &str,
    run: &str,
    json: &str,
) -> StorageResult<FeedbackRequestSnapshot> {
    let request: FeedbackRequestSnapshot = serde_json::from_str(json)
        .map_err(|source| internal_error("feedback data invalid", source))?;
    if request.request_id != id
        || request.session_id.as_str() != session
        || request.run_id.as_str() != run
    {
        return Err(invalid_data(
            "feedback control identity does not match its snapshot",
        ));
    }
    Ok(request)
}

/// pending 行证明原问题身份，可跨越其父 Run 的终结；创建新问题仍须真实 Run 控制。
/// 终态在文件同步后才删除原 pending；预检回滚与最终回执共用此 CAS，不提交或发布事件。
pub(super) fn apply_feedback_mutation(
    connection: &Connection,
    mutation: &FeedbackMutation,
    legacy: bool,
) -> StorageResult<()> {
    let request = &mutation.request;
    let row: Option<(String, String, String)> = connection
        .query_row(
            "SELECT session_id,run_id,snapshot_json FROM session_feedback WHERE request_id=?1",
            [&request.request_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|source| internal_error("feedback read failed", source))?;
    if let Some((session, run, json)) = row {
        let current = decode_control(&request.request_id, &session, &run, &json)?;
        if current == *request && (legacy || request.status == FeedbackStatus::Pending) {
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
        if !legacy {
            let changed=connection.execute("DELETE FROM session_feedback WHERE request_id=?1 AND session_id=?2 AND run_id=?3 AND snapshot_json=?4",
                params![request.request_id,request.session_id.as_str(),request.run_id.as_str(),json])
                .map_err(|source| internal_error("feedback control could not be released",source))?;
            if changed != 1 {
                return Err(conflict("feedback control changed before finalization"));
            }
            return Ok(());
        }
    } else {
        if mutation.expected_status.is_some() || request.status != FeedbackStatus::Pending {
            return Err(conflict("feedback request does not exist"));
        }
        let owns_run: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE run_id=?1 AND session_id=?2
             AND (?3 OR status IN ('accepted','running','cancelling')))",
                params![request.run_id.as_str(), request.session_id.as_str(), legacy],
                |row| row.get(0),
            )
            .map_err(|source| internal_error("feedback owner lookup failed", source))?;
        if !owns_run {
            return Err(conflict("feedback Run does not belong to Session"));
        }
    }
    let json = serde_json::to_string(request)
        .map_err(|source| internal_error("feedback encode failed", source))?;
    connection.execute("INSERT INTO session_feedback(request_id,session_id,run_id,snapshot_json) VALUES(?1,?2,?3,?4) ON CONFLICT(request_id) DO UPDATE SET snapshot_json=excluded.snapshot_json",
        params![request.request_id,request.session_id.as_str(),request.run_id.as_str(),json])
        .map_err(|source| internal_error("feedback write failed",source))?;
    Ok(())
}
