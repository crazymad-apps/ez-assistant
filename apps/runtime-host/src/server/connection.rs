//! 单个客户端连接的握手、读写任务与事件转发。

use std::{sync::Arc, time::Duration};

use assistant_protocol::{PROTOCOL_VERSION, RuntimeErrorCode, RuntimeErrorInfo};
use assistant_runtime::AssistantRuntime;
use thiserror::Error;
use tokio::{
    net::UnixStream,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use super::dispatch::dispatch;
use crate::wire::{ClientFrame, ServerFrame, read_frame, write_frame};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_QUEUE_CAPACITY: usize = 16;
const EVENT_QUEUE_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConnectionEnd {
    Disconnected,
    ShutdownRequested,
}

#[derive(Debug, Error)]
pub(super) enum ConnectionError {
    #[error("connection {task} task failed")]
    TaskFailed { task: &'static str },
}

/// 响应帧必须等待 writer 确认落到 socket，关闭命令才能安全结束 Host。
struct ReliableFrame {
    frame: ServerFrame,
    flushed: oneshot::Sender<Result<(), String>>,
}

pub(super) async fn serve_connection(
    runtime: Arc<AssistantRuntime>,
    mut stream: UnixStream,
    host_shutdown: CancellationToken,
) -> Result<ConnectionEnd, ConnectionError> {
    if !handshake(&mut stream).await {
        return Ok(ConnectionEnd::Disconnected);
    }

    let (mut reader, writer) = stream.into_split();
    let connection = host_shutdown.child_token();
    let (response_tx, response_rx) = mpsc::channel(RESPONSE_QUEUE_CAPACITY);
    let (event_tx, event_rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
    let writer_task = spawn_writer(writer, response_rx, event_rx, connection.clone());
    let event_task = spawn_event_forwarder(runtime.clone(), event_tx.clone(), connection.clone());
    let mut end = ConnectionEnd::Disconnected;

    loop {
        let incoming = tokio::select! {
            () = connection.cancelled() => break,
            incoming = read_frame::<_, ClientFrame>(&mut reader) => incoming,
        };
        let frame = match incoming {
            Ok(Some(frame)) => frame,
            Ok(None) | Err(_) => break,
        };
        let ClientFrame::Request {
            request_id,
            command,
        } = frame
        else {
            break;
        };
        if request_id.trim().is_empty() {
            break;
        }

        let (frame, shutdown_requested) = dispatch(&runtime, request_id, command).await;
        if shutdown_requested {
            end = ConnectionEnd::ShutdownRequested;
        }
        if !send_reliable(&response_tx, frame, &connection).await {
            break;
        }
        if shutdown_requested {
            break;
        }
    }

    connection.cancel();
    drop(response_tx);
    drop(event_tx);
    observe_connection_tasks(event_task, writer_task).await?;
    Ok(end)
}

/// 正常取消由任务本身收敛为 `Ok(())`；JoinError 始终表示 Host 缺陷。
async fn observe_connection_tasks(
    event_task: JoinHandle<()>,
    writer_task: JoinHandle<()>,
) -> Result<(), ConnectionError> {
    let (event_result, writer_result) = tokio::join!(event_task, writer_task);
    event_result.map_err(|_| ConnectionError::TaskFailed {
        task: "event forwarder",
    })?;
    writer_result.map_err(|_| ConnectionError::TaskFailed { task: "writer" })?;
    Ok(())
}

async fn handshake(stream: &mut UnixStream) -> bool {
    let incoming = timeout(HANDSHAKE_TIMEOUT, read_frame::<_, ClientFrame>(stream)).await;
    let Ok(Ok(Some(ClientFrame::Hello {
        protocol_version,
        client_name,
    }))) = incoming
    else {
        return false;
    };

    if protocol_version != PROTOCOL_VERSION || client_name.trim().is_empty() {
        let _ = write_frame(
            stream,
            &ServerFrame::Error {
                request_id: "handshake".to_owned(),
                error: RuntimeErrorInfo::new(
                    RuntimeErrorCode::InvalidRequest,
                    "runtime protocol version or client name is invalid",
                ),
            },
        )
        .await;
        return false;
    }

    write_frame(
        stream,
        &ServerFrame::HelloAck {
            protocol_version: PROTOCOL_VERSION,
            runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    )
    .await
    .is_ok()
}

fn spawn_writer(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    mut responses: mpsc::Receiver<ReliableFrame>,
    mut events: mpsc::Receiver<ServerFrame>,
    cancellation: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                // 可靠响应优先于允许丢失的实时事件，避免事件洪峰饿死命令结果。
                biased;
                () = cancellation.cancelled() => break,
                Some(response) = responses.recv() => {
                    let result = tokio::select! {
                        () = cancellation.cancelled() => Err("connection closed".to_owned()),
                        result = write_frame(&mut writer, &response.frame) => {
                            result.map_err(|error| error.to_string())
                        }
                    };
                    let failed = result.is_err();
                    let _ = response.flushed.send(result);
                    if failed {
                        cancellation.cancel();
                        break;
                    }
                }
                Some(event) = events.recv() => {
                    let result = tokio::select! {
                        () = cancellation.cancelled() => break,
                        result = write_frame(&mut writer, &event) => result,
                    };
                    if result.is_err() {
                        cancellation.cancel();
                        break;
                    }
                }
                else => break,
            }
        }
    })
}

fn spawn_event_forwarder(
    runtime: Arc<AssistantRuntime>,
    events: mpsc::Sender<ServerFrame>,
    cancellation: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut receiver = runtime.subscribe_events();
        loop {
            let event = tokio::select! {
                () = cancellation.cancelled() => break,
                event = receiver.recv() => event,
            };
            match event {
                // Runtime 快照是恢复来源；慢客户端的瞬时事件可以丢弃，不能反压 Runtime。
                Ok(event) => match events.try_send(ServerFrame::Event { event }) {
                    Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

async fn send_reliable(
    sender: &mpsc::Sender<ReliableFrame>,
    frame: ServerFrame,
    cancellation: &CancellationToken,
) -> bool {
    let (flushed, receiver) = oneshot::channel();
    let queued = ReliableFrame { frame, flushed };
    let sent = tokio::select! {
        () = cancellation.cancelled() => return false,
        sent = sender.send(queued) => sent,
    };
    if sent.is_err() {
        return false;
    }
    matches!(
        tokio::select! {
            () = cancellation.cancelled() => return false,
            result = receiver => result,
        },
        Ok(Ok(()))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connection_task_join_distinguishes_success_and_panic() {
        observe_connection_tasks(tokio::spawn(async {}), tokio::spawn(async {}))
            .await
            .expect("normal tasks");

        let error = observe_connection_tasks(
            tokio::spawn(async { panic!("fixture panic") }),
            tokio::spawn(async {}),
        )
        .await
        .expect_err("panic must be observed");
        assert!(matches!(
            error,
            ConnectionError::TaskFailed {
                task: "event forwarder"
            }
        ));
    }
}
