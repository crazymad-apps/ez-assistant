use super::*;
use serde_json::json;
#[test]
fn target_is_unique_explicit_and_never_a_url() {
    let id = "00112233-4455-4677-8899-aabbccddeeff";
    let request = Request::builder()
        .uri(format!("/commands?target_device={id}"))
        .header(DEVICE_HEADER, id)
        .body(Body::empty())
        .unwrap();
    assert_eq!(target(&request).unwrap(), Some(id.into()));
    for query in [
        "target_device=",
        "target_device=http%3A%2F%2Flocalhost",
        &format!("target_device={id}&target_device={id}"),
    ] {
        let request = Request::builder()
            .uri(format!("/commands?{query}"))
            .body(Body::empty())
            .unwrap();
        assert!(target(&request).is_err());
    }
    assert!(target(&Request::new(Body::empty())).unwrap().is_none());
}
#[test]
fn command_gate_rejects_global_management_and_allows_scoped_permissions() {
    let request = |scope: &str, command| {
        serde_json::from_value::<CommandRequest>(
            json!({"request_id":"test","command":{"scope":scope,"payload":command}}),
        )
        .unwrap()
    };
    assert!(allowed::allowed(&request(
        "runtime",
        json!({"type":"list_workspaces","payload":{}})
    )));
    assert!(allowed::allowed(&request(
        "runtime",
        json!({"type":"prepare_custom_agent","payload":{"selection":{"name":"test","source":{"type":"personal"}}}})
    )));
    assert!(!allowed::allowed(&request(
        "runtime",
        json!({"type":"shutdown_runtime","payload":{}})
    )));
    assert!(!allowed::allowed(&request(
        "host_access",
        json!({"type":"get_status"})
    )));
    assert!(!allowed::allowed(&request(
        "runtime",
        json!({"type":"get_permission_document","payload":{"scope":{"type":"global"}}})
    )));
    assert!(allowed::allowed(&request(
        "runtime",
        json!({"type":"get_permission_document","payload":{"scope":{"type":"session","payload":{"session_id":"test"}}}})
    )));
}
#[test]
fn response_header_filter_does_not_leak_target_credentials() {
    let mut headers = HeaderMap::new();
    headers.insert("set-cookie", "secret=value".parse().unwrap());
    headers.insert("location", "http://target/private".parse().unwrap());
    headers.insert("content-type", "application/json".parse().unwrap());
    assert_eq!(
        filtered_headers(&headers),
        vec![("content-type".into(), "application/json".into())]
    );
}

#[test]
fn only_existing_business_routes_cross_the_bridge() {
    use paths::{Route, route};
    assert_eq!(route("GET", "/events"), Some(Route::Events));
    assert_eq!(
        route("POST", "/session-materializations"),
        Some(Route::Files)
    );
    assert_eq!(
        route(
            "GET",
            "/sessions/s/child-tasks/c/messages/m/resources/r/download"
        ),
        Some(Route::Media)
    );
    for path in [
        "/auth/session",
        "/host-network/events",
        "/plugins/inspect",
        "/user-terminals/socket",
        "/sessions/s/resource-files/native-path",
        "/sessions/s/messages/m/resources/r/native-path",
        "/sessions/%2e%2e/export.md",
        "/sessions/../export.md",
        "/events?target_device=other",
        "https://host/events",
    ] {
        assert_eq!(route("GET", path), None, "{path}");
        assert_eq!(route("POST", path), None, "{path}");
    }
}

#[test]
fn request_headers_keep_multipart_and_ranges_without_credentials() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        "multipart/form-data; boundary=fixture".parse().unwrap(),
    );
    headers.insert("range", "bytes=0-100".parse().unwrap());
    headers.insert("authorization", "Bearer secret".parse().unwrap());
    headers.insert("cookie", "private=value".parse().unwrap());
    headers.insert("host", "source".parse().unwrap());
    let values = paths::headers(&headers).unwrap();
    let restored = paths::restore(&values).unwrap();
    assert_eq!(restored.len(), 2);
    assert_eq!(restored["content-type"], headers["content-type"]);
    headers.insert("connection", "content-type".parse().unwrap());
    assert!(paths::headers(&headers).is_err());
    assert!(paths::restore(&[("cookie".into(), "forged".into())]).is_err());
}

use crate::access::registration::network::streams::Streams;
use tokio_util::sync::CancellationToken;
const ACCESS: &str = "00112233-4455-4677-8899-aabbccddeeff";
fn receive_frame(streams: &Streams, id: u64, kind: u8, payload: &[u8]) {
    let mut wire = vec![
        2, kind, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
        0xdd, 0xee, 0xff,
    ];
    wire.extend(id.to_be_bytes());
    wire.extend((payload.len() as u32).to_be_bytes());
    wire.extend(payload);
    streams.receive(&wire).unwrap();
}
async fn opened() -> (Arc<Streams>, Arc<Flow>) {
    let streams = Streams::new();
    let flow = Arc::new(
        streams
            .open(
                ACCESS.into(),
                CancellationToken::new(),
                crate::access::registration::network::streams::Kind::Http,
            )
            .unwrap(),
    );
    streams
        .control(&json!({"type":"stream_ready","access_id":ACCESS,"stream_id":flow.id.to_string()}))
        .unwrap();
    receive_frame(&streams, flow.id, 2, b"{}");
    flow.recv().await.unwrap();
    (streams, flow)
}
#[tokio::test]
async fn content_length_does_not_mask_a_missing_fin() {
    let (streams, flow) = opened().await;
    receive_frame(&streams, flow.id, 3, b"bytes");
    let mut body = transfer::incoming(flow, Some(5), None, true).into_data_stream();
    assert_eq!(body.next().await.unwrap().unwrap(), "byte");
    streams.close();
    assert!(body.next().await.unwrap().is_err());
}
#[tokio::test]
async fn response_fin_ends_an_early_rejection_without_waiting_for_upload() {
    let (streams, flow) = opened().await;
    receive_frame(&streams, flow.id, 3, b"denied");
    receive_frame(&streams, flow.id, 4, b"");
    let upload: transfer::Upload = Some(Box::pin(std::future::pending()));
    let bytes = timeout(
        Duration::from_secs(1),
        to_bytes(transfer::incoming(flow, Some(6), upload, true), 100),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(bytes, "denied");
}
#[tokio::test(start_paused = true)]
async fn idle_sse_is_not_a_failed_or_completed_response() {
    let (streams, flow) = opened().await;
    let id = flow.id;
    let mut body = transfer::incoming(flow, None, None, false).into_data_stream();
    assert!(timeout(Duration::from_secs(65), body.next()).await.is_err());
    receive_frame(
        &streams,
        id,
        3,
        b"event: stream_gap\ndata: {\"dropped_events\":2}\n\n",
    );
    receive_frame(&streams, id, 4, b"");
    assert!(
        std::str::from_utf8(&body.next().await.unwrap().unwrap())
            .unwrap()
            .contains("stream_gap")
    );
    assert!(body.next().await.is_none());
}
