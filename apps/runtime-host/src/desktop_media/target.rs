//! 被查看端：许可存在期间采集编码一次；信令、视频与独立控制管道均有界。
use super::{
    MediaError,
    capture::Frame,
    peer::{self, Event, Peer},
    service::DesktopViews,
};
use crate::access::registration::network::DesktopAccess;
use rtc::rtp::{
    packet::Packet,
    packetizer::{Packetizer, new_packetizer},
    sequence::new_random_sequencer,
};
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::watch, time::timeout};
use webrtc::media_stream::track_local::{TrackLocal, static_rtp::TrackLocalStaticRTP};

pub(super) async fn serve(
    mut access: DesktopAccess,
    owner: Arc<DesktopViews>,
    identity: super::control::Identity,
) -> Result<(), MediaError> {
    let (_registration, grants) = owner.register_view(access.id.clone(), identity);
    let mut input = super::control::State::default();
    let mut peer = Peer::new(false, &owner.stun).await?;

    let cancel = access.cancelled.clone();
    let permit = access.permit.clone();
    let failed = peer.failed.clone();
    let result = tokio::select! { biased;
        () = owner.shutdown.cancelled() => Err(MediaError::Ended),
        () = cancel.cancelled() => Err(MediaError::Ended),
        () = permit.ended() => Err(MediaError::Ended),
        () = failed.cancelled() => Err(MediaError::Ended),
        result = run(&mut access, &mut peer, &mut input, grants, owner.clone()) => result,
    };
    input.stop().await;
    let closed = peer.close().await;
    if let Err(error) = &result {
        let _ = timeout(
            Duration::from_millis(500),
            access.send(json!({"type":"error","data":{"code":super::socket::error_code(error)}})),
        )
        .await;
    }
    result.and(closed)
}
async fn run(
    access: &mut DesktopAccess,
    peer: &mut Peer,
    input: &mut super::control::State,
    grants: tokio::sync::mpsc::Receiver<DesktopAccess>,
    owner: Arc<DesktopViews>,
) -> Result<(), MediaError> {
    let track = Arc::new(TrackLocalStaticRTP::new(peer::media(10_001)));
    let sender = peer.connection.add_track(track.clone()).await?;
    let (display, binding) = timeout(Duration::from_secs(15), async {
        let prepare = access.messages.recv().await.ok_or(MediaError::Ended)?;
        if prepare["type"] != "prepare" || prepare["data"]["fmtp"] != peer::FMTP { return Err(MediaError::Protocol); }
        let catalog = super::capture::displays(&super::helper_path()?).await?;
        let choice = choose_display(&catalog, &prepare["data"]["display_id"]);
        let metadata = match &choice { Ok((_, _, metadata)) => metadata.clone(), Err(_) => { let displays: Vec<assistant_protocol::DesktopDisplay> = serde_json::from_value(catalog.clone()).map_err(|_| MediaError::Protocol)?; json!({"selected":null,"displays":displays}) } };
        validate_displays(&metadata)?;
        let mut offer = peer.description(false).await?;
        offer["desktop"] = metadata;
        // 显示目录随现有 offer 返回，中心仍只转发有界协商，无需理解显示器字段。
        access.send(json!({"type":"offer", "data":offer})).await?;
        let (display, binding, _) = choice?;
        let mut answered = false;
        loop {
            tokio::select! {
                event = peer.events.recv() => match event.ok_or(MediaError::Ended)? {
                    Event::Candidate(candidate) => access.send(json!({"type":"candidate","data":candidate})).await?,
                    Event::Connected if answered => { peer.verify_path().await?; break; },
                    _ => return Err(MediaError::Protocol),
                },
                message = access.messages.recv() => {
                    let message = message.ok_or(MediaError::Ended)?;
                    match message["type"].as_str() {
                        Some("answer") if !answered => { peer.remote(message["data"].clone(), true).await?; answered = true; },
                        Some("candidate") if answered => peer.candidate(message["data"].clone()).await?,
                        _ => return Err(MediaError::Protocol),
                    }
                }
            }
        }
        Ok::<_, MediaError>((display, binding))
    }).await.map_err(|_| MediaError::ConnectTimeout)??;
    let pt = sender
        .get_parameters()
        .await?
        .rtp_parameters
        .codecs
        .first()
        .ok_or(MediaError::Protocol)?
        .payload_type;
    let mut capture = owner.capture(&display, &binding).await?;
    // 两条 future 持续存在；控制反馈不会取消并重新读取半帧。
    let view_id = access.id.clone();
    tokio::select! {
        result = super::control::serve(input, grants, peer.data.clone(), owner, &view_id, &display, &binding) => result,
        result = frames(&mut capture, track, pt, peer.feedback.clone()) => result,
        result = async {
            loop {
                tokio::select! {
                    event = peer.events.recv() => match event.ok_or(MediaError::Ended)? { Event::Candidate(candidate) => access.send(json!({"type":"candidate","data":candidate})).await?, _ => return Err(MediaError::Protocol) },
                    message = access.messages.recv() => { let message = message.ok_or(MediaError::Ended)?; if message["type"] != "candidate" { return Err(MediaError::Protocol); } peer.candidate(message["data"].clone()).await?; }
                }
            }
        } => result,
    }
}
async fn frames(
    capture: &mut super::shared::Subscription,
    track: Arc<TrackLocalStaticRTP>,
    pt: u8,
    mut feedback: watch::Receiver<super::feedback::Feedback>,
) -> Result<(), MediaError> {
    // 复用 WebRTC 的 H.264 分包器和 1200 字节 MTU；仅在此适配原生采集时间戳。
    let mut packetizer = new_packetizer(
        Instant::now(),
        1200,
        pt,
        10_001,
        peer::codec().rtp_codec.payloader()?,
        Box::new(new_random_sequencer()),
        90_000,
    );
    let mut keyframe = 0;
    let mut started = false;
    let first_frame_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut window = Instant::now();
    let mut bytes = 0u64;
    let mut slow_since = None;
    loop {
        let ended = capture.ended();
        let frame = tokio::select! {
            frame = capture.next_frame() => frame?,
            error = ended => return Err(error),
            changed = feedback.changed() => {
                changed.map_err(|_| MediaError::Ended)?;
                let state = *feedback.borrow_and_update();
                if keyframe != state.keyframe { keyframe = state.keyframe; capture.request_keyframe(); }
                continue;
            },
            () = tokio::time::sleep_until(first_frame_deadline), if !started => return Err(MediaError::NoFrames),
        };
        let state = *feedback.borrow();
        if !started && !frame.keyframe {
            if tokio::time::Instant::now() >= first_frame_deadline {
                return Err(MediaError::NoFrames);
            }
            continue;
        }
        started = true;
        bytes += frame.data.len() as u64;
        if window.elapsed() >= Duration::from_secs(1) {
            // 使用实际编码流量检查独立查看者，不拿慢用户的预算降低共享画质。
            let rate = (bytes as f64 * 8.0 / window.elapsed().as_secs_f64()) as u32;
            if state.bitrate != 0 && f64::from(state.bitrate) * 1.25 < f64::from(rate) {
                let since = slow_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(5) {
                    return Err(MediaError::Unavailable("desktop_slow_viewer".into()));
                }
            } else {
                slow_since = None;
            }
            bytes = 0;
            window = Instant::now();
        }
        let packets = packetize_frame(&mut packetizer, &frame)?;
        timeout(Duration::from_secs(1), async {
            for packet in packets {
                track.write_rtp(packet).await?;
            }
            Ok::<(), MediaError>(())
        })
        .await
        .map_err(|_| MediaError::Ended)??;
    }
}

/// 原生 PTS 属于当前帧。Sample.duration 在分包后才推进下一帧，不能用上一帧间隔代替。
/// 保留分包器的随机时间基点，按当前 PTS 映射 90kHz；空闲、变帧率和回绕均不累积误差。
fn packetize_frame(
    packetizer: &mut impl Packetizer,
    frame: &Frame,
) -> Result<Vec<Packet>, MediaError> {
    let ticks = (u128::from(frame.timestamp_us) * 9 / 100) as u32;
    let mut packets = packetizer.packetize(Instant::now(), &frame.data.clone().into(), 0)?;
    for packet in &mut packets {
        packet.header.timestamp = packet.header.timestamp.wrapping_add(ticks);
    }
    Ok(packets)
}

fn choose_display(
    catalog: &serde_json::Value,
    requested: &serde_json::Value,
) -> Result<(String, String, serde_json::Value), MediaError> {
    let items = catalog
        .as_array()
        .filter(|items| !items.is_empty() && items.len() <= 16)
        .ok_or(MediaError::Protocol)?;
    let displays: Vec<assistant_protocol::DesktopDisplay> =
        serde_json::from_value(catalog.clone()).map_err(|_| MediaError::Protocol)?;
    let selected = if requested.is_null() {
        items.iter().find(|item| item["primary"] == true)
    } else {
        let id = requested.as_str().ok_or(MediaError::Protocol)?;
        items.iter().find(|item| item["id"] == id)
    };
    let selected = selected.ok_or_else(|| MediaError::Unavailable("display_unavailable".into()))?;
    let id = selected["id"]
        .as_str()
        .ok_or(MediaError::Protocol)?
        .to_owned();
    let binding = selected["binding"]
        .as_str()
        .filter(|value| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        })
        .ok_or(MediaError::Protocol)?
        .to_owned();
    let metadata = json!({"selected":id,"displays":displays});
    validate_displays(&metadata)?;
    Ok((id, binding, metadata))
}
pub(super) fn validate_displays(value: &serde_json::Value) -> Result<(), MediaError> {
    let displays: Vec<assistant_protocol::DesktopDisplay> =
        serde_json::from_value(value["displays"].clone()).map_err(|_| MediaError::Protocol)?;
    let mut ids = std::collections::HashSet::new();
    if displays.is_empty()
        || displays.len() > 16
        || displays.iter().any(|d| {
            d.id.is_empty()
                || d.id.len() > 256
                || d.id.chars().any(char::is_control)
                || !ids.insert(&d.id)
                || d.width == 0
                || d.height == 0
                || d.width > 16384
                || d.height > 16384
        })
        || (!value["selected"].is_null() && !displays.iter().any(|d| value["selected"] == d.id))
    {
        return Err(MediaError::Protocol);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_bind_native_identity_not_enumeration_order() {
        let main = json!({"id":"native-primary","width":1920,"height":1080,"primary":true,"binding":"main-bounds"});
        let side = json!({"id":"native-left","width":1080,"height":1920,"primary":false,"binding":"negative-bounds"});
        for catalog in [json!([main, side]), json!([side, main])] {
            assert_eq!(
                choose_display(&catalog, &json!(null)).unwrap().0,
                "native-primary"
            );
            let (id, binding, metadata) = choose_display(&catalog, &json!("native-left")).unwrap();
            assert_eq!(id, "native-left");
            assert_eq!(binding, "negative-bounds");
            assert!(metadata["displays"][0].get("binding").is_none());
            assert!(
                matches!(choose_display(&catalog, &json!("removed")), Err(MediaError::Unavailable(code)) if code == "display_unavailable")
            );
        }
        assert!(validate_displays(&json!({"selected":"a","displays":[{"id":"a","width":0,"height":1080,"primary":true}]})).is_err());
    }

    #[test]
    fn capture_pts_remain_on_current_frame_after_idle_and_clock_wrap() {
        let mut packetizer = new_packetizer(
            Instant::now(),
            1200,
            102,
            10_001,
            peer::codec().rtp_codec.payloader().unwrap(),
            Box::new(new_random_sequencer()),
            90_000,
        );
        // 跨越 RTP 32 位回绕，随后静止 5 秒再恢复；每帧刻意拆成多个 RTP 包。
        let times = [
            47_721_800_000u64,
            47_721_833_333,
            47_726_833_333,
            47_726_866_666,
        ];
        let mut origin = None;
        let mut sequence: Option<u16> = None;
        for pts in times {
            let mut data = vec![0, 0, 0, 1, 0x65];
            data.extend(vec![0x88; 3000]);
            let packets = packetize_frame(
                &mut packetizer,
                &Frame {
                    data,
                    timestamp_us: pts,
                    keyframe: true,
                },
            )
            .unwrap();
            assert!(packets.len() > 1);
            let timestamp = packets[0].header.timestamp;
            let origin = *origin.get_or_insert(timestamp);
            let expected = ((u128::from(pts) * 9 / 100) - (u128::from(times[0]) * 9 / 100)) as u32;
            assert_eq!(timestamp.wrapping_sub(origin), expected);
            for (index, packet) in packets.iter().enumerate() {
                assert_eq!(packet.header.timestamp, timestamp);
                assert_eq!(packet.header.marker, index + 1 == packets.len());
                assert_eq!(packet.header.payload_type, 102);
                assert_eq!(packet.header.ssrc, 10_001);
                if let Some(previous) = sequence {
                    assert_eq!(packet.header.sequence_number, previous.wrapping_add(1));
                }
                sequence = Some(packet.header.sequence_number);
            }
        }
    }
}
