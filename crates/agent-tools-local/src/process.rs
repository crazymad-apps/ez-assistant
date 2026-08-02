//! 本地子进程的跨平台进程树包装。
//!
//! ripgrep 与 Shell Adapter 共用这一启动边界：Unix 建立独立进程组，Windows 建立
//! Job Object。受管启动保留 kill-on-drop；可脱管启动由调用方在成功交接前显式清理。

#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use std::process::ExitStatus;

use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::process::Command;

pub(crate) type ManagedChild = Box<dyn ChildWrapper>;

/// 启动受进程组或 Job Object 管理的子进程树。
pub(crate) fn spawn(command: Command) -> std::io::Result<ManagedChild> {
    spawn_wrapped(command, true)
}

/// 启动可在显式交接后脱离 Adapter 生命周期的进程树。
///
/// 成功交接前调用方仍必须在 timeout/cancellation 时显式 terminate + wait；这里不设置
/// kill-on-drop，是为了让交接时释放 ProcessGroup/Job Object 不会终止后台后代。
pub(crate) fn spawn_detachable(command: Command) -> std::io::Result<ManagedChild> {
    spawn_wrapped(command, false)
}

fn spawn_wrapped(command: Command, kill_on_drop: bool) -> std::io::Result<ManagedChild> {
    let mut command = CommandWrap::from(command);
    if kill_on_drop {
        command.wrap(KillOnDrop);
    }
    #[cfg(unix)]
    command.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(JobObject);
    command.spawn()
}

/// 终止受管进程树并等待所有可管理后代收敛。
///
/// `start_kill` 与进程自然退出可能竞态：发送终止失败后若 `wait`
/// 仍能确认整个进程树已结束，则清理仍然成功；两者都失败时保留
/// 两段错误上下文。
pub(crate) async fn terminate_and_wait(child: &mut ManagedChild) -> std::io::Result<ExitStatus> {
    let kill_error = child.start_kill().err();
    match child.wait().await {
        Ok(status) => Ok(status),
        Err(wait_error) => {
            if let Some(kill_error) = kill_error {
                Err(std::io::Error::other(format!(
                    "terminate process tree failed ({kill_error}) and wait failed ({wait_error})"
                )))
            } else {
                Err(wait_error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use super::*;

    #[cfg(unix)]
    fn command(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        command
    }

    #[cfg(windows)]
    fn command(script: &str) -> Command {
        let mut command =
            Command::new(std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()));
        command.arg("/C").arg(script);
        command
    }

    #[tokio::test]
    async fn managed_process_wait_returns_exit_status() {
        #[cfg(unix)]
        let script = "exit 7";
        #[cfg(windows)]
        let script = "exit /B 7";

        let mut command = command(script);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn(command).expect("spawn controlled command");
        let status = child.wait().await.expect("wait for controlled command");

        assert_eq!(status.code(), Some(7));
    }

    #[tokio::test]
    async fn terminate_waits_for_managed_process() {
        #[cfg(unix)]
        let script = "sleep 30";
        #[cfg(windows)]
        let script = "ping -n 31 127.0.0.1 >NUL";

        let mut command = command(script);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn(command).expect("spawn controlled command");
        let status = terminate_and_wait(&mut child)
            .await
            .expect("terminate and wait for command");

        assert!(!status.success());
        assert!(child.try_wait().expect("query reaped child").is_some());
    }
}
