//! 真实本地 Shell 能力实现。

mod background;
mod output;

pub use background::{BACKGROUND_SHELL_DIRECTORY, LocalBackgroundShell, recover_background_shells};

use std::{
    ffi::OsString,
    process::{ExitStatus, Stdio},
    sync::Arc,
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
/// `program`、`fixed_args` 和 `command_prefix` 是宿主配置。执行时仅在完整
/// command 前执行冻结的编码初始化语句，不做方言转换；CMD 在初始化后启动命令解释器。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellLauncher {
    pub program: OsString,
    pub fixed_args: Vec<OsString>,
    /// 宿主冻结的编码初始化语句；不分析或改写模型 command，二者拼成唯一脚本参数。
    pub command_prefix: String,
}

#[cfg(unix)]
impl Default for ShellLauncher {
    fn default() -> Self {
        Self {
            program: OsString::from("/bin/sh"),
            fixed_args: vec![OsString::from("-c")],
            command_prefix: String::new(),
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
            command_prefix: String::new(),
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

/// 供需要保存取消前输出的受管宿主使用。
pub enum LocalShellCompletion {
    Exited(ShellOutcome),
    Cancelled(ShellOutcome),
}

impl LocalShell {
    pub fn new(config: LocalShellConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &LocalShellConfig {
        &self.config
    }

    /// 复用 Shell 的平台进程树和输出上限，并返回取消前收集的文本。
    ///
    /// # Errors
    /// 参数、启动、输出、超时或回收失败时返回 Shell 错误。
    pub async fn exec_managed(
        &self,
        mut request: ShellRequest,
        cancellation: CancellationToken,
    ) -> Result<LocalShellCompletion, ShellToolError> {
        request.process_mode = ShellProcessMode::Managed;
        self.run(request, Arc::new(|_| {}), cancellation, None)
            .await
    }

    /// 带持久启动身份的受管 Shell，沿用分离管道和原始结果。仅在进程回收确认后删除身份。
    ///
    /// # Errors
    /// 启动登记或执行失败返回已有 Shell 错误，身份保留表示恢复仍需确认。
    pub async fn exec_recoverable_managed(
        &self,
        mut request: ShellRequest,
        cancellation: CancellationToken,
        user_root: &std::path::Path,
        id: &str,
    ) -> Result<LocalShellCompletion, ShellToolError> {
        request.process_mode = ShellProcessMode::Managed;
        let directory =
            background::prepare_managed_registration(user_root, id).map_err(|error| {
                ShellToolError::CleanupUnconfirmed {
                    message: error.to_string(),
                }
            })?;
        let result = self
            .run(request, Arc::new(|_| {}), cancellation, Some(&directory))
            .await;
        if !matches!(result, Err(ShellToolError::CleanupUnconfirmed { .. })) {
            background::finish_managed_registration(&directory).map_err(|error| {
                ShellToolError::CleanupUnconfirmed {
                    message: error.to_string(),
                }
            })?;
        }
        result
    }

    async fn run(
        &self,
        request: ShellRequest,
        sink: ShellOutputSink,
        cancellation: CancellationToken,
        registration: Option<&std::path::Path>,
    ) -> Result<LocalShellCompletion, ShellToolError> {
        if request.command.trim().is_empty() {
            return Err(ShellToolError::InvalidInput {
                message: "command must not be empty".to_owned(),
            });
        }
        if cancellation.is_cancelled() {
            return Ok(LocalShellCompletion::Cancelled(ShellOutcome {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                truncated: false,
                process_mode: request.process_mode,
            }));
        }

        let command = self.command(&request.workdir, request.command);
        let spawn = match request.process_mode {
            ShellProcessMode::Managed => process::spawn,
            ShellProcessMode::Detached => process::spawn_detachable,
            ShellProcessMode::Background => {
                return Err(ShellToolError::InvalidInput {
                    message: "background shell requires Runtime supervision".into(),
                });
            }
        };
        let mut child = spawn(command).map_err(|error| ShellToolError::Io {
            message: format!("start shell failed: {error}"),
        })?;
        if let Some(directory) = registration
            && let Err(error) = background::record_spawned_process(&mut child, directory).await
        {
            cleanup_after_setup_failure(&mut child).await?;
            return Err(ShellToolError::Io {
                message: format!("managed shell identity could not be recorded: {error}"),
            });
        }
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
        let process_completion = match wait_for_parent(&mut child, deadline, &cancellation).await {
            Ok(completion) => completion,
            Err(error) => {
                cleanup_after_setup_failure(&mut child).await?;
                abort_and_settle_output(stdout_task, stderr_task, output_task).await?;
                return Err(error);
            }
        };
        match process_completion {
            ProcessCompletion::Exited(status) => {
                if request.process_mode == ShellProcessMode::Managed {
                    process::terminate_and_wait(&mut child)
                        .await
                        .map_err(|error| ShellToolError::CleanupUnconfirmed {
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
                        // 管道于 deadline 内 EOF 后 drop，明确把仍存活的后代交给操作系统。
                        // 在此边界前，timeout/cancellation 分支仍负责清理并等待进程组。
                        drop(child);
                        Ok(LocalShellCompletion::Exited(ShellOutcome {
                            exit_code: status.code(),
                            stdout: output.stdout,
                            stderr: output.stderr,
                            truncated: output.truncated,
                            process_mode: request.process_mode,
                        }))
                    }
                    OutputCompletion::TimedOut(output) => {
                        process::terminate_and_wait(&mut child).await.map_err(|error| {
                            ShellToolError::CleanupUnconfirmed {
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
                    OutputCompletion::Cancelled(output) => {
                        process::terminate_and_wait(&mut child).await.map_err(|error| {
                            ShellToolError::CleanupUnconfirmed {
                                message: format!(
                                    "terminate cancelled shell process tree during output settlement failed: {error}"
                                ),
                            }
                        })?;
                        Ok(LocalShellCompletion::Cancelled(ShellOutcome {
                            exit_code: status.code(),
                            stdout: output.stdout,
                            stderr: output.stderr,
                            truncated: output.truncated,
                            process_mode: request.process_mode,
                        }))
                    }
                }
            }
            ProcessCompletion::TimedOut => {
                process::terminate_and_wait(&mut child)
                    .await
                    .map_err(|error| ShellToolError::CleanupUnconfirmed {
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
                    .map_err(|error| ShellToolError::CleanupUnconfirmed {
                        message: format!("terminate cancelled shell process tree failed: {error}"),
                    })?;
                let output = abort_and_settle_output(stdout_task, stderr_task, output_task).await?;
                Ok(LocalShellCompletion::Cancelled(ShellOutcome {
                    exit_code: None,
                    stdout: output.stdout,
                    stderr: output.stderr,
                    truncated: output.truncated,
                    process_mode: request.process_mode,
                }))
            }
        }
    }
    fn command(&self, workdir: &agent_tools::AbsolutePath, script: String) -> Command {
        let mut command = Command::new(&self.config.launcher.program);
        command
            .current_dir(workdir.as_path())
            .env_clear()
            .envs(self.config.environment.resolve_current())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        append_script_argument(&mut command, &self.config.launcher, script);
        command
    }
}

fn append_script_argument(command: &mut Command, launcher: &ShellLauncher, script: String) {
    #[cfg(windows)]
    if std::path::Path::new(&launcher.program)
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("cmd.exe"))
    {
        if !launcher.command_prefix.is_empty() {
            command.arg("/v:on");
        }
        command.args(&launcher.fixed_args);
        // 无窗口控制台初始为 OEM 代码页。先在外层完成 chcp，再让内层 CMD 在
        // UTF-8 环境初始化输出编码；同一个 /c 中 chcp & echo 会沿用旧的编码缓存。
        let script = if launcher.command_prefix.is_empty() {
            script
        } else {
            // 延迟展开只负责把原文传给内层，防止外层提前展开模型脚本中的 %VAR%。
            // 内层关闭延迟展开，保留 CMD 默认的 ! 与 ^ 语义；变量只存在于本次子进程。
            command.env("EZ_ASSISTANT_CMD_SCRIPT", script);
            format!(
                "{}\"{}\" /d /v:off /s /c \"!EZ_ASSISTANT_CMD_SCRIPT!\"",
                launcher.command_prefix,
                launcher.program.to_string_lossy()
            )
        };
        // CMD 不使用 CRT 的参数解码规则；arg 会把脚本中的双引号变成反斜杠+引号。
        // 按 std::os::windows::process::CommandExt::raw_arg 的 CMD 约定包裹整个脚本。
        // 此处接收的是已授权完整 Shell 程序，而非需要转义成单个词的外部数据。
        command.raw_arg(format!("\"{script}\""));
        return;
    }
    command
        .args(&launcher.fixed_args)
        .arg(format!("{}{script}", launcher.command_prefix));
}

impl ShellTool for LocalShell {
    fn exec<'a>(
        &'a self,
        request: ShellRequest,
        sink: ShellOutputSink,
        cancellation: CancellationToken,
    ) -> ShellFuture<'a> {
        Box::pin(async move {
            match self.run(request, sink, cancellation, None).await? {
                LocalShellCompletion::Exited(outcome) => Ok(outcome),
                LocalShellCompletion::Cancelled(_) => Err(ShellToolError::Cancelled),
            }
        })
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
        // 此处只观察根解释器，不消费 Job 的完成端口。process-wrap 9.1 的 Windows
        // try_wait 会取走通知，随后 wait 可能永远等待已被取走的完成事件。
        // 保留包装层与其 Job，直到 terminate_and_wait 统一回收整个进程树。
        #[cfg(windows)]
        let status = child.inner_mut().try_wait();
        #[cfg(not(windows))]
        let status = child.try_wait();
        if let Some(status) = status.map_err(|error| ShellToolError::Io {
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
        .map_err(|error| ShellToolError::CleanupUnconfirmed {
            message: format!("clean up shell after pipe setup failure failed: {error}"),
        })
}

#[cfg(test)]
mod tests;
