//! 源端只代理消息；目标任务交给既有终端 Supervisor，控制连接退出不能丢弃正在创建的 PTY。
use super::{
    TerminalError, failure,
    socket::{self, Open},
    transport::{self, Transport},
};
use crate::{
    access::{
        AccessPermit,
        registration::network::streams::{Flow, Frame, Kind},
    },
    http::HttpState,
};
use assistant_protocol::{ClientCompatibility, UserTerminalNotice as Notice, check_compatibility};
use axum::extract::ws::Message;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::{sync::OwnedSemaphorePermit, time::timeout};

pub(super) async fn connect(
    mut socket: Transport,
    state: HttpState,
    permit: AccessPermit,
    device: String,
    open: Open,
    slot: OwnedSemaphorePermit,
) -> Result<(), TerminalError> {
    let owner = state.terminals.clone();
    let opening = async {
        if permit.native || permit.user_key().is_none() {
            return Err(failure("当前登录不可访问组网设备。"));
        }
        let flow = state
            .access
            .network
            .open_flow(device, permit.clone(), Kind::Terminal)
            .await
            .map_err(|_| failure("目标设备不可用或没有访问权限。"))?;
        flow.ready()
            .await
            .map_err(|_| failure("目标终端通道不可用。"))?;
        flow.send(Frame {
            kind: 5,
            payload: serde_json::to_vec(&open).map_err(|_| failure("终端参数编码失败。"))?,
        })
        .await
        .map_err(|_| failure("目标终端通道已断开，请重新打开终端。"))?;
        Ok::<_, TerminalError>(flow)
    };
    let result = tokio::select! { biased;
        () = owner.shutdown.cancelled() => return Ok(()),
        () = permit.ended() => Err(failure("登录已失效。")),
        result = opening => result,
    };
    drop(slot);
    let flow = match result {
        Ok(flow) => flow,
        Err(error) => {
            let _ = socket::notice(
                &mut socket,
                Notice::Error {
                    message: error.to_string(),
                },
            )
            .await;
            return Ok(());
        }
    };
    let Transport::Local(socket) = socket else {
        return Err(failure("禁止再次代理终端。"));
    };
    let (mut sender, mut receiver) = socket.split();
    // 每方向只持有一条待写消息。双向独立推进，慢输出不能堵住用户 ACK。
    let result = {
        let upstream = async {
            while let Some(message) = receiver.next().await {
                let message = message.map_err(|_| failure("终端连接已断开。"))?;
                let close = matches!(message, Message::Close(_));
                flow.send(transport::encode(message))
                    .await
                    .map_err(|_| failure("远端终端连接已断开。"))?;
                if close {
                    break;
                }
            }
            Ok::<_, TerminalError>(())
        };
        let downstream = async {
            loop {
                let message = transport::decode(
                    flow.recv()
                        .await
                        .map_err(|_| failure("远端终端连接已断开。"))?,
                )?;
                let close = matches!(message, Message::Close(_));
                timeout(Duration::from_secs(3), sender.send(message))
                    .await
                    .map_err(|_| failure("终端网络写入超时。"))?
                    .map_err(|_| failure("终端连接已断开。"))?;
                if close {
                    flow.send(Frame {
                        kind: 10,
                        payload: Vec::new(),
                    })
                    .await
                    .map_err(|_| failure("终端关闭未确认。"))?;
                    flow.finish()
                        .await
                        .map_err(|_| failure("终端关闭未确认。"))?;
                    return Ok(());
                }
            }
        };
        tokio::select! { biased;
            () = owner.shutdown.cancelled() => Ok(()),
            () = permit.ended() => Err(failure("登录或访问权限已失效。")),
            result = upstream => result,
            result = downstream => result,
        }
    };
    if let Err(error) = &result {
        let text = serde_json::to_string(&Notice::Error {
            message: error.to_string(),
        })
        .map_err(|_| failure("终端消息编码失败。"))?;
        let _ = timeout(
            Duration::from_secs(3),
            sender.send(Message::Text(text.into())),
        )
        .await;
    }
    let _ = timeout(Duration::from_secs(3), sender.send(Message::Close(None))).await;
    result
}

pub(crate) fn submit(state: HttpState, flow: Flow, permit: AccessPermit) {
    let owner = state.terminals.clone();
    // 目标也使用相同连接及握手预算；拒绝时 Flow 的 drop 只 reset 此流。
    let Ok(connection) = owner.sockets.clone().try_acquire_owned() else {
        return;
    };
    let Ok(slot) = owner.handshakes.clone().try_acquire_owned() else {
        return;
    };
    let _ = owner.submit(Box::pin(async move {
        let _connection = connection;
        let frame = timeout(Duration::from_secs(5), flow.recv())
            .await
            .map_err(|_| failure("终端打开超时。"))?
            .map_err(|_| failure("终端通道已关闭。"))?;
        if frame.kind != 5 {
            return Err(failure("终端打开消息无效。"));
        }
        let open: Open =
            serde_json::from_slice(&frame.payload).map_err(|_| failure("终端打开参数无效。"))?;
        let mut socket = Transport::Bridge(flow);
        if let Err(error) = check_compatibility(Some(&open.client), &ClientCompatibility::current())
        {
            let _ = socket::notice(&mut socket, Notice::CompatibilityError { error }).await;
            let _ = timeout(Duration::from_secs(3), socket.send(Message::Close(None))).await;
            return Ok(());
        }
        permit.check().map_err(|_| failure("访问权限已失效。"))?;
        socket::run(socket, state, permit, open, slot).await
    }));
}
