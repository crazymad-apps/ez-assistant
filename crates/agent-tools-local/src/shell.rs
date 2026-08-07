//! 真实本地 Shell 能力实现。

mod output;

use std::{
    ffi::OsString,
    process::{ExitStatus, Stdio},
};

use agent_tools::{
    ShellFuture, ShellOutcome, ShellOutputChannel, ShellOutputSink, ShellProcessMode, ShellRequest,
    ShellTool, ShellToolError,
};
use tokio::{
    process::Command,
    sync::mpsc,
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;

use crate::{
    environment::EnvironmentPolicy,
    process::{self, ManagedChild},
};

use output::{
    OUTPUT_CHANNEL_CAPACITY, OutputCompletion, abort_and_settle_output, collect_output,
    drain_output, settle_output_until,
};

#[cfg(test)]
use agent_tools::ShellOutputChunk;
#[cfg(test)]
use output::{IncrementalUtf8Decoder, OutputCollector, RawOutput};

const PARENT_STATUS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// 平台 Shell 启动器。
///
/// `program` 和 `fixed_args` 是 Adapter 配置，不是模型输入。执行时会将
/// 模型给出的完整 command 原样追加为最后一个参数。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellLauncher {
    pub program: OsString,
    pub fixed_args: Vec<OsString>,
}

#[cfg(unix)]
impl Default for ShellLauncher {
    fn default() -> Self {
        Self {
            program: OsString::from("/bin/sh"),
            fixed_args: vec![OsString::from("-c")],
        }
    }
}

#[cfg(windows)]
impl Default for ShellLauncher {
    fn default() -> Self {
        let program = std::env::var_os("COMSPEC")
            .filter(|program| !program.is_empty())
            .unwrap_or_else(|| OsString::from("cmd.exe"));
        Self {
            program,
            fixed_args: vec![OsString::from("/C")],
        }
    }
}

/// 本地 Shell Adapter 的构造配置。
///
/// 环境策略必须由宿主显式传入，避免 Adapter 暗中假定具体 Provider
/// credential 名称。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalShellConfig {
    pub launcher: ShellLauncher,
    pub environment: EnvironmentPolicy,
}

impl LocalShellConfig {
    /// 使用平台默认 launcher 和显式环境策略构造配置。
    pub fn new(environment: EnvironmentPolicy) -> Self {
        Self {
            launcher: ShellLauncher::default(),
            environment,
        }
    }
}

/// 使用当前用户权限启动平台 Shell 的本地 Adapter。
///
/// 它不是 OS 沙箱：workdir 只决定子进程的初始目录，Shell 仍然可以
/// 使用当前用户权限访问其他路径。
#[derive(Debug)]
pub struct LocalShell {
    config: LocalShellConfig,
}

impl LocalShell {
    pub fn new(config: LocalShellConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &LocalShellConfig {
        &self.config
    }

    async fn run(
        &self,
        request: ShellRequest,
        sink: ShellOutputSink,
        cancellation: CancellationToken,
    ) -> Result<ShellOutcome, ShellToolError> {
        if request.command.trim().is_empty() {
            return Err(ShellToolError::InvalidInput {
                message: "command must not be empty".to_owned(),
            });
        }
        if cancellation.is_cancelled() {
            return Err(ShellToolError::Cancelled);
        }

        let mut command = Command::new(&self.config.launcher.program);
        command
            .args(&self.config.launcher.fixed_args)
            .arg(&request.command)
            .current_dir(request.workdir.as_path())
            .env_clear()
            .envs(self.config.environment.resolve_current())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let spawn = match request.process_mode {
            ShellProcessMode::Managed => process::spawn,
            ShellProcessMode::Detached => process::spawn_detachable,
        };
        let mut child = spawn(command).map_err(|error| ShellToolError::Io {
            message: format!("start shell failed: {error}"),
        })?;
        let stdout = match child.stdout().take() {
            Some(stdout) => stdout,
            None => {
                cleanup_after_setup_failure(&mut child).await?;
                return Err(ShellToolError::Io {
                    message: "shell stdout pipe was not created".to_owned(),
                });
            }
        };
        let stderr = match child.stderr().take() {
            Some(stderr) => stderr,
            None => {
                cleanup_after_setup_failure(&mut child).await?;
                return Err(ShellToolError::Io {
                    message: "shell stderr pipe was not created".to_owned(),
                });
            }
        };

        let (sender, receiver) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
        let stdout_task = tokio::spawn(drain_output(
            stdout,
            ShellOutputChannel::Stdout,
            sender.clone(),
        ));
        let stderr_task = tokio::spawn(drain_output(
            stderr,
            ShellOutputChannel::Stderr,
            sender.clone(),
        ));
        drop(sender);
        let output_task = tokio::spawn(collect_output(
            receiver,
            request.max_output_bytes.get(),
            sink,
        ));

        let deadline = Instant::now() + request.timeout;
        let process_completion = wait_for_parent(&mut child, deadline, &cancellation).await?;
        match process_completion {
            ProcessCompletion::Exited(status) => {
                if request.process_mode == ShellProcessMode::Managed {
                    process::terminate_and_wait(&mut child)
                        .await
                        .map_err(|error| ShellToolError::Io {
                            message: format!(
                                "clean up managed shell process tree after parent exit failed: {error}"
                            ),
                        })?;
                }
                let output_completion = settle_output_until(
                    stdout_task,
                    stderr_task,
                    output_task,
                    deadline,
                    &cancellation,
                )
                .await?;
                match output_completion {
                    OutputCompletion::Complete(output) => {
                        // Detached 使用不带 kill-on-drop 的包装；只在父进程退出且两个输出
                        // 管道 EOF 后 drop，明确把仍存活的后代交给操作系统继续管理。
                        drop(child);
                        Ok(ShellOutcome {
                            exit_code: status.code(),
                            stdout: output.stdout,
                            stderr: output.stderr,
                            truncated: output.truncated,
                            process_mode: request.process_mode,
                        })
                    }
                    OutputCompletion::TimedOut(output) => {
                        process::terminate_and_wait(&mut child).await.map_err(|error| {
                            ShellToolError::Io {
                                message: format!(
                                    "terminate shell process tree during output settlement failed: {error}"
                                ),
                            }
                        })?;
                        Err(ShellToolError::TimedOut {
                            stdout: output.stdout,
                            stderr: output.stderr,
                            truncated: output.truncated,
                        })
                    }
                    OutputCompletion::Cancelled => {
                        process::terminate_and_wait(&mut child).await.map_err(|error| {
                            ShellToolError::Io {
                                message: format!(
                                    "terminate cancelled shell process tree during output settlement failed: {error}"
                                ),
                            }
                        })?;
                        Err(ShellToolError::Cancelled)
                    }
                }
            }
            ProcessCompletion::TimedOut => {
                process::terminate_and_wait(&mut child)
                    .await
                    .map_err(|error| ShellToolError::Io {
                        message: format!("terminate timed out shell process tree failed: {error}"),
                    })?;
                let output = abort_and_settle_output(stdout_task, stderr_task, output_task).await?;
                Err(ShellToolError::TimedOut {
                    stdout: output.stdout,
                    stderr: output.stderr,
                    truncated: output.truncated,
                })
            }
            ProcessCompletion::Cancelled => {
                process::terminate_and_wait(&mut child)
                    .await
                    .map_err(|error| ShellToolError::Io {
                        message: format!("terminate cancelled shell process tree failed: {error}"),
                    })?;
                abort_and_settle_output(stdout_task, stderr_task, output_task).await?;
                Err(ShellToolError::Cancelled)
            }
        }
    }
}

impl ShellTool for LocalShell {
    fn exec<'a>(
        &'a self,
        request: ShellRequest,
        sink: ShellOutputSink,
        cancellation: CancellationToken,
    ) -> ShellFuture<'a> {
        Box::pin(self.run(request, sink, cancellation))
    }
}

enum ProcessCompletion {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
}

async fn wait_for_parent(
    child: &mut ManagedChild,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> Result<ProcessCompletion, ShellToolError> {
    loop {
        if let Some(status) = child.try_wait().map_err(|error| ShellToolError::Io {
            message: format!("check shell parent status failed: {error}"),
        })? {
            return Ok(ProcessCompletion::Exited(status));
        }
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(ProcessCompletion::Cancelled),
            _ = sleep_until(deadline) => return Ok(ProcessCompletion::TimedOut),
            _ = tokio::time::sleep(PARENT_STATUS_POLL_INTERVAL) => {}
        }
    }
}

async fn cleanup_after_setup_failure(child: &mut ManagedChild) -> Result<(), ShellToolError> {
    process::terminate_and_wait(child)
        .await
        .map(|_| ())
        .map_err(|error| ShellToolError::Io {
            message: format!("clean up shell after pipe setup failure failed: {error}"),
        })
}

#[cfg(test)]
mod tests;
