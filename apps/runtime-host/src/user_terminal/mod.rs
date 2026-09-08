//! Host 私有用户终端 owner：连接任务由 Supervisor 收齐，PTY 不进入 Agent 或 Runtime Store。

mod process;
pub(crate) mod socket;
#[cfg(test)]
mod tests;

use crate::access::AccessPermit;
use assistant_protocol::{SessionId, UserTerminalSize, WorkspaceId};
use futures_util::future::BoxFuture;
use process::TerminalProcess;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    sync::{Mutex, Semaphore, mpsc},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct TerminalError {
    message: String,
}

pub(crate) fn failure(message: impl Into<String>) -> TerminalError {
    TerminalError {
        message: message.into(),
    }
}

pub(super) enum TerminalEvent {
    Output { bytes: Vec<u8> },
    Exited { code: u32 },
    Error { message: String },
}

fn pty_size(size: UserTerminalSize) -> Result<portable_pty::PtySize, TerminalError> {
    if !(2..=1000).contains(&size.cols) || !(1..=500).contains(&size.rows) {
        return Err(failure("终端尺寸无效。"));
    }
    Ok(portable_pty::PtySize {
        cols: size.cols,
        rows: size.rows,
        pixel_width: 0,
        pixel_height: 0,
    })
}

/// 只保存连接的来源关联用于删除时回收，不复制 Session/Workspace 状态。
pub(crate) struct TerminalOrigin {
    pub(crate) session: Option<SessionId>,
    pub(crate) workspace: Option<WorkspaceId>,
}

struct Entry {
    origin: TerminalOrigin,
    login: Option<[u8; 32]>,
    process: Arc<TerminalProcess>,
    cancelled: CancellationToken,
}

type ConnectionTask = BoxFuture<'static, Result<(), TerminalError>>;

pub(crate) struct UserTerminals {
    /// 来源查询/启动与删除命令提交共享此 gate；不跨网络 I/O 持有。
    pub(crate) source_gate: Mutex<()>,
    entries: Mutex<HashMap<String, Entry>>,
    next_id: AtomicU64,
    pub(crate) handshakes: Arc<Semaphore>,
    pub(crate) sockets: Arc<Semaphore>,
    connections: mpsc::Sender<ConnectionTask>,
    shutdown: CancellationToken,
}

pub(crate) struct UserTerminalService {
    pub(crate) handle: Arc<UserTerminals>,
    connections: mpsc::Receiver<ConnectionTask>,
}

impl UserTerminalService {
    pub(crate) fn new() -> Self {
        let (connections, receiver) = mpsc::channel(32);
        Self {
            handle: Arc::new(UserTerminals {
                source_gate: Mutex::new(()),
                entries: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                handshakes: Arc::new(Semaphore::new(32)),
                sockets: Arc::new(Semaphore::new(64)),
                connections,
                shutdown: CancellationToken::new(),
            }),
            connections: receiver,
        }
    }

    pub(crate) async fn run_until(
        mut self,
        shutdown: CancellationToken,
    ) -> Result<(), TerminalError> {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! { biased;
                () = shutdown.cancelled() => break,
                completed = tasks.join_next(), if !tasks.is_empty() => observe(completed),
                task = self.connections.recv() => if let Some(task) = task { tasks.spawn(task); } else { break; },
            }
        }
        self.handle.shutdown.cancel();
        self.connections.close();
        // 已 Upgrade 但尚未启动的连接同样交给 owner 执行取消路径，不能遗留 socket。
        while let Some(task) = self.connections.recv().await {
            tasks.spawn(task);
        }
        while let Some(completed) = tasks.join_next().await {
            observe(Some(completed));
        }
        // 关闭失败的进程一直留在 entries；关闭期再核验一次，不把失败报告成成功。
        let remaining: Vec<_> = self
            .handle
            .entries
            .lock()
            .await
            .values()
            .map(|entry| entry.process.clone())
            .collect();
        for process in &remaining {
            process.cancel();
        }
        let mut error = None;
        for process in remaining {
            if let Err(failure) = process.close().await {
                error = Some(failure);
            }
        }
        error.map_or(Ok(()), Err)
    }
}

fn observe(result: Option<Result<Result<(), TerminalError>, tokio::task::JoinError>>) {
    match result {
        Some(Ok(Err(error))) => eprintln!("runtime-host: terminal connection failed: {error}"),
        Some(Err(_)) => eprintln!("runtime-host: terminal connection task failed"),
        _ => {}
    }
}

impl UserTerminals {
    pub(crate) fn submit(&self, task: ConnectionTask) -> Result<(), TerminalError> {
        if self.shutdown.is_cancelled() {
            return Err(failure("Host 正在关闭。"));
        }
        self.connections
            .try_send(task)
            .map_err(|_| failure("终端连接繁忙，请稍后重试。"))
    }

    async fn create(
        &self,
        origin: TerminalOrigin,
        directory: PathBuf,
        size: UserTerminalSize,
        permit: &AccessPermit,
        events: mpsc::Sender<TerminalEvent>,
    ) -> Result<(String, Arc<TerminalProcess>, CancellationToken), TerminalError> {
        permit.check().map_err(|_| failure("登录已失效。"))?;
        if self.shutdown.is_cancelled() {
            return Err(failure("Host 正在关闭。"));
        }
        let mut entries = self.entries.lock().await;
        if entries.len() >= 32 {
            return Err(failure("Host 最多同时打开 32 个终端。"));
        }
        if entries
            .values()
            .filter(|entry| entry.login == permit.login_id())
            .count()
            >= 8
        {
            return Err(failure("当前登录最多同时打开 8 个终端。"));
        }
        let process = TerminalProcess::spawn(directory, pty_size(size)?, move |event| {
            events
                .try_send(event)
                .map_err(|_| failure("终端输出连接已关闭。"))
        })
        .await?;
        let id = format!(
            "user-terminal-{}-{}",
            std::process::id(),
            self.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let cancelled = self.shutdown.child_token();
        entries.insert(
            id.clone(),
            Entry {
                origin,
                login: permit.login_id(),
                process: process.clone(),
                cancelled: cancelled.clone(),
            },
        );
        Ok((id, process, cancelled))
    }

    async fn close(&self, id: &str, process: &TerminalProcess) -> Result<(), TerminalError> {
        process.close().await?;
        self.entries.lock().await.remove(id);
        Ok(())
    }

    pub(crate) async fn source_removed(
        &self,
        session: Option<&SessionId>,
        workspace: Option<&WorkspaceId>,
    ) {
        for entry in self.entries.lock().await.values() {
            if session.is_some_and(|id| entry.origin.session.as_ref() == Some(id))
                || workspace.is_some_and(|id| entry.origin.workspace.as_ref() == Some(id))
            {
                entry.cancelled.cancel();
                entry.process.cancel();
            }
        }
    }
}
