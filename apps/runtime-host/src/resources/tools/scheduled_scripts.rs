//! 已接纳的脚本在所属 Host 用户域选择本地工作目录和现有受管 Shell。

use std::{io, path::PathBuf};

use agent_tools::{AbsolutePath, ShellOutcome, ShellProcessMode, ShellRequest, ShellToolError};
use agent_tools_local::LocalShellCompletion;
use assistant_protocol::{
    ScheduleExecutionId, ScheduleExecutionReason as Reason, ScheduleExecutionStatus as Status,
    ScheduleId, ScheduleScriptResult,
};
use assistant_runtime::{
    RunToolFactory, ScheduleScriptExecutor, ScheduleScriptFuture, StoredWorkspace,
};
use tokio_util::sync::CancellationToken;

use super::HostRunToolFactory;

impl ScheduleScriptExecutor for HostRunToolFactory {
    fn records_process_identity(&self) -> bool {
        true
    }

    fn startup_cleanup_confirmed(&self) -> bool {
        let directory = self
            .resources
            .paths
            .user_root
            .join(agent_tools_local::BACKGROUND_SHELL_DIRECTORY);
        match std::fs::read_dir(directory) {
            Ok(entries) => entries.into_iter().all(|entry| {
                entry.is_ok_and(|entry| {
                    !entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("scheduled_")
                })
            }),
            Err(error) => error.kind() == io::ErrorKind::NotFound,
        }
    }

    fn execute<'a>(
        &'a self,
        schedule_id: &'a ScheduleId,
        execution_id: &'a ScheduleExecutionId,
        workspace: Option<&'a StoredWorkspace>,
        command: String,
        cancellation: CancellationToken,
    ) -> ScheduleScriptFuture<'a> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return (Status::Cancelled, ScheduleScriptResult::default());
            }
            let project = workspace.map(|workspace| {
                PathBuf::from(
                    workspace
                        .user_directory
                        .as_deref()
                        .unwrap_or(&workspace.agent_directory),
                )
            });
            let paths = self.resources.paths.clone();
            let id = schedule_id.clone();
            let resolved = tokio::task::spawn_blocking(move || workdir(&paths, &id, project)).await;
            let Ok(Ok(workdir)) = resolved else {
                return failure(Status::Failed, Reason::WorkspaceUnavailable);
            };
            let Ok(shell) = self.freeze_shell(None) else {
                return failure(Status::Failed, Reason::ShellUnavailable);
            };
            let request = ShellRequest {
                command,
                workdir,
                timeout: self.resources.shell_config.default_timeout(),
                max_output_bytes: self.resources.shell_config.max_output_bytes(),
                process_mode: ShellProcessMode::Managed,
            };
            match self
                .resources
                .shell_for(shell.as_ref())
                .exec_recoverable_managed(
                    request,
                    cancellation,
                    &self.resources.paths.user_root,
                    execution_id.as_str(),
                )
                .await
            {
                Ok(LocalShellCompletion::Exited(outcome)) => match outcome.exit_code {
                    Some(0) => (Status::Succeeded, captured(outcome)),
                    Some(_) => (Status::Failed, captured(outcome)),
                    None => (
                        Status::Failed,
                        ScheduleScriptResult {
                            reason: Some(Reason::Signalled),
                            ..captured(outcome)
                        },
                    ),
                },
                Ok(LocalShellCompletion::Cancelled(outcome)) => {
                    (Status::Cancelled, captured(outcome))
                }
                Err(ShellToolError::TimedOut {
                    stdout,
                    stderr,
                    truncated,
                }) => (
                    Status::TimedOut,
                    ScheduleScriptResult {
                        stdout,
                        stderr,
                        truncated,
                        ..Default::default()
                    },
                ),
                Err(ShellToolError::CleanupUnconfirmed { .. }) => {
                    failure(Status::CleanupUnconfirmed, Reason::CleanupUnconfirmed)
                }
                Err(_) => failure(Status::Failed, Reason::ShellUnavailable),
            }
        })
    }
}

fn workdir(
    paths: &crate::user_paths::UserPaths,
    id: &ScheduleId,
    project: Option<PathBuf>,
) -> io::Result<AbsolutePath> {
    let path = if let Some(path) = project {
        if paths.resolve(&path, false)? != path || !path.is_dir() {
            return Err(io::Error::other("schedule project directory changed"));
        }
        path
    } else {
        if !id
            .as_str()
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'_' | b'-'))
        {
            return Err(io::Error::other("invalid schedule path component"));
        }
        let parent = paths.user_root.join("data").join("schedules");
        crate::config_source::prepare_private_directory(&parent).map_err(io::Error::other)?;
        let path = parent.join(id.as_str());
        crate::config_source::prepare_private_directory(&path).map_err(io::Error::other)?;
        let canonical = std::fs::canonicalize(&path)?;
        if !canonical.starts_with(std::fs::canonicalize(&paths.user_root)?) {
            return Err(io::Error::other(
                "schedule directory is outside its user root",
            ));
        }
        canonical
    };
    AbsolutePath::new(path).map_err(io::Error::other)
}

fn captured(outcome: ShellOutcome) -> ScheduleScriptResult {
    ScheduleScriptResult {
        exit_code: outcome.exit_code,
        stdout: outcome.stdout,
        stderr: outcome.stderr,
        truncated: outcome.truncated,
        reason: None,
    }
}

fn failure(status: Status, reason: Reason) -> (Status, ScheduleScriptResult) {
    (
        status,
        ScheduleScriptResult {
            reason: Some(reason),
            ..Default::default()
        },
    )
}
