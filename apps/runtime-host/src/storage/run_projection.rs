//! SQLite Run 行、Message 引用与领域投影之间的严格转换。

use agent_types::MessageId;
use assistant_protocol::{
    InputId, RunId, RunStatus, RuntimeErrorCode, RuntimeErrorInfo, SessionId,
};
use assistant_runtime::StoredRun;

use super::{
    StorageEngine, StorageResult, internal_error, invalid_data, invalid_data_with_source,
    mode::{parse_agent_variant, parse_approval_mode},
};

impl StorageEngine {
    /// 加载全部 Run，并在进入 Runtime 恢复投影前校验数据库原始值。
    pub(super) fn load_runs(&self) -> StorageResult<Vec<StoredRun>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT runs.run_id, runs.session_id, runs.input_id, runs.attempt, runs.status,
                        runs.cancel_requested, inputs.agent_variant, runs.approval_mode,
                        runs.error_code, runs.error_message, runs.created_at_ms,
                        runs.started_at_ms, runs.finished_at_ms
                 FROM runs JOIN inputs ON inputs.input_id = runs.input_id
                 ORDER BY runs.created_at_ms, runs.run_id",
            )
            .map_err(|source| internal_error("runtime runs could not be queried", source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                ))
            })
            .map_err(|source| internal_error("runtime runs could not be read", source))?;
        let mut runs = Vec::new();
        for row in rows {
            let (
                run_id,
                session_id,
                input_id,
                attempt,
                status,
                cancel_requested,
                agent_variant,
                approval_mode,
                error_code,
                error_message,
                created_at_ms,
                started_at_ms,
                finished_at_ms,
            ) =
                row.map_err(|source| internal_error("runtime run row could not be read", source))?;
            if !matches!(cancel_requested, 0 | 1) {
                return Err(invalid_data("stored run cancellation flag is invalid"));
            }
            let run_id = RunId::new(run_id)
                .map_err(|source| invalid_data_with_source("stored run id is invalid", source))?;
            let session_id = SessionId::new(session_id).map_err(|source| {
                invalid_data_with_source("stored run session id is invalid", source)
            })?;
            let error = match (error_code, error_message) {
                (None, None) => None,
                (Some(code), Some(message)) => {
                    Some(RuntimeErrorInfo::new(parse_error_code(&code)?, message))
                }
                _ => return Err(invalid_data("stored run error is incomplete")),
            };
            runs.push(StoredRun {
                message_ids: self.load_run_message_ids(&run_id)?,
                run_id,
                session_id,
                input_id: InputId::new(input_id).map_err(|source| {
                    invalid_data_with_source("stored input id is invalid", source)
                })?,
                attempt: u32::try_from(attempt).map_err(|source| {
                    invalid_data_with_source("stored run attempt is invalid", source)
                })?,
                status: parse_run_status(&status)?,
                agent_variant: parse_agent_variant(&agent_variant)?,
                approval_mode: parse_approval_mode(&approval_mode)?,
                cancel_requested: cancel_requested == 1,
                error,
                created_at_ms,
                started_at_ms,
                finished_at_ms,
            });
        }
        Ok(runs)
    }

    /// 按持久插入顺序恢复某次 Run 对规范消息的引用。
    fn load_run_message_ids(&self, run_id: &RunId) -> StorageResult<Vec<MessageId>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT message_id FROM run_message_refs
                 WHERE run_id = ?1 ORDER BY rowid",
            )
            .map_err(|source| {
                internal_error("run message references could not be queried", source)
            })?;
        let rows = statement
            .query_map([run_id.as_str()], |row| row.get::<_, String>(0))
            .map_err(|source| internal_error("run message references could not be read", source))?;
        rows.map(|row| {
            let value = row.map_err(|source| {
                internal_error("run message reference could not be read", source)
            })?;
            MessageId::new(value).map_err(|source| {
                invalid_data_with_source("stored run message id is invalid", source)
            })
        })
        .collect()
    }
}

fn parse_run_status(value: &str) -> StorageResult<RunStatus> {
    match value {
        "accepted" => Ok(RunStatus::Accepted),
        "running" => Ok(RunStatus::Running),
        "cancelling" => Ok(RunStatus::Cancelling),
        "completed" => Ok(RunStatus::Completed),
        "failed" => Ok(RunStatus::Failed),
        "cancelled" => Ok(RunStatus::Cancelled),
        "interrupted" => Ok(RunStatus::Interrupted),
        "compaction_required" => Ok(RunStatus::CompactionRequired),
        _ => Err(invalid_data("stored run status is invalid")),
    }
}

pub(super) fn parse_error_code(value: &str) -> StorageResult<RuntimeErrorCode> {
    match value {
        "invalid_request" => Ok(RuntimeErrorCode::InvalidRequest),
        "session_not_found" => Ok(RuntimeErrorCode::SessionNotFound),
        "session_busy" => Ok(RuntimeErrorCode::SessionBusy),
        "session_archived" => Ok(RuntimeErrorCode::SessionArchived),
        "session_not_idle" => Ok(RuntimeErrorCode::SessionNotIdle),
        "run_not_found" => Ok(RuntimeErrorCode::RunNotFound),
        "child_task_not_found" => Ok(RuntimeErrorCode::ChildTaskNotFound),
        "input_not_found" => Ok(RuntimeErrorCode::InputNotFound),
        "run_not_retryable" => Ok(RuntimeErrorCode::RunNotRetryable),
        "storage_unavailable" => Ok(RuntimeErrorCode::StorageUnavailable),
        "runtime_shutting_down" => Ok(RuntimeErrorCode::RuntimeShuttingDown),
        "agent_build_failed" => Ok(RuntimeErrorCode::AgentBuildFailed),
        "configuration_unavailable" => Ok(RuntimeErrorCode::ConfigurationUnavailable),
        "model_not_found" => Ok(RuntimeErrorCode::ModelNotFound),
        "model_unavailable" => Ok(RuntimeErrorCode::ModelUnavailable),
        "model_build_failed" => Ok(RuntimeErrorCode::ModelBuildFailed),
        "model_execution_failed" => Ok(RuntimeErrorCode::ModelExecutionFailed),
        "context_compaction_failed" => Ok(RuntimeErrorCode::ContextCompactionFailed),
        "timeout" => Ok(RuntimeErrorCode::Timeout),
        "cancelled" => Ok(RuntimeErrorCode::Cancelled),
        "workspace_not_found" => Ok(RuntimeErrorCode::WorkspaceNotFound),
        "workspace_removed" => Ok(RuntimeErrorCode::WorkspaceRemoved),
        "workspace_unavailable" => Ok(RuntimeErrorCode::WorkspaceUnavailable),
        "attachment_not_found" => Ok(RuntimeErrorCode::AttachmentNotFound),
        "attachment_unavailable" => Ok(RuntimeErrorCode::AttachmentUnavailable),
        "attachment_too_large" => Ok(RuntimeErrorCode::AttachmentTooLarge),
        "attachment_upload_invalid" => Ok(RuntimeErrorCode::AttachmentUploadInvalid),
        "permission_file_invalid" => Ok(RuntimeErrorCode::PermissionFileInvalid),
        "permission_file_conflict" => Ok(RuntimeErrorCode::PermissionFileConflict),
        "permission_reload_failed" => Ok(RuntimeErrorCode::PermissionReloadFailed),
        "permission_persistence_failed" => Ok(RuntimeErrorCode::PermissionPersistenceFailed),
        "approval_not_found" => Ok(RuntimeErrorCode::ApprovalNotFound),
        "approval_expired" => Ok(RuntimeErrorCode::ApprovalExpired),
        "approval_not_head" => Ok(RuntimeErrorCode::ApprovalNotHead),
        "approval_already_resolved" => Ok(RuntimeErrorCode::ApprovalAlreadyResolved),
        "permission_scope_unavailable" => Ok(RuntimeErrorCode::PermissionScopeUnavailable),
        "conflict" => Ok(RuntimeErrorCode::Conflict),
        "configuration_conflict" => Ok(RuntimeErrorCode::ConfigurationConflict),
        "queue_conflict" => Ok(RuntimeErrorCode::QueueConflict),
        "snapshot_stale" => Ok(RuntimeErrorCode::SnapshotStale),
        "snapshot_busy" => Ok(RuntimeErrorCode::SnapshotBusy),
        "operation_not_allowed" => Ok(RuntimeErrorCode::OperationNotAllowed),
        "resource_not_previewable" => Ok(RuntimeErrorCode::ResourceNotPreviewable),
        "resource_too_large" => Ok(RuntimeErrorCode::ResourceTooLarge),
        "internal" => Ok(RuntimeErrorCode::Internal),
        _ => Err(invalid_data("stored run error code is invalid")),
    }
}

pub(super) fn run_status_value(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Accepted => "accepted",
        RunStatus::Running => "running",
        RunStatus::Cancelling => "cancelling",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
        RunStatus::Interrupted => "interrupted",
        RunStatus::CompactionRequired => "compaction_required",
    }
}

pub(super) fn error_code_value(code: RuntimeErrorCode) -> &'static str {
    match code {
        RuntimeErrorCode::InvalidRequest => "invalid_request",
        RuntimeErrorCode::SessionNotFound => "session_not_found",
        RuntimeErrorCode::SessionBusy => "session_busy",
        RuntimeErrorCode::SessionArchived => "session_archived",
        RuntimeErrorCode::SessionNotIdle => "session_not_idle",
        RuntimeErrorCode::RunNotFound => "run_not_found",
        RuntimeErrorCode::ChildTaskNotFound => "child_task_not_found",
        RuntimeErrorCode::InputNotFound => "input_not_found",
        RuntimeErrorCode::RunNotRetryable => "run_not_retryable",
        RuntimeErrorCode::StorageUnavailable => "storage_unavailable",
        RuntimeErrorCode::RuntimeShuttingDown => "runtime_shutting_down",
        RuntimeErrorCode::AgentBuildFailed => "agent_build_failed",
        RuntimeErrorCode::ConfigurationUnavailable => "configuration_unavailable",
        RuntimeErrorCode::ModelNotFound => "model_not_found",
        RuntimeErrorCode::ModelUnavailable => "model_unavailable",
        RuntimeErrorCode::ModelBuildFailed => "model_build_failed",
        RuntimeErrorCode::ModelExecutionFailed => "model_execution_failed",
        RuntimeErrorCode::ContextCompactionFailed => "context_compaction_failed",
        RuntimeErrorCode::Timeout => "timeout",
        RuntimeErrorCode::Cancelled => "cancelled",
        RuntimeErrorCode::WorkspaceNotFound => "workspace_not_found",
        RuntimeErrorCode::WorkspaceRemoved => "workspace_removed",
        RuntimeErrorCode::WorkspaceUnavailable => "workspace_unavailable",
        RuntimeErrorCode::AttachmentNotFound => "attachment_not_found",
        RuntimeErrorCode::AttachmentUnavailable => "attachment_unavailable",
        RuntimeErrorCode::AttachmentTooLarge => "attachment_too_large",
        RuntimeErrorCode::AttachmentUploadInvalid => "attachment_upload_invalid",
        RuntimeErrorCode::PermissionFileInvalid => "permission_file_invalid",
        RuntimeErrorCode::PermissionFileConflict => "permission_file_conflict",
        RuntimeErrorCode::PermissionReloadFailed => "permission_reload_failed",
        RuntimeErrorCode::PermissionPersistenceFailed => "permission_persistence_failed",
        RuntimeErrorCode::ApprovalNotFound => "approval_not_found",
        RuntimeErrorCode::ApprovalExpired => "approval_expired",
        RuntimeErrorCode::ApprovalNotHead => "approval_not_head",
        RuntimeErrorCode::ApprovalAlreadyResolved => "approval_already_resolved",
        RuntimeErrorCode::PermissionScopeUnavailable => "permission_scope_unavailable",
        RuntimeErrorCode::Conflict => "conflict",
        RuntimeErrorCode::ConfigurationConflict => "configuration_conflict",
        RuntimeErrorCode::QueueConflict => "queue_conflict",
        RuntimeErrorCode::SnapshotStale => "snapshot_stale",
        RuntimeErrorCode::SnapshotBusy => "snapshot_busy",
        RuntimeErrorCode::OperationNotAllowed => "operation_not_allowed",
        RuntimeErrorCode::ResourceNotPreviewable => "resource_not_previewable",
        RuntimeErrorCode::ResourceTooLarge => "resource_too_large",
        RuntimeErrorCode::Internal => "internal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_runtime_error_code_round_trips_through_sqlite_text() {
        let codes = [
            RuntimeErrorCode::InvalidRequest,
            RuntimeErrorCode::SessionNotFound,
            RuntimeErrorCode::SessionBusy,
            RuntimeErrorCode::SessionArchived,
            RuntimeErrorCode::SessionNotIdle,
            RuntimeErrorCode::RunNotFound,
            RuntimeErrorCode::ChildTaskNotFound,
            RuntimeErrorCode::InputNotFound,
            RuntimeErrorCode::RunNotRetryable,
            RuntimeErrorCode::StorageUnavailable,
            RuntimeErrorCode::RuntimeShuttingDown,
            RuntimeErrorCode::AgentBuildFailed,
            RuntimeErrorCode::ConfigurationUnavailable,
            RuntimeErrorCode::ModelNotFound,
            RuntimeErrorCode::ModelUnavailable,
            RuntimeErrorCode::ModelBuildFailed,
            RuntimeErrorCode::ModelExecutionFailed,
            RuntimeErrorCode::ContextCompactionFailed,
            RuntimeErrorCode::Timeout,
            RuntimeErrorCode::Cancelled,
            RuntimeErrorCode::WorkspaceNotFound,
            RuntimeErrorCode::WorkspaceRemoved,
            RuntimeErrorCode::WorkspaceUnavailable,
            RuntimeErrorCode::AttachmentNotFound,
            RuntimeErrorCode::AttachmentUnavailable,
            RuntimeErrorCode::AttachmentTooLarge,
            RuntimeErrorCode::AttachmentUploadInvalid,
            RuntimeErrorCode::PermissionFileInvalid,
            RuntimeErrorCode::PermissionFileConflict,
            RuntimeErrorCode::PermissionReloadFailed,
            RuntimeErrorCode::PermissionPersistenceFailed,
            RuntimeErrorCode::ApprovalNotFound,
            RuntimeErrorCode::ApprovalExpired,
            RuntimeErrorCode::ApprovalNotHead,
            RuntimeErrorCode::ApprovalAlreadyResolved,
            RuntimeErrorCode::PermissionScopeUnavailable,
            RuntimeErrorCode::Conflict,
            RuntimeErrorCode::ConfigurationConflict,
            RuntimeErrorCode::QueueConflict,
            RuntimeErrorCode::SnapshotStale,
            RuntimeErrorCode::SnapshotBusy,
            RuntimeErrorCode::OperationNotAllowed,
            RuntimeErrorCode::ResourceNotPreviewable,
            RuntimeErrorCode::ResourceTooLarge,
            RuntimeErrorCode::Internal,
        ];

        for code in codes {
            assert_eq!(
                parse_error_code(error_code_value(code)).expect("stored error code"),
                code
            );
        }
    }
}
