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
            .await
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
    Ok((HostCommandResult::Runtime(result), shutdown))
}
