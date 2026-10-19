use super::*;
const ACCESS: &str = "00112233-4455-4677-8899-aabbccddeeff";
#[test]
fn binary_roundtrip_preserves_u64_and_rejects_bad_lengths() {
    let frame = Frame {
        kind: 3,
        payload: vec![0, 255, 1],
    };
    let bytes = frame::encode(ACCESS, u64::MAX, &frame).unwrap();
    let (access, id, decoded) = frame::decode(&bytes).unwrap();
    assert_eq!(access, ACCESS);
    assert_eq!(id, u64::MAX);
    assert_eq!(decoded.payload, frame.payload);
    assert!(frame::decode(&bytes[..bytes.len() - 1]).is_err());
    let mut bad = bytes;
    bad[1] = 11;
    assert!(frame::decode(&bad).is_err());
    assert!(control_key(&json!({"access_id":ACCESS,"stream_id":"01"})).is_err());
}
#[tokio::test]
async fn readiness_credit_and_fin_are_independent() {
    let streams = Streams::new();
    let flow = streams
        .open(
            ACCESS.into(),
            CancellationToken::new(),
            crate::access::registration::network::streams::Kind::Http,
        )
        .unwrap();
    let open = streams.outgoing().await;
    assert!(matches!(open.message, Message::Text(_)));
    streams
        .control(&json!({"type":"stream_ready","access_id":ACCESS,"stream_id":flow.id.to_string()}))
        .unwrap();
    flow.ready().await.unwrap();
    flow.send(Frame {
        kind: 1,
        payload: b"{}".to_vec(),
    })
    .await
    .unwrap();
    flow.send(Frame {
        kind: 4,
        payload: vec![],
    })
    .await
    .unwrap();
    let packet = streams.outgoing().await;
    assert!(!packet.fin);
    streams.written(&packet);
    let packet = streams.outgoing().await;
    assert!(packet.fin);
    streams.written(&packet);
    streams
        .receive(
            &frame::encode(
                ACCESS,
                flow.id,
                &Frame {
                    kind: 2,
                    payload: b"{}".to_vec(),
                },
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(flow.recv().await.unwrap().kind, 2);
    assert!(!streams.state.lock().unwrap().slots[&(ACCESS.into(), flow.id)].received_fin);
    streams
        .receive(
            &frame::encode(
                ACCESS,
                flow.id,
                &Frame {
                    kind: 4,
                    payload: vec![],
                },
            )
            .unwrap(),
        )
        .unwrap();
    flow.recv().await.unwrap();
    flow.finish().await.unwrap();
    drop(flow);
    assert!(streams.state.lock().unwrap().slots.is_empty());
}
#[tokio::test]
async fn revoke_wakes_waiter_and_does_not_end_other_access() {
    let streams = Streams::new();
    let a = streams
        .open(
            ACCESS.into(),
            CancellationToken::new(),
            crate::access::registration::network::streams::Kind::Http,
        )
        .unwrap();
    let b = streams
        .open(
            "11223344-5566-4788-99aa-bbccddeeff00".into(),
            CancellationToken::new(),
            crate::access::registration::network::streams::Kind::Http,
        )
        .unwrap();
    streams.revoke(ACCESS);
    assert!(matches!(a.recv().await, Err(StreamError::Ended)));
    assert!(
        streams.state.lock().unwrap().slots[&(b.access.clone(), b.id)]
            .error
            .is_none()
    );
    streams.close();
    assert!(matches!(b.recv().await, Err(StreamError::Ended)));
}
#[tokio::test]
async fn peer_cannot_overspend_credit_or_send_body_before_head() {
    let streams = Streams::new();
    let flow = streams
        .accept(
            ACCESS.into(),
            1,
            CancellationToken::new(),
            crate::access::registration::network::streams::Kind::Http,
        )
        .unwrap();
    streams
        .receive(
            &frame::encode(
                ACCESS,
                1,
                &Frame {
                    kind: 3,
                    payload: vec![1],
                },
            )
            .unwrap(),
        )
        .unwrap();
    assert!(matches!(flow.recv().await, Err(StreamError::Invalid)));
    assert!(
        streams
            .control(&json!({"type":"stream_credit","access_id":ACCESS,"stream_id":"1","bytes":1}))
            .is_ok()
    );
    assert!(
        streams
            .accept(
                ACCESS.into(),
                1,
                CancellationToken::new(),
                crate::access::registration::network::streams::Kind::Http
            )
            .is_err()
    );
}
#[tokio::test(start_paused = true)]
async fn slow_sender_is_bounded_and_times_out() {
    let streams = Streams::new();
    let flow = streams
        .accept(
            ACCESS.into(),
            1,
            CancellationToken::new(),
            crate::access::registration::network::streams::Kind::Http,
        )
        .unwrap();
    flow.bytes(&vec![7; WINDOW - 2 * frame::HEADER])
        .await
        .unwrap();
    assert!(matches!(flow.bytes(&[7]).await, Err(StreamError::Timeout)));
    assert_eq!(
        streams.state.lock().unwrap().slots[&(ACCESS.into(), 1)]
            .outgoing
            .len(),
        2
    );
}

#[test]
fn cross_language_fixture_is_exact() {
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/network-stream-v2.json"
    )))
    .unwrap();
    let bytes = frame::encode(
        fixture["access"].as_str().unwrap(),
        fixture["stream"].as_str().unwrap().parse().unwrap(),
        &Frame {
            kind: 3,
            payload: vec![0, 255, 1],
        },
    )
    .unwrap();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, fixture["hex"].as_str().unwrap());
}
#[tokio::test]
async fn reset_cancels_only_one_flow_and_releases_capacity() {
    let streams = Streams::new();
    let a = streams
        .accept(
            ACCESS.into(),
            1,
            CancellationToken::new(),
            crate::access::registration::network::streams::Kind::Http,
        )
        .unwrap();
    let b = streams
        .accept(
            ACCESS.into(),
            2,
            CancellationToken::new(),
            crate::access::registration::network::streams::Kind::Http,
        )
        .unwrap();
    streams
        .control(&json!({"type":"stream_reset","access_id":ACCESS,"stream_id":"1"}))
        .unwrap();
    assert!(a.cancelled().is_cancelled());
    assert!(!b.cancelled().is_cancelled());
    drop(a);
    assert_eq!(streams.state.lock().unwrap().slots.len(), 1);
}

#[tokio::test]
async fn terminal_boundaries_close_and_http_isolation() {
    let streams = Streams::new();
    let flow = streams
        .accept(ACCESS.into(), 1, CancellationToken::new(), Kind::Terminal)
        .unwrap();
    for kind in [5, 6, 7, 8, 9, 10] {
        let payload = if kind == 7 { vec![0xff; 32768] } else { vec![] };
        streams
            .receive(
                &frame::encode(
                    ACCESS,
                    1,
                    &Frame {
                        kind,
                        payload: payload.clone(),
                    },
                )
                .unwrap(),
            )
            .unwrap();
        let received = flow.recv().await.unwrap();
        assert_eq!(received.kind, kind);
        assert_eq!(received.payload, payload);
    }
    streams
        .receive(
            &frame::encode(
                ACCESS,
                1,
                &Frame {
                    kind: 7,
                    payload: vec![1],
                },
            )
            .unwrap(),
        )
        .unwrap();
    assert!(flow.recv().await.is_err());
    let http = streams
        .accept(ACCESS.into(), 2, CancellationToken::new(), Kind::Http)
        .unwrap();
    streams
        .receive(
            &frame::encode(
                ACCESS,
                2,
                &Frame {
                    kind: 5,
                    payload: vec![],
                },
            )
            .unwrap(),
        )
        .unwrap();
    assert!(http.recv().await.is_err());
    for (kind, size) in [
        (5, 16385),
        (6, 16385),
        (7, 32769),
        (8, 126),
        (9, 126),
        (10, 1),
    ] {
        assert!(
            frame::encode(
                ACCESS,
                3,
                &Frame {
                    kind,
                    payload: vec![0; size]
                }
            )
            .is_err()
        );
    }
}
