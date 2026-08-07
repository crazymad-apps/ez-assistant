use super::*;

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
