use super::*;
use axum::{Router, body::Body, routing::post};
use futures_util::stream;
use tokio::task::JoinHandle;

async fn fixture(
    status: StatusCode,
    content_type: &'static str,
    bytes: Vec<u8>,
    pending_tail: bool,
) -> (Arc<str>, JoinHandle<io::Result<()>>) {
    let app = Router::new().route(
        "/mcp",
        post(move || {
            let bytes = bytes.clone();
            async move {
                // 用流式 Body 强制覆盖没有 Content-Length 的分块响应。
                let first = stream::once(async { Ok::<_, io::Error>(bytes) });
                let tail = stream::poll_fn(move |_| {
                    if pending_tail {
                        std::task::Poll::Pending
                    } else {
                        std::task::Poll::Ready(None)
                    }
                });
                axum::response::Response::builder()
                    .status(status)
                    .header(CONTENT_TYPE, content_type)
                    .body(Body::from_stream(first.chain(tail)))
                    .expect("fixture response")
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let uri = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    (uri.into(), task)
}

async fn stop(task: JoinHandle<io::Result<()>>) {
    task.abort();
    let error = task.await.expect_err("fixture aborted");
    assert!(error.is_cancelled());
}

#[tokio::test]
async fn rejects_oversized_chunked_json_and_error_without_waiting_for_eof() {
    for status in [StatusCode::OK, StatusCode::INTERNAL_SERVER_ERROR] {
        let (uri, task) = fixture(
            status,
            "application/json",
            vec![b' '; MAX_MESSAGE_BYTES + 1],
            true,
        )
        .await;
        let client = BoundedHttpClient(Client::new());
        let message = serde_json::from_value(
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .expect("request");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.post_message(uri, message, None, None, HashMap::new()),
        )
        .await;
        stop(task).await;
        assert!(matches!(
            result.expect("must reject before EOF"),
            Err(StreamableHttpError::Io(_))
        ));
    }
}

#[tokio::test]
async fn json_body_accepts_exact_limit_and_rejects_one_extra_byte() {
    for size in [16, 17] {
        let (uri, task) =
            fixture(StatusCode::OK, "application/json", vec![b' '; size], false).await;
        let response = Client::new()
            .post(uri.as_ref())
            .send()
            .await
            .expect("response");
        let result = bounded_body(response, 16).await;
        stop(task).await;
        assert_eq!(result.is_ok(), size == 16);
    }
}

#[tokio::test]
async fn sse_limits_individual_events_but_not_the_whole_stream() {
    for (body, succeeds) in [
        (b"data: 1\r\n\r\ndata: 2\r\n\r\n".to_vec(), true),
        (b"data: 12345678901234567890\n\n".to_vec(), false),
    ] {
        let (uri, task) = fixture(StatusCode::OK, "text/event-stream", body, false).await;
        let response = Client::new()
            .post(uri.as_ref())
            .send()
            .await
            .expect("response");
        let result = bounded_sse(response, 16).try_collect::<Vec<_>>().await;
        stop(task).await;
        assert_eq!(result.is_ok(), succeeds);
        if let Ok(events) = result {
            assert_eq!(events.len(), 2);
        }
    }
}

#[tokio::test]
async fn only_handshake_sse_is_normalized_and_preserves_error_identity() {
    for method in ["server/discover", "initialize", "tools/call"] {
        let request = match method {
            "initialize" => serde_json::json!({"jsonrpc":"2.0","id":17,"method":method,"params":{
                "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}
            }}),
            "tools/call" => {
                serde_json::json!({"jsonrpc":"2.0","id":17,"method":method,"params":{"name":"fixture"}})
            }
            _ => serde_json::json!({"jsonrpc":"2.0","id":17,"method":method,"params":{}}),
        };
        let error = serde_json::json!({"jsonrpc":"2.0","id":17,"error":{"code":-32601,"message":"fixture error"}});
        let body = format!(": keepalive\n\nevent: message\ndata: {error}\n\n");
        let (uri, task) = fixture(
            StatusCode::OK,
            "text/event-stream",
            body.into_bytes(),
            false,
        )
        .await;
        let response = BoundedHttpClient(Client::new())
            .post_message(
                uri,
                serde_json::from_value(request).expect("request"),
                None,
                None,
                HashMap::new(),
            )
            .await;
        stop(task).await;
        if method == "tools/call" {
            assert!(matches!(
                response.expect("tool stream"),
                StreamableHttpPostResponse::Sse(..)
            ));
        } else {
            let StreamableHttpPostResponse::Json(message, _) =
                response.expect("handshake error response")
            else {
                panic!("handshake must use the JSON response branch");
            };
            assert_eq!(serde_json::to_value(message).expect("message"), error);
        }
    }
}

#[tokio::test]
async fn handshake_sse_rejects_empty_malformed_and_oversized_responses() {
    for (body, pending) in [
        (b": keepalive\n\n".to_vec(), false),
        (b"data: invalid-json\n\n".to_vec(), false),
        (
            [b"data: ".as_slice(), &vec![b'x'; MAX_MESSAGE_BYTES]].concat(),
            true,
        ),
    ] {
        let (uri, task) = fixture(StatusCode::OK, "text/event-stream", body, pending).await;
        let request = serde_json::from_value(
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{}}),
        )
        .expect("request");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            BoundedHttpClient(Client::new()).post_message(uri, request, None, None, HashMap::new()),
        )
        .await;
        stop(task).await;
        assert!(result.expect("must reject before EOF").is_err());
    }
}
