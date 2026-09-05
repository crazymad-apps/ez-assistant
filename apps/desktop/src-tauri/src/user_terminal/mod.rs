//! 用户终端的 Desktop owner。启动路径经现有 Host 资源边界解析，PTY 不进入 Agent 或 Runtime。

mod process;
#[cfg(test)]
mod tests;

use crate::{
    native_resource::{NativeResourceBridge, resolve_session_resource_path},
    runtime_bootstrap::RuntimeBootstrapCoordinator,
};
use assistant_protocol::{SessionResourceLocator, WorkspaceId};
use process::TerminalProcess;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tauri::{Manager, Webview, ipc::Channel};
use tokio::sync::Mutex;

#[derive(Default)]
pub(crate) struct UserTerminalManager {
    // 生命周期 gate 串行创建/重启/重载；输出 ack 只访问短持有的注册表，不等待路径解析。
    lifecycle: Mutex<bool>,
    terminals: Mutex<HashMap<String, Arc<TerminalProcess>>>,
    next_id: AtomicU64,
}

#[derive(Clone, Debug, Serialize, thiserror::Error)]
#[error("{message}")]
pub(crate) struct TerminalError {
    code: &'static str,
    message: String,
}

fn failure(message: impl Into<String>) -> TerminalError {
    TerminalError {
        code: "native_operation_failed",
        message: message.into(),
    }
}

fn require_main(caller: &Webview) -> Result<(), TerminalError> {
    if caller.label() == "main" {
        Ok(())
    } else {
        Err(TerminalError {
            code: "terminal_forbidden",
            message: "不允许此页面操作终端。".into(),
        })
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum TerminalEvent {
    Output { bytes: Vec<u8> },
    Exited { code: u32 },
    Error { message: String },
}

#[derive(Clone, Copy, Deserialize)]
pub(crate) struct TerminalSize {
    cols: u16,
    rows: u16,
}

impl TerminalSize {
    fn validate(self) -> Result<portable_pty::PtySize, TerminalError> {
        if !(2..=1000).contains(&self.cols) || !(1..=500).contains(&self.rows) {
            return Err(TerminalError {
                code: "invalid_request",
                message: "终端尺寸无效。".into(),
            });
        }
        Ok(portable_pty::PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
    }
}

#[derive(Serialize)]
pub(crate) struct CreatedTerminal {
    terminal_id: String,
    directory_name: String,
}

impl UserTerminalManager {
    async fn get(&self, id: &str) -> Result<Arc<TerminalProcess>, TerminalError> {
        self.terminals
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| TerminalError {
                code: "terminal_not_found",
                message: "终端已关闭。".into(),
            })
    }

    /// 等待全部 PTY 释放后再允许新建；主 WebView 重载时旧 xterm 无法继续 ack，必须取消输出等待。
    pub(crate) async fn close_all(&self) -> Result<(), TerminalError> {
        let _gate = self.lifecycle.lock().await;
        self.close_processes().await
    }

    /// 真正退出先冻结创建/重启，再取消并等待全部 PTY；失败保留句柄以供重试。
    pub(crate) async fn shutdown(&self) -> Result<(), TerminalError> {
        let mut shutting_down = self.lifecycle.lock().await;
        *shutting_down = true;
        let result = self.close_processes().await;
        if result.is_err() {
            *shutting_down = false;
        }
        result
    }

    pub(crate) async fn resume(&self) {
        *self.lifecycle.lock().await = false;
    }

    async fn close_processes(&self) -> Result<(), TerminalError> {
        let processes: Vec<_> = self.terminals.lock().await.values().cloned().collect();
        for process in &processes {
            process.cancel();
        }
        let mut failure = None;
        for process in processes {
            if let Err(error) = process.close().await {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        self.terminals.lock().await.clear();
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum TerminalSource {
    Session {
        session_id: String,
        locator: SessionResourceLocator,
    },
    Workspace {
        workspace_id: String,
    },
}

#[tauri::command]
pub(crate) async fn create_user_terminal(
    caller: Webview,
    source: TerminalSource,
    size: TerminalSize,
    events: Channel<TerminalEvent>,
) -> Result<CreatedTerminal, TerminalError> {
    require_main(&caller)?;
    let size = size.validate()?;
    let manager = caller.state::<UserTerminalManager>();
    let shutting_down = manager.lifecycle.lock().await;
    ensure_accepting(*shutting_down)?;
    let path = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let path = match source {
            TerminalSource::Session {
                session_id,
                locator,
            } => resolve_session_resource_path(
                &caller.state::<NativeResourceBridge>(),
                &caller.state::<RuntimeBootstrapCoordinator>(),
                session_id,
                locator,
            )
            .await
            .map_err(|_| failure("无法读取会话的终端启动目录。"))?,
            TerminalSource::Workspace { workspace_id } => {
                let id = WorkspaceId::new(workspace_id).map_err(|_| failure("工作空间无效。"))?;
                caller
                    .state::<RuntimeBootstrapCoordinator>()
                    .workspace_directory(id)
                    .await
                    .map_err(|_| failure("无法读取工作空间的终端启动目录。"))?
            }
        };
        Ok::<_, TerminalError>(path)
    })
    .await
    .map_err(|_| failure("终端启动目录解析超时，请重试。"))??;
    let directory = PathBuf::from(path);
    if !tokio::fs::metadata(&directory)
        .await
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(failure("终端启动目录不存在。"));
    }
    let directory_name = directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".into());
    let process = TerminalProcess::spawn(directory, size, move |event| {
        events
            .send(event)
            .map_err(|_| failure("终端输出连接已关闭。"))
    })
    .await?;
    let terminal_id = format!(
        "user-terminal-{}-{}",
        std::process::id(),
        manager.next_id.fetch_add(1, Ordering::Relaxed)
    );
    manager
        .terminals
        .lock()
        .await
        .insert(terminal_id.clone(), process);
    Ok(CreatedTerminal {
        terminal_id,
        directory_name,
    })
}

#[tauri::command]
pub(crate) async fn write_user_terminal(
    caller: Webview,
    terminal_id: String,
    bytes: Vec<u8>,
) -> Result<(), TerminalError> {
    require_main(&caller)?;
    if bytes.is_empty() || bytes.len() > 16 * 1024 {
        return Err(failure("终端输入块大小无效。"));
    }
    let process = caller
        .state::<UserTerminalManager>()
        .get(&terminal_id)
        .await?;
    tokio::task::spawn_blocking(move || process.write(&bytes))
        .await
        .map_err(|_| failure("终端输入任务异常。"))?
}

#[tauri::command]
pub(crate) async fn resize_user_terminal(
    caller: Webview,
    terminal_id: String,
    size: TerminalSize,
) -> Result<(), TerminalError> {
    require_main(&caller)?;
    let size = size.validate()?;
    let process = caller
        .state::<UserTerminalManager>()
        .get(&terminal_id)
        .await?;
    tokio::task::spawn_blocking(move || process.resize(size))
        .await
        .map_err(|_| failure("终端尺寸任务异常。"))?
}

#[tauri::command]
pub(crate) async fn acknowledge_user_terminal(
    caller: Webview,
    terminal_id: String,
) -> Result<(), TerminalError> {
    require_main(&caller)?;
    caller
        .state::<UserTerminalManager>()
        .get(&terminal_id)
        .await?
        .acknowledge();
    Ok(())
}

#[tauri::command]
pub(crate) async fn close_user_terminal(
    caller: Webview,
    terminal_id: String,
) -> Result<(), TerminalError> {
    require_main(&caller)?;
    let manager = caller.state::<UserTerminalManager>();
    let _gate = manager.lifecycle.lock().await;
    let process = manager.terminals.lock().await.get(&terminal_id).cloned();
    if let Some(process) = process {
        process.close().await?;
        manager.terminals.lock().await.remove(&terminal_id);
    }
    Ok(())
}

/// 重启复用 Desktop 创建时冻结的目录，不重新读取当前会话或已编辑的工作空间。
#[tauri::command]
pub(crate) async fn restart_user_terminal(
    caller: Webview,
    terminal_id: String,
    size: TerminalSize,
    events: Channel<TerminalEvent>,
) -> Result<(), TerminalError> {
    require_main(&caller)?;
    let size = size.validate()?;
    let manager = caller.state::<UserTerminalManager>();
    let shutting_down = manager.lifecycle.lock().await;
    ensure_accepting(*shutting_down)?;
    let previous = manager.get(&terminal_id).await?;
    previous.close().await?;
    let process = TerminalProcess::spawn(previous.directory.clone(), size, move |event| {
        events
            .send(event)
            .map_err(|_| failure("终端输出连接已关闭。"))
    })
    .await?;
    manager.terminals.lock().await.insert(terminal_id, process);
    Ok(())
}

fn ensure_accepting(shutting_down: bool) -> Result<(), TerminalError> {
    if shutting_down {
        Err(failure("正在退出桌面客户端，无法创建或重启终端。"))
    } else {
        Ok(())
    }
}

#[tauri::command]
pub(crate) async fn shutdown_user_terminals(caller: Webview) -> Result<(), TerminalError> {
    require_main(&caller)?;
    caller.state::<UserTerminalManager>().shutdown().await
}

#[tauri::command]
pub(crate) async fn resume_user_terminals(caller: Webview) -> Result<(), TerminalError> {
    require_main(&caller)?;
    caller.state::<UserTerminalManager>().resume().await;
    Ok(())
}
