use super::*;
use std::time::Duration;
use tokio::{io::AsyncWriteExt, time::timeout};
use webrtc::peer_connection::PeerConnection;

#[test]
fn desktop_dimensions_allow_hd_aspect_ratios_without_square_overflow() {
    for (w, h) in [
        (1920, 1080),
        (1920, 1200),
        (1920, 1440),
        (1080, 1920),
        (1440, 1920),
        (1280, 720),
    ] {
        assert!(valid_dimensions(Some(w), Some(h)));
    }
    for (w, h) in [
        (1920, 1920),
        (1922, 1080),
        (1920, 1442),
        (1919, 1080),
        (0, 1080),
    ] {
        assert!(!valid_dimensions(Some(w), Some(h)));
    }
    assert!(!valid_dimensions(None, Some(1080)));
}

#[tokio::test]
async fn media_stack_uses_host_crypto_and_closes() {
    crate::server::initialize_tls_provider();
    struct Events;
    impl webrtc::peer_connection::PeerConnectionEventHandler for Events {}
    let peer = webrtc::peer_connection::PeerConnectionBuilder::new()
        .with_handler(std::sync::Arc::new(Events))
        .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
        .build()
        .await
        .expect("media stack can coexist with Host TLS");
    timeout(Duration::from_secs(3), peer.close())
        .await
        .unwrap()
        .unwrap();
}

#[cfg(unix)]
fn helper(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("helper");
    std::fs::write(&path, format!("#!/bin/sh\n{contents}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    (directory, path)
}

#[cfg(unix)]
#[tokio::test]
async fn capture_stop_does_not_depend_on_video_consumer() {
    let (_directory, path) = helper(
        "printf '%s\\n' '{\"type\":\"configured\",\"codec\":\"h264\",\"width\":1280,\"height\":720,\"fps\":15,\"hardware\":false}' >&2; while IFS= read -r line; do exit 0; done",
    );
    let capture = Capture::start(&path, "1").await.unwrap();
    timeout(Duration::from_secs(2), capture.stop())
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn native_annex_b_frame_fits_media_packetizer() {
    use rtc::rtp::{codec::h264::H264Payloader, packetizer::Payloader};
    let mut data = vec![0, 0, 0, 1, 0x65];
    data.resize(3005, 0x11);
    let packets = H264Payloader::default()
        .payload(1200, &data.into())
        .unwrap();
    assert!(packets.len() > 1);
    assert!(packets.iter().all(|packet| packet.len() <= 1200));
    assert_eq!(packets[0][1] & 0x80, 0x80); // FU-A 起始标志，最后分片必须明确结束。
    assert_eq!(packets.last().unwrap()[1] & 0x40, 0x40);
}

#[cfg(unix)]
#[tokio::test]
async fn enumeration_preserves_native_failure_and_does_not_treat_eof_as_success() {
    let (_directory, path) = helper(
        "printf '%s\\n' '{\"type\":\"error\",\"code\":\"screen_permission_required\"}' >&2; exit 1",
    );
    let result = displays(&path).await;
    assert!(
        matches!(result, Err(MediaError::Unavailable(code)) if code == "screen_permission_required")
    );
    let (_directory, path) = helper("printf '%s\\n' '[]'");
    assert_eq!(displays(&path).await.unwrap(), serde_json::json!([]));
}

#[tokio::test]
async fn frame_reader_rejects_oversize_before_allocating_and_rejects_truncation() {
    let data = ((FRAME_LIMIT + 1) as u32).to_be_bytes();
    assert!(matches!(
        read_frame(&mut data.as_slice()).await,
        Err(MediaError::Protocol)
    ));
    let mut packet = 8u32.to_be_bytes().to_vec();
    packet.extend(1u64.to_be_bytes());
    packet.push(1);
    packet.extend([0, 0, 0, 1, 0x65]);
    assert!(matches!(
        read_frame(&mut packet.as_slice()).await,
        Err(MediaError::Io(_))
    ));
}

#[tokio::test]
async fn status_partial_line_survives_other_ready_frames() {
    let (mut writer, mut reader) = tokio::io::duplex(128);
    writer.write_all(b"{\"type\":").await.unwrap();
    let mut partial = Vec::new();
    // 明确轮询到 Pending：前半行已消费，模拟另一个视频 future 就绪后取消本次状态读取。
    let mut status = Box::pin(read_status(&mut reader, &mut partial));
    assert!(futures_util::poll!(status.as_mut()).is_pending());
    drop(status);
    writer.write_all(b"\"configured\"}\n").await.unwrap();
    assert_eq!(
        read_status(&mut reader, &mut partial).await.unwrap()["type"],
        "configured"
    );
}
