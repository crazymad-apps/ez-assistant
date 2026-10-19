//! 本机 UI 的控制连接只传首帧身份、协商和播放事实；视频走独立 loopback WebRTC。
use super::{
    MediaError,
    peer::{self, Event, Peer},
    service::DesktopViews,
};
use crate::{
    access::registration::network::DesktopAccess,
    http::socket_auth::{self, SocketAuthentication},
};
use axum::extract::ws::{Message, WebSocket};
use futures_util::SinkExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::watch,
    time::{Instant, timeout},
};
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::{
    track_local::static_rtp::TrackLocalStaticRTP,
    track_remote::{TrackRemote, TrackRemoteEvent},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    #[serde(rename = "type")]
    kind: String,
    data: assistant_protocol::DesktopViewOpenRequest,
}
pub(crate) async fn serve(
    mut socket: WebSocket,
    auth: SocketAuthentication,
    owner: Arc<DesktopViews>,
) -> Result<(), MediaError> {
    let result = tokio::select! {
        () = owner.shutdown.cancelled() => Err(MediaError::Ended),
        result = authenticate(&mut socket, &auth) => result,
    };
    let (request, permit) = match result {
        Ok(value) => value,
        Err(error) => {
            finish(&mut socket, &error).await;
            return Err(error);
        }
    };
    // 打开访问许可之前不创建采集/媒体；本机 UI 必须仍然属于首次认证的登录。
    let access = tokio::select! {
        () = owner.shutdown.cancelled() => Err(MediaError::Ended),
        () = permit.ended() => Err(MediaError::Ended),
        message = socket.recv() => { let _ = message; Err(MediaError::Ended) },
        result = auth.state.access.network.open_desktop(request.device_id, permit.clone()) => result.map_err(MediaError::from),
    };
    let mut access = match access {
        Ok(access) => access,
        Err(error) => {
            finish(&mut socket, &error).await;
            return Err(error);
        }
    };
    let mut local = match Peer::new(true, &[]).await {
        Ok(peer) => peer,
        Err(error) => {
            finish(&mut socket, &error).await;
            return Err(error);
        }
    };
    let mut remote = None;
    let cancel = access.cancelled.clone();
    let local_failed = local.failed.clone();
    let result = tokio::select! { biased;
        () = owner.shutdown.cancelled() => Err(MediaError::Ended),
        () = cancel.cancelled() => Err(MediaError::Ended),
        () = permit.ended() => Err(MediaError::Ended),
        () = local_failed.cancelled() => Err(MediaError::Ended),
        result = view(&mut socket, &mut access, &mut local, &mut remote, &owner, request.display_id.as_deref()) => result,
    };
    // 显式释放两个 peer，保留 cleanup 错误；关闭 UI 会话会通过 access Drop 撤销目标采集。
    let remote_close = if let Some(remote) = remote {
        remote.close().await
    } else {
        Ok(())
    };
    let local_close = local.close().await;
    drop(access);
    if let Err(error) = &result {
        finish(&mut socket, error).await;
    }
    result.and(remote_close).and(local_close)
}
async fn authenticate(
    socket: &mut WebSocket,
    auth: &SocketAuthentication,
) -> Result<
    (
        assistant_protocol::DesktopViewOpenRequest,
        crate::access::AccessPermit,
    ),
    MediaError,
> {
    let value = timeout(Duration::from_secs(10), receive(socket))
        .await
        .map_err(|_| MediaError::ConnectTimeout)??;
    let open: Open = serde_json::from_value(value).map_err(|_| MediaError::Protocol)?;
    if open.kind != "open" || !auth.peer.ip().is_loopback() || auth.target.is_some() {
        return Err(MediaError::Protocol);
    }
    socket_auth::validate_compatibility(&auth.headers, open.data.client_compatibility.as_ref())
        .map_err(|_| MediaError::Protocol)?;
    let permit = socket_auth::authenticate(
        auth,
        open.data.bearer.as_ref().map(|token| token.expose()),
        open.data.login_context.as_deref(),
    )
    .await
    .map_err(|_| MediaError::Access(crate::access::AccessError::Unauthorized))?;
    if open
        .data
        .display_id
        .as_ref()
        .is_some_and(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_control))
    {
        return Err(MediaError::Protocol);
    }
    Ok((open.data, permit))
}
async fn view(
    socket: &mut WebSocket,
    access: &mut DesktopAccess,
    local: &mut Peer,
    remote: &mut Option<Peer>,
    owner: &DesktopViews,
    display: Option<&str>,
) -> Result<(), MediaError> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let output = Arc::new(TrackLocalStaticRTP::new(peer::media(20_001)));
    let sender = local.connection.add_track(output.clone()).await?;
    send(
        socket,
        json!({"type":"offer","data":local.description(false).await?}),
    )
    .await?;
    tokio::time::timeout_at(deadline, async {
        loop {
            tokio::select! {
                message = receive(socket) => {
                    let message = message?;
                    match message["type"].as_str() {
                        Some("answer") => { local.remote(message["data"].clone(), true).await?; break; },
                        Some("candidate") => local.candidate(message["data"].clone()).await?,
                        _ => return Err(MediaError::Protocol),
                    }
                }
                event = local.events.recv() => match event.ok_or(MediaError::Ended)? { Event::Candidate(candidate) => send(socket, json!({"type":"candidate","data":candidate})).await?, _ => return Err(MediaError::Protocol) }
            }
        }
        Ok::<(),MediaError>(())
    }).await.map_err(|_| MediaError::ConnectTimeout)??;
    // WebView 已接受固定的 H264 能力后才准备目标，避免先采集再发现本地无法播放。
    *remote = Some(Peer::new(false, &owner.stun).await?);
    let remote = remote.as_mut().ok_or(MediaError::Protocol)?;
    access
        .send(json!({"type":"prepare","data":{"fmtp":peer::FMTP,"display_id":display}}))
        .await?;
    let pt = sender
        .get_parameters()
        .await?
        .rtp_parameters
        .codecs
        .first()
        .ok_or(MediaError::Protocol)?
        .payload_type;
    let mut track: Option<Arc<dyn TrackRemote>> = None;
    let remote_failed = remote.failed.clone();
    let mut offered = false;
    let mut decoded = false;
    let mut local_verified = false;
    let mut feedback_tick = tokio::time::interval(Duration::from_millis(500));
    feedback_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_keyframe = 0;
    let (control_send, control_receive) = tokio::sync::mpsc::channel(4);
    let controls = super::control::source::run(
        access.id.clone(),
        access.control_opener(),
        local.data.clone(),
        remote.data.clone(),
        control_receive,
    );
    tokio::pin!(controls);
    loop {
        tokio::select! {
            result = &mut controls => return result,
            () = remote_failed.cancelled() => return Err(MediaError::Ended),
            () = tokio::time::sleep_until(deadline), if !decoded => return Err(MediaError::ConnectTimeout),
            message = receive(socket) => {
                let message = message?;
                match message["type"].as_str() {
                    Some("candidate") => local.candidate(message["data"].clone()).await?,
                    Some("decoded") if track.is_some() && !decoded => { decoded = true; control_send.try_send(super::control::source::Command::Viewed).map_err(|_| MediaError::Ended)?; send(socket, json!({"type":"ready"})).await?; },
                    Some("control_start" | "control_stop" | "control_status") if decoded => {
                        use super::control::source::Command;
                        let command = match message["type"].as_str() { Some("control_start") => Command::Start(message["request_id"].as_u64().filter(|id| *id > 0 && *id <= 9_007_199_254_740_991).ok_or(MediaError::Protocol)?), Some("control_stop") => Command::Stop, _ => Command::Status };
                        control_send.try_send(command).map_err(|_| MediaError::Ended)?;
                    },
                    Some("close") => return Ok(()),
                    _ => return Err(MediaError::Protocol),
                }
            }
            event = local.events.recv() => match event.ok_or(MediaError::Ended)? { Event::Candidate(candidate) => send(socket, json!({"type":"candidate","data":candidate})).await?, Event::Connected => { local.verify_path().await?; local_verified = true; }, _ => return Err(MediaError::Protocol) },
            event = remote.events.recv() => match event.ok_or(MediaError::Ended)? {
                Event::Candidate(candidate) => access.send(json!({"type":"candidate","data":candidate})).await?,
                // 接收端句柄在首个轨道事件时才装配；校验完成前不转发任何 RTP。
                Event::Connected => {},
                Event::Track(incoming) if track.is_none() => { remote.verify_path().await?; track = Some(incoming); },
                _ => return Err(MediaError::Protocol),
            },
            message = access.messages.recv() => {
                let message = message.ok_or(MediaError::Ended)?;
                match message["type"].as_str() {
                    Some("offer") if !offered => {
                        let mut offer = message["data"].clone();
                        let metadata = offer.as_object_mut().and_then(|value| value.remove("desktop")).ok_or(MediaError::Protocol)?;
                        super::target::validate_displays(&metadata)?;
                        send(socket, json!({"type":"displays","data":metadata})).await?;
                        remote.remote(offer, false).await?; offered = true;
                        access.send(json!({"type":"answer","data":remote.description(true).await?})).await?;
                    },
                    Some("candidate") if offered => remote.candidate(message["data"].clone()).await?,
                    Some("error") => return Err(MediaError::Unavailable(message["data"]["code"].as_str().unwrap_or("desktop_unavailable").chars().take(64).collect())),
                    _ => return Err(MediaError::Protocol),
                }
            }
            // poll() 是队列读取，可取消；每包写入只等待有界时间，不积压画面。
            packet = next_packet(&track), if track.is_some() && local_verified => {
                match packet? {
                    TrackRemoteEvent::OnRtpPacket(packet) => {
                        timeout(Duration::from_secs(1), output.write_rtp(relay_packet(packet, pt))).await.map_err(|_| MediaError::Ended)??;
                    }
                    TrackRemoteEvent::OnError | TrackRemoteEvent::OnEnded | TrackRemoteEvent::OnEnding => return Err(MediaError::Ended),
                    _ => {},
                }
            }
            _ = feedback_tick.tick(), if track.is_some() => {
                timeout(Duration::from_millis(500), relay_feedback(&track, &mut local.feedback, &mut last_keyframe)).await.map_err(|_| MediaError::Ended)??;
            }
        }
    }
}
fn relay_packet(mut packet: rtc::rtp::Packet, payload_type: u8) -> rtc::rtp::Packet {
    packet.header.ssrc = 20_001;
    packet.header.payload_type = payload_type;
    // 每段自行生成 TWCC 等扩展。必须同时清空填充，避免下一段追加扩展后长度错位。
    packet.header.extension = false;
    packet.header.extensions.clear();
    packet.header.extension_profile = 0;
    packet.header.extensions_padding = 0;
    packet
}

async fn next_packet(track: &Option<Arc<dyn TrackRemote>>) -> Result<TrackRemoteEvent, MediaError> {
    track
        .as_ref()
        .ok_or(MediaError::Protocol)?
        .poll()
        .await
        .ok_or(MediaError::Ended)
}
async fn relay_feedback(
    track: &Option<Arc<dyn TrackRemote>>,
    feedback: &mut watch::Receiver<super::feedback::Feedback>,
    last_keyframe: &mut u64,
) -> Result<(), MediaError> {
    if !feedback.has_changed().map_err(|_| MediaError::Ended)? {
        return Ok(());
    }
    let state = *feedback.borrow_and_update();
    let track = track.as_ref().ok_or(MediaError::Protocol)?;
    // 不能跨段重放 NACK：本段已自行重传。关键帧/码率请求使用目标轨道的 SSRC。
    use rtc::rtcp::payload_feedbacks::{
        picture_loss_indication::PictureLossIndication,
        receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate,
    };
    let ssrc = *track.ssrcs().await.first().ok_or(MediaError::Protocol)?;
    let mut packets: Vec<Box<dyn rtc::rtcp::Packet>> = vec![];
    if state.keyframe != *last_keyframe {
        *last_keyframe = state.keyframe;
        packets.push(Box::new(PictureLossIndication {
            sender_ssrc: 30_001,
            media_ssrc: ssrc,
        }));
    }
    if state.bitrate != 0 {
        packets.push(Box::new(ReceiverEstimatedMaximumBitrate {
            sender_ssrc: 30_001,
            bitrate: state.bitrate as f32,
            ssrcs: vec![ssrc],
        }));
    }
    if !packets.is_empty() {
        track.write_rtcp(packets).await?;
    }
    Ok(())
}
async fn receive(socket: &mut WebSocket) -> Result<Value, MediaError> {
    match socket.recv().await {
        Some(Ok(Message::Text(text))) if text.len() <= 16_384 => {
            serde_json::from_str(&text).map_err(|_| MediaError::Protocol)
        }
        _ => Err(MediaError::Ended),
    }
}
async fn send(socket: &mut WebSocket, value: Value) -> Result<(), MediaError> {
    timeout(
        Duration::from_secs(1),
        socket.send(Message::Text(value.to_string().into())),
    )
    .await
    .map_err(|_| MediaError::Ended)?
    .map_err(|_| MediaError::Ended)
}
pub(super) fn error_code(error: &MediaError) -> &'static str {
    match error {
        MediaError::Access(_) => "desktop_access_denied",
        MediaError::ConnectTimeout => "desktop_direct_unavailable",
        MediaError::MissingHelper | MediaError::UnsupportedPlatform => {
            "desktop_capture_unavailable"
        }
        MediaError::Unavailable(code) => match code.as_str() {
            "display_unavailable" | "display_changed" | "desktop_display_unavailable" => {
                "desktop_display_unavailable"
            }
            "desktop_busy" => "desktop_busy",
            "desktop_slow_viewer" => "desktop_slow_viewer",
            _ => "desktop_capture_unavailable",
        },
        _ => "desktop_connection_ended",
    }
}
async fn finish(socket: &mut WebSocket, error: &MediaError) {
    let _ = send(socket, json!({"type":"error","code":error_code(error)})).await;
    let _ = timeout(Duration::from_millis(300), socket.close()).await;
}

#[cfg(test)]
mod tests {
    use super::relay_packet;
    use rtc::shared::marshal::{Marshal, Unmarshal};

    #[test]
    fn relay_rebuilds_extensions_without_corrupting_video_payload() {
        let mut packet = rtc::rtp::Packet::default();
        packet.header.version = 2;
        packet.header.ssrc = 10_001;
        packet.header.sequence_number = 42;
        packet.payload = vec![0x65, 0x80, 1, 2, 3].into();
        packet.header.set_extension(3, vec![0, 1].into()).unwrap();
        assert_ne!(packet.header.extensions_padding, 0);
        let mut outgoing = relay_packet(packet, 102);
        outgoing.header.set_extension(5, vec![0, 7].into()).unwrap();
        let expected = outgoing.payload.clone();
        let mut wire = outgoing.marshal().unwrap();
        let decoded = rtc::rtp::Packet::unmarshal(&mut wire).unwrap();
        assert_eq!(decoded.payload, expected);
        assert_eq!(decoded.header.ssrc, 20_001);
        assert_eq!(decoded.header.sequence_number, 42);
        assert_eq!(decoded.header.get_extension_ids(), vec![5]);
    }
}
