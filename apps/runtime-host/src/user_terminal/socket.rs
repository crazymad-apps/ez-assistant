//! 单连接控制、心跳与有界 I/O；所有退出路径都先取消并等待所属 PTY。

use super::{TerminalError, TerminalEvent, failure, process::TerminalProcess, pty_size};
use crate::{
    access::AccessPermit,
    http::{HttpState, terminals},
};
use assistant_protocol::{UserTerminalControl as Control, UserTerminalNotice as Notice};
use axum::{
    extract::ws::{Message, WebSocket},
    http::HeaderMap,
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    sync::{OwnedSemaphorePermit, mpsc},
    time::{Instant, timeout},
};

const WRITE_TIMEOUT: Duration = Duration::from_secs(3);
const HEARTBEAT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct SocketAuthentication {
    pub(crate) state: HttpState,
    pub(crate) headers: HeaderMap,
    pub(crate) peer: SocketAddr,
    pub(crate) permit: Option<AccessPermit>,
}

pub(crate) async fn serve(
    mut socket: WebSocket,
    auth: SocketAuthentication,
    slot: OwnedSemaphorePermit,
) -> Result<(), TerminalError> {
    let owner = auth.state.terminals.clone();
    let open = tokio::select! { biased;
        () = owner.shutdown.cancelled() => return Ok(()),
        message = timeout(Duration::from_secs(5), socket.recv()) => match message {
            Ok(Some(Ok(Message::Text(text)))) => control(&text),
            _ => Err(failure("终端连接认证超时或已断开。")),
        },
    };
    let (permit, source, size) = match open.and_then(|message| match message {
        Control::Open {
            bearer,
            source,
            size,
        } => terminals::authenticate(&auth, bearer.as_ref().map(|token| token.expose()))
            .map(|permit| (permit, source, size))
            .map_err(|_| failure("登录已失效，请重新登录。")),
        _ => Err(failure("终端连接必须先认证并选择启动目录。")),
    }) {
        Ok(open) => open,
        Err(error) => {
            let _ = notice(
                &mut socket,
                Notice::Error {
                    message: error.message,
                },
            )
            .await;
            return Ok(());
        }
    };
    let (events, mut output) = mpsc::channel(2);
    // 不可取消 PTY spawn 的 JoinHandle；取得结果并登记后才观察关闭，避免中途丢下子进程。
    let created = async {
        let _gate = tokio::select! { biased;
            () = owner.shutdown.cancelled() => return Err(failure("Host 正在关闭。")),
            () = permit.ended() => return Err(failure("登录已失效。")),
            gate = owner.source_gate.lock() => gate,
        };
        let (origin, directory) = tokio::select! { biased;
            () = owner.shutdown.cancelled() => return Err(failure("Host 正在关闭。")),
            () = permit.ended() => return Err(failure("登录已失效。")),
            directory = timeout(Duration::from_secs(10), terminals::directory(&auth.state, &source)) =>
                directory.map_err(|_| failure("终端启动目录解析超时。"))??,
        };
        let directory_name = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".into());
        let (id, process, cancelled) = owner
            .create(origin, directory, size, &permit, events)
            .await?;
        Ok::<_, TerminalError>((id, directory_name, process, cancelled))
    }
    .await;
    drop(slot);
    let (id, directory_name, process, cancelled) = match created {
        Ok(created) => created,
        Err(error) => {
            let _ = notice(
                &mut socket,
                Notice::Error {
                    message: error.message,
                },
            )
            .await;
            return Ok(());
        }
    };
    let _cancel_on_unwind = CancelProcess(process.clone());
    let (input, mut pending_input) = mpsc::channel::<Vec<u8>>(8);
    let (written, mut input_acks) = mpsc::channel(1);
    let writer_process = process.clone();
    let writer = tokio::task::spawn_blocking(move || {
        while let Some(bytes) = pending_input.blocking_recv() {
            writer_process.write(&bytes)?;
            if written.blocking_send(()).is_err() {
                break;
            }
        }
        Ok::<_, TerminalError>(())
    });
    let result = async {
        notice(&mut socket, Notice::Created { terminal_id: id.clone(), directory_name }).await?;
        let mut heartbeat = tokio::time::interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
        let mut last_seen = Instant::now();
        loop {
            tokio::select! { biased;
                () = cancelled.cancelled() => return Ok(()),
                () = permit.ended() => return Err(failure("登录或访问权限已失效。")),
                // 截止时间独立于 10 秒 Ping tick，避免末次响应稍晚于 tick 时多保活一轮。
                () = tokio::time::sleep_until(last_seen + IDLE_TIMEOUT) => return Err(failure("终端连接已超时。")),
                _ = heartbeat.tick() => {
                    send(&mut socket, Message::Ping(Vec::new().into())).await?;
                },
                message = socket.recv() => {
                    last_seen = Instant::now();
                    match message {
                        Some(Ok(Message::Binary(bytes))) if !bytes.is_empty() && bytes.len() <= 16 * 1024 => {
                            input.try_send(bytes.to_vec()).map_err(|_| failure("终端输入过快，连接已关闭。"))?;
                        },
                        Some(Ok(Message::Text(text))) => match control(&text)? {
                            Control::Ack => process.acknowledge(),
                            Control::Resize { size } => process.resize(pty_size(size)?)?,
                            Control::Close => return Ok(()),
                            Control::Open { .. } => return Err(failure("该连接已经创建终端。")),
                        },
                        Some(Ok(Message::Ping(bytes))) => send(&mut socket, Message::Pong(bytes)).await?,
                        Some(Ok(Message::Pong(_))) => {},
                        None | Some(Ok(Message::Close(_))) => return Ok(()),
                        _ => return Err(failure("终端连接已断开或输入无效。")),
                    }
                },
                ack = input_acks.recv() => match ack {
                    Some(()) => notice(&mut socket, Notice::InputAck).await?,
                    None => return Err(failure("终端输入已关闭。")),
                },
                event = output.recv() => match event {
                    Some(TerminalEvent::Output { bytes }) => send(&mut socket, Message::Binary(bytes.into())).await?,
                    Some(TerminalEvent::Exited { code }) => { notice(&mut socket, Notice::Exited { code }).await?; return Ok(()); },
                    Some(TerminalEvent::Error { message }) => return Err(failure(message)),
                    None => return Ok(()),
                },
            }
        }
    }.await;
    // 先取消 writer 的非阻塞重试，再关闭队列并等待；慢输入不能阻止回收。
    process.cancel();
    drop(input);
    drop(input_acks);
    drop(output);
    let cleanup = owner.close(&id, &process).await;
    let write_result = writer.await.map_err(|_| failure("终端输入任务异常。"))?;
    if let Err(error) = &result {
        let _ = notice(
            &mut socket,
            Notice::Error {
                message: error.message.clone(),
            },
        )
        .await;
    }
    if let Err(error) = &cleanup {
        let _ = notice(
            &mut socket,
            Notice::Error {
                message: error.message.clone(),
            },
        )
        .await;
    }
    if cleanup.is_ok() {
        let _ = notice(&mut socket, Notice::Closed).await;
    }
    let _ = send(&mut socket, Message::Close(None)).await;
    // 关闭过程中 writer 因取消返回错误是预期行为；panic 仍由上面的 JoinError 上报。
    let _ = write_result;
    cleanup
}

struct CancelProcess(Arc<TerminalProcess>);
impl Drop for CancelProcess {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn control(text: &str) -> Result<Control, TerminalError> {
    if text.len() > 8 * 1024 {
        return Err(failure("终端控制消息过大。"));
    }
    serde_json::from_str(text).map_err(|_| failure("终端控制消息无效。"))
}

async fn notice(socket: &mut WebSocket, notice: Notice) -> Result<(), TerminalError> {
    send(
        socket,
        Message::Text(
            serde_json::to_string(&notice)
                .map_err(|_| failure("终端消息编码失败。"))?
                .into(),
        ),
    )
    .await
}

async fn send(socket: &mut WebSocket, message: Message) -> Result<(), TerminalError> {
    timeout(WRITE_TIMEOUT, socket.send(message))
        .await
        .map_err(|_| failure("终端网络写入超时。"))?
        .map_err(|_| failure("终端连接已关闭。"))
}
