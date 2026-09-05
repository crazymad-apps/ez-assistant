//! 单个 PTY 的 I/O 与回收。读/等待在阻塞池执行；监督任务持有并等待全部子任务。

use super::{TerminalError, TerminalEvent, failure};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

const OUTPUT_BYTES: usize = 32 * 1024;
const IO_WAIT: Duration = Duration::from_millis(10);

pub(super) struct TerminalProcess {
    pub(super) directory: PathBuf,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    cancel: CancellationToken,
    outstanding: AtomicBool,
    ack: Semaphore,
    child_exited: AtomicBool,
    task: tokio::sync::Mutex<ProcessCompletion>,
}

#[derive(Default)]
struct ProcessCompletion {
    task: Option<tokio::task::JoinHandle<Result<(), TerminalError>>>,
    result: Option<Result<(), TerminalError>>,
}

impl TerminalProcess {
    pub(super) async fn spawn(
        directory: PathBuf,
        size: PtySize,
        send: impl Fn(TerminalEvent) -> Result<(), TerminalError> + Send + Sync + 'static,
    ) -> Result<Arc<Self>, TerminalError> {
        Self::spawn_command(directory, size, CommandBuilder::new_default_prog(), send).await
    }

    pub(super) async fn spawn_command(
        directory: PathBuf,
        size: PtySize,
        mut command: CommandBuilder,
        send: impl Fn(TerminalEvent) -> Result<(), TerminalError> + Send + Sync + 'static,
    ) -> Result<Arc<Self>, TerminalError> {
        filter_environment(&mut command);
        command.cwd(&directory);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "ez-assistant");
        let (pair, reader, writer, child) = tokio::task::spawn_blocking(move || {
            let pair = portable_pty::native_pty_system()
                .openpty(size)
                .map_err(|_| failure("无法创建终端。"))?;
            nonblocking(pair.master.as_ref())?;
            // 所有可失败的 I/O 克隆在启动子进程之前完成，失败时不会遗留 Shell。
            let reader = pair
                .master
                .try_clone_reader()
                .map_err(|_| failure("无法读取终端。"))?;
            let writer = pair
                .master
                .take_writer()
                .map_err(|_| failure("无法写入终端。"))?;
            let child = pair
                .slave
                .spawn_command(command)
                .map_err(|_| failure("无法启动系统登录 Shell。"))?;
            Ok::<_, TerminalError>((pair, reader, writer, child))
        })
        .await
        .map_err(|_| failure("终端创建任务异常。"))??;
        drop(pair.slave);
        let process = Arc::new(Self {
            directory,
            master: Mutex::new(Some(pair.master)),
            writer: Mutex::new(Some(writer)),
            cancel: CancellationToken::new(),
            outstanding: AtomicBool::new(false),
            ack: Semaphore::new(0),
            child_exited: AtomicBool::new(false),
            task: tokio::sync::Mutex::new(ProcessCompletion::default()),
        });
        let owner = process.clone();
        process.task.lock().await.task = Some(tokio::spawn(async move {
            owner.supervise(reader, child, send).await
        }));
        Ok(process)
    }

    pub(super) fn cancel(&self) {
        self.cancel.cancel();
    }

    pub(super) fn acknowledge(&self) {
        // 只允许确认当前一个输出块，重复 ack 不得积累成未来输出的额度。
        if self.outstanding.swap(false, Ordering::AcqRel) {
            self.ack.add_permits(1);
        }
    }

    pub(super) fn write(&self, mut bytes: &[u8]) -> Result<(), TerminalError> {
        // 唯一 writer 串行保序；非阻塞 fd 使 close 能取消遇到终端输入背压的写操作。
        let mut guard = self
            .writer
            .lock()
            .map_err(|_| failure("终端输入状态异常。"))?;
        let writer = guard.as_mut().ok_or_else(|| failure("终端进程已退出。"))?;
        while !bytes.is_empty() {
            if self.cancel.is_cancelled() || self.child_exited.load(Ordering::Acquire) {
                return Err(failure("终端已关闭。"));
            }
            match writer.write(bytes) {
                Ok(0) => return Err(failure("终端输入已关闭。")),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(IO_WAIT)
                }
                Err(_) => return Err(failure("无法写入终端。")),
            }
        }
        Ok(())
    }

    pub(super) fn resize(&self, size: PtySize) -> Result<(), TerminalError> {
        let guard = self
            .master
            .lock()
            .map_err(|_| failure("终端尺寸状态异常。"))?;
        if let Some(master) = guard.as_ref() {
            master
                .resize(size)
                .map_err(|_| failure("无法调整终端尺寸。"))?;
        }
        Ok(())
    }

    pub(super) async fn close(&self) -> Result<(), TerminalError> {
        self.cancel();
        // close gate 保证重复关闭也等待同一次回收完成，不能提前报告成功。
        let mut completion = self.task.lock().await;
        if let Some(task) = completion.task.as_mut() {
            let result = tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .map_err(|_| failure("终端清理尚未完成，请稍后重试。"))?
                .unwrap_or_else(|_| Err(failure("终端清理任务异常。")));
            completion.task.take();
            completion.result = Some(result);
        }
        completion.result.clone().unwrap_or(Ok(()))
    }

    async fn supervise(
        self: Arc<Self>,
        mut reader: Box<dyn Read + Send>,
        mut child: Box<dyn Child + Send + Sync>,
        send: impl Fn(TerminalEvent) -> Result<(), TerminalError> + Send + Sync,
    ) -> Result<(), TerminalError> {
        let mut killer = child.clone_killer();
        let shell_pid = child.process_id();
        let (output, mut chunks) = mpsc::channel(1);
        let reader_owner = self.clone();
        let read_task = tokio::task::spawn_blocking(move || {
            let mut buffer = [0; OUTPUT_BYTES];
            while !reader_owner.cancel.is_cancelled() {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        if output.blocking_send(buffer[..count].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if reader_owner.child_exited.load(Ordering::Acquire) {
                            break;
                        }
                        std::thread::sleep(IO_WAIT);
                    }
                    Err(error) if error.raw_os_error() == Some(5) => break,
                    Err(_) => return Err(failure("终端输出读取失败。")),
                }
            }
            Ok(())
        });
        let wait_owner = self.clone();
        let wait_task = tokio::task::spawn_blocking(move || {
            let result = child.wait().map_err(|_| failure("无法取得终端退出状态。"));
            wait_owner.child_exited.store(true, Ordering::Release);
            result
        });
        loop {
            let chunk = tokio::select! { biased;
                _ = self.cancel.cancelled() => break,
                chunk = chunks.recv() => match chunk { Some(chunk) => chunk, None => break },
            };
            self.outstanding.store(true, Ordering::Release);
            if send(TerminalEvent::Output { bytes: chunk }).is_err() {
                self.cancel();
                break;
            }
            tokio::select! { biased;
                _ = self.cancel.cancelled() => break,
                permit = self.ack.acquire() => { if let Ok(permit) = permit { permit.forget(); } },
            }
        }
        drop(chunks); // 先释放有界队列，唤醒可能在 blocking_send 中等待的 reader。
        let closing = self.cancel.is_cancelled();
        // EOF 与 wait() 可能竞态：先给正常退出一次短暂收尾机会，再清理关闭 stdio 后仍存活的 Shell。
        let mut wait_task = wait_task;
        let natural_exit = if closing {
            None
        } else {
            tokio::time::timeout(Duration::from_millis(100), &mut wait_task)
                .await
                .ok()
        };
        let cleanup = if closing || natural_exit.is_none() {
            self.cancel();
            let owner = self.clone();
            tokio::task::spawn_blocking(move || owner.terminate(&mut *killer, shell_pid))
                .await
                .unwrap_or_else(|_| Err(failure("终端进程清理异常。")))
        } else {
            Ok(Vec::new())
        };
        // 即使某一步报错也收齐已启动任务；只在 reader 和子进程真正退出后释放 owner。
        let read_result = read_task
            .await
            .unwrap_or_else(|_| Err(failure("终端读取任务异常。")));
        let exit = match natural_exit {
            Some(exit) => exit,
            None => wait_task.await,
        }
        .unwrap_or_else(|_| Err(failure("终端等待任务异常。")));
        self.writer
            .lock()
            .map_err(|_| failure("终端输入状态异常。"))?
            .take();
        self.master
            .lock()
            .map_err(|_| failure("终端状态异常。"))?
            .take();
        if !closing {
            let event = match read_result.and(exit) {
                Ok(status) => TerminalEvent::Exited {
                    code: status.exit_code(),
                },
                Err(error) => TerminalEvent::Error {
                    message: error.message,
                },
            };
            // 页面已销毁时事件不可达；子进程已回收，不能把投递失败误报为清理失败。
            let _ = send(event);
        }
        let groups = cleanup?;
        tokio::task::spawn_blocking(move || verify_groups_gone(groups))
            .await
            .unwrap_or_else(|_| Err(failure("终端进程核验异常。")))
    }

    fn terminate(
        &self,
        _killer: &mut dyn ChildKiller,
        shell_pid: Option<u32>,
    ) -> Result<Vec<i32>, TerminalError> {
        #[cfg(unix)]
        {
            use nix::{
                sys::signal::{Signal, kill},
                unistd::Pid,
            };
            let foreground = self
                .master
                .lock()
                .map_err(|_| failure("终端状态异常。"))?
                .as_ref()
                .and_then(|master| master.process_group_leader());
            // 关闭 PTY 前冻结前台组，避免进程结束时组切回 Shell。
            let mut groups: Vec<_> = [
                foreground,
                shell_pid.and_then(|pid| i32::try_from(pid).ok()),
            ]
            .into_iter()
            .flatten()
            .filter(|pid| *pid > 1)
            .collect();
            groups.sort_unstable();
            groups.dedup();
            for signal in [Signal::SIGHUP, Signal::SIGTERM, Signal::SIGKILL] {
                groups.retain(|pid| {
                    kill(Pid::from_raw(-*pid), signal) != Err(nix::errno::Errno::ESRCH)
                });
                if groups.is_empty() {
                    break;
                }
                if signal != Signal::SIGKILL {
                    std::thread::sleep(Duration::from_millis(150));
                }
            }
            if !self.child_exited.load(Ordering::Acquire)
                && let Some(pid) = shell_pid
                    .and_then(|pid| i32::try_from(pid).ok())
                    .filter(|pid| *pid > 1)
            {
                // portable-pty 的 clone_killer 在 Unix 只发送 HUP，最终兜底需显式 KILL。
                let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
            }
            // writer Drop 会发送换行/EOF，因此先发完终止信号，再关闭句柄，避免执行未提交输入。
            // 同时必须先关闭 master 再 wait：Darwin 的 PTY 排空可能阻塞进程退出。
            self.writer
                .lock()
                .map_err(|_| failure("终端输入状态异常。"))?
                .take();
            self.master
                .lock()
                .map_err(|_| failure("终端状态异常。"))?
                .take();
            Ok(groups)
        }
        #[cfg(not(unix))]
        {
            _killer
                .kill()
                .map_err(|_| failure("无法终止终端 Shell。"))?;
            Ok(Vec::new())
        }
    }
}

fn verify_groups_gone(mut groups: Vec<i32>) -> Result<(), TerminalError> {
    #[cfg(unix)]
    {
        use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
        // Darwin 在退出中的组只含 zombie 时也返回 EPERM。因此在 wait 收尸后核验组消失，
        // 不把信号发送成功或临时 EPERM 当作回收结果；持续存在的组仍是关闭失败。
        for _ in 0..10 {
            groups.retain(|pid| kill(Pid::from_raw(-*pid), None) != Err(Errno::ESRCH));
            if groups.is_empty() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if groups.is_empty() {
        Ok(())
    } else {
        Err(failure("终端前台进程组尚未退出。"))
    }
}

#[cfg(unix)]
fn nonblocking(master: &dyn MasterPty) -> Result<(), TerminalError> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let fd = master
        .as_raw_fd()
        .ok_or_else(|| failure("终端不提供可取消的 I/O。"))?;
    let flags = fcntl(fd, FcntlArg::F_GETFL).map_err(|_| failure("无法读取终端 I/O 设置。"))?;
    fcntl(
        fd,
        FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
    )
    .map_err(|_| failure("无法设置终端 I/O。"))?;
    Ok(())
}

#[cfg(not(unix))]
fn nonblocking(_master: &dyn MasterPty) -> Result<(), TerminalError> {
    Err(failure("当前平台尚不支持可取消的用户终端。"))
}

fn filter_environment(command: &mut CommandBuilder) {
    let blocked: Vec<_> = command
        .iter_full_env_as_str()
        .filter_map(|(key, _)| {
            let name = key.to_ascii_uppercase();
            (name.starts_with("EZ_ASSISTANT_")
                || name.starts_with("MCP_")
                || name.starts_with("TAURI_")
                || name.starts_with("APPLE_")
                || name.starts_with("AWS_")
                || name.starts_with("CODESIGN_")
                || name.contains("TOKEN")
                || name.contains("SECRET")
                || name.contains("PASSWORD")
                || name.contains("PRIVATE_KEY")
                || name.ends_with("API_KEY")
                || name == "GH_TOKEN")
                .then(|| key.to_owned())
        })
        .collect();
    for key in blocked {
        command.env_remove(key);
    }
}
