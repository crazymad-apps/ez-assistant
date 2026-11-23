//! 受管 Shell 进程与临时输出。应用归属和任务状态由 Runtime 持有。

mod identity;
mod log;
mod recovery;

pub use recovery::{BACKGROUND_SHELL_DIRECTORY, recover_background_shells};

use std::{io, path::PathBuf, sync::Arc, time::Duration};

use agent_tools::{
    BackgroundOutput, BackgroundProcess, BackgroundProcessExit, BackgroundShell,
    BackgroundShellFuture, BackgroundShellRequest, ShellOutputChannel, ShellToolError,
};
use tokio::{sync::mpsc, time::timeout};
use tokio_util::task::AbortOnDropHandle;

use super::{
    LocalShell,
    output::{OUTPUT_CHANNEL_CAPACITY, drain_output},
};
use crate::process::{self, ManagedChild};

const OUTPUT_SETTLEMENT_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) async fn record_spawned_process(
    child: &mut ManagedChild,
    directory: &std::path::Path,
) -> io::Result<()> {
    let pid = child
        .id()
        .ok_or_else(|| io::Error::other("missing background PID"))?;
    if let Some(identity) = identity::capture(pid)? {
        let directory = directory.to_owned();
        tokio::task::spawn_blocking(move || recovery::record_identity(&directory, &identity))
            .await
            .map_err(io::Error::other)??;
    } else {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let ended = identity::owned_parent_exited(pid)?;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let ended = child.inner_mut().try_wait()?.is_some();
        if !ended {
            return Err(io::Error::other("background process identity unavailable"));
        }
    }
    Ok(())
}

pub(super) fn prepare_managed_registration(
    user: &std::path::Path,
    id: &str,
) -> io::Result<PathBuf> {
    if !id.starts_with("scheduled_") || !recovery::valid_id(id) {
        return Err(io::Error::other("invalid recoverable shell identity"));
    }
    let directory = recovery::create_run_directory(&user.join(BACKGROUND_SHELL_DIRECTORY), id)?;
    if let Err(error) = recovery::mark_starting(&directory) {
        let _ = recovery::clean_run_directory(&directory);
        return Err(error);
    }
    Ok(directory)
}

pub(super) fn finish_managed_registration(directory: &std::path::Path) -> io::Result<()> {
    recovery::clean_run_directory(directory)
}

pub struct LocalBackgroundShell {
    shell: Arc<LocalShell>,
    root: PathBuf,
}

impl LocalBackgroundShell {
    /// 用户根由宿主选择，临时目录按首次启动惰性创建。
    pub fn new(shell: Arc<LocalShell>, user_home: PathBuf) -> Self {
        Self {
            shell,
            root: user_home.join(BACKGROUND_SHELL_DIRECTORY),
        }
    }

    async fn spawn(
        &self,
        id: &str,
        request: BackgroundShellRequest,
    ) -> Result<Box<dyn BackgroundProcess>, ShellToolError> {
        if request.command.trim().is_empty() || !recovery::valid_id(id) {
            return Err(ShellToolError::InvalidInput {
                message: "invalid background shell request".into(),
            });
        }
        let directory = recovery::create_run_directory(&self.root, id).map_err(io_error)?;
        let result = self.spawn_in(&directory, request).await;
        if let Err(error) = &result
            && !matches!(error, ShellToolError::CleanupUnconfirmed { .. })
            && let Err(cleanup) = recovery::clean_run_directory(&directory)
        {
            return Err(ShellToolError::Io {
                message: format!("{error}; temporary background files retained: {cleanup}"),
            });
        }
        result
    }

    async fn spawn_in(
        &self,
        directory: &std::path::Path,
        request: BackgroundShellRequest,
    ) -> Result<Box<dyn BackgroundProcess>, ShellToolError> {
        let log = Arc::new(
            Log::open(directory.join(recovery::OUTPUT_FILE))
                .await
                .map_err(io_error)?,
        );
        let mut child = process::spawn(self.shell.command(&request.workdir, request.command))
            .map_err(io_error)?;
        // 主进程仍由启动者持有。落盘和管道交接失败必须等待清理后才允许释放容量。
        let setup = async {
            record_spawned_process(&mut child, directory).await?;
            let stdout = child
                .stdout()
                .take()
                .ok_or_else(|| io::Error::other("missing stdout"))?;
            let stderr = child
                .stderr()
                .take()
                .ok_or_else(|| io::Error::other("missing stderr"))?;
            Ok((stdout, stderr))
        }
        .await;
        let (stdout, stderr) = match setup {
            Ok(pipes) => pipes,
            Err(error) => {
                stop_tree(&mut child).await?;
                return Err(io_error(error));
            }
        };

        let output = log.clone();
        let output_task = AbortOnDropHandle::new(tokio::spawn(async move {
            let (tx, rx) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
            let (stdout, stderr, ()) = tokio::join!(
                drain_output(stdout, ShellOutputChannel::Stdout, tx.clone()),
                drain_output(stderr, ShellOutputChannel::Stderr, tx),
                output.collect(rx),
            );
            stdout.and(stderr)
        }));
        Ok(Box::new(LocalBackgroundProcess {
            child,
            log,
            directory: directory.to_owned(),
            output_task: Some(output_task),
        }))
    }
}

use self::log::Log;

impl BackgroundShell for LocalBackgroundShell {
    fn start<'a>(
        &'a self,
        id: &'a str,
        request: BackgroundShellRequest,
    ) -> BackgroundShellFuture<'a, Box<dyn BackgroundProcess>> {
        Box::pin(self.spawn(id, request))
    }
}

struct LocalBackgroundProcess {
    child: ManagedChild,
    log: Arc<Log>,
    directory: PathBuf,
    output_task: Option<AbortOnDropHandle<io::Result<()>>>,
}

impl BackgroundProcess for LocalBackgroundProcess {
    fn output(&self) -> Arc<dyn BackgroundOutput> {
        self.log.clone()
    }

    fn wait_parent(&mut self) -> BackgroundShellFuture<'_, ()> {
        Box::pin(async {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                // 先观察退出再向组发送 SIGKILL，保留未 reap 的主 PID，避免此间复用组编号。
                let Some(pid) = self.child.id() else {
                    return Ok(());
                };
                while !identity::owned_parent_exited(pid).map_err(io_error)? {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
                Ok(())
            }
            // 在 Windows 上只等真实子进程，不消耗 Job Object 的完成端口通知。
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            self.child
                .inner_mut()
                .wait()
                .await
                .map(|_| ())
                .map_err(io_error)
        })
    }

    fn finish(&mut self) -> BackgroundShellFuture<'_, BackgroundProcessExit> {
        Box::pin(async {
            let status = stop_tree(&mut self.child).await?;
            let output_task = self
                .output_task
                .take()
                .ok_or_else(|| io_error(io::Error::other("background output already settled")))?;
            let mut output_task = output_task;
            let mut output_error = match timeout(OUTPUT_SETTLEMENT_TIMEOUT, &mut output_task).await
            {
                Ok(Ok(Ok(()))) => None,
                Ok(Ok(Err(error))) => Some(error.to_string()),
                Ok(Err(error)) => Some(error.to_string()),
                Err(_) => {
                    output_task.abort();
                    let _ = output_task.await;
                    Some("output pipes did not close after process cleanup".into())
                }
            };
            let output = self
                .log
                .close()
                .await
                .map_err(|error| {
                    output_error.get_or_insert_with(|| error.to_string());
                })
                .ok();
            if let Err(error) = recovery::clean_run_directory(&self.directory) {
                let message = format!("temporary background files retained: {error}");
                eprintln!("{message}");
                output_error.get_or_insert(message);
            }
            Ok(BackgroundProcessExit {
                exit_code: status.code(),
                output,
                output_error,
            })
        })
    }
}

async fn stop_tree(child: &mut ManagedChild) -> Result<std::process::ExitStatus, ShellToolError> {
    let unconfirmed = |error: io::Error| ShellToolError::CleanupUnconfirmed {
        message: format!("background process tree cleanup unconfirmed: {error}"),
    };
    if let Err(error) = child.start_kill()
        && !identity::process_gone(&error)
    {
        #[cfg(target_os = "macos")]
        let already_exited = error.kind() == io::ErrorKind::PermissionDenied
            && child
                .id()
                .is_some_and(|pid| identity::group_already_exited(pid).unwrap_or(false));
        #[cfg(not(target_os = "macos"))]
        let already_exited = false;
        if !already_exited {
            return Err(unconfirmed(error));
        }
    }
    child.wait().await.map_err(unconfirmed)
}

fn io_error(error: io::Error) -> ShellToolError {
    ShellToolError::Io {
        message: format!("background shell I/O failed: {error}"),
    }
}

#[cfg(test)]
mod tests;
