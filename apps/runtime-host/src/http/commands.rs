//! 有界 JSON Command ingress 与 Runtime 薄 dispatch。

use agent_types::ConversationSnapshot;
use assistant_protocol::{RuntimeCommand, RuntimeCommandResult, RuntimeErrorInfo, SessionId};
use assistant_runtime::{AssistantRuntime, RuntimeError};
use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use super::{HttpState, error::runtime_status};

const MAX_REQUEST_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CommandRequest {
    pub(crate) request_id: String,
    pub(crate) command: HostCommand,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
pub(crate) enum HostCommand {
    Runtime(RuntimeCommand),
    /// 私有验收查询；完整 Conversation 尚未进入公共应用协议。
    ConversationSnapshot {
        session_id: SessionId,
    },
    ChildTaskConversationSnapshot {
        session_id: SessionId,
        child_task_id: assistant_protocol::ChildTaskId,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct CommandResponse {
    pub(crate) request_id: String,
    pub(crate) result: HostCommandResult,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
pub(crate) enum HostCommandResult {
    Runtime(Box<RuntimeCommandResult>),
    ConversationSnapshot { conversation: ConversationSnapshot },
    ChildTaskConversationSnapshot { conversation: ConversationSnapshot },
}

#[derive(Serialize)]
struct CommandErrorBody {
    request_id: Option<String>,
    error: RuntimeErrorInfo,
}

pub(super) async fn handle_command(
    State(state): State<HttpState>,
    payload: Result<Json<CommandRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match payload {
        Ok(payload) => payload,
        Err(_) => {
            return command_error(
                None,
                RuntimeErrorInfo::new(
                    assistant_protocol::RuntimeErrorCode::InvalidRequest,
                    "command body must be valid bounded JSON",
                ),
            );
        }
    };
    if request.request_id.trim().is_empty() || request.request_id.len() > MAX_REQUEST_ID_BYTES {
        return command_error(
            Some(request.request_id),
            RuntimeErrorInfo::new(
                assistant_protocol::RuntimeErrorCode::InvalidRequest,
                "request_id must be non-empty and at most 128 bytes",
            ),
        );
    }

    match dispatch(&state.runtime, request.command).await {
        Ok((result, shutdown_requested)) => {
            let response = (
                StatusCode::OK,
                Json(CommandResponse {
                    request_id: request.request_id,
                    result,
                }),
            )
                .into_response();
            if shutdown_requested {
                state.shutdown.cancel();
            }
            response
        }
        Err(error) => command_error(Some(request.request_id), error.to_protocol_info()),
    }
}

fn command_error(request_id: Option<String>, error: RuntimeErrorInfo) -> Response {
    let status = runtime_status(error.code);
    (status, Json(CommandErrorBody { request_id, error })).into_response()
}

async fn dispatch(
    runtime: &AssistantRuntime,
    command: HostCommand,
) -> Result<(HostCommandResult, bool), RuntimeError> {
    match command {
        HostCommand::Runtime(command) => dispatch_runtime(runtime, command).await,
        HostCommand::ConversationSnapshot { session_id } => runtime
            .conversation_snapshot(&session_id)
            .await
            .map(|conversation| {
                (
                    HostCommandResult::ConversationSnapshot { conversation },
                    false,
                )
            }),
        HostCommand::ChildTaskConversationSnapshot {
            session_id,
            child_task_id,
        } => runtime
            .child_task_conversation_snapshot(&session_id, &child_task_id)
            .await
            .map(|conversation| {
                (
                    HostCommandResult::ChildTaskConversationSnapshot { conversation },
                    false,
                )
            }),
    }
}

async fn dispatch_runtime(
    runtime: &AssistantRuntime,
    command: RuntimeCommand,
) -> Result<(HostCommandResult, bool), RuntimeError> {
    let (result, shutdown) = match command {
        RuntimeCommand::GetConfigStatus(request) => (
            RuntimeCommandResult::GetConfigStatus(runtime.get_config_status(request)?),
            false,
        ),
        RuntimeCommand::ListModels(request) => (
            RuntimeCommandResult::ListModels(runtime.list_models(request)?),
            false,
        ),
        RuntimeCommand::GetModel(request) => (
            RuntimeCommandResult::GetModel(runtime.get_model(request)?),
            false,
        ),
        RuntimeCommand::ReloadConfig(request) => (
            RuntimeCommandResult::ReloadConfig(runtime.reload_config(request).await?),
            false,
        ),
        RuntimeCommand::ReloadPermissions(request) => (
            RuntimeCommandResult::ReloadPermissions(runtime.reload_permissions(request).await?),
            false,
        ),
        RuntimeCommand::ListPendingApprovals(request) => (
            RuntimeCommandResult::ListPendingApprovals(runtime.list_pending_approvals(request)?),
            false,
        ),
        RuntimeCommand::DecideApproval(request) => (
            RuntimeCommandResult::DecideApproval(runtime.decide_approval(request).await?),
            false,
        ),
        RuntimeCommand::ValidateModelConnection(request) => (
            RuntimeCommandResult::ValidateModelConnection(
                runtime.validate_model_connection(request).await?,
            ),
            false,
        ),
        RuntimeCommand::RegisterWorkspace(request) => (
            RuntimeCommandResult::RegisterWorkspace(runtime.register_workspace(request).await?),
            false,
        ),
        RuntimeCommand::GetWorkspace(request) => (
            RuntimeCommandResult::GetWorkspace(runtime.get_workspace(request)?),
            false,
        ),
        RuntimeCommand::ListWorkspaces(request) => (
            RuntimeCommandResult::ListWorkspaces(runtime.list_workspaces(request)?),
            false,
        ),
        RuntimeCommand::RemoveWorkspace(request) => (
            RuntimeCommandResult::RemoveWorkspace(runtime.remove_workspace(request).await?),
            false,
        ),
        RuntimeCommand::GetAttachment(request) => (
            RuntimeCommandResult::GetAttachment(runtime.get_attachment(request)?),
            false,
        ),
        RuntimeCommand::ListAttachments(request) => (
            RuntimeCommandResult::ListAttachments(runtime.list_attachments(request)?),
            false,
        ),
        RuntimeCommand::CreateSession(request) => (
            RuntimeCommandResult::CreateSession(runtime.create_session(request).await?),
            false,
        ),
        RuntimeCommand::ListSessions(request) => (
            RuntimeCommandResult::ListSessions(runtime.list_sessions(request)?),
            false,
        ),
        RuntimeCommand::GetSession(request) => (
            RuntimeCommandResult::GetSession(runtime.get_session(request)?),
            false,
        ),
        RuntimeCommand::SubmitInput(request) => (
            RuntimeCommandResult::SubmitInput(runtime.submit_input(request).await?),
            false,
        ),
        RuntimeCommand::CancelQueuedInput(request) => (
            RuntimeCommandResult::CancelQueuedInput(runtime.cancel_queued_input(request).await?),
            false,
        ),
        RuntimeCommand::ResumeSession(request) => (
            RuntimeCommandResult::ResumeSession(runtime.resume_session(request).await?),
            false,
        ),
        RuntimeCommand::RetryRun(request) => (
            RuntimeCommandResult::RetryRun(runtime.retry_run(request).await?),
            false,
        ),
        RuntimeCommand::GetRun(request) => (
            RuntimeCommandResult::GetRun(runtime.get_run(request).await?),
            false,
        ),
        RuntimeCommand::ListRuns(request) => (
            RuntimeCommandResult::ListRuns(runtime.list_runs(request).await?),
            false,
        ),
        RuntimeCommand::ListChildTasks(request) => (
            RuntimeCommandResult::ListChildTasks(runtime.list_child_tasks(request).await?),
            false,
        ),
        RuntimeCommand::GetChildTask(request) => (
            RuntimeCommandResult::GetChildTask(runtime.get_child_task(request).await?),
            false,
        ),
        RuntimeCommand::CancelChildTask(request) => (
            RuntimeCommandResult::CancelChildTask(runtime.cancel_child_task(request).await?),
            false,
        ),
        RuntimeCommand::ArchiveSession(request) => (
            RuntimeCommandResult::ArchiveSession(runtime.archive_session(request).await?),
            false,
        ),
        RuntimeCommand::RestoreSession(request) => (
            RuntimeCommandResult::RestoreSession(runtime.restore_session(request).await?),
            false,
        ),
        RuntimeCommand::SetSessionModel(request) => (
            RuntimeCommandResult::SetSessionModel(runtime.set_session_model(request).await?),
            false,
        ),
        RuntimeCommand::SetSessionVariant(request) => (
            RuntimeCommandResult::SetSessionVariant(runtime.set_session_variant(request).await?),
            false,
        ),
        RuntimeCommand::SetSessionApprovalMode(request) => (
            RuntimeCommandResult::SetSessionApprovalMode(
                runtime.set_session_approval_mode(request).await?,
            ),
            false,
        ),
        RuntimeCommand::ReenterFromUserMessage(request) => (
            RuntimeCommandResult::ReenterFromUserMessage(
                runtime.reenter_from_user_message(request).await?,
            ),
            false,
        ),
        RuntimeCommand::CancelRun(request) => (
            RuntimeCommandResult::CancelRun(runtime.cancel_run(request).await?),
            false,
        ),
        RuntimeCommand::ShutdownRuntime(request) => (
            RuntimeCommandResult::ShutdownRuntime(runtime.shutdown(request).await?),
            true,
        ),
    };
    Ok((HostCommandResult::Runtime(Box::new(result)), shutdown))
}
