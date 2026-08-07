//! Host 私有命令到 Assistant Runtime 公共命令的薄路由。

use assistant_protocol::{RuntimeCommand, RuntimeCommandResult};
use assistant_runtime::AssistantRuntime;

use crate::wire::{HostCommand, HostCommandResult, ServerFrame};

pub(super) async fn dispatch(
    runtime: &AssistantRuntime,
    request_id: String,
    command: HostCommand,
) -> (ServerFrame, bool) {
    let result = match command {
        HostCommand::Runtime(command) => dispatch_runtime(runtime, command).await,
        HostCommand::ConversationSnapshot { session_id } => runtime
            .conversation_snapshot(&session_id)
            .map(|conversation| {
                (
                    HostCommandResult::ConversationSnapshot { conversation },
                    false,
                )
            }),
    };

    match result {
        Ok((result, shutdown)) => (ServerFrame::Response { request_id, result }, shutdown),
        Err(error) => (
            ServerFrame::Error {
                request_id,
                error: error.to_protocol_info(),
            },
            false,
        ),
    }
}

async fn dispatch_runtime(
    runtime: &AssistantRuntime,
    command: RuntimeCommand,
) -> Result<(HostCommandResult, bool), assistant_runtime::RuntimeError> {
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
        RuntimeCommand::ValidateModelConnection(request) => (
            RuntimeCommandResult::ValidateModelConnection(
                runtime.validate_model_connection(request).await?,
            ),
            false,
        ),
        RuntimeCommand::CreateSession(request) => (
            RuntimeCommandResult::CreateSession(runtime.create_session(request)?),
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
        RuntimeCommand::StartRun(request) => (
            RuntimeCommandResult::StartRun(runtime.start_run(request)?),
            false,
        ),
        RuntimeCommand::GetRun(request) => (
            RuntimeCommandResult::GetRun(runtime.get_run(request)?),
            false,
        ),
        RuntimeCommand::CancelRun(request) => (
            RuntimeCommandResult::CancelRun(runtime.cancel_run(request)?),
            false,
        ),
        RuntimeCommand::ShutdownRuntime(request) => (
            RuntimeCommandResult::ShutdownRuntime(runtime.shutdown(request).await?),
            true,
        ),
    };
    Ok((HostCommandResult::Runtime(result), shutdown))
}
