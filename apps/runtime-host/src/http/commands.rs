//! 有界 JSON Command ingress 与 Runtime 薄 dispatch。

use assistant_protocol::{
    DeviceGatewayCommand, DeviceGatewayCommandResult, DeviceGatewayMutationResult,
    HostAccessCommand, HostAccessStatus, RuntimeCommand, RuntimeCommandResult, RuntimeErrorInfo,
};
use assistant_runtime::RuntimeError;
use axum::{
    Extension, Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use super::{HttpState, error::runtime_status};
use crate::access::AccessPermit;

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
    HostAccess(HostAccessCommand),
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
    HostAccess(HostAccessStatus),
}

/// Command 失败时返回的脱敏响应体。
#[derive(Serialize)]
struct CommandErrorBody {
    request_id: Option<String>,
    error: RuntimeErrorInfo,
}

pub(super) async fn handle_command(
    State(state): State<HttpState>,
    Extension(permit): Extension<AccessPermit>,
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

    if let Err(error) = permit.check() {
        return command_error(Some(request.request_id), error.protocol_info());
    }
    if matches!(
        request.command,
        HostCommand::Runtime(RuntimeCommand::ShutdownRuntime(_))
    ) && !permit.native
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": {"message": "只能通过本机原生入口停止 Runtime。"}})),
        )
            .into_response();
    }
    // 本机关闭属于进程控制；未就绪时不能进入任何业务 dispatch。
    if state.startup.services().is_err() {
        if matches!(
            request.command,
            HostCommand::Runtime(RuntimeCommand::ShutdownRuntime(_))
        ) {
            state.shutdown.cancel();
            return Json(CommandResponse {
                request_id: request.request_id,
                result: HostCommandResult::Runtime(Box::new(
                    RuntimeCommandResult::ShutdownRuntime(
                        assistant_protocol::ShutdownRuntimeResult {
                            lifecycle: assistant_protocol::RuntimeLifecycle::ShuttingDown,
                        },
                    ),
                )),
            })
            .into_response();
        }
        return super::error::HttpError::unavailable().into_response();
    }
    if matches!(
        request.command,
        HostCommand::Runtime(RuntimeCommand::ReloadConfig(_))
    ) && let Err(error) = state.access.command(None, permit.clone()).await
    {
        return command_error(Some(request.request_id), error.protocol_info());
    }
    let dispatched = dispatch(&state, request.command, permit).await;
    match dispatched {
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
    permit: AccessPermit,
) -> Result<(HostCommandResult, bool), RuntimeErrorInfo> {
    match command {
        HostCommand::Runtime(command) => dispatch_runtime(state, command)
            .await
            .map_err(|error| error.to_protocol_info()),
        HostCommand::DeviceGateway(command) => dispatch_device_gateway(state, command).await,
        HostCommand::HostAccess(command) => state
            .access
            .command(Some(command), permit)
            .await
            .map(|status| (HostCommandResult::HostAccess(status), false))
            .map_err(|error| error.protocol_info()),
    }
}

async fn dispatch_device_gateway(
    state: &HttpState,
    command: DeviceGatewayCommand,
) -> Result<(HostCommandResult, bool), RuntimeErrorInfo> {
    let services = state
        .startup
        .services()
        .map_err(|error| error.to_protocol_info())?;
    let result = match command {
        DeviceGatewayCommand::GetSnapshot(_) => DeviceGatewayCommandResult::GetSnapshot(
            services
                .device_gateway
                .snapshot()
                .await
                .map_err(|error| error.to_protocol_info())?,
        ),
        DeviceGatewayCommand::SetAccessEnabled(request) => {
            services
                .device_gateway
                .set_enabled(request.enabled)
                .await
                .map_err(|error| error.to_protocol_info())?;
            DeviceGatewayCommandResult::SetAccessEnabled(DeviceGatewayMutationResult {
                snapshot: services
                    .device_gateway
                    .snapshot()
                    .await
                    .map_err(|error| error.to_protocol_info())?,
            })
        }
        DeviceGatewayCommand::OpenPairingWindow(_) => {
            services
                .device_gateway
                .open_pairing_window()
                .await
                .map_err(|error| error.to_protocol_info())?;
            DeviceGatewayCommandResult::OpenPairingWindow(DeviceGatewayMutationResult {
                snapshot: services
                    .device_gateway
                    .snapshot()
                    .await
                    .map_err(|error| error.to_protocol_info())?,
            })
        }
        DeviceGatewayCommand::ClosePairingWindow(_) => {
            services.device_gateway.close_pairing_window().await;
            DeviceGatewayCommandResult::ClosePairingWindow(DeviceGatewayMutationResult {
                snapshot: services
                    .device_gateway
                    .snapshot()
                    .await
                    .map_err(|error| error.to_protocol_info())?,
            })
        }
        DeviceGatewayCommand::ConfirmPairing(request) => {
            services
                .device_gateway
                .confirm_pairing(request)
                .await
                .map_err(|error| error.to_protocol_info())?;
            DeviceGatewayCommandResult::ConfirmPairing(DeviceGatewayMutationResult {
                snapshot: services
                    .device_gateway
                    .snapshot()
                    .await
                    .map_err(|error| error.to_protocol_info())?,
            })
        }
        DeviceGatewayCommand::RenameDevice(request) => {
            services
                .runtime
                .rename_paired_device(request.device_id, request.display_name)
                .await
                .map_err(|error| error.to_protocol_info())?;
            services.device_gateway.notify_changed();
            DeviceGatewayCommandResult::RenameDevice(DeviceGatewayMutationResult {
                snapshot: services
                    .device_gateway
                    .snapshot()
                    .await
                    .map_err(|error| error.to_protocol_info())?,
            })
        }
        DeviceGatewayCommand::RevokeDevice(request) => {
            services
                .runtime
                .revoke_paired_device(request.device_id.clone())
                .await
                .map_err(|error| error.to_protocol_info())?;
            services
                .device_gateway
                .revoke_connection(&request.device_id)
                .await;
            services.device_gateway.notify_changed();
            DeviceGatewayCommandResult::RevokeDevice(DeviceGatewayMutationResult {
                snapshot: services
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
    let services = state.startup.services()?;
    let runtime = services.runtime.as_ref();
    let (result, shutdown) = match command {
        RuntimeCommand::ListProviders(_) => (
            RuntimeCommandResult::ListProviders(runtime.list_providers()?),
            false,
        ),
        RuntimeCommand::CreateProvider(request) => (
            RuntimeCommandResult::CreateProvider(runtime.create_provider(request).await?),
            false,
        ),
        RuntimeCommand::UpdateProvider(request) => (
            RuntimeCommandResult::UpdateProvider(runtime.update_provider(request).await?),
            false,
        ),
        RuntimeCommand::GetProviderUsage(request) => (
            RuntimeCommandResult::GetProviderUsage(
                runtime
                    .get_provider_usage(request.provider_instance_id)
                    .await?,
            ),
            false,
        ),
        RuntimeCommand::DeleteProvider(request) => (
            RuntimeCommandResult::DeleteProvider(
                runtime
                    .delete_provider(request.provider_instance_id)
                    .await?,
            ),
            false,
        ),
        RuntimeCommand::ListProviderModels(request) => (
            RuntimeCommandResult::ListProviderModels(
                runtime
                    .list_provider_models(request.provider_instance_id)
                    .await?,
            ),
            false,
        ),
        RuntimeCommand::GetModelSettings(_) => (
            RuntimeCommandResult::GetModelSettings(runtime.get_model_settings()?),
            false,
        ),
        RuntimeCommand::GetModelConfiguration(request) => (
            RuntimeCommandResult::GetModelConfiguration(
                runtime.get_model_configuration(request).await?,
            ),
            false,
        ),
        RuntimeCommand::SaveModelFixedConfig(request) => (
            RuntimeCommandResult::SaveModelFixedConfig(
                runtime.save_model_fixed_config(request).await?,
            ),
            false,
        ),
        RuntimeCommand::ResetModelFixedConfig(request) => (
            RuntimeCommandResult::ResetModelFixedConfig(
                runtime.reset_model_fixed_config(request).await?,
            ),
            false,
        ),
        RuntimeCommand::ListFixedModelConfigs(request) => (
            RuntimeCommandResult::ListFixedModelConfigs(
                runtime.list_fixed_model_configs(request).await?,
            ),
            false,
        ),
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
        RuntimeCommand::GetMcpConfiguration(request) => (
            RuntimeCommandResult::GetMcpConfiguration(
                runtime.get_mcp_configuration(request).await?,
            ),
            false,
        ),
        RuntimeCommand::PreviewMcpImport(request) => (
            RuntimeCommandResult::PreviewMcpImport(runtime.preview_mcp_import(request).await?),
            false,
        ),
        RuntimeCommand::MutateMcpConfiguration(request) => (
            RuntimeCommandResult::MutateMcpConfiguration(
                runtime.mutate_mcp_configuration(request).await?,
            ),
            false,
        ),
        RuntimeCommand::TestMcpServer(request) => (
            RuntimeCommandResult::TestMcpServer(runtime.test_mcp_server(request).await?),
            false,
        ),
        RuntimeCommand::CancelMcpServerTest(request) => (
            RuntimeCommandResult::CancelMcpServerTest(runtime.cancel_mcp_server_test(request)?),
            false,
        ),
        RuntimeCommand::ListMcpServerOptions(request) => (
            RuntimeCommandResult::ListMcpServerOptions(
                runtime.list_mcp_server_options(request).await?,
            ),
            false,
        ),
        RuntimeCommand::SubmitSessionCommand(request) => (
            RuntimeCommandResult::SubmitSessionCommand(
                runtime.submit_session_command(request).await?,
            ),
            false,
        ),
        RuntimeCommand::GetConfigStatus(request) => (
            RuntimeCommandResult::GetConfigStatus(runtime.get_config_status(request)?),
            false,
        ),
        RuntimeCommand::ReloadConfig(request) => {
            let result = runtime.reload_config(request).await?;
            state.startup.services()?.speech.reload().await;
            state.startup.services()?.device_gateway.notify_changed();
            (RuntimeCommandResult::ReloadConfig(result), false)
        }
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
            RuntimeCommandResult::ListPendingApprovals(
                runtime.list_pending_approvals(request).await?,
            ),
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
        RuntimeCommand::UpdateWorkspace(request) => (
            RuntimeCommandResult::UpdateWorkspace(runtime.update_workspace(request).await?),
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
        RuntimeCommand::RemoveWorkspace(request) => {
            let _gate = state.terminals.source_gate.lock().await;
            let id = request.workspace_id.clone();
            let result = runtime.remove_workspace(request).await?;
            state.terminals.source_removed(None, Some(&id)).await;
            (RuntimeCommandResult::RemoveWorkspace(result), false)
        }
        RuntimeCommand::GetAttachment(request) => (
            RuntimeCommandResult::GetAttachment(runtime.get_attachment(request).await?),
            false,
        ),
        RuntimeCommand::ListAttachments(request) => (
            RuntimeCommandResult::ListAttachments(runtime.list_attachments(request).await?),
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
        RuntimeCommand::DeleteSession(request) => {
            let _gate = state.terminals.source_gate.lock().await;
            let id = request.session_id.clone();
            let result = runtime.delete_session(request).await?;
            state.terminals.source_removed(Some(&id), None).await;
            (RuntimeCommandResult::DeleteSession(result), false)
        }
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
            RuntimeCommandResult::ListSessions(runtime.list_sessions(request).await?),
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
            RuntimeCommandResult::GetSession(runtime.get_session(request).await?),
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
        RuntimeCommand::GenerateSessionTitle(request) => (
            RuntimeCommandResult::GenerateSessionTitle(
                runtime.generate_session_title(request).await?,
            ),
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
