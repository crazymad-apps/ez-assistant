//! WS 与桥接流仅转换消息边界；PTY、ACK 和终端心跳仍由 socket 执行循环负责。
use super::{TerminalError, failure};
use crate::access::registration::network::streams::{Flow, Frame};
use axum::extract::ws::{Message, WebSocket};
use tokio_util::sync::CancellationToken;

pub(super) enum Transport {
    Local(Box<WebSocket>),
    Bridge(Flow),
}
impl Transport {
    pub fn cancellation(&self) -> (CancellationToken, CancellationToken) {
        match self {
            Self::Local(_) => (CancellationToken::new(), CancellationToken::new()),
            Self::Bridge(flow) => (flow.cancelled(), flow.ended()),
        }
    }
    pub async fn recv(&mut self) -> Option<Result<Message, TerminalError>> {
        match self {
            Self::Local(socket) => socket
                .recv()
                .await
                .map(|r| r.map_err(|_| failure("终端连接已关闭。"))),
            Self::Bridge(flow) => Some(match flow.recv().await {
                Ok(frame) => decode(frame),
                Err(_) => Err(failure("远端终端连接已关闭。")),
            }),
        }
    }
    pub async fn send(&mut self, message: Message) -> Result<(), TerminalError> {
        match self {
            Self::Local(socket) => socket
                .send(message)
                .await
                .map_err(|_| failure("终端连接已关闭。")),
            Self::Bridge(flow) => {
                let close = matches!(message, Message::Close(_));
                flow.send(encode(message))
                    .await
                    .map_err(|_| failure("远端终端连接已关闭。"))?;
                if close {
                    flow.finish()
                        .await
                        .map_err(|_| failure("远端终端关闭未确认。"))?;
                }
                Ok(())
            }
        }
    }
}
pub(super) fn encode(message: Message) -> Frame {
    let (kind, payload) = match message {
        Message::Text(text) => (6, text.as_bytes().to_vec()),
        Message::Binary(bytes) => (7, bytes.to_vec()),
        Message::Ping(bytes) => (8, bytes.to_vec()),
        Message::Pong(bytes) => (9, bytes.to_vec()),
        Message::Close(_) => (10, Vec::new()),
    };
    Frame { kind, payload }
}
pub(super) fn decode(frame: Frame) -> Result<Message, TerminalError> {
    Ok(match frame.kind {
        6 => Message::Text(
            String::from_utf8(frame.payload)
                .map_err(|_| failure("终端文本无效。"))?
                .into(),
        ),
        7 => Message::Binary(frame.payload.into()),
        8 => Message::Ping(frame.payload.into()),
        9 => Message::Pong(frame.payload.into()),
        10 => Message::Close(None),
        _ => return Err(failure("终端消息类型无效。")),
    })
}
