use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

struct LocalResponseServer {
    url: String,
    task: thread::JoinHandle<()>,
}

impl LocalResponseServer {
    fn join(self) {
        self.task.join().expect("local response server should stop");
    }
}

/// 启动只服务一个请求的本地 chunked HTTP server。
///
/// 每个 `(delay, bytes)` 的 delay 都发生在对应 chunk 写入前，便于分别验证响应
/// 建立超时、相邻 chunk 空闲超时和不受总时长限制的持续流。
fn local_chunked_response(
    header_delay: Duration,
    chunks: Vec<(Duration, &'static [u8])>,
) -> LocalResponseServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local response server");
    let address = listener.local_addr().expect("local response address");
    let task = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept local request");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        thread::sleep(header_delay);
        if stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .is_err()
        {
            return;
        }
        for (delay, chunk) in chunks {
            thread::sleep(delay);
            let frame = format!("{:X}\r\n", chunk.len());
            if stream.write_all(frame.as_bytes()).is_err()
                || stream.write_all(chunk).is_err()
                || stream.write_all(b"\r\n").is_err()
                || stream.flush().is_err()
            {
                return;
            }
        }
        let _ = stream.write_all(b"0\r\n\r\n");
        let _ = stream.flush();
    });
    LocalResponseServer {
        url: format!("http://{address}/stream"),
        task,
    }
}

fn local_transport_request(url: String) -> TransportRequest {
    TransportRequest {
        trace: None,
        method: "GET".to_owned(),
        url,
        headers: Vec::new(),
        body: Vec::new(),
    }
}

#[tokio::test]
async fn encode_failure_fails_before_any_request() {
    // base Profile 不支持 reasoning effort：编码阶段以 Config 失败，Transport 无请求。
    let transport = Arc::new(RecordedTransport::new([]));
    let service = service_with(&transport);
    let mut request = simple_request();
    request.reasoning = Some(ReasoningConfig {
        effort: Some(ReasoningEffort::High),
    });
    let error = service
        .stream(request, ModelCallContext::default())
        .await
        .err()
        .expect("encode must fail");
    assert!(matches!(error, ModelError::Config(_)));
    assert!(transport.take_requests().is_empty());
}

#[tokio::test]
async fn connect_failure_returns_transport_err_without_leaking() {
    let transport = Arc::new(RecordedTransport::new([Err(
        RecordedTransportError::Connect("connection refused".to_owned()),
    )]));
    let service = service_with(&transport);
    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("connect must fail");
    let ModelError::Transport { kind, message } = error else {
        panic!("expected transport error, got {error:?}");
    };
    assert_eq!(kind, ModelTransportErrorKind::Connection);
    assert!(message.contains("connection refused"));
    assert!(!message.contains(TOKEN));
    assert!(!message.contains(USER_MARKER));
}

#[tokio::test]
async fn timeout_returns_transport_err() {
    let service = OpenAiCompatibleService::with_transport(
        BASE_URL,
        BearerCredential::new(TOKEN),
        "gpt-test",
        128_000,
        base_profile(),
        Arc::new(StubTransport {
            error: TransportError::Timeout,
        }),
    )
    .expect("test base URL should be valid");
    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("timeout must fail");
    assert_eq!(
        error,
        ModelError::Transport {
            kind: ModelTransportErrorKind::Timeout,
            message: "request timed out".to_owned(),
        }
    );
}

#[tokio::test]
async fn reqwest_timeout_bounds_response_establishment() {
    let server = local_chunked_response(Duration::from_millis(250), Vec::new());
    let transport = ReqwestTransport::with_timeouts(TransportTimeouts {
        connect: Duration::from_millis(50),
        request: Duration::from_millis(75),
    })
    .expect("transport should build");

    let result = transport
        .execute(local_transport_request(server.url.clone()))
        .await;

    assert!(matches!(result, Err(TransportError::Timeout)));
    server.join();
}

#[tokio::test]
async fn reqwest_stream_can_outlive_request_timeout_while_chunks_keep_arriving() {
    let server = local_chunked_response(
        Duration::ZERO,
        vec![
            (Duration::ZERO, b"a"),
            (Duration::from_millis(120), b"b"),
            (Duration::from_millis(120), b"c"),
        ],
    );
    let transport = ReqwestTransport::with_timeouts(TransportTimeouts {
        connect: Duration::from_millis(100),
        request: Duration::from_millis(200),
    })
    .expect("transport should build");
    let mut response = transport
        .execute(local_transport_request(server.url.clone()))
        .await
        .expect("response should establish");
    let mut received = Vec::new();

    while let Some(item) = response.body.next().await {
        received.extend(item.expect("each chunk should arrive before the idle timeout"));
    }

    assert_eq!(received, b"abc");
    server.join();
}

#[tokio::test]
async fn reqwest_stream_times_out_after_a_chunk_goes_idle() {
    let server = local_chunked_response(
        Duration::ZERO,
        vec![
            (Duration::ZERO, b"a"),
            (Duration::from_millis(250), b"late"),
        ],
    );
    let transport = ReqwestTransport::with_timeouts(TransportTimeouts {
        connect: Duration::from_millis(50),
        request: Duration::from_millis(75),
    })
    .expect("transport should build");
    let mut response = transport
        .execute(local_transport_request(server.url.clone()))
        .await
        .expect("response should establish");

    assert_eq!(
        response.body.next().await.expect("first body item"),
        Ok(b"a".to_vec())
    );
    assert_eq!(
        response.body.next().await.expect("timeout body item"),
        Err(TransportError::Timeout)
    );
    assert!(response.body.next().await.is_none());
    drop(response);
    server.join();
}

#[tokio::test]
async fn http_401_and_403_map_to_auth() {
    let transport = Arc::new(RecordedTransport::new([
        Ok(RecordedResponse::new(
            401,
            r#"{"error":{"message":"invalid api key","type":"authentication_error"}}"#,
        )),
        Ok(RecordedResponse::new(403, "")),
    ]));
    let service = service_with(&transport);

    // 401 + 结构化错误正文：Auth，消息取自正文。
    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("401 must fail");
    assert_eq!(error, ModelError::Auth("invalid api key".to_owned()));

    // 403 无正文：Auth，默认文案带状态码。
    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("403 must fail");
    let ModelError::Auth(message) = error else {
        panic!("expected auth error, got {error:?}");
    };
    assert!(message.contains("403"));
    assert!(!message.contains(TOKEN));
}

#[tokio::test]
async fn http_429_maps_to_rate_limited() {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        429,
        r#"{"error":{"message":"slow down","type":"rate_limit_error"}}"#,
    )
    .with_header("Retry-After", "2"))]));
    let service = service_with(&transport);
    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("429 must fail");
    assert_eq!(
        error,
        ModelError::RateLimited {
            message: "slow down".to_owned(),
            retry_after_ms: Some(2_000),
        }
    );
}

#[tokio::test]
async fn http_500_maps_to_unavailable_with_status() {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        500,
        "upstream exploded: BODY-MARKER",
    ))]));
    let service = service_with(&transport);
    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("500 must fail");
    let ModelError::Unavailable {
        message,
        status,
        retry_after_ms,
    } = error
    else {
        panic!("expected unavailable error, got {error:?}");
    };
    assert_eq!(status, Some(500));
    assert_eq!(retry_after_ms, None);
    // 非结构化正文不进入错误文本。
    assert!(!message.contains("BODY-MARKER"));
}

#[tokio::test]
async fn http_408_425_and_5xx_map_to_unavailable() {
    let transport = Arc::new(RecordedTransport::new([
        Ok(RecordedResponse::new(408, "")),
        Ok(RecordedResponse::new(425, "")),
        Ok(
            RecordedResponse::new(503, r#"{"error":{"message":"service overloaded"}}"#)
                .with_header("retry-after", " 3 "),
        ),
    ]));
    let service = service_with(&transport);

    for (status, expected_message, retry_after_ms) in [
        (408, "provider temporarily unavailable (status 408)", None),
        (425, "provider temporarily unavailable (status 425)", None),
        (503, "service overloaded", Some(3_000)),
    ] {
        let error = service
            .stream(simple_request(), ModelCallContext::default())
            .await
            .err()
            .expect("status must fail");
        assert_eq!(
            error,
            ModelError::Unavailable {
                message: expected_message.to_owned(),
                status: Some(status),
                retry_after_ms,
            }
        );
    }
}

#[tokio::test]
async fn retry_after_rejects_invalid_negative_overflow_and_missing_values() {
    let transport = Arc::new(RecordedTransport::new([
        Ok(RecordedResponse::new(429, "").with_header("retry-after", "tomorrow")),
        Ok(RecordedResponse::new(429, "").with_header("retry-after", "-1")),
        Ok(RecordedResponse::new(429, "").with_header("retry-after", u64::MAX.to_string())),
        Ok(RecordedResponse::new(429, "")),
    ]));
    let service = service_with(&transport);

    for _ in 0..4 {
        let error = service
            .stream(simple_request(), ModelCallContext::default())
            .await
            .err()
            .expect("429 must fail");
        assert!(matches!(
            error,
            ModelError::RateLimited {
                retry_after_ms: None,
                ..
            }
        ));
    }
}

#[tokio::test]
async fn structured_error_body_refines_classification() {
    let transport = Arc::new(RecordedTransport::new([
        Ok(RecordedResponse::new(
            500,
            r#"{"error":{"message":"quota exhausted","type":"rate_limit_error"}}"#,
        )),
        Ok(RecordedResponse::new(
            400,
            r#"{"error":{"message":"model is overloaded","type":"invalid_request_error"}}"#,
        )),
    ]));
    let service = service_with(&transport);

    // 5xx 首先表达 Provider 暂时不可用，不被正文中的类型降级为普通限流。
    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("500 must fail");
    assert_eq!(
        error,
        ModelError::Unavailable {
            message: "quota exhausted".to_owned(),
            status: Some(500),
            retry_after_ms: None,
        }
    );

    // 400 + 未知类型：Provider 并补上状态码。
    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("400 must fail");
    assert_eq!(
        error,
        ModelError::Provider {
            message: "model is overloaded".to_owned(),
            status: Some(400),
        }
    );
}

#[tokio::test]
async fn structured_context_overflow_is_an_establishment_error() {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        400,
        CONTEXT_OVERFLOW_BODY,
    ))]));
    let service = service_with(&transport);

    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("context overflow must fail before stream establishment");
    assert_eq!(
        error,
        ModelError::ContextOverflow {
            message: "request is too large".to_owned(),
        }
    );
}

#[tokio::test]
async fn oversized_error_body_is_sampled_without_leaking() {
    // 超过采样上限的结构化正文被截断后按无结构化正文处理，且正文内容不外泄。
    let huge_message = "x".repeat(4096);
    let body = format!(r#"{{"error":{{"message":"{huge_message}"}}}}"#);
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        500, body,
    ))]));
    let service = service_with(&transport);
    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("500 must fail");
    assert_eq!(
        error,
        ModelError::Unavailable {
            message: "provider temporarily unavailable (status 500)".to_owned(),
            status: Some(500),
            retry_after_ms: None,
        }
    );
    assert!(!error.to_string().contains("xxxx"));
}

#[tokio::test]
async fn cancellation_before_establishment_returns_err_without_a_request() {
    let gate = CancelGate::new();
    gate.cancel_now();
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        OK_SSE_BODY,
    ))]));
    let service = service_with(&transport);
    let error = service
        .stream(simple_request(), ModelCallContext::new(gate.token()))
        .await
        .err()
        .expect("cancelled before establishment");
    assert_eq!(error, ModelError::Cancelled);
    assert!(transport.take_requests().is_empty());
}

#[test]
fn reqwest_transport_and_service_build_with_explicit_timeouts() {
    // 默认与自定义超时都能构造；ReqwestTransport 满足 Transport 边界。
    let transport: Arc<dyn Transport> =
        Arc::new(ReqwestTransport::new().expect("default transport builds"));
    drop(transport);
    let custom = TransportTimeouts {
        connect: Duration::from_secs(1),
        request: Duration::from_secs(2),
    };
    ReqwestTransport::with_timeouts(custom).expect("custom transport builds");
    let defaults = TransportTimeouts::default();
    assert!(defaults.connect < defaults.request);
    let service = OpenAiCompatibleService::new(
        BASE_URL,
        BearerCredential::new(TOKEN),
        "gpt-test",
        128_000,
        base_profile(),
        defaults,
    )
    .expect("service builds on reqwest transport");
    assert!(!service.capabilities().reasoning);
    assert!(service.capabilities().tool_calls);
    assert!(service.capabilities().streaming);
    assert_eq!(service.context_window_tokens(), 128_000);
}

#[test]
fn transport_request_debug_redacts_authorization_and_body() {
    let request = TransportRequest {
        trace: None,
        method: "POST".to_owned(),
        url: format!("{BASE_URL}/chat/completions"),
        headers: vec![
            ("authorization".to_owned(), format!("Bearer {TOKEN}")),
            ("content-type".to_owned(), "application/json".to_owned()),
        ],
        body: b"{\"secret\":\"BODY-MARKER\"}".to_vec(),
    };
    let debug = format!("{request:?}");
    assert!(debug.contains("<redacted>"));
    assert!(debug.contains("application/json"));
    assert!(!debug.contains(TOKEN));
    assert!(!debug.contains("BODY-MARKER"));
}

#[test]
fn transport_error_display_is_sanitized() {
    assert_eq!(TransportError::Timeout.to_string(), "request timed out");
    let connect = TransportError::Connect("connection refused".to_owned());
    assert_eq!(
        connect.to_string(),
        "request failed before the response started: connection refused"
    );
    let interrupted = TransportError::Interrupted("connection reset".to_owned());
    assert_eq!(
        interrupted.to_string(),
        "response stream was interrupted: connection reset"
    );
}
