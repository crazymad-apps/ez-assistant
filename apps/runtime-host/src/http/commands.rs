//! 有界 JSON Command ingress 与 Runtime 薄 dispatch。

use assistant_protocol::{
    DeviceGatewayCommand, DeviceGatewayCommandResult, DeviceGatewayMutationResult, RuntimeCommand,
    RuntimeCommandResult, RuntimeErrorInfo,
};
use assistant_runtime::RuntimeError;
use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use super::{HttpState, error::runtime_status};

const MAX_REQUEST_ID_BYTES: usize = 128;

/// Desktop HTTP Command 的关联 ID 与 Host 级命令外壳。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CommandRequest {
    pub(crate) request_id: String,
    pub(crate) command: HostCommand,
}

/// Host 接受的顶层命令域。
///
/// Device Gateway 管理动作在 Host 处理；只有 Runtime 分支进入业务 Runtime。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
pub(crate) enum HostCommand {
    Runtime(RuntimeCommand),
    DeviceGateway(DeviceGatewayCommand),
}

/// Host Command 成功后的关联响应。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct CommandResponse {
    pub(crate) request_id: String,
    pub(crate) result: HostCommandResult,
}

/// 与 [`HostCommand`] 分域对应的成功结果。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
pub(crate) enum HostCommandResult {
    Runtime(Box<RuntimeCommandResult>),
    DeviceGateway(DeviceGatewayCommandResult),
}

/// Command 失败时返回的脱敏响应体。
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

    match dispatch(&state, request.command).await {
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
        Err(error) => command_error(Some(request.request_id), error),
    }
}

fn command_error(request_id: Option<String>, error: RuntimeErrorInfo) -> Response {
    let status = runtime_status(error.code);
    (status, Json(CommandErrorBody { request_id, error })).into_response()
}

async fn dispatch(
    state: &HttpState,
    command: HostCommand,
) -> Result<(HostCommandResult, bool), RuntimeErrorInfo> {
    match command {
        HostCommand::Runtime(command) => dispatch_runtime(state, command)
            .await
            .map_err(|error| error.to_protocol_info()),
        HostCommand::DeviceGateway(command) => dispatch_device_gateway(state, command).await,
    }
}

async fn dispatch_device_gateway(
    state: &HttpState,
    command: DeviceGatewayCommand,
) -> Result<(HostCommandResult, bool), RuntimeErrorInfo> {
    let result = match command {
        DeviceGatewayCommand::GetSnapshot(_) => DeviceGatewayCommandResult::GetSnapshot(
            state
                .device_gateway
                .snapshot()
                .await
                .map_err(|error| error.to_protocol_info())?,
        ),
        DeviceGatewayCommand::SetAccessEnabled(request) => {
            state
                .device_gateway
                .set_enabled(request.enabled)
                .await
                .map_err(|error| error.to_protocol_info())?;
            DeviceGatewayCommandResult::SetAccessEnabled(DeviceGatewayMutationResult {
                snapshot: state
                    .device_gateway
                    .snapshot()
                    .await
                    .map_err(|error| error.to_protocol_info())?,
            })
        }
        DeviceGatewayCommand::OpenPairingWindow(_) => {
            state
                .device_gateway
                .open_pairing_window()
                .await
                .map_err(|error| error.to_protocol_info())?;
            DeviceGatewayCommandResult::OpenPairingWindow(DeviceGatewayMutationResult {
                snapshot: state
                    .device_gateway
                    .snapshot()
                    .await
                    .map_err(|error| error.to_protocol_info())?,
            })
        }
        DeviceGatewayCommand::ClosePairingWindow(_) => {
            state.device_gateway.close_pairing_window().await;
            DeviceGatewayCommandResult::ClosePairingWindow(DeviceGatewayMutationResult {
                snapshot: state
                    .device_gateway
                    .snapshot()
                    .await
                    .map_err(|error| error.to_protocol_info())?,
            })
        }
        DeviceGatewayCommand::ConfirmPairing(request) => {
            state
                .device_gateway
                .confirm_pairing(request)
                .await
                .map_err(|error| error.to_protocol_info())?;
            DeviceGatewayCommandResult::ConfirmPairing(DeviceGatewayMutationResult {
                snapshot: state
                    .device_gateway
                    .snapshot()
                    .await
                    .map_err(|error| error.to_protocol_info())?,
            })
        }
        DeviceGatewayCommand::RenameDevice(request) => {
            state
                .runtime
                .rename_paired_device(request.device_id, request.display_name)
                .await
                .map_err(|error| error.to_protocol_info())?;
            state.device_gateway.notify_changed();
            DeviceGatewayCommandResult::RenameDevice(DeviceGatewayMutationResult {
                snapshot: state
                    .device_gateway
                    .snapshot()
                    .await
                    .map_err(|error| error.to_protocol_info())?,
            })
        }
        DeviceGatewayCommand::RevokeDevice(request) => {
            state
                .runtime
                .revoke_paired_device(request.device_id.clone())
                .await
                .map_err(|error| error.to_protocol_info())?;
            state
                .device_gateway
                .revoke_connection(&request.device_id)
                .await;
            state.device_gateway.notify_changed();
            DeviceGatewayCommandResult::RevokeDevice(DeviceGatewayMutationResult {
                snapshot: state
                    .device_gateway
                    .snapshot()
                    .await
                    .map_err(|error| error.to_protocol_info())?,
            })
        }
    };
    Ok((HostCommandResult::DeviceGateway(result), false))
}

async fn dispatch_runtime(
    state: &HttpState,
    command: RuntimeCommand,
) -> Result<(HostCommandResult, bool), RuntimeError> {
    let runtime = state.runtime.as_ref();
    let (result, shutdown) = match command {
        RuntimeCommand::GetApplicationSnapshot(request) => (
            RuntimeCommandResult::GetApplicationSnapshot(
                runtime.get_application_snapshot(request).await?,
            ),
            false,
        ),
        RuntimeCommand::GetSessionView(request) => (
            RuntimeCommandResult::GetSessionView(Box::new(
                runtime.get_session_view(request).await?,
            )),
            false,
        ),
        RuntimeCommand::GetChildTaskView(request) => (
            RuntimeCommandResult::GetChildTaskView(Box::new(
                runtime.get_child_task_view(request).await?,
            )),
            false,
        ),
        RuntimeCommand::ListConversationPage(request) => (
            RuntimeCommandResult::ListConversationPage(
                runtime.list_conversation_page(request).await?,
            ),
            false,
        ),
        RuntimeCommand::GetConversationPageAroundRun(request) => (
            RuntimeCommandResult::GetConversationPageAroundRun(
                runtime.get_conversation_page_around_run(request).await?,
            ),
            false,
        ),
        RuntimeCommand::GetConversationPageAroundMessage(request) => (
            RuntimeCommandResult::GetConversationPageAroundMessage(
                runtime
                    .get_conversation_page_around_message(request)
                    .await?,
            ),
            false,
        ),
        RuntimeCommand::SearchConversationHistory(request) => (
            RuntimeCommandResult::SearchConversationHistory(
                runtime.search_conversation_history(request).await?,
            ),
            false,
        ),
        RuntimeCommand::GetConversationRecallWindow(request) => (
            RuntimeCommandResult::GetConversationRecallWindow(
                runtime.get_conversation_recall_window(request).await?,
            ),
            false,
        ),
        RuntimeCommand::GetToolDetail(request) => (
            RuntimeCommandResult::GetToolDetail(Box::new(runtime.get_tool_detail(request).await?)),
            false,
        ),
        RuntimeCommand::PrioritizeQueuedInput(request) => (
            RuntimeCommandResult::PrioritizeQueuedInput(
                runtime.prioritize_queued_input(request).await?,
            ),
            false,
        ),
        RuntimeCommand::InterruptRun(request) => (
            RuntimeCommandResult::InterruptRun(runtime.interrupt_run(request).await?),
            false,
        ),
        RuntimeCommand::ResumeQueuedInput(request) => (
            RuntimeCommandResult::ResumeQueuedInput(runtime.resume_queued_input(request).await?),
            false,
        ),
        RuntimeCommand::RejectApprovalAndStopRun(request) => (
            RuntimeCommandResult::RejectApprovalAndStopRun(
                runtime.reject_approval_and_stop_run(request).await?,
            ),
            false,
        ),
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
        RuntimeCommand::ReloadConfig(request) => {
            let result = runtime.reload_config(request).await?;
            state.speech.reload().await;
            state.device_gateway.notify_changed();
            (RuntimeCommandResult::ReloadConfig(result), false)
        }
        RuntimeCommand::CreateModel(request) => (
            RuntimeCommandResult::CreateModel(runtime.create_model(request).await?),
            false,
        ),
        RuntimeCommand::UpdateModel(request) => (
            RuntimeCommandResult::UpdateModel(runtime.update_model(request).await?),
            false,
        ),
        RuntimeCommand::DeleteModel(request) => (
            RuntimeCommandResult::DeleteModel(runtime.delete_model(request).await?),
            false,
        ),
        RuntimeCommand::SetDefaultModel(request) => (
            RuntimeCommandResult::SetDefaultModel(runtime.set_default_model(request).await?),
            false,
        ),
        RuntimeCommand::SetAuxiliaryVisionModel(request) => (
            RuntimeCommandResult::SetAuxiliaryVisionModel(
                runtime.set_auxiliary_vision_model(request).await?,
            ),
            false,
        ),
        RuntimeCommand::GetMemoryCapabilities(request) => (
            RuntimeCommandResult::GetMemoryCapabilities(
                runtime.get_memory_capabilities(request).await?,
            ),
            false,
        ),
        RuntimeCommand::GetPersona(request) => (
            RuntimeCommandResult::GetPersona(runtime.get_persona(request).await?),
            false,
        ),
        RuntimeCommand::SetPersona(request) => (
            RuntimeCommandResult::SetPersona(runtime.set_persona(request).await?),
            false,
        ),
        RuntimeCommand::ListPinnedMemories(request) => (
            RuntimeCommandResult::ListPinnedMemories(runtime.list_pinned_memories(request).await?),
            false,
        ),
        RuntimeCommand::CreatePinnedMemory(request) => (
            RuntimeCommandResult::CreatePinnedMemory(runtime.create_pinned_memory(request).await?),
            false,
        ),
        RuntimeCommand::UpdatePinnedMemory(request) => (
            RuntimeCommandResult::UpdatePinnedMemory(runtime.update_pinned_memory(request).await?),
            false,
        ),
        RuntimeCommand::DeletePinnedMemory(request) => (
            RuntimeCommandResult::DeletePinnedMemory(runtime.delete_pinned_memory(request).await?),
            false,
        ),
        RuntimeCommand::GetSystemContext(request) => (
            RuntimeCommandResult::GetSystemContext(runtime.get_system_context(request).await?),
            false,
        ),
        RuntimeCommand::ReloadPermissions(request) => (
            RuntimeCommandResult::ReloadPermissions(runtime.reload_permissions(request).await?),
            false,
        ),
        RuntimeCommand::GetPermissionDocument(request) => (
            RuntimeCommandResult::GetPermissionDocument(
                runtime.get_permission_document(request).await?,
            ),
            false,
        ),
        RuntimeCommand::ReplacePermissionDocument(request) => (
            RuntimeCommandResult::ReplacePermissionDocument(
                runtime.replace_permission_document(request).await?,
            ),
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
        RuntimeCommand::ForkSession(request) => (
            RuntimeCommandResult::ForkSession(runtime.fork_session(request).await?),
            false,
        ),
        RuntimeCommand::PrepareDeleteSession(request) => (
            RuntimeCommandResult::PrepareDeleteSession(
                runtime.prepare_delete_session(request).await?,
            ),
            false,
        ),
        RuntimeCommand::DeleteSession(request) => (
            RuntimeCommandResult::DeleteSession(runtime.delete_session(request).await?),
            false,
        ),
        RuntimeCommand::ClearSession(request) => (
            RuntimeCommandResult::ClearSession(runtime.clear_session(request).await?),
            false,
        ),
        RuntimeCommand::CompactSession(request) => (
            RuntimeCommandResult::CompactSession(runtime.compact_session(request).await?),
            false,
        ),
        RuntimeCommand::CancelSessionCompaction(request) => (
            RuntimeCommandResult::CancelSessionCompaction(
                runtime.cancel_session_compaction(request).await?,
            ),
            false,
        ),
        RuntimeCommand::ListSessions(request) => (
            RuntimeCommandResult::ListSessions(runtime.list_sessions(request)?),
            false,
        ),
        RuntimeCommand::ListSkills(request) => (
            RuntimeCommandResult::ListSkills(runtime.list_skills(request).await?),
            false,
        ),
        RuntimeCommand::GetSkillDetail(request) => (
            RuntimeCommandResult::GetSkillDetail(runtime.get_skill_detail(request).await?),
            false,
        ),
        RuntimeCommand::SetSkillEnabled(request) => (
            RuntimeCommandResult::SetSkillEnabled(runtime.set_skill_enabled(request).await?),
            false,
        ),
        RuntimeCommand::GetSession(request) => (
            RuntimeCommandResult::GetSession(runtime.get_session(request)?),
            false,
        ),
        RuntimeCommand::SubmitInput(request) => (
            RuntimeCommandResult::SubmitInput(
                runtime
                    .submit_session_input(assistant_runtime::SubmitSessionInputRequest {
                        input: request,
                        source: assistant_runtime::InputChannelSource::desktop_text(),
                    })
                    .await?,
            ),
            false,
        ),
        RuntimeCommand::ClearWorkPlan(request) => (
            RuntimeCommandResult::ClearWorkPlan(runtime.clear_work_plan(request).await?),
            false,
        ),
        RuntimeCommand::StopGoal(request) => (
            RuntimeCommandResult::StopGoal(runtime.stop_goal(request).await?),
            false,
        ),
        RuntimeCommand::ResumeGoal(request) => (
            RuntimeCommandResult::ResumeGoal(runtime.resume_goal(request).await?),
            false,
        ),
        RuntimeCommand::ClearGoal(request) => (
            RuntimeCommandResult::ClearGoal(runtime.clear_goal(request).await?),
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
        RuntimeCommand::RenameSession(request) => (
            RuntimeCommandResult::RenameSession(runtime.rename_session(request).await?),
            false,
        ),
        RuntimeCommand::SetSessionPinned(request) => (
            RuntimeCommandResult::SetSessionPinned(runtime.set_session_pinned(request).await?),
            false,
        ),
        RuntimeCommand::SetSessionProxy(request) => (
            RuntimeCommandResult::SetSessionProxy(runtime.set_session_proxy(request).await?),
            false,
        ),
        RuntimeCommand::SetCurrentControllerOutputHosting(request) => (
            RuntimeCommandResult::SetCurrentControllerOutputHosting(
                runtime
                    .set_current_controller_output_hosting(request)
                    .await?,
            ),
            false,
        ),
        RuntimeCommand::SetMessageFeedback(request) => (
            RuntimeCommandResult::SetMessageFeedback(runtime.set_message_feedback(request).await?),
            false,
        ),
        RuntimeCommand::SetSessionModel(request) => (
            RuntimeCommandResult::SetSessionModel(runtime.set_session_model(request).await?),
            false,
        ),
        RuntimeCommand::SetSessionReasoningEffort(request) => (
            RuntimeCommandResult::SetSessionReasoningEffort(
                runtime.set_session_reasoning_effort(request).await?,
            ),
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
