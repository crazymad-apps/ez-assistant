//! 两段连接使用同一 H.264 子集；本地段只绑定 loopback，跨设备段仅显式 STUN。
use super::{
    MediaError,
    feedback::{EncoderFeedback, Feedback, MAX_BITRATE},
};
use rtc::{
    interceptor::{Gcc, PacerBuilder, Registry, Slot},
    media_stream::MediaStreamTrack,
    peer_connection::configuration::{
        interceptor_registry::{
            CongestionFeedback, configure_congestion_control, register_default_interceptors,
        },
        media_engine::{MIME_TYPE_H264, MediaEngine},
    },
    rtp_transceiver::rtp_sender::{
        RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
        RtpCodecKind,
    },
};
use std::{net::IpAddr, sync::Arc};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use webrtc::{
    media_stream::track_remote::TrackRemote,
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
        RTCIceCandidateInit, RTCIceServer, RTCPeerConnectionIceEvent, RTCPeerConnectionState,
        RTCSdpType, RTCSessionDescription,
    },
};

pub(super) const FMTP: &str =
    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e032";
pub(super) enum Event {
    Candidate(RTCIceCandidateInit),
    Connected,
    Track(Arc<dyn TrackRemote>),
}
struct Handler {
    send: mpsc::Sender<Event>,
    failed: CancellationToken,
}
impl Handler {
    fn send(&self, event: Event) {
        if self.send.try_send(event).is_err() {
            self.failed.cancel();
        }
    }
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        match event.candidate.to_json() {
            Ok(candidate) => self.send(Event::Candidate(candidate)),
            Err(_) => self.failed.cancel(),
        }
    }
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        match state {
            RTCPeerConnectionState::Connected => self.send(Event::Connected),
            RTCPeerConnectionState::Failed
            | RTCPeerConnectionState::Disconnected
            | RTCPeerConnectionState::Closed => self.failed.cancel(),
            _ => {}
        }
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        self.send(Event::Track(track));
    }
}
pub(super) fn codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.into(),
            clock_rate: 90_000,
            sdp_fmtp_line: FMTP.into(),
            ..Default::default()
        },
        payload_type: 102,
    }
}
pub(super) fn media(ssrc: u32) -> MediaStreamTrack {
    MediaStreamTrack::new(
        "desktop".into(),
        "desktop-video".into(),
        "desktop".into(),
        RtpCodecKind::Video,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(ssrc),
                ..Default::default()
            },
            codec: codec().rtp_codec,
            ..Default::default()
        }],
    )
}
fn media_interceptors(engine: &mut MediaEngine) -> Result<Registry, MediaError> {
    let registry = register_default_interceptors(Registry::new(), engine)?;
    let registry = configure_congestion_control(
        registry,
        // 与编码器起始预算一致，避免默认 300kbps 把高清桌面立即压成低画质。
        Gcc::new(f64::from(MAX_BITRATE), 150_000.0, f64::from(MAX_BITRATE)),
        CongestionFeedback::Twcc,
        engine,
    )?;
    // 库默认 pacer 只有 1Mbps，且 GCC 仅在估计变化时同步速率。
    // 替换同一槽位的初始预算；后续拥塞反馈仍正常调整发送速率。
    Ok(registry.with(
        Slot::Pacer,
        PacerBuilder::new()
            .with_target_bitrate(f64::from(MAX_BITRATE))
            .build(),
    ))
}
pub(super) struct Peer {
    pub(super) connection: Arc<dyn PeerConnection>,
    pub(super) events: mpsc::Receiver<Event>,
    pub(super) feedback: watch::Receiver<Feedback>,
    pub(super) data: Arc<dyn webrtc::data_channel::DataChannel>,
    pub(super) failed: CancellationToken,
    local: bool,
    candidates: usize,
}
impl Peer {
    pub(super) async fn new(local: bool, stun: &[RTCIceServer]) -> Result<Self, MediaError> {
        let mut engine = MediaEngine::default();
        engine.register_codec(codec(), RtpCodecKind::Video)?;
        let registry = media_interceptors(&mut engine)?;
        let (feedback, rx) = watch::channel(Feedback::default());
        let registry = registry.with(Slot::from(13_500), EncoderFeedback::new(feedback));
        let (send, events) = mpsc::channel(72); // 64 ICE + 连接/轨道事件；满即关闭本次访问。
        let failed = CancellationToken::new();
        let pc = PeerConnectionBuilder::<String>::new()
            .with_media_engine(engine)
            .with_interceptor_registry(registry)
            .with_configuration(
                RTCConfigurationBuilder::new()
                    .with_ice_servers(if local { vec![] } else { stun.to_vec() })
                    .build(),
            )
            .with_handler(Arc::new(Handler {
                send,
                failed: failed.clone(),
            }))
            .with_udp_addrs(vec![if local { "127.0.0.1:0" } else { "0.0.0.0:0" }.into()])
            .with_data_channel_send_buffer_limit(65_536)
            .build()
            .await?;
        let data = pc
            .create_data_channel(
                "desktop-input",
                Some(webrtc::data_channel::RTCDataChannelInit {
                    negotiated: Some(0),
                    ..Default::default()
                }),
            )
            .await?;
        Ok(Self {
            data,
            connection: Arc::new(pc),
            events,
            feedback: rx,
            failed,
            local,
            candidates: 0,
        })
    }
    pub(super) async fn description(&self, answer: bool) -> Result<serde_json::Value, MediaError> {
        let description = if answer {
            self.connection.create_answer(None).await?
        } else {
            self.connection.create_offer(None).await?
        };
        if description.sdp.len() > 8192 {
            return Err(MediaError::Protocol);
        }
        self.connection
            .set_local_description(description.clone())
            .await?;
        serde_json::to_value(description).map_err(|_| MediaError::Protocol)
    }
    pub(super) async fn remote(
        &self,
        value: serde_json::Value,
        answer: bool,
    ) -> Result<(), MediaError> {
        let description: RTCSessionDescription =
            serde_json::from_value(value).map_err(|_| MediaError::Protocol)?;
        validate_sdp(&description, answer, self.local).inspect_err(|_| {
            eprintln!(
                "runtime-host: desktop SDP rejected (local={}, answer={answer})",
                self.local
            );
        })?;
        self.connection.set_remote_description(description).await?;
        Ok(())
    }
    pub(super) async fn candidate(&mut self, value: serde_json::Value) -> Result<(), MediaError> {
        self.candidates += 1;
        if self.candidates > 64 || value.to_string().len() > 1024 {
            return Err(MediaError::Protocol);
        }
        let candidate: RTCIceCandidateInit =
            serde_json::from_value(value).map_err(|_| MediaError::Protocol)?;
        // WebView 可能枚举网卡/mDNS；本地媒体 socket 不向这些地址发包。
        if candidate.candidate.is_empty()
            || (self.local && !loopback_candidate(&candidate.candidate))
        {
            return Ok(());
        }
        if candidate
            .candidate
            .split_ascii_whitespace()
            .any(|s| s == "relay")
        {
            return Err(MediaError::Protocol);
        }
        self.connection.add_ice_candidate(candidate).await?;
        Ok(())
    }
    pub(super) async fn verify_path(&self) -> Result<(), MediaError> {
        // 直接读取已选 ICE 路径；统计表不包含浏览器动态发现的 peer-reflexive 候选。
        let transceivers = self.connection.get_transceivers().await;
        let transceiver = transceivers.first().ok_or(MediaError::Protocol)?;
        let mut transport = if let Some(sender) = transceiver.sender().await? {
            sender.transport().await?
        } else {
            None
        };
        if transport.is_none()
            && let Some(receiver) = transceiver.receiver().await?
        {
            transport = receiver.transport().await?;
        }
        let transport = transport.ok_or(MediaError::Protocol)?;
        let pair = transport
            .ice_transport()
            .get_selected_candidate_pair()
            .await?
            .ok_or(MediaError::Protocol)?;
        for candidate in [pair.local(), pair.remote()] {
            if candidate.typ == webrtc::peer_connection::RTCIceCandidateType::Relay
                || (self.local
                    && !candidate
                        .address
                        .parse::<IpAddr>()
                        .is_ok_and(|address| address.is_loopback()))
            {
                return Err(MediaError::Protocol);
            }
        }
        Ok(())
    }
    pub(super) async fn close(&self) -> Result<(), MediaError> {
        self.connection.close().await?;
        Ok(())
    }
}
fn loopback_candidate(candidate: &str) -> bool {
    candidate
        .split_ascii_whitespace()
        .nth(4)
        .and_then(|ip| ip.parse::<IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback())
}
fn validate_sdp(sdp: &RTCSessionDescription, answer: bool, local: bool) -> Result<(), MediaError> {
    if sdp.sdp.len() > 8192
        || sdp.sdp_type
            != if answer {
                RTCSdpType::Answer
            } else {
                RTCSdpType::Offer
            }
    {
        return Err(MediaError::Protocol);
    }
    // 单路视频及可靠有序输入通道；其余媒体拒绝。
    // 只接收协商的 baseline 5.0、packetization-mode 1。不能让内嵌候选绕过逐条 ICE 核验。
    let lines: Vec<_> = sdp.sdp.lines().collect();
    if lines.iter().filter(|line| line.starts_with("m=")).count() != 2
        || !lines
            .iter()
            .any(|line| line.starts_with("m=video ") && !line.starts_with("m=video 0 "))
    {
        return Err(MediaError::Protocol);
    }
    if !lines
        .iter()
        .any(|line| line.starts_with("m=application ") && !line.starts_with("m=application 0 "))
    {
        return Err(MediaError::Protocol);
    }
    if !lines.iter().any(|line| {
        line.starts_with("a=fmtp:")
            && line.contains("profile-level-id=42e032")
            && line.contains("packetization-mode=1")
    }) {
        return Err(MediaError::Protocol);
    }
    if lines.iter().any(|line| {
        line.starts_with("a=candidate:")
            && ((local && !loopback_candidate(&line[2..]))
                || line.split_ascii_whitespace().any(|s| s == "relay"))
    }) {
        return Err(MediaError::Protocol);
    }
    Ok(())
}
pub(super) fn stun_servers(value: &str) -> Result<Vec<RTCIceServer>, MediaError> {
    if value.len() > 2048 {
        return Err(MediaError::Protocol);
    }
    let urls: Vec<_> = value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if urls.len() > 4
        || urls
            .iter()
            .any(|s| !s.starts_with("stun:") || s.contains('@'))
    {
        return Err(MediaError::Protocol);
    }
    if urls.is_empty() {
        return Ok(vec![]);
    }
    let server = RTCIceServer {
        urls,
        ..Default::default()
    };
    server.urls()?;
    Ok(vec![server])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtc::{
        interceptor::{AttributedPacket, Packet, TaggedPacket},
        sansio::Protocol,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn initial_pacing_can_carry_hd_video_without_waiting_for_a_rate_change() {
        let mut engine = MediaEngine::default();
        engine.register_codec(codec(), RtpCodecKind::Video).unwrap();
        let mut chain = media_interceptors(&mut engine).unwrap().build();
        let epoch = Instant::now();
        // Queue more than 100ms of HD video, before any remote feedback exists.
        for sequence_number in 0..320 {
            chain
                .handle_write(TaggedPacket {
                    now: epoch,
                    transport: Default::default(),
                    message: AttributedPacket::new(Packet::Rtp(rtc::rtp::Packet {
                        header: rtc::rtp::header::Header {
                            version: 2,
                            payload_type: 102,
                            sequence_number,
                            ssrc: 10001,
                            ..Default::default()
                        },
                        payload: vec![0u8; 1188].into(),
                    })),
                })
                .unwrap();
            while chain.poll_write().is_some() {}
        }
        // The pacer releases on timeout. Discard its initial burst before measuring.
        chain.handle_timeout(epoch).unwrap();
        while chain.poll_write().is_some() {}
        let mut released = 0;
        for millis in 1..=100 {
            chain
                .handle_timeout(epoch + Duration::from_millis(millis))
                .unwrap();
            while let Some(packet) = chain.poll_write() {
                released += usize::from(matches!(packet.message.packet, Packet::Rtp(_)));
            }
        }
        // 8Mbps permits about 83 packets; the library default of 1Mbps only permits 10.
        // An upper bound also guards against accidentally removing pacing altogether.
        assert!(
            (75..=100).contains(&released),
            "released {released} packets"
        );
    }

    #[test]
    fn local_candidates_and_stun_never_admit_relay() {
        assert!(loopback_candidate(
            "candidate:1 1 udp 1 127.0.0.1 123 typ host"
        ));
        assert!(!loopback_candidate(
            "candidate:1 1 udp 1 192.0.2.1 123 typ host"
        ));
        assert!(!loopback_candidate(
            "candidate:1 1 udp 1 device.local 123 typ host"
        ));
        assert!(stun_servers("").unwrap().is_empty());
        assert!(stun_servers("stun:stun.example.com:3478").is_ok());
        assert!(stun_servers("turn:relay.example.com:3478").is_err());
    }
    #[test]
    fn embedded_nonlocal_candidate_and_other_media_are_rejected() {
        let sdp = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=fmtp:102 profile-level-id=42e032;packetization-mode=1\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n";
        let answer = |s: String| {
            serde_json::from_value::<RTCSessionDescription>(
                serde_json::json!({"type":"answer","sdp":s}),
            )
            .unwrap()
        };
        assert!(validate_sdp(&answer(sdp.into()), true, true).is_ok());
        assert!(
            validate_sdp(
                &answer(format!(
                    "{sdp}a=candidate:1 1 udp 1 192.0.2.1 123 typ host\r\n"
                )),
                true,
                true
            )
            .is_err()
        );
        assert!(
            validate_sdp(
                &answer(format!(
                    "{sdp}m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n"
                )),
                true,
                true
            )
            .is_err()
        );
    }
}
