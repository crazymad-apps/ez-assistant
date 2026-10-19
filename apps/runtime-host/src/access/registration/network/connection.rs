//! 控制 WS 的协商、读写和期限；所有 future 属于连接 owner，取消时一起丢弃 socket。
use super::{Network, Target, session::Session};
use crate::http::HttpState;
use assistant_protocol::HostNetworkState as State;
use futures_util::FutureExt;
use futures_util::stream::FuturesUnordered;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::{
    sync::mpsc,
    time::{Instant, sleep_until, timeout},
};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Error, Message, client::IntoClientRequest, protocol::WebSocketConfig},
};

const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Deserialize)]
struct Hello {
    #[serde(rename = "type")]
    kind: String,
    protocol_version: u32,
    center_id: String,
    registration_id: Option<String>,
}

pub(super) async fn run(
    target: &Target,
    network: &Network,
    state: Option<HttpState>,
) -> (State, bool) {
    let mut ready_at = None;
    let result = run_inner(target, network, &mut ready_at, state).await;
    (
        result.unwrap_or_else(|state| state),
        ready_at.is_some_and(|at: Instant| at.elapsed() >= Duration::from_secs(60)),
    )
}

async fn run_inner(
    target: &Target,
    network: &Network,
    ready_at: &mut Option<Instant>,
    state: Option<HttpState>,
) -> Result<State, State> {
    target
        .client
        .verify_center(&target.center_id)
        .await
        .map_err(|error| match error {
            crate::access::enterprise::CenterError::IdentityMismatch
            | crate::access::enterprise::CenterError::ProtocolIncompatible => State::Incompatible,
            _ => State::Retrying,
        })?;
    let e = target.enrollment();
    let url = endpoint(&target.client.url)?;
    let mut request = url.into_client_request().map_err(|_| State::Failed)?;
    let secret = if let Some(e) = e {
        request
            .headers_mut()
            .insert("x-ez-device-id", e.id.parse().map_err(|_| State::Failed)?);
        e.secret.clone()
    } else {
        request.headers_mut().insert(
            "x-ez-network-mode",
            "caller".parse().map_err(|_| State::Failed)?,
        );
        network
            .credentials
            .network_caller(&target.center_id)
            .ok_or(State::CredentialInvalid)?
    };
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", secret.expose())
            .parse()
            .map_err(|_| State::Failed)?,
    );
    let config = WebSocketConfig::default()
        .max_message_size(Some(32768 + 30))
        .max_frame_size(Some(32768 + 30))
        .write_buffer_size(0)
        .max_write_buffer_size(262144);
    let ws = timeout(Duration::from_secs(10), async {
        let (mut ws, _) = connect_async_with_config(request, Some(config), true)
            .await
            .map_err(map_error)?;
        timeout(Duration::from_secs(5), async {
            let Some(Ok(Message::Text(text))) = ws.next().await else {
                return Err(State::Incompatible);
            };
            let hello: Hello = serde_json::from_str(&text).map_err(|_| State::Incompatible)?;
            if hello.kind != "hello"
                || hello.protocol_version != 2
                || hello.center_id != target.center_id
                || hello.registration_id.as_deref() != e.map(|e| e.id.as_str())
            {
                return Err(State::Incompatible);
            }
            ws.send(Message::Text(
                serde_json::json!({"type":"ready", "protocol_version":2,
                "center_id":target.center_id, "registration_id":e.map(|e| &e.id)})
                .to_string()
                .into(),
            ))
            .await
            .map_err(map_error)?;
            let Some(Ok(Message::Text(ack))) = ws.next().await else {
                return Err(State::Incompatible);
            };
            if !changed(&ack) {
                return Err(State::Incompatible);
            }
            Ok(())
        })
        .await
        .map_err(|_| State::Retrying)??;
        Ok(ws)
    })
    .await
    .map_err(|_| State::Retrying)??;
    *ready_at = Some(Instant::now());
    network.status.send_replace(State::Online);
    network.refresh.notify_one();
    drive(ws, network, Some(target), state).await
}

/// 直接驱动既有 socket，不派生后台读写任务；泛型流允许用受限管道验证真实背压。
pub(super) async fn drive<S>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    network: &Network,
    target: Option<&Target>,
    state: Option<HttpState>,
) -> Result<State, State>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // read 与 write 并行轮询；慢写不阻塞独立心跳期限，没有脱离 owner 的任务。
    let (mut sink, mut stream) = ws.split();
    let (send, mut receive) = mpsc::channel::<Message>(16);
    let (session, mut jobs) = Session::new(
        send.clone(),
        network.credentials.clone(),
        target.and_then(Target::enrollment).map(|e| e.id.clone()),
    );
    let _guard = SessionGuard {
        network,
        session: session.clone(),
    };
    network.connected.send_replace(Some(session.clone()));
    let mut work = FuturesUnordered::new();
    let writer = async {
        loop {
            let (message, packet) = tokio::select! {
                biased;
                message = receive.recv() => match message { Some(message) => (message, None), None => break },
                packet = session.streams.outgoing() => (packet.message.clone(), Some(packet)),
            };
            timeout(WRITE_TIMEOUT, sink.send(message))
                .await
                .map_err(|_| State::Retrying)?
                .map_err(map_error)?;
            if let Some(packet) = packet {
                session.streams.written(&packet);
            }
        }
        Ok::<(), State>(())
    };
    tokio::pin!(writer);
    let mut last_ping = Instant::now();
    loop {
        tokio::select! {
            biased;
            () = session.ended.cancelled() => return Err(State::Retrying),
            () = session.streams.ended.cancelled() => return Err(State::Retrying),
            Some(()) = work.next(), if !work.is_empty() => {},
            Some(job) = jobs.recv() => {
                let target = target.ok_or(State::Incompatible)?;
                if work.len() >= 128 { return Err(State::Retrying); }
                work.push(session.clone().work(job, target.client.clone(), target.center_id.clone(), state.clone()).boxed());
            },
            _ = sleep_until(last_ping + HEARTBEAT_TIMEOUT) => return Err(State::Retrying),
            result = &mut writer => { result?; return Err(State::Retrying); },
            message = stream.next() => {
                match message {
                    Some(Ok(Message::Ping(probe))) if probe.len() == 8 => {
                        if last_ping.elapsed() >= HEARTBEAT_TIMEOUT { return Err(State::Retrying); }
                        last_ping = Instant::now();
                        send.try_send(Message::Pong(probe)).map_err(|_| State::Retrying)?;
                    },
                    Some(Ok(Message::Text(text))) if text.len() <= 16384 && changed(&text) => {
                        network.refresh.notify_one();
                        network.status.send_replace(State::Online);
                    },
                    Some(Ok(Message::Text(text))) if text.len() <= 16384 => {
                        let value = serde_json::from_str(&text).map_err(|_| State::Incompatible)?;
                        session.receive(value, target.is_some_and(|t| t.enrollment().is_some())).map_err(|_| State::Incompatible)?;
                    },
                    Some(Ok(Message::Binary(bytes))) => { session.streams.receive(&bytes).map_err(|_| State::Incompatible)?; },
                    Some(Ok(Message::Close(_))) | None => return Err(State::Retrying),
                    Some(Err(error)) => return Err(map_error(error)),
                    _ => return Err(State::Incompatible),
                }
            }
        }
    }
}
fn changed(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .is_ok_and(|v| v["type"] == "registration_changed" && v["protocol_version"] == 2)
}
pub(super) fn endpoint(center: &str) -> Result<String, State> {
    let url = url::Url::parse(center).map_err(|_| State::Failed)?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err(State::Failed),
    };
    Ok(format!(
        "{scheme}://{}/api/host-network/connect",
        center
            .split_once("://")
            .ok_or(State::Failed)?
            .1
            .trim_end_matches('/')
    ))
}
fn map_error(error: Error) -> State {
    match error {
        Error::Http(response) => match response.status().as_u16() {
            409 => State::Conflict,
            401 | 403 => State::CredentialInvalid,
            404 | 426 => State::Incompatible,
            _ => State::Retrying,
        },
        // TCP 被中心或网络中断不等于协议不兼容；允许重新准入，但不重放旧流。
        Error::Protocol(
            tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
        ) => State::Retrying,
        Error::Capacity(_) | Error::Protocol(_) => State::Incompatible,
        _ => State::Retrying,
    }
}

struct SessionGuard<'a> {
    network: &'a Network,
    session: Arc<Session>,
}
impl Drop for SessionGuard<'_> {
    fn drop(&mut self) {
        self.session.close();
        self.network.connected.send_if_modified(|current| {
            if current
                .as_ref()
                .is_some_and(|value| Arc::ptr_eq(value, &self.session))
            {
                *current = None;
                true
            } else {
                false
            }
        });
    }
}
